//! GUI 应用桥接层
//!
//! 负责：
//! 1. 将 Slint 窗口的所有 `callback` 连接到 IPC 命令
//! 2. 启动定时器，在后台线程轮询 IPC，再用 invoke_from_event_loop 推送到 UI
//! 3. 驱动托盘事件循环
//!
//! 架构：
//! ```text
//!   Slint 主线程
//!   ├─ UI 渲染（Slint event loop）
//!   ├─ Slint Timer (500ms) ──► spawn thread ──► IPC ──► invoke_from_event_loop ──► set_xxx()
//!   ├─ Slint Timer (50ms)  ──► TrayManager::poll() ──► 窗口 show/hide
//!   └─ 所有 callback         ──► spawn thread ──► IPC ──► invoke_from_event_loop
//!
//!   tokio Runtime（后台线程池）
//!   ├─ RendezvousMediator::start_all()
//!   ├─ lan::start_listening()
//!   ├─ ipc::start_ipc_server()
//!   └─ auth::start_token_refresh_watcher()
//! ```

use crate::config::ClientConfig;
use crate::i18n;
use crate::ui::tray::{TrayAction, TrayManager};
use core_common::log;
use slint::{ComponentHandle, ModelRc, VecModel};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

slint::include_modules!();

// ──────────────────────────────────────────────────────────────────────────────
// 轮询快照（纯 Rust 类型，可跨线程传递）
// ──────────────────────────────────────────────────────────────────────────────

struct RawConn {
    uuid: String,
    conn_type: String,
    peer_addr: String,
    local_port: i32,
    bytes_sent: String,
    bytes_recv: String,
    created_at: String,
}

struct RawPeer {
    id: String,
    ip: String,
    hostname: String,
    username: String,
    platform: String,
    online: bool,
}

struct RawDevice {
    id: i32,
    device_id: String,
    device_name: String,
    is_active: bool,
    online: bool,
    created_at: String,
}

struct RawRule {
    rule_id: String,
    name: String,
    target_host: String,
    target_port: String,
    peer_id_filter: String,
}

struct RawSub {
    plan: String,
    device_used: i32,
    device_limit: i32,
    expires: String,
}

struct PollSnapshot {
    online: bool,
    nat_type: i32,
    peer_id: String,
    server_addr: String,
    connections: Vec<RawConn>,
    peers: Vec<RawPeer>,
    logged_in: bool,
    username: String,
    role: String,
    token_remaining_secs: i64,
    sub: Option<RawSub>,
    devices: Option<Vec<RawDevice>>,
    rules: Option<Vec<RawRule>>,
}

// ──────────────────────────────────────────────────────────────────────────────
// 入口函数
// ──────────────────────────────────────────────────────────────────────────────

/// 运行 GUI（在主线程阻塞，直到窗口关闭）
pub fn run_gui(ipc_port: u16) -> Result<(), slint::PlatformError> {
    let window = AppWindow::new()?;

    init_config_fields(&window);

    if let Some(logo) = load_logo_image() {
        window.set_app_logo(logo);
    }

    window.set_update_current_ver(env!("CARGO_PKG_VERSION").into());

    {
        let win = window.as_weak();
        let api_url = ClientConfig::get_api_url();
        if !api_url.is_empty() {
            crate::updater::start_auto_update_check(api_url, move |info| {
                let version = info.version.clone();
                let changelog = info.changelog.clone();
                let win2 = win.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = win2.upgrade() {
                        w.set_update_available(true);
                        w.set_update_latest_ver(version.as_str().into());
                        w.set_update_changelog(changelog.as_str().into());
                        w.set_update_msg("发现新版本，请前往设置页更新".into());
                        w.set_update_msg_ok(true);
                    }
                });
            });
        }
    }

    window
        .global::<ThemeState>()
        .set_dark(ClientConfig::get_dark_mode());

    let lang = ClientConfig::get_language();
    apply_translations(&window, &lang);

    let tray = match TrayManager::new() {
        Ok(t) => {
            log::info!("[gui] 系统托盘已创建");
            Some(Arc::new(Mutex::new(t)))
        }
        Err(e) => {
            log::warn!("[gui] 系统托盘创建失败（将以无托盘模式运行）: {}", e);
            None
        }
    };

    bind_callbacks(&window, ipc_port);

    // ── 状态轮询 Timer（每 500ms 触发，后台线程执行 IPC）────────────────────
    {
        let win = window.as_weak();
        let tray_ref = tray.clone();
        let polling = Arc::new(AtomicBool::new(false));
        let timer = slint::Timer::default();
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(500),
            move || {
                // 上次轮询还未完成则跳过，避免线程堆积
                if polling.swap(true, Ordering::SeqCst) {
                    return;
                }
                let page = win.upgrade().map(|w| w.get_page()).unwrap_or(0);
                let win2 = win.clone();
                let polling2 = polling.clone();
                std::thread::spawn(move || {
                    let snap = collect_poll_data(ipc_port, page);
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(w) = win2.upgrade() {
                            apply_poll_snapshot(&w, snap);
                        }
                        polling2.store(false, Ordering::SeqCst);
                    });
                });
            },
        );
        std::mem::forget(timer);
    }

    // ── 托盘事件 Timer（每 50ms）────────────────────────────────────────────
    if let Some(tray_ref) = tray {
        let win = window.as_weak();
        let timer = slint::Timer::default();
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(50),
            move || {
                let mut guard = match tray_ref.lock() {
                    Ok(g) => g,
                    Err(_) => return,
                };
                // 从窗口同步 online 状态到托盘 tooltip
                if let Some(w) = win.upgrade() {
                    guard.set_online(w.get_online());
                }
                if let Some(action) = guard.poll() {
                    drop(guard);
                    let Some(w) = win.upgrade() else { return };
                    handle_tray_action(&w, action);
                }
            },
        );
        std::mem::forget(timer);
    }

    // ── 启动后立即拉取一次（延迟 400ms 等 IPC 服务就绪）───────────────────
    {
        let win = window.as_weak();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(400));
            let page = 0i32;
            let snap = collect_poll_data(ipc_port, page);
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(w) = win.upgrade() {
                    apply_poll_snapshot(&w, snap);
                }
            });
        });
    }

    let result = window.run();
    crate::updater::stop_auto_update();
    result
}

// ──────────────────────────────────────────────────────────────────────────────
// 后台收集轮询数据（在普通线程执行，不得调用任何 Slint API）
// ──────────────────────────────────────────────────────────────────────────────

