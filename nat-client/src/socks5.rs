//! SOCKS5 (RFC 1928) + HTTP CONNECT 代理服务器
//!
//! 将本地代理连接通过 relay 隧道转发到指定出口 nat-client 节点。
//!
//! 架构：
//! ```text
//! [本地App] → SOCKS5 127.0.0.1:1080 → [nat-client A] → hbbr relay → [nat-client B] → [目标]
//! ```

use crate::config::ClientConfig;
use core_common::{
    config::{CONNECT_TIMEOUT, RENDEZVOUS_PORT},
    log,
    rendezvous_codec,
    rendezvous_proto::{RelayResponse, RendezvousMessage},
    socket_client::{self, connect_tcp},
    ResultType,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

// ──────────────────────────────────────────────────────────────────────────────
// 入口：启动代理服务器
// ──────────────────────────────────────────────────────────────────────────────

/// 启动 SOCKS5 + HTTP CONNECT 代理服务器（永不返回）
pub async fn start_proxy_server(port: u16, exit_peer: String) -> ResultType<()> {
    let addr = format!("127.0.0.1:{}", port);
    let listener = TcpListener::bind(&addr).await?;
    log::info!(
        "[proxy] SOCKS5/HTTP 代理服务器监听 {}，出口节点: {}",
        addr,
        exit_peer
    );

    loop {
        match listener.accept().await {
            Ok((stream, from)) => {
                log::debug!("[proxy] 新代理连接来自 {}", from);
                let peer = exit_peer.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_client(stream, peer).await {
                        log::debug!("[proxy] 代理连接处理失败: {}", e);
                    }
                });
            }
            Err(e) => {
                log::error!("[proxy] accept 错误: {}", e);
            }
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// 客户端分发：自动识别 SOCKS5 / HTTP CONNECT
// ──────────────────────────────────────────────────────────────────────────────

async fn handle_client(mut stream: TcpStream, exit_peer: String) -> ResultType<()> {
    // 读取第一个字节来判断协议
    let mut first = [0u8; 1];
    stream.read_exact(&mut first).await?;

    if first[0] == 0x05 {
        // SOCKS5
        handle_socks5(stream, exit_peer, first[0]).await
    } else {
        // 可能是 HTTP CONNECT — 读取完整第一行
        let mut line = Vec::new();
        line.push(first[0]);

        let mut buf = [0u8; 1];
        loop {
            stream.read_exact(&mut buf).await?;
            line.push(buf[0]);
            if line.len() >= 2
                && line[line.len() - 2] == b'\r'
                && line[line.len() - 1] == b'\n'
            {
                break;
            }
            if line.len() > 8192 {
                return Err(core_common::anyhow::anyhow!("HTTP 请求行过长"));
            }
        }

        let first_line = String::from_utf8_lossy(&line).to_string();
        handle_http_connect(stream, exit_peer, first_line).await
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// SOCKS5 握手（RFC 1928）
// ──────────────────────────────────────────────────────────────────────────────

async fn handle_socks5(
    mut stream: TcpStream,
    exit_peer: String,
    _first_byte: u8,
) -> ResultType<()> {
    // 读取方法数量
    let mut nmethods = [0u8; 1];
    stream.read_exact(&mut nmethods).await?;
    let nmethods = nmethods[0] as usize;

    // 读取方法列表（丢弃，我们只支持 NoAuth）
    let mut methods = vec![0u8; nmethods];
    stream.read_exact(&mut methods).await?;

    // 回复：选择 NoAuth (0x00)
    stream.write_all(&[0x05, 0x00]).await?;

    // 读取请求头
    let mut req_header = [0u8; 4];
    stream.read_exact(&mut req_header).await?;

    let ver = req_header[0];
    let cmd = req_header[1];
    // req_header[2] = reserved
    let atyp = req_header[3];

    if ver != 0x05 {
        return Err(core_common::anyhow::anyhow!("SOCKS5 版本不匹配: {}", ver));
    }

    if cmd != 0x01 {
        // 只支持 CONNECT（0x01）
        stream
            .write_all(&[0x05, 0x07, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
            .await?;
        return Err(core_common::anyhow::anyhow!(
            "不支持的 SOCKS5 命令: {}",
            cmd
        ));
    }

    // 解析目标地址
    let (target_host, target_port) = parse_socks5_addr(&mut stream, atyp).await?;

    log::info!(
        "[proxy] SOCKS5 CONNECT {}:{} 通过出口 {}",
        target_host,
        target_port,
        exit_peer
    );

    // 通过 relay 连接目标
    match connect_via_relay(&exit_peer, &target_host, target_port).await {
        Ok(relay_conn) => {
            // 回复成功
            stream
                .write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                .await?;

            // 双向转发
            bridge_with_relay(stream, relay_conn).await;
        }
        Err(e) => {
            log::warn!("[proxy] relay 连接失败: {}", e);
            // 回复连接拒绝
            stream
                .write_all(&[0x05, 0x05, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                .await
                .ok();
            return Err(e);
        }
    }

    Ok(())
}

async fn parse_socks5_addr(
    stream: &mut TcpStream,
    atyp: u8,
) -> ResultType<(String, u16)> {
    let host = match atyp {
        0x01 => {
            // IPv4: 4 字节
            let mut ip = [0u8; 4];
            stream.read_exact(&mut ip).await?;
            format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3])
        }
        0x03 => {
            // 域名: 1 字节长度 + 域名
            let mut len = [0u8; 1];
            stream.read_exact(&mut len).await?;
            let mut domain = vec![0u8; len[0] as usize];
            stream.read_exact(&mut domain).await?;
            String::from_utf8(domain)
                .map_err(|e| core_common::anyhow::anyhow!("域名解析失败: {}", e))?
        }
        0x04 => {
            // IPv6: 16 字节
            let mut ip = [0u8; 16];
            stream.read_exact(&mut ip).await?;
            format!(
                "{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:\
                 {:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}:{:02x}{:02x}",
                ip[0],
                ip[1],
                ip[2],
                ip[3],
                ip[4],
                ip[5],
                ip[6],
                ip[7],
                ip[8],
                ip[9],
                ip[10],
                ip[11],
                ip[12],
                ip[13],
                ip[14],
                ip[15]
            )
        }
        _ => {
            return Err(core_common::anyhow::anyhow!(
                "不支持的 SOCKS5 地址类型: {}",
                atyp
            ));
        }
    };

    // 读取端口（2 字节大端）
    let mut port_bytes = [0u8; 2];
    stream.read_exact(&mut port_bytes).await?;
    let port = u16::from_be_bytes(port_bytes);

    Ok((host, port))
}

// ──────────────────────────────────────────────────────────────────────────────
// HTTP CONNECT 处理
// ──────────────────────────────────────────────────────────────────────────────

async fn handle_http_connect(
    mut stream: TcpStream,
    exit_peer: String,
    first_line: String,
) -> ResultType<()> {
    // 解析 CONNECT host:port HTTP/1.x
    let first_line = first_line.trim_end().to_owned();
    if !first_line.to_ascii_uppercase().starts_with("CONNECT ") {
        return Err(core_common::anyhow::anyhow!(
            "不支持的 HTTP 方法: {}",
            &first_line[..first_line.len().min(20)]
        ));
    }

    // 吸收剩余请求头（直到空行 \r\n\r\n）
    let mut buf = [0u8; 1];
    let mut tail = [0u8; 4];
    loop {
        stream.read_exact(&mut buf).await?;
        tail[0] = tail[1];
        tail[1] = tail[2];
        tail[2] = tail[3];
        tail[3] = buf[0];
        if tail == [b'\r', b'\n', b'\r', b'\n'] {
            break;
        }
    }

    // 提取 host:port（格式：CONNECT host:port HTTP/1.x）
    let parts: Vec<&str> = first_line.splitn(3, ' ').collect();
    if parts.len() < 2 {
        return Err(core_common::anyhow::anyhow!("HTTP CONNECT 格式错误"));
    }
    let host_port = parts[1];

    let (host, port) = if let Some(pos) = host_port.rfind(':') {
        let h = &host_port[..pos];
        let p: u16 = host_port[pos + 1..].parse().unwrap_or(80);
        (h.to_owned(), p)
    } else {
        (host_port.to_owned(), 80u16)
    };

    log::info!(
        "[proxy] HTTP CONNECT {}:{} 通过出口 {}",
        host,
        port,
        exit_peer
    );

    match connect_via_relay(&exit_peer, &host, port).await {
        Ok(relay_conn) => {
            stream
                .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                .await?;
            bridge_with_relay(stream, relay_conn).await;
        }
        Err(e) => {
            stream
                .write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n")
                .await
                .ok();
            return Err(e);
        }
    }

    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
// 通过 relay 建立到目标的连接
// ──────────────────────────────────────────────────────────────────────────────

/// 通过 relay 隧道连接到目标（经由 exit_peer 出口节点）
///
/// 协议流程：
/// 1. 生成 UUID = `proxy:<base64url(host:port)>:<hex8>`
/// 2. 连接 hbbs，发送 RelayResponse 通知 exit_peer 准备好中继
/// 3. 连接 hbbr，发送 UUID 握手（与 port_forward.rs 一致）等待配对
/// 4. 返回 hbbr relay 连接
async fn connect_via_relay(
    exit_peer: &str,
    target_host: &str,
    target_port: u16,
) -> ResultType<core_common::Stream> {
    use base64::Engine as _;

    // 生成 proxy UUID
    let target_encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(format!("{}:{}", target_host, target_port).as_bytes());
    let hex8: String = (0..4)
        .map(|_| format!("{:02x}", rand::random::<u8>()))
        .collect();
    let proxy_uuid = format!("proxy:{}:{}", target_encoded, hex8);

    log::debug!("[proxy] relay UUID: {}", proxy_uuid);

    // 获取服务器地址
    let servers = ClientConfig::get_rendezvous_servers();
    if servers.is_empty() {
        return Err(core_common::anyhow::anyhow!("未配置 rendezvous 服务器"));
    }
    let hbbs_addr = socket_client::check_port(&servers[0], RENDEZVOUS_PORT);

    // 获取 relay 服务器地址
    let relay_cfg = ClientConfig::get_relay_server();
    let relay_server = if !relay_cfg.is_empty() {
        relay_cfg
    } else {
        socket_client::increase_port(&servers[0], 1)
    };

    let wire = ClientConfig::get_rendezvous_wire_protocol();

    // 步骤 1: 连接 hbbs，发送 RelayResponse 通知出口节点准备好
    log::debug!(
        "[proxy] 连接 hbbs {} 通知出口节点 {}",
        hbbs_addr,
        exit_peer
    );
    let mut hbbs_conn = connect_tcp(hbbs_addr.clone(), CONNECT_TIMEOUT).await?;

    let mut rr = RelayResponse {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        uuid: proxy_uuid.clone(),
        relay_server: relay_server.clone(),
        ..Default::default()
    };
    rr.set_id(exit_peer.to_owned());

    let mut msg_out = RendezvousMessage::new();
    msg_out.set_relay_response(rr);

    if let Some(b) = rendezvous_codec::serialize(&msg_out, wire) {
        hbbs_conn.send_bytes(b).await?;
    } else {
        hbbs_conn.send(&msg_out).await?;
    }

    log::debug!("[proxy] RelayResponse 已发送，等待出口节点接受");

    // 步骤 2: 连接 hbbr，发送 UUID 握手（与 port_forward.rs 使用相同格式）
    let relay_addr = socket_client::check_port(&relay_server, RENDEZVOUS_PORT + 1);
    log::debug!("[proxy] 连接 hbbr: {}", relay_addr);
    let mut relay_conn = connect_tcp(relay_addr.clone(), CONNECT_TIMEOUT).await?;

    // 发送 UUID 握手：[len_byte, uuid_bytes...]（与 port_forward.rs 一致）
    let uuid_bytes = proxy_uuid.as_bytes();
    let mut handshake = Vec::new();
    handshake.push(uuid_bytes.len() as u8);
    handshake.extend_from_slice(uuid_bytes);
    relay_conn.send_raw(handshake).await?;

    log::info!(
        "[proxy] hbbr 已连接，等待出口节点配对 uuid={}",
        proxy_uuid
    );

    Ok(relay_conn)
}

// ──────────────────────────────────────────────────────────────────────────────
// 双向转发：本地 TCP stream ↔ relay Stream
// ──────────────────────────────────────────────────────────────────────────────

/// 双向转发本地 TCP 流与 relay 流的数据
async fn bridge_with_relay(local: TcpStream, relay: core_common::Stream) {
    let (mut lr, mut lw) = local.into_split();
    let relay = std::sync::Arc::new(tokio::sync::Mutex::new(relay));

    // 本地 → relay
    let relay_send = std::sync::Arc::clone(&relay);
    let local_to_relay = tokio::spawn(async move {
        let mut buf = vec![0u8; 32 * 1024];
        loop {
            let n = match lr.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            let mut r = relay_send.lock().await;
            if r.send_raw(buf[..n].to_vec()).await.is_err() {
                break;
            }
        }
        log::debug!("[proxy] 本地→relay 通道关闭");
    });

    // relay → 本地
    let relay_recv = std::sync::Arc::clone(&relay);
    let relay_to_local = tokio::spawn(async move {
        loop {
            let data = {
                let mut r = relay_recv.lock().await;
                r.next().await
            };
            match data {
                Some(Ok(b)) => {
                    if lw.write_all(&b).await.is_err() {
                        break;
                    }
                }
                _ => break,
            }
        }
        log::debug!("[proxy] relay→本地 通道关闭");
    });

    tokio::select! {
        _ = local_to_relay => {}
        _ = relay_to_local => {}
    }
}
