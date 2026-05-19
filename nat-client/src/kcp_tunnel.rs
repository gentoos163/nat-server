/// UDP 打洞 + KCP 可靠传输隧道
///
/// 流程概述：
///   1. 双方各自通过 `query_udp_public_port` 向 hbbs 发 TestNatRequest，
///      获得自己的 UDP 公网端口。
///   2. 主动方在 PunchHoleRequest.upnp_port 中携带本端 UDP 公网端口；
///      被动方收到 PunchHole 后，也执行 STUN，将自己端口写入 PunchHoleSent.upnp_port。
///   3. 双方各自发送若干 UDP 探针以打通 NAT。
///   4. 主动方调用 `kcp_connect`；被动方调用 `kcp_accept`。
///      握手成功后各自得到 `(KcpSender, KcpReceiver)` 通道，
///      交给 `PortForwardManager` 使用。
use bytes::Bytes;
use std::{
    collections::VecDeque,
    io::Write,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    net::UdpSocket,
    sync::mpsc,
    time::timeout,
};

/// KCP 会话 ID（"KCP2" 的 ASCII 编码）
const KCP_CONV: u32 = 0x4b435032;

/// 握手魔数
const CONNECT_MAGIC: &[u8] = b"\x01KCP_CONNECT";
const ACCEPT_MAGIC:  &[u8] = b"\x01KCP_ACCEPT";
/// UDP 探针标记（不以 \x01 开头，KCP 会忽略）
const PUNCH_MARKER:  &[u8] = b"\x00PUNCH";

/// 探针发送间隔
const PROBE_INTERVAL_MS: u64 = 50;
/// 探针发送次数
const PROBE_COUNT: usize = 10;
/// KCP 更新间隔（ms）
const KCP_TICK_MS: u64 = 10;
/// 握手等待超时
const HANDSHAKE_TIMEOUT_MS: u64 = 1500;

// ─── 对外类型别名 ─────────────────────────────────────────────────────────────

pub type KcpSender   = mpsc::Sender<Bytes>;
pub type KcpReceiver = mpsc::Receiver<Bytes>;

// ─── KCP 输出队列（实现 std::io::Write） ─────────────────────────────────────

#[derive(Clone)]
struct KcpOutput(Arc<Mutex<VecDeque<Vec<u8>>>>);

impl Write for KcpOutput {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().push_back(buf.to_vec());
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

// ─── STUN-like：查询 UDP 公网端口 ─────────────────────────────────────────────

/// 向 hbbs UDP 端口发送 `TestNatRequest`，返回 hbbs 观测到的源端口。
/// `sock` 必须已绑定本地地址，`hbbs_addr` 是 hbbs 的 UDP 地址（port 21116）。
pub async fn query_udp_public_port(
    sock: &UdpSocket,
    hbbs_addr: SocketAddr,
) -> Option<u16> {
    use core_common::{
        protobuf::Message as _,
        rendezvous_proto::{RendezvousMessage, TestNatRequest},
    };

    let mut msg = RendezvousMessage::new();
    msg.set_test_nat_request(TestNatRequest::new());
    let buf = msg.write_to_bytes().ok()?;

    for _ in 0..3 {
        sock.send_to(&buf, hbbs_addr).await.ok()?;
        let mut rbuf = vec![0u8; 1024];
        if let Ok(Ok((n, _))) =
            timeout(Duration::from_millis(500), sock.recv_from(&mut rbuf)).await
        {
            let resp = RendezvousMessage::parse_from_bytes(&rbuf[..n]).ok()?;
            if let Some(core_common::rendezvous_proto::rendezvous_message::Union::TestNatResponse(r)) =
                resp.union
            {
                return Some(r.port as u16);
            }
        }
    }
    None
}

// ─── UDP 打洞探针 ─────────────────────────────────────────────────────────────

/// 向 `peer` 发送多条 UDP 探针，帮助路由器建立映射条目。
pub async fn send_udp_probes(sock: &UdpSocket, peer: SocketAddr) {
    for _ in 0..PROBE_COUNT {
        let _ = sock.send_to(PUNCH_MARKER, peer).await;
        tokio::time::sleep(Duration::from_millis(PROBE_INTERVAL_MS)).await;
    }
}

// ─── 内部：获取当前毫秒时间戳（KCP update 用）────────────────────────────────

fn now_ms() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| (d.as_millis() & 0xffff_ffff) as u32)
        .unwrap_or(0)
}

// ─── KCP 驱动 Loop ────────────────────────────────────────────────────────────