fn collect_poll_data(ipc_port: u16, page: i32) -> PollSnapshot {
    let status_raw = ipc_call(ipc_port, r#"{"cmd":"get_status"}"#);
    let config_raw = ipc_call(ipc_port, r#"{"cmd":"get_config"}"#);
    let conns_raw  = ipc_call(ipc_port, r#"{"cmd":"get_connections"}"#);
    let peers_raw  = ipc_call(ipc_port, r#"{"cmd":"get_peers"}"#);
    let auth_raw   = ipc_call(ipc_port, r#"{"cmd":"auth_status"}"#);

    let sv = serde_json::from_str::<serde_json::Value>(&status_raw).unwrap_or_default();
    let cv = serde_json::from_str::<serde_json::Value>(&config_raw).unwrap_or_default();
    let connv = serde_json::from_str::<serde_json::Value>(&conns_raw).unwrap_or_default();
    let peerv = serde_json::from_str::<serde_json::Value>(&peers_raw).unwrap_or_default();
    let authv = serde_json::from_str::<serde_json::Value>(&auth_raw).unwrap_or_default();

    let online   = sv["online"].as_bool().unwrap_or(false);
    let nat_type = sv["nat_type"].as_i64().unwrap_or(0) as i32;
    let peer_id  = cv["id"].as_str().unwrap_or("").to_owned();
    let server_addr = ClientConfig::get().rendezvous_servers;

    let connections = connv["connections"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|c| RawConn {
                    uuid:       c["uuid"].as_str().unwrap_or("").to_owned(),
                    conn_type:  c["conn_type"].as_str().unwrap_or("").to_owned(),
                    peer_addr:  c["peer_addr"].as_str().unwrap_or("").to_owned(),
                    local_port: c["local_port"].as_i64().unwrap_or(0) as i32,
                    bytes_sent: format_bytes(c["bytes_sent"].as_u64().unwrap_or(0)),
                    bytes_recv: format_bytes(c["bytes_recv"].as_u64().unwrap_or(0)),
                    created_at: format_ts(c["created_at"].as_u64().unwrap_or(0)),
                })
                .collect()
        })
        .unwrap_or_default();

    let peers = peerv["peers"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|p| RawPeer {
                    id:       p["id"].as_str().unwrap_or("").to_owned(),
                    ip:       p["ip"].as_str().unwrap_or("").to_owned(),
                    hostname: p["hostname"].as_str().unwrap_or("").to_owned(),
                    username: p["username"].as_str().unwrap_or("").to_owned(),
                    platform: p["platform"].as_str().unwrap_or("").to_owned(),
                    online:   p["online"].as_bool().unwrap_or(false),
                })
                .collect()
        })
        .unwrap_or_default();

    let (logged_in, username, role, token_remaining_secs) =
        if let Some(a) = authv.get("auth") {
            (
                a["logged_in"].as_bool().unwrap_or(false),
                a["username"].as_str().unwrap_or("").to_owned(),
                a["role"].as_str().unwrap_or("").to_owned(),
                a["token_remaining_secs"].as_i64().unwrap_or(0),
            )
        } else {
            (false, String::new(), String::new(), 0)
        };

    let sub = if logged_in {
        let sub_raw = ipc_call(ipc_port, r#"{"cmd":"auth_subscription"}"#);
        let sv2 = serde_json::from_str::<serde_json::Value>(&sub_raw).unwrap_or_default();
        sv2.get("subscription").map(|s| {
            let exp = s["expires_at"].as_str().unwrap_or("");
            let expires = if exp.is_empty() {
                String::new()
            } else if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(exp) {
                dt.format("%Y-%m-%d").to_string()
            } else {
                exp.to_owned()
            };
            RawSub {
                plan:         s["plan_display_name"].as_str().unwrap_or("免费版").to_owned(),
                device_used:  s["device_used"].as_i64().unwrap_or(0) as i32,
                device_limit: s["device_limit"].as_i64().unwrap_or(0) as i32,
                expires,
            }
        })
    } else {
        None
    };

    let devices = if page == 4 && logged_in {
        let dev_raw = ipc_call(ipc_port, r#"{"cmd":"auth_list_devices"}"#);
        let dv = serde_json::from_str::<serde_json::Value>(&dev_raw).unwrap_or_default();
        Some(
            dv["devices"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .map(|d| RawDevice {
                            id:          d["id"].as_i64().unwrap_or(0) as i32,
                            device_id:   d["device_id"].as_str().unwrap_or("").to_owned(),
                            device_name: d["device_name"].as_str().unwrap_or("").to_owned(),
                            is_active:   d["is_active"].as_bool().unwrap_or(false),
                            online:      d["online"].as_bool().unwrap_or(false),
                            created_at:  d["created_at"].as_str().unwrap_or("").to_owned(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        )
    } else {
        None
    };

    let rules = if page == 6 {
        let rules_raw = ipc_call(ipc_port, r#"{"cmd":"list_rules"}"#);
        let rv = serde_json::from_str::<serde_json::Value>(&rules_raw).unwrap_or_default();
        Some(
            rv["rules"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .map(|r| RawRule {
                            rule_id:       r["rule_id"].as_str().unwrap_or("").to_owned(),
                            name:          r["name"].as_str().unwrap_or("").to_owned(),
                            target_host:   r["target_host"].as_str().unwrap_or("127.0.0.1").to_owned(),
                            target_port:   r["target_port"].as_u64().unwrap_or(0).to_string(),
                            peer_id_filter: r["peer_id_filter"].as_str().unwrap_or("").to_owned(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        )
    } else {
        None
    };

    PollSnapshot {
        online,
        nat_type,
        peer_id,
        server_addr,
        connections,
        peers,
        logged_in,
        username,
        role,
        token_remaining_secs,
        sub,
        devices,
        rules,
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// 将快照应用到 UI（必须在 Slint 主线程调用）
// ──────────────────────────────────────────────────────────────────────────────

fn apply_poll_snapshot(w: &AppWindow, snap: PollSnapshot) {
    w.set_online(snap.online);
    w.set_nat_type(nat_type_name(snap.nat_type as i64).into());

    w.set_peer_id(snap.peer_id.as_str().into());
    w.set_server_addr(snap.server_addr.as_str().into());

    w.set_connections(ModelRc::new(VecModel::from(
        snap.connections
            .into_iter()
            .map(|c| ActiveConn {
                uuid:       c.uuid.into(),
                conn_type:  c.conn_type.into(),
                peer_addr:  c.peer_addr.into(),
                local_port: c.local_port,
                bytes_sent: c.bytes_sent.into(),
                bytes_recv: c.bytes_recv.into(),
                created_at: c.created_at.into(),
            })
            .collect::<Vec<_>>(),
    )));

    w.set_lan_peers(ModelRc::new(VecModel::from(
        snap.peers
            .into_iter()
            .map(|p| LanPeer {
                id:       p.id.into(),
                ip:       p.ip.into(),
                hostname: p.hostname.into(),
                username: p.username.into(),
                platform: p.platform.into(),
                online:   p.online,
            })
            .collect::<Vec<_>>(),
    )));

    w.set_logged_in(snap.logged_in);
    w.set_username(snap.username.as_str().into());
    w.set_role(snap.role.as_str().into());
    w.set_token_remaining(format_duration(snap.token_remaining_secs).into());

    if let Some(s) = snap.sub {
        w.set_sub_plan(s.plan.as_str().into());
        w.set_sub_device_used(s.device_used);
        w.set_sub_device_limit(s.device_limit);
        w.set_sub_expires(s.expires.as_str().into());
    }

    if let Some(devices) = snap.devices {
        w.set_devices_loading(false);
        w.set_devices(ModelRc::new(VecModel::from(
            devices
                .into_iter()
                .map(|d| BoundDevice {
                    id:          d.id,
                    device_id:   d.device_id.into(),
                    device_name: d.device_name.into(),
                    is_active:   d.is_active,
                    is_online:   d.online,
                    created_at:  d.created_at.into(),
                })
                .collect::<Vec<_>>(),
        )));
    }

    if let Some(rules) = snap.rules {
        w.set_tunnel_rules_loading(false);
        w.set_tunnel_rules(ModelRc::new(VecModel::from(
            rules
                .into_iter()
                .map(|r| ForwardRule {
                    rule_id:        r.rule_id.into(),
                    name:           r.name.into(),
                    target_host:    r.target_host.into(),
                    target_port:    r.target_port.into(),
                    peer_id_filter: r.peer_id_filter.into(),
                })
                .collect::<Vec<_>>(),
        )));
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// 初始化：从配置文件读取默认值填入设置页
// ──────────────────────────────────────────────────────────────────────────────

fn init_config_fields(w: &AppWindow) {
    let cfg = ClientConfig::get();
    w.set_cfg_server(cfg.rendezvous_servers.as_str().into());
    w.set_cfg_relay(cfg.relay_server.as_str().into());
    w.set_cfg_api_url(cfg.api_url.as_str().into());
    w.set_cfg_ipc_port(cfg.ipc_port.to_string().as_str().into());

    let (s5_en, s5_peer, s5_port) = ClientConfig::get_socks5_config();
    w.set_socks5_enabled(s5_en);
    w.set_socks5_exit_peer(s5_peer.to_string().as_str().into());
    w.set_socks5_port_str(s5_port.as_str().into());

    let tr = i18n::get("zh");
    let g = w.global::<Tr>();

    g.set_nav_dashboard(tr.nav_dashboard.into());
    g.set_nav_connections(tr.nav_connections.into());
    g.set_nav_peers(tr.nav_peers.into());
    g.set_nav_account(tr.nav_account.into());
    g.set_nav_devices(tr.nav_devices.into());
    g.set_nav_settings(tr.nav_settings.into());
    g.set_btn_save(tr.btn_save.into());
    g.set_btn_cancel(tr.btn_cancel.into());
    g.set_btn_refresh(tr.btn_refresh.into());
    g.set_btn_add(tr.btn_add.into());
    g.set_btn_delete(tr.btn_delete.into());
    g.set_btn_connect(tr.btn_connect.into());
    g.set_btn_login(tr.btn_login.into());
    g.set_btn_logout(tr.btn_logout.into());
    g.set_btn_register(tr.btn_register.into());
    g.set_btn_change_password(tr.btn_change_password.into());
    g.set_status_online(tr.status_online.into());
    g.set_status_offline(tr.status_offline.into());
    g.set_status_loading(tr.status_loading.into());
    g.set_status_processing(tr.status_processing.into());
    g.set_dashboard_title(tr.dashboard_title.into());
    g.set_dashboard_peer_id(tr.dashboard_peer_id.into());
    g.set_dashboard_server(tr.dashboard_server.into());
    g.set_dashboard_nat_type(tr.dashboard_nat_type.into());
    g.set_dashboard_online(tr.dashboard_online.into());
    g.set_dashboard_offline(tr.dashboard_offline.into());
    g.set_conn_title(tr.conn_title.into());
    g.set_conn_peer_id(tr.conn_peer_id.into());
    g.set_conn_local_port(tr.conn_local_port.into());
    g.set_conn_type(tr.conn_type.into());
    g.set_conn_sent(tr.conn_sent.into());
    g.set_conn_recv(tr.conn_recv.into());
    g.set_conn_close(tr.conn_close.into());
    g.set_conn_no_connections(tr.conn_no_connections.into());
    g.set_conn_input_peer_id(tr.conn_input_peer_id.into());
    g.set_conn_input_port(tr.conn_input_port.into());
    g.set_peers_title(tr.peers_title.into());
    g.set_peers_scanning(tr.peers_scanning.into());
    g.set_peers_discover(tr.peers_discover.into());
    g.set_peers_no_peers(tr.peers_no_peers.into());
    g.set_peers_connect(tr.peers_connect.into());
    g.set_account_title(tr.account_title.into());
    g.set_account_username(tr.account_username.into());
    g.set_account_password(tr.account_password.into());
    g.set_account_email(tr.account_email.into());
    g.set_account_device_name(tr.account_device_name.into());
    g.set_account_old_password(tr.account_old_password.into());
    g.set_account_new_password(tr.account_new_password.into());
    g.set_account_change_password(tr.account_change_password.into());
    g.set_account_token_remaining(tr.account_token_remaining.into());
    g.set_account_register_link(tr.account_register_link.into());
    g.set_account_login_link(tr.account_login_link.into());
    g.set_account_sub_plan(tr.account_sub_plan.into());
    g.set_account_sub_current(tr.account_sub_current.into());
    g.set_account_sub_device_usage(tr.account_sub_device_usage.into());
    g.set_account_sub_expires(tr.account_sub_expires.into());
    g.set_account_sub_forever(tr.account_sub_forever.into());
    g.set_devices_title(tr.devices_title.into());
    g.set_devices_id(tr.devices_id.into());
    g.set_devices_name(tr.devices_name.into());
    g.set_devices_remove(tr.devices_remove.into());
    g.set_devices_no_devices(tr.devices_no_devices.into());
    g.set_settings_title(tr.settings_title.into());
    g.set_settings_server(tr.settings_server.into());
    g.set_settings_relay(tr.settings_relay.into());
    g.set_settings_peer_id(tr.settings_peer_id.into());
    g.set_settings_protocol(tr.settings_protocol.into());
    g.set_settings_language(tr.settings_language.into());
    g.set_settings_open_config(tr.settings_open_config.into());
    g.set_settings_save(tr.settings_save.into());
    g.set_lang_zh(tr.lang_zh.into());
    g.set_lang_en(tr.lang_en.into());
}

fn apply_translations(w: &AppWindow, lang: &str) {
    let tr = i18n::get(lang);
    let g = w.global::<Tr>();
    g.set_nav_dashboard(tr.nav_dashboard.into());
    g.set_nav_connections(tr.nav_connections.into());
    g.set_nav_peers(tr.nav_peers.into());
    g.set_nav_account(tr.nav_account.into());
    g.set_nav_devices(tr.nav_devices.into());
    g.set_nav_settings(tr.nav_settings.into());
    g.set_btn_save(tr.btn_save.into());
    g.set_btn_cancel(tr.btn_cancel.into());
    g.set_btn_refresh(tr.btn_refresh.into());
    g.set_btn_add(tr.btn_add.into());
    g.set_btn_delete(tr.btn_delete.into());
    g.set_btn_connect(tr.btn_connect.into());
    g.set_btn_login(tr.btn_login.into());
    g.set_btn_logout(tr.btn_logout.into());
    g.set_btn_register(tr.btn_register.into());
    g.set_btn_change_password(tr.btn_change_password.into());
    g.set_status_online(tr.status_online.into());
    g.set_status_offline(tr.status_offline.into());
    g.set_status_loading(tr.status_loading.into());
    g.set_status_processing(tr.status_processing.into());
    g.set_lang_zh(tr.lang_zh.into());
    g.set_lang_en(tr.lang_en.into());
}

// ──────────────────────────────────────────────────────────────────────────────
// 回调绑定
// ──────────────────────────────────────────────────────────────────────────────

fn bind_callbacks(w: &AppWindow, ipc_port: u16) {
    // ── 刷新 ─────────────────────────────────────────────────────────────────
    {
        let win = w.as_weak();
        w.on_do_refresh(move || {
            let Some(w) = win.upgrade() else { return };
            let page = w.get_page();
            let win2 = win.clone();
            std::thread::spawn(move || {
                let snap = collect_poll_data(ipc_port, page);
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = win2.upgrade() {
                        apply_poll_snapshot(&w, snap);
                    }
                });
            });
        });
    }

    // ── 连接对端 ─────────────────────────────────────────────────────────────
    {
        let win = w.as_weak();
        w.on_do_connect(move |peer_id, local_port| {
            let Some(w) = win.upgrade() else { return };
            w.set_connecting(true);
            w.set_connect_result("".into());
            let cmd = format!(
                r#"{{"cmd":"connect","peer_id":"{}","local_port":{}}}"#,
                peer_id,
                local_port.to_string().parse::<u16>().unwrap_or(0)
            );
            let win2 = win.clone();
            std::thread::spawn(move || {
                let resp = ipc_call(ipc_port, &cmd);
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(w) = win2.upgrade() else { return };
                    w.set_connecting(false);
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&resp) {
                        if let Some(port) = v["local_port"].as_u64() {
                            w.set_connect_result(
                                format!("✅ 隧道已建立！连接 127.0.0.1:{} 访问对端", port).into(),
                            );
                        } else {
                            let err = v["error"].as_str().unwrap_or("未知错误");
                            w.set_connect_result(format!("❌ {}", err).into());
                        }
                    }
                });
            });
        });
    }

    // ── LAN 发现 ─────────────────────────────────────────────────────────────
    {
        let win = w.as_weak();
        w.on_do_discover(move || {
            let Some(w) = win.upgrade() else { return };
            w.set_discovering(true);
            let win2 = w.as_weak();
            std::thread::spawn(move || {
                let resp = ipc_call(ipc_port, r#"{"cmd":"discover"}"#);
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(w) = win2.upgrade() else { return };
                    w.set_discovering(false);
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&resp) {
                        if let Some(arr) = v["peers"].as_array() {
                            let items: Vec<LanPeer> = arr
                                .iter()
                                .map(|p| LanPeer {
                                    id:       p["id"].as_str().unwrap_or("").into(),
                                    ip:       p["ip"].as_str().unwrap_or("").into(),
                                    hostname: p["hostname"].as_str().unwrap_or("").into(),
                                    username: p["username"].as_str().unwrap_or("").into(),
                                    platform: p["platform"].as_str().unwrap_or("").into(),
                                    online:   p["online"].as_bool().unwrap_or(false),
                                })
                                .collect();
                            w.set_lan_peers(ModelRc::new(VecModel::from(items)));
                        }
                    }
                });
            });
        });
    }

    // ── 断开连接 ─────────────────────────────────────────────────────────────
    {
        let win = w.as_weak();
        w.on_close_conn(move |uuid| {
            let cmd = format!(r#"{{"cmd":"close_conn","uuid":"{}"}}"#, uuid);
            let win2 = win.clone();
            std::thread::spawn(move || {
                ipc_call(ipc_port, &cmd);
                let snap = collect_poll_data(ipc_port, 1);
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = win2.upgrade() {
                        apply_poll_snapshot(&w, snap);
                    }
                });
            });
        });
    }

    // ── 连接 LAN 节点 ────────────────────────────────────────────────────────
    {
        let win = w.as_weak();
        w.on_connect_peer(move |peer_id| {
            let Some(w) = win.upgrade() else { return };
            let cmd = format!(
                r#"{{"cmd":"connect","peer_id":"{}","local_port":0}}"#,
                peer_id
            );
            w.set_connecting(true);
            w.set_page(0);
            let win2 = win.clone();
            std::thread::spawn(move || {
                let resp = ipc_call(ipc_port, &cmd);
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(w) = win2.upgrade() else { return };
                    w.set_connecting(false);
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&resp) {
                        if let Some(port) = v["local_port"].as_u64() {
                            w.set_connect_result(format!("✅ 已连接！本地端口 {}", port).into());
                        } else {
                            let err = v["error"].as_str().unwrap_or("连接失败");
                            w.set_connect_result(format!("❌ {}", err).into());
                        }
                    }
                });
            });
        });
    }

    // ── 登录 ─────────────────────────────────────────────────────────────────
    {
        let win = w.as_weak();
        w.on_do_login(move || {
            let Some(w) = win.upgrade() else { return };
            let username = w.get_login_user().to_string();
            let password = w.get_login_pass().to_string();
            if username.is_empty() || password.is_empty() { return; }
            w.set_account_busy(true);
            w.set_account_status("".into());
            let cmd = format!(
                r#"{{"cmd":"auth_login","username":"{}","password":"{}"}}"#,
                escape_json(&username),
                escape_json(&password)
            );
            let win2 = win.clone();
            std::thread::spawn(move || {
                let resp = ipc_call(ipc_port, &cmd);
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(w) = win2.upgrade() else { return };
                    w.set_account_busy(false);
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&resp) {
                        if let Some(a) = v.get("auth") {
                            w.set_logged_in(a["logged_in"].as_bool().unwrap_or(false));
                            w.set_username(a["username"].as_str().unwrap_or("").into());
                            w.set_role(a["role"].as_str().unwrap_or("").into());
                            w.set_account_status("✅ 登录成功".into());
                            w.set_login_pass("".into());
                        } else if let Some(e) = v["error"].as_str() {
                            w.set_account_status(format!("❌ {}", e).into());
                        }
                    }
                });
            });
        });
    }

    // ── 注销 ─────────────────────────────────────────────────────────────────
    {
        let win = w.as_weak();
        w.on_do_logout(move || {
            let win2 = win.clone();
            std::thread::spawn(move || {
                ipc_call(ipc_port, r#"{"cmd":"auth_logout"}"#);
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = win2.upgrade() {
                        w.set_logged_in(false);
                        w.set_username("".into());
                        w.set_role("".into());
                        w.set_account_status("✅ 已退出登录".into());
                    }
                });
            });
        });
    }

    // ── 注册 ─────────────────────────────────────────────────────────────────
    {
        let win = w.as_weak();
        w.on_do_register(move || {
            let Some(w) = win.upgrade() else { return };
            let username = w.get_login_user().to_string();
            let password = w.get_login_pass().to_string();
            let email    = w.get_reg_email().to_string();
            let dname    = w.get_reg_device_name().to_string();
            if username.is_empty() || password.is_empty() || email.is_empty() { return; }
            w.set_account_busy(true);
            let cmd = format!(
                r#"{{"cmd":"auth_register","username":"{}","email":"{}","password":"{}","device_name":"{}"}}"#,
                escape_json(&username), escape_json(&email),
                escape_json(&password), escape_json(&dname)
            );
            let win2 = win.clone();
            std::thread::spawn(move || {
                let resp = ipc_call(ipc_port, &cmd);
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(w) = win2.upgrade() else { return };
                    w.set_account_busy(false);
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&resp) {
                        if let Some(a) = v.get("auth") {
                            w.set_logged_in(a["logged_in"].as_bool().unwrap_or(false));
                            w.set_username(a["username"].as_str().unwrap_or("").into());
                            w.set_role(a["role"].as_str().unwrap_or("").into());
                            w.set_account_status("✅ 注册并登录成功".into());
                            w.set_show_register(false);
                        } else if let Some(e) = v["error"].as_str() {
                            w.set_account_status(format!("❌ {}", e).into());
                        }
                    }
                });
            });
        });
    }

    // ── 修改密码 ─────────────────────────────────────────────────────────────
    {
        let win = w.as_weak();
        w.on_do_change_password(move || {
            let Some(w) = win.upgrade() else { return };
            let old = w.get_old_pass().to_string();
            let new = w.get_new_pass().to_string();
            if old.is_empty() || new.is_empty() { return; }
            w.set_account_busy(true);
            let cmd = format!(
                r#"{{"cmd":"auth_change_password","old_password":"{}","new_password":"{}"}}"#,
                escape_json(&old), escape_json(&new)
            );
            let win2 = win.clone();
            std::thread::spawn(move || {
                let resp = ipc_call(ipc_port, &cmd);
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(w) = win2.upgrade() else { return };
                    w.set_account_busy(false);
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&resp) {
                        if v["ok"].as_bool().unwrap_or(false) {
                            w.set_account_status("✅ 密码已修改，请重新登录".into());
                            w.set_logged_in(false);
                            w.set_old_pass("".into());
                            w.set_new_pass("".into());
                        } else {
                            let e = v["error"].as_str().unwrap_or("修改失败");
                            w.set_account_status(format!("❌ {}", e).into());
                        }
                    }
                });
            });
        });
    }

    // ── 移除设备 ─────────────────────────────────────────────────────────────
    {
        let win = w.as_weak();
        w.on_do_remove_device(move |device_id| {
            let cmd = format!(r#"{{"cmd":"auth_remove_device","device_id":"{}"}}"#, device_id);
            let win2 = win.clone();
            std::thread::spawn(move || {
                ipc_call(ipc_port, &cmd);
                let dev_raw = ipc_call(ipc_port, r#"{"cmd":"auth_list_devices"}"#);
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(w) = win2.upgrade() else { return };
                    if let Ok(dv) = serde_json::from_str::<serde_json::Value>(&dev_raw) {
                        if let Some(arr) = dv["devices"].as_array() {
                            let items: Vec<BoundDevice> = arr.iter().map(|d| BoundDevice {
                                id:          d["id"].as_i64().unwrap_or(0) as i32,
                                device_id:   d["device_id"].as_str().unwrap_or("").into(),
                                device_name: d["device_name"].as_str().unwrap_or("").into(),
                                is_active:   d["is_active"].as_bool().unwrap_or(false),
                                is_online:   d["online"].as_bool().unwrap_or(false),
                                created_at:  d["created_at"].as_str().unwrap_or("").into(),
                            }).collect();
                            w.set_devices(ModelRc::new(VecModel::from(items)));
                        }
                    }
                });
            });
        });
    }

    // ── 保存设置 ─────────────────────────────────────────────────────────────
    {
        let win = w.as_weak();
        w.on_do_save_settings(move || {
            let Some(w) = win.upgrade() else { return };
            let server      = w.get_cfg_server().to_string();
            let relay       = w.get_cfg_relay().to_string();
            let api_url     = w.get_cfg_api_url().to_string();
            let ipc_port_str = w.get_cfg_ipc_port().to_string();
            ClientConfig::update(|c| {
                c.rendezvous_servers = server.clone();
                c.relay_server       = relay.clone();
                c.api_url            = api_url.clone();
                if let Ok(p) = ipc_port_str.parse::<u16>() {
                    c.ipc_port = p;
                }
            });
            w.set_settings_status("✅ 配置已保存".into());
            log::info!("[gui] 设置已保存，触发中介重启");
            let win2 = win.clone();
            std::thread::spawn(move || {
                ipc_call(ipc_port, r#"{"cmd":"restart_mediator"}"#);
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = win2.upgrade() {
                        w.set_settings_status("✅ 配置已保存，中介已重启".into());
                    }
                });
            });
        });
    }

    // ── 重启中介 ─────────────────────────────────────────────────────────────
    {
        let win = w.as_weak();
        w.on_do_restart_mediator(move || {
            let win2 = win.clone();
            std::thread::spawn(move || {
                ipc_call(ipc_port, r#"{"cmd":"restart_mediator"}"#);
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = win2.upgrade() {
                        w.set_settings_status("✅ 中介已重启".into());
                    }
                });
            });
        });
    }

    // ── 打开配置文件 ─────────────────────────────────────────────────────────
    w.on_do_open_config(|| {
        let path = ClientConfig::config_path();
        log::info!("[gui] 打开配置文件: {}", path.display());
        #[cfg(target_os = "windows")]
        let _ = std::process::Command::new("explorer").arg(&path).spawn();
        #[cfg(target_os = "macos")]
        let _ = std::process::Command::new("open").arg(&path).spawn();
        #[cfg(target_os = "linux")]
        let _ = std::process::Command::new("xdg-open").arg(&path).spawn();
    });

    // ── 切换注册/登录表单 ────────────────────────────────────────────────────
    {
        let win = w.as_weak();
        w.on_toggle_register(move || {
            if let Some(w) = win.upgrade() {
                let cur = w.get_show_register();
                w.set_show_register(!cur);
                w.set_account_status("".into());
            }
        });
    }

    // ── 切换语言 ─────────────────────────────────────────────────────────────
    {
        let win = w.as_weak();
        w.on_set_language(move |lang| {
            let lang_str = lang.to_string();
            ClientConfig::set_language(&lang_str);
            if let Some(w) = win.upgrade() {
                apply_translations(&w, &lang_str);
            }
        });
    }

    // ── 切换 SOCKS5 代理开关 ──────────────────────────────────────────────────
    {
        let win = w.as_weak();
        w.on_toggle_socks5(move || {
            ClientConfig::update(|c| c.socks5_enabled = !c.socks5_enabled);
            if let Some(w) = win.upgrade() {
                let (s5_en, _, _) = ClientConfig::get_socks5_config();
                w.set_socks5_enabled(s5_en);
                w.set_settings_status(
                    if s5_en { "✅ SOCKS5 代理已启用（重启后生效）".into() }
                    else { "SOCKS5 代理已禁用".into() }
                );
            }
        });
    }

    // ── 保存代理设置 ─────────────────────────────────────────────────────────
    {
        let win = w.as_weak();
        w.on_save_proxy_settings(move || {
            let Some(w) = win.upgrade() else { return };
            let exit_peer = w.get_socks5_exit_peer().to_string();
            let port_str  = w.get_socks5_port_str().to_string();
            let port: u16 = port_str.parse().unwrap_or(1080);
            ClientConfig::update(|c| {
                c.socks5_exit_peer = exit_peer.clone();
                c.socks5_port      = port;
            });
            w.set_settings_status("✅ 代理设置已保存（重启后生效）".into());
            let cmd = format!(r#"{{"cmd":"proxy_save","port":{},"exit_peer":"{}"}}"#, port, exit_peer);
            std::thread::spawn(move || { ipc_call(ipc_port, &cmd); });
        });
    }

    // ── 刷新转发规则 ─────────────────────────────────────────────────────────
    {
        let win = w.as_weak();
        w.on_do_refresh_rules(move || {
            let Some(w) = win.upgrade() else { return };
            w.set_tunnel_rules_loading(true);
            let win2 = win.clone();
            std::thread::spawn(move || {
                let resp = ipc_call(ipc_port, r#"{"cmd":"list_rules"}"#);
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(w) = win2.upgrade() else { return };
                    w.set_tunnel_rules_loading(false);
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&resp) {
                        if let Some(arr) = v["rules"].as_array() {
                            let items: Vec<ForwardRule> = arr.iter().map(|r| ForwardRule {
                                rule_id:        r["rule_id"].as_str().unwrap_or("").into(),
                                name:           r["name"].as_str().unwrap_or("").into(),
                                target_host:    r["target_host"].as_str().unwrap_or("127.0.0.1").into(),
                                target_port:    r["target_port"].as_u64().unwrap_or(0).to_string().into(),
                                peer_id_filter: r["peer_id_filter"].as_str().unwrap_or("").into(),
                            }).collect();
                            w.set_tunnel_rules(ModelRc::new(VecModel::from(items)));
                        }
                    }
                });
            });
        });
    }

    // ── 添加转发规则 ─────────────────────────────────────────────────────────
    {
        let win = w.as_weak();
        w.on_do_add_rule(move || {
            let Some(w) = win.upgrade() else { return };
            let name        = w.get_tunnel_new_name().to_string();
            let port_str    = w.get_tunnel_new_port().to_string();
            let target_host = w.get_tunnel_new_host().to_string();
            let peer_filter = w.get_tunnel_new_filter().to_string();
            let target_port: u16 = match port_str.parse() {
                Ok(p) => p,
                Err(_) => {
                    w.set_tunnel_status_msg("端口必须为 1-65535 的整数".into());
                    w.set_tunnel_status_ok(false);
                    return;
                }
            };
            let cmd = serde_json::json!({
                "cmd": "add_rule",
                "rule_name": name,
                "target_port": target_port,
                "target_host": target_host,
                "peer_id_filter": peer_filter,
            }).to_string();
            let name_display = w.get_tunnel_new_name().to_string();
            let win2 = win.clone();
            std::thread::spawn(move || {
                let resp = ipc_call(ipc_port, &cmd);
                let rules_resp = ipc_call(ipc_port, r#"{"cmd":"list_rules"}"#);
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(w) = win2.upgrade() else { return };
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&resp) {
                        if v["ok"].as_bool().unwrap_or(false) {
                            w.set_tunnel_status_msg(format!("✅ 规则「{}」已添加", name_display).into());
                            w.set_tunnel_status_ok(true);
                            w.set_tunnel_new_name("".into());
                            w.set_tunnel_new_port("".into());
                            w.set_tunnel_new_filter("".into());
                        } else {
                            let err = v["error"].as_str().unwrap_or("添加失败");
                            w.set_tunnel_status_msg(format!("❌ {}", err).into());
                            w.set_tunnel_status_ok(false);
                        }
                    }
                    if let Ok(rv) = serde_json::from_str::<serde_json::Value>(&rules_resp) {
                        if let Some(arr) = rv["rules"].as_array() {
                            let items: Vec<ForwardRule> = arr.iter().map(|r| ForwardRule {
                                rule_id:        r["rule_id"].as_str().unwrap_or("").into(),
                                name:           r["name"].as_str().unwrap_or("").into(),
                                target_host:    r["target_host"].as_str().unwrap_or("127.0.0.1").into(),
                                target_port:    r["target_port"].as_u64().unwrap_or(0).to_string().into(),
                                peer_id_filter: r["peer_id_filter"].as_str().unwrap_or("").into(),
                            }).collect();
                            w.set_tunnel_rules(ModelRc::new(VecModel::from(items)));
                        }
                    }
                });
            });
        });
    }

    // ── 删除转发规则 ─────────────────────────────────────────────────────────
    {
        let win = w.as_weak();
        w.on_do_remove_rule(move |rule_id| {
            let cmd = serde_json::json!({ "cmd": "remove_rule", "rule_id": rule_id.to_string() }).to_string();
            let win2 = win.clone();
            std::thread::spawn(move || {
                ipc_call(ipc_port, &cmd);
                let rules_resp = ipc_call(ipc_port, r#"{"cmd":"list_rules"}"#);
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(w) = win2.upgrade() else { return };
                    w.set_tunnel_status_msg("规则已删除".into());
                    w.set_tunnel_status_ok(true);
                    if let Ok(rv) = serde_json::from_str::<serde_json::Value>(&rules_resp) {
                        if let Some(arr) = rv["rules"].as_array() {
                            let items: Vec<ForwardRule> = arr.iter().map(|r| ForwardRule {
                                rule_id:        r["rule_id"].as_str().unwrap_or("").into(),
                                name:           r["name"].as_str().unwrap_or("").into(),
                                target_host:    r["target_host"].as_str().unwrap_or("127.0.0.1").into(),
                                target_port:    r["target_port"].as_u64().unwrap_or(0).to_string().into(),
                                peer_id_filter: r["peer_id_filter"].as_str().unwrap_or("").into(),
                            }).collect();
                            w.set_tunnel_rules(ModelRc::new(VecModel::from(items)));
                        }
                    }
                });
            });
        });
    }

    // ── 主题切换 ─────────────────────────────────────────────────────────────
    {
        let win = w.as_weak();
        w.on_do_toggle_theme(move || {
            let Some(w) = win.upgrade() else { return };
            let dark = w.global::<ThemeState>().get_dark();
            let new_dark = !dark;
            w.global::<ThemeState>().set_dark(new_dark);
            ClientConfig::set_dark_mode(new_dark);
        });
    }

    // ── 刷新订阅套餐列表 ─────────────────────────────────────────────────────
    {
        let win = w.as_weak();
        w.on_do_refresh_plans(move || {
            let Some(w) = win.upgrade() else { return };
            w.set_pay_loading(true);
            let win2 = win.clone();
            std::thread::spawn(move || {
                let resp_plans = ipc_call(ipc_port, r#"{"cmd":"get_plans"}"#);
                let resp_my    = ipc_call(ipc_port, r#"{"cmd":"auth_subscription"}"#);
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(w) = win2.upgrade() else { return };
                    w.set_pay_loading(false);
                    let current_plan_name = serde_json::from_str::<serde_json::Value>(&resp_my)
                        .ok()
                        .and_then(|v| v["subscription"]["plan_name"].as_str().map(|s| s.to_owned()))
                        .unwrap_or_default();
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&resp_plans) {
                        if let Some(arr) = v["plans"].as_array() {
                            let items: Vec<PlanItem> = arr.iter().map(|p| PlanItem {
                                id:               p["id"].as_i64().unwrap_or(0) as i32,
                                name:             p["name"].as_str().unwrap_or("").into(),
                                display_name:     p["display_name"].as_str().unwrap_or("").into(),
                                price_monthly:    p["price_monthly"].as_f64().unwrap_or(0.0) as f32,
                                device_limit:     p["device_limit"].as_i64().unwrap_or(0) as i32,
                                speed_limit_kbps: p["speed_limit_kbps"].as_i64().unwrap_or(0) as i32,
                                description:      p["description"].as_str().unwrap_or("").into(),
                                is_current:       p["name"].as_str().unwrap_or("") == current_plan_name,
                            }).collect();
                            w.set_pay_plans(ModelRc::new(VecModel::from(items)));
                        }
                    }
                });
            });
        });
    }

    // ── 支付宝扫码支付 ───────────────────────────────────────────────────────
    {
        let win = w.as_weak();
        w.on_do_pay_alipay(move |plan_id| {
            let Some(w) = win.upgrade() else { return };
            w.set_pay_status_msg("正在创建支付宝订单…".into());
            w.set_pay_status_ok(false);
            w.set_pay_qr_visible(false);
            let win2 = w.as_weak();
            std::thread::spawn(move || {
                let cmd = format!(r#"{{"cmd":"payment_create","plan_id":{},"method":"alipay"}}"#, plan_id);
                let resp = ipc_call(ipc_port, &cmd);
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(w) = win2.upgrade() else { return };
                    match serde_json::from_str::<serde_json::Value>(&resp) {
                        Ok(v) => {
                            let d = v.get("data").unwrap_or(&v);
                            if let Some(qr) = d["qr_content"].as_str() {
                                if let Some(name) = d["plan_name"].as_str() {
                                    w.set_pay_selected_plan_name(name.into());
                                }
                                match make_qr_image(qr) {
                                    Ok(img) => {
                                        w.set_pay_qr_image(img);
                                        w.set_pay_qr_visible(true);
                                        w.set_pay_status_msg("请使用支付宝扫码完成支付".into());
                                        w.set_pay_status_ok(true);
                                        if let Some(order_no) = d["order_no"].as_str() {
                                            start_payment_poll(win2.clone(), ipc_port, order_no.to_string());
                                        }
                                    }
                                    Err(e) => {
                                        w.set_pay_status_msg(format!("二维码生成失败: {}", e).into());
                                        w.set_pay_status_ok(false);
                                    }
                                }
                            } else {
                                let msg = v["message"].as_str().unwrap_or("下单失败，请检查支付宝配置");
                                w.set_pay_status_msg(format!("❌ {}", msg).into());
                                w.set_pay_status_ok(false);
                            }
                        }
                        Err(_) => {
                            w.set_pay_status_msg("❌ 网络错误".into());
                            w.set_pay_status_ok(false);
                        }
                    }
                });
            });
        });
    }

    // ── Stripe 支付 ──────────────────────────────────────────────────────────
    {
        let win = w.as_weak();
        w.on_do_pay_stripe(move |plan_id| {
            let Some(w) = win.upgrade() else { return };
            w.set_pay_status_msg("正在创建 Stripe 会话…".into());
            w.set_pay_status_ok(false);
            let win2 = w.as_weak();
            std::thread::spawn(move || {
                let cmd = format!(r#"{{"cmd":"payment_create","plan_id":{},"method":"stripe"}}"#, plan_id);
                let resp = ipc_call(ipc_port, &cmd);
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(w) = win2.upgrade() else { return };
                    match serde_json::from_str::<serde_json::Value>(&resp) {
                        Ok(v) => {
                            let d = v.get("data").unwrap_or(&v);
                            if let Some(url) = d["checkout_url"].as_str() {
                                if let Err(e) = open::that(url) {
                                    w.set_pay_status_msg(format!("❌ 无法打开浏览器: {}", e).into());
                                    w.set_pay_status_ok(false);
                                } else {
                                    w.set_pay_status_msg("✅ 已在浏览器打开 Stripe，请完成支付后刷新套餐".into());
                                    w.set_pay_status_ok(true);
                                    if let Some(order_no) = d["order_no"].as_str() {
                                        start_payment_poll(win2.clone(), ipc_port, order_no.to_string());
                                    }
                                }
                            } else {
                                let msg = v["message"].as_str().unwrap_or("Stripe 下单失败");
                                w.set_pay_status_msg(format!("❌ {}", msg).into());
                                w.set_pay_status_ok(false);
                            }
                        }
                        Err(_) => {
                            w.set_pay_status_msg("❌ 网络错误".into());
                            w.set_pay_status_ok(false);
                        }
                    }
                });
            });
        });
    }

    // ── 关闭支付宝二维码 ─────────────────────────────────────────────────────
    {
        let win = w.as_weak();
        w.on_do_close_qr(move || {
            if let Some(w) = win.upgrade() {
                w.set_pay_qr_visible(false);
                w.set_pay_status_msg("".into());
            }
        });
    }

    // ── 手动检查更新 ─────────────────────────────────────────────────────────
    {
        let win = w.as_weak();
        w.on_do_check_update(move || {
            let api_url = crate::config::ClientConfig::get_api_url();
            if api_url.is_empty() {
                if let Some(w) = win.upgrade() {
                    w.set_update_msg("请先在设置中配置 API 地址".into());
                    w.set_update_msg_ok(false);
                }
                return;
            }
            if let Some(w) = win.upgrade() {
                w.set_update_checking(true);
                w.set_update_msg("".into());
                w.set_update_available(false);
            }
            let win2 = win.clone();
            std::thread::spawn(move || {
                let result = crate::updater::check_update(&api_url);
                if let Ok(Some(ref info)) = result {
                    if let Ok(mut stored) = crate::updater::SOFTWARE_UPDATE_INFO.lock() {
                        *stored = Some(info.clone());
                    }
                    if let Ok(mut url) = crate::updater::SOFTWARE_UPDATE_URL.lock() {
                        *url = info.download_url.clone();
                    }
                }
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(w) = win2.upgrade() else { return };
                    w.set_update_checking(false);
                    match result {
                        Ok(Some(info)) => {
                            w.set_update_available(true);
                            w.set_update_latest_ver(info.version.as_str().into());
                            w.set_update_changelog(info.changelog.as_str().into());
                            w.set_update_msg("发现新版本！".into());
                            w.set_update_msg_ok(true);
                        }
                        Ok(None) => {
                            w.set_update_msg("已是最新版本".into());
                            w.set_update_msg_ok(true);
                        }
                        Err(e) => {
                            w.set_update_msg(format!("检查失败: {e}").as_str().into());
                            w.set_update_msg_ok(false);
                        }
                    }
                });
            });
        });
    }

    // ── 下载并安装更新 ───────────────────────────────────────────────────────
    {
        let win = w.as_weak();
        w.on_do_apply_update(move || {
            let info = match crate::updater::SOFTWARE_UPDATE_INFO.lock().ok().and_then(|g| g.clone()) {
                Some(i) => i,
                None => {
                    if let Some(w) = win.upgrade() {
                        w.set_update_msg("请先点击 [检查更新]".into());
                        w.set_update_msg_ok(false);
                    }
                    return;
                }
            };
            if let Some(w) = win.upgrade() {
                w.set_update_downloading(true);
                w.set_update_progress(0.0);
                w.set_update_msg("正在下载…".into());
                w.set_update_msg_ok(true);
            }
            let win2 = win.clone();
            std::thread::spawn(move || {
                let win3 = win2.clone();
                let result = crate::updater::download_and_apply(&info, move |p| {
                    let win4 = win3.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(w) = win4.upgrade() {
                            w.set_update_progress(p);
                        }
                    });
                });
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = win2.upgrade() {
                        w.set_update_downloading(false);
                        if let Err(e) = result {
                            w.set_update_msg(format!("更新失败: {e}").as_str().into());
                            w.set_update_msg_ok(false);
                        }
                    }
                });
            });
        });
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// 托盘事件处理
// ──────────────────────────────────────────────────────────────────────────────

