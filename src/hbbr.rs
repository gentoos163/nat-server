///
/// hbbr - RustDesk Heartbeat Bridge Server
///
/// This is a Rust implementation of the hbbr (heartbeat bridge) server
/// that handles NAT traversal and connection establishment for RustDesk.
///
/// The hbbr server is responsible for:
/// - Maintaining NAT mappings through periodic heartbeat messages
/// - Facilitating direct peer-to-peer connections between RustDesk clients
/// - Acting as a bridge when direct connections cannot be established
///
use clap::App;
mod common;
mod relay_server;
use core_common::{config::RELAY_PORT, ResultType};
use flexi_logger::*;
use relay_server::*;
mod version;

/// 从 INI/TOML 文件的顶层（无分节）读取键值并写入环境变量（大写 key）。
fn load_flat_env(path: &str) {
    if let Ok(v) = ini::Ini::load_from_file(path) {
        if let Some(section) = v.section(None::<String>) {
            section.iter().for_each(|(k, v)| {
                std::env::set_var(k.to_uppercase(), v);
            });
        }
    }
}

/// 从 hbbr.toml 的各分节中读取参数，写入环境变量。
/// 映射关系：[server].port → PORT，[key].secret → KEY。
fn load_hbbr_toml_env(path: &str) {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return,
    };
    let mut section = String::new();
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with('[') && t.ends_with(']') {
            section = t[1..t.len() - 1].to_string();
            continue;
        }
        if t.starts_with('#') || t.is_empty() {
            continue;
        }
        if let Some(pos) = t.find('=') {
            let k = t[..pos].trim();
            let v = t[pos + 1..].trim().trim_matches('"');
            match (section.as_str(), k) {
                ("server", "port")   => std::env::set_var("PORT", v),
                ("key",    "secret") => std::env::set_var("KEY", v),
                _ => {}
            }
        }
    }
}

fn main() -> ResultType<()> {
    let _logger = Logger::try_with_env_or_str("debug")?
        .log_to_stdout()
        .format(opt_format)
        .write_mode(WriteMode::Async)
        .start()?;
    let args = format!(
        "-p, --port=[NUMBER(default={RELAY_PORT})] 'Sets the listening port'
        -k, --key=[KEY] 'Only allow the client with the same key'
        ",
    );
    let matches = App::new("hbbr")
        .version(version::VERSION)
        .author("Purslane Ltd. <info@rustdesk.com>")
        .about("RustDesk Relay Server")
        .args_from_usage(&args)
        .get_matches();

    // 加载顺序（后者覆盖前者）：.env → config.toml → hbbr.toml → 命令行
    load_flat_env(".env");
    load_flat_env("config.toml");
    load_hbbr_toml_env("hbbr.toml");

    let mut port = RELAY_PORT;
    if let Ok(v) = std::env::var("PORT") {
        if let Ok(p) = v.parse::<i32>() {
            if p > 0 {
                port = p;
            }
        }
    }
    let key = matches
        .value_of("key")
        .map(|s| s.to_string())
        .unwrap_or_else(|| std::env::var("KEY").unwrap_or_default());
    let port_str = matches
        .value_of("port")
        .map(|s| s.to_string())
        .unwrap_or_else(|| port.to_string());

    start(&port_str, &key)?;
    Ok(())
}