async fn kcp_driver(
    sock:   Arc<UdpSocket>,
    peer:   SocketAddr,
    kcp:    Arc<Mutex<kcp::Kcp<KcpOutput>>>,
    outq:   Arc<Mutex<VecDeque<Vec<u8>>>>,
    app_tx: mpsc::Sender<Bytes>,
    mut app_rx: mpsc::Receiver<Bytes>,
) {
    let mut tick = tokio::time::interval(Duration::from_millis(KCP_TICK_MS));
    let mut rbuf = vec![0u8; 65536];
    let mut recv_buf = vec![0u8; 65536];

    loop {
        tokio::select! {
            // 1. KCP 定时更新
            _ = tick.tick() => {
                {
                    let mut k = kcp.lock().unwrap();
                    let _ = k.update(now_ms());
                }
                flush_outq(&sock, peer, &outq).await;
            }

            // 2. 从网络收包
            res = sock.recv_from(&mut rbuf) => {
                if let Ok((n, src)) = res {
                    if src != peer { continue; }
                    let pkt = &rbuf[..n];
                    // 忽略探针包
                    if pkt.starts_with(b"\x00") { continue; }
                    // 收包并取出所有上层数据，先放到 Vec，再 await
                    let collected: Vec<Bytes> = {
                        let mut k = kcp.lock().unwrap();
                        let _ = k.input(pkt);
                        let _ = k.update(now_ms());
                        let mut out = Vec::new();
                        loop {
                            match k.recv(&mut recv_buf) {
                                Ok(n) => out.push(Bytes::copy_from_slice(&recv_buf[..n])),
                                Err(_) => break,
                            }
                        }
                        out
                    }; // MutexGuard 在此 drop
                    for data in collected {
                        let _ = app_tx.send(data).await;
                    }
                    flush_outq(&sock, peer, &outq).await;
                }
            }

            // 3. 应用层要发送数据
            data = app_rx.recv() => {
                match data {
                    Some(d) => {
                        {
                            let mut k = kcp.lock().unwrap();
                            let _ = k.send(&d);
                            let _ = k.update(now_ms());
                        }
                        flush_outq(&sock, peer, &outq).await;
                    }
                    None => return,
                }
            }
        }
    }
}

async fn flush_outq(sock: &UdpSocket, peer: SocketAddr, outq: &Arc<Mutex<VecDeque<Vec<u8>>>>) {
    loop {
        let pkt = outq.lock().unwrap().pop_front();
        match pkt {
            Some(p) => { let _ = sock.send_to(&p, peer).await; }
            None => break,
        }
    }
}

// ─── 建立 KCP 实例 ────────────────────────────────────────────────────────────

fn make_kcp(output: KcpOutput) -> kcp::Kcp<KcpOutput> {
    let mut k = kcp::Kcp::new(KCP_CONV, output);
    k.set_nodelay(true, 10, 2, true);
    k.set_wndsize(128, 128);
    let _ = k.set_mtu(1400);
    k
}

fn spawn_kcp_driver(
    sock: Arc<UdpSocket>,
    peer: SocketAddr,
    kcp:  Arc<Mutex<kcp::Kcp<KcpOutput>>>,
    outq: Arc<Mutex<VecDeque<Vec<u8>>>>,
) -> (KcpSender, KcpReceiver) {
    let (app_tx, kcp_rx) = mpsc::channel::<Bytes>(256);
    let (kcp_tx, app_rx) = mpsc::channel::<Bytes>(256);
    tokio::spawn(kcp_driver(sock, peer, kcp, outq, app_tx, app_rx));
    (kcp_tx, kcp_rx)
}

// ─── 主动方（发起连接） ───────────────────────────────────────────────────────

/// 主动发起 KCP 握手。成功返回 `(send, recv)` 通道对。
pub async fn kcp_connect(
    sock: Arc<UdpSocket>,
    peer: SocketAddr,
) -> Option<(KcpSender, KcpReceiver)> {
    let outq   = Arc::new(Mutex::new(VecDeque::<Vec<u8>>::new()));
    let output = KcpOutput(outq.clone());
    let kcp    = Arc::new(Mutex::new(make_kcp(output)));

    let deadline = Instant::now() + Duration::from_millis(HANDSHAKE_TIMEOUT_MS);
    let mut rbuf = vec![0u8; 256];

    loop {
        if Instant::now() > deadline {
            return None;
        }
        let _ = sock.send_to(CONNECT_MAGIC, peer).await;
        match timeout(Duration::from_millis(100), sock.recv_from(&mut rbuf)).await {
            Ok(Ok((n, src))) if src == peer => {
                if &rbuf[..n] == ACCEPT_MAGIC {
                    return Some(spawn_kcp_driver(sock, peer, kcp, outq));
                }
            }
            _ => {}
        }
    }
}

// ─── 被动方（等待握手） ───────────────────────────────────────────────────────

/// 被动等待 KCP 握手。成功返回 `(send, recv)` 通道对。
pub async fn kcp_accept(
    sock: Arc<UdpSocket>,
    peer: SocketAddr,
) -> Option<(KcpSender, KcpReceiver)> {
    let outq   = Arc::new(Mutex::new(VecDeque::<Vec<u8>>::new()));
    let output = KcpOutput(outq.clone());
    let kcp    = Arc::new(Mutex::new(make_kcp(output)));

    let deadline = Instant::now() + Duration::from_millis(HANDSHAKE_TIMEOUT_MS);
    let mut rbuf = vec![0u8; 256];

    loop {
        if Instant::now() > deadline {
            return None;
        }
        match timeout(Duration::from_millis(200), sock.recv_from(&mut rbuf)).await {
            Ok(Ok((n, src))) if src == peer => {
                if &rbuf[..n] == CONNECT_MAGIC {
                    let _ = sock.send_to(ACCEPT_MAGIC, peer).await;
                    return Some(spawn_kcp_driver(sock, peer, kcp, outq));
                }
            }
            _ => {}
        }
    }
}