fn handle_tray_action(w: &AppWindow, action: TrayAction) {
    match action {
        TrayAction::ToggleWindow => {
            let visible = w.window().is_visible();
            if visible { w.window().hide().ok(); }
            else { w.window().show().ok(); w.window().request_redraw(); }
        }
        TrayAction::GoHome    => { w.window().show().ok(); w.set_page(0); }
        TrayAction::GoConnect => { w.window().show().ok(); w.set_page(0); }
        TrayAction::GoAccount => { w.window().show().ok(); w.set_page(3); }
        TrayAction::Quit      => slint::quit_event_loop().ok().unwrap_or(()),
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// IPC 调用（在后台线程使用）
// ──────────────────────────────────────────────────────────────────────────────

fn ipc_call(ipc_port: u16, cmd: &str) -> String {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpStream;

    let addr = format!("127.0.0.1:{}", ipc_port);
    let Ok(mut stream) = TcpStream::connect_timeout(
        &addr.parse().unwrap_or("127.0.0.1:21114".parse().unwrap()),
        Duration::from_millis(800),
    ) else {
        return r#"{"error":"IPC 连接失败（守护进程未运行？）"}"#.to_owned();
    };

    let mut msg = cmd.to_owned();
    msg.push('\n');
    if stream.write_all(msg.as_bytes()).is_err() {
        return r#"{"error":"IPC 写入失败"}"#.to_owned();
    }

    let mut reader = BufReader::new(stream);
    let mut resp = String::new();
    if reader.read_line(&mut resp).is_err() {
        return r#"{"error":"IPC 读取失败"}"#.to_owned();
    }
    resp.trim().to_owned()
}

// ──────────────────────────────────────────────────────────────────────────────
// 支付状态轮询
// ──────────────────────────────────────────────────────────────────────────────

fn start_payment_poll(win: slint::Weak<AppWindow>, ipc_port: u16, order_no: String) {
    std::thread::spawn(move || {
        for _ in 0..40 {
            std::thread::sleep(Duration::from_secs(3));
            let cmd = format!(r#"{{"cmd":"payment_status","order_no":"{}"}}"#, order_no);
            let resp = ipc_call(ipc_port, &cmd);
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&resp) {
                if v["status"].as_str().unwrap_or("") == "paid" {
                    let resp_plans = ipc_call(ipc_port, r#"{"cmd":"get_plans"}"#);
                    let resp_my    = ipc_call(ipc_port, r#"{"cmd":"auth_subscription"}"#);
                    let win2 = win.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        let Some(w) = win2.upgrade() else { return };
                        w.set_pay_qr_visible(false);
                        w.set_pay_status_msg("✅ 支付成功！订阅已激活".into());
                        w.set_pay_status_ok(true);
                        let current_plan_name = serde_json::from_str::<serde_json::Value>(&resp_my)
                            .ok()
                            .and_then(|v| v["subscription"]["plan_name"].as_str().map(|s| s.to_owned()))
                            .unwrap_or_default();
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&resp_plans) {
                            if let Some(arr) = v["plans"].as_array() {
                                let items: Vec<PlanItem> = arr.iter().map(|p| PlanItem {
                                    id:               p["id"].as_i64().unwrap_or(0) as i32,
                                    name:             p["name"].as_str().unwrap_or("").into(),
                                    display_name:     p["display_name"].as_str().unwrap_or("").into(),
                                    price_monthly:    p["price_monthly"].as_f64().unwrap_or(0.0) as f32,
                                    device_limit:     p["device_limit"].as_i64().unwrap_or(0) as i32,
                                    speed_limit_kbps: p["speed_limit_kbps"].as_i64().unwrap_or(0) as i32,
                                    description:      p["description"].as_str().unwrap_or("").into(),
                                    is_current:       p["name"].as_str().unwrap_or("") == current_plan_name,
                                }).collect();
                                w.set_pay_plans(ModelRc::new(VecModel::from(items)));
                            }
                        }
                    });
                    return;
                }
            }
        }
    });
}

// ──────────────────────────────────────────────────────────────────────────────
// 格式化工具函数
// ──────────────────────────────────────────────────────────────────────────────

fn nat_type_name(t: i64) -> &'static str {
    match t {
        1 => "对称 NAT（仅中继）",
        2 => "对称 UDP 防火墙",
        3 => "完全锥形（最优）",
        4 => "受限锥形",
        5 => "端口受限锥形",
        _ => "未知",
    }
}

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 { format!("{} B", bytes) }
    else if bytes < 1024 * 1024 { format!("{:.1} KB", bytes as f64 / 1024.0) }
    else { format!("{:.2} MB", bytes as f64 / 1024.0 / 1024.0) }
}

fn format_ts(ts: u64) -> String {
    if ts == 0 { return "—".to_owned(); }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let diff = now.saturating_sub(ts);
    if diff < 60 { format!("{}秒前", diff) }
    else if diff < 3600 { format!("{}分钟前", diff / 60) }
    else { format!("{}小时前", diff / 3600) }
}

fn format_duration(secs: i64) -> String {
    if secs <= 0 { return "已过期".to_owned(); }
    if secs < 60 { format!("{}秒", secs) }
    else if secs < 3600 { format!("{}分钟", secs / 60) }
    else { format!("{:.1}小时", secs as f64 / 3600.0) }
}

fn escape_json(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

// ──────────────────────────────────────────────────────────────────────────────
// 二维码图片生成
// ──────────────────────────────────────────────────────────────────────────────

fn make_qr_image(content: &str) -> Result<slint::Image, String> {
    use image::{DynamicImage, Luma};
    use qrcode::QrCode;

    let code = QrCode::new(content.as_bytes())
        .map_err(|e| format!("QR 编码失败: {}", e))?;
    let luma: image::ImageBuffer<Luma<u8>, Vec<u8>> = code
        .render::<Luma<u8>>()
        .quiet_zone(true)
        .module_dimensions(6, 6)
        .build();
    let rgba   = DynamicImage::ImageLuma8(luma).to_rgba8();
    let width  = rgba.width();
    let height = rgba.height();
    let raw    = rgba.into_raw();
    let pixel_buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
        bytemuck::cast_slice(&raw), width, height,
    );
    Ok(slint::Image::from_rgba8(pixel_buf))
}

// ──────────────────────────────────────────────────────────────────────────────
// Logo 加载
// ──────────────────────────────────────────────────────────────────────────────

fn load_logo_image() -> Option<slint::Image> {
    const ICON_BYTES: &[u8] = include_bytes!("../../icons/logo.ico");
    let img = image::load_from_memory_with_format(ICON_BYTES, image::ImageFormat::Ico).ok()?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    let pixel_buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
        bytemuck::cast_slice(rgba.as_raw()), w, h,
    );
    Some(slint::Image::from_rgba8(pixel_buf))
}
