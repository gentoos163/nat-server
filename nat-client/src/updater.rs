//! 自动更新模块（参考 RustDesk 实现）
//!
//! ## 设计
//! - **后台线程** 由 `start_auto_update_check` 创建，每 24 小时自动检查一次；
//!   启动 30 秒后首次执行，通过 `TX_MSG` channel 接收外部控制信号。
//! - **手动检查** 由 UI 直接调用 `check_update`（在独立线程中），与后台线程无关。
//! - **下载优化** 先发 HEAD 请求获取文件大小，若本地临时文件已存在且大小一致则跳过下载。
//! - **Windows 替换策略**（参考 RustDesk `update_new_version`）：
//!   旧进程将下载好的新 exe 以 `--apply-update <old-exe-path>` 启动后立即 `exit(0)`；
//!   新 exe 等待旧进程释放文件锁（轮询直到可写），复制自身覆盖旧路径，再从旧路径重启。
//! - **Unix 替换策略**：直接覆写当前路径（运行中进程 inode 不变），`exec` 替换进程镜像。

use core_common::log;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

// ── 全局状态（对应 RustDesk 的 TX_MSG / SOFTWARE_UPDATE_URL）────────────────

/// 控制后台检查线程的发送端
static TX_MSG: Mutex<Option<Sender<UpdateMsg>>> = Mutex::new(None);

/// 最近一次检查到的更新信息（含下载 URL 和 SHA256），供 apply 阶段使用
pub static SOFTWARE_UPDATE_URL: Mutex<String> = Mutex::new(String::new());
pub static SOFTWARE_UPDATE_INFO: Mutex<Option<UpdateInfo>> = Mutex::new(None);

/// 线程控制消息
pub enum UpdateMsg {
    CheckUpdate,
    Exit,
}

const INITIAL_DELAY: Duration = Duration::from_secs(30);
const AUTO_CHECK_INTERVAL: Duration = Duration::from_secs(24 * 3600);
const MIN_CHECK_INTERVAL: Duration = Duration::from_secs(10 * 60);

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

// ── 数据结构 ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct UpdateInfo {
    pub version: String,
    pub download_url: String,
    pub sha256: String,
    pub changelog: String,
}

// ── 公开控制 API ──────────────────────────────────────────────────────────────

/// 启动后台自动检查线程（每 24h 一次）。
/// `on_update_found` 在发现新版本时被调用（来自后台线程，需通过 invoke_from_event_loop 回 UI）。
pub fn start_auto_update_check<F>(api_url: String, on_update_found: F)
where
    F: Fn(UpdateInfo) + Send + 'static,
{
    let (tx, rx) = mpsc::channel::<UpdateMsg>();
    if let Ok(mut guard) = TX_MSG.lock() {
        *guard = Some(tx);
    }

    std::thread::spawn(move || {
        // 启动后等待 30 秒，避免影响启动速度
        // 用 recv_timeout 等待，以便 Exit 信号能立即中断
        match rx.recv_timeout(INITIAL_DELAY) {
            Ok(UpdateMsg::Exit) | Err(RecvTimeoutError::Disconnected) => return,
            _ => {}
        }

        // 首次检查（每次启动 app 均执行）
        do_auto_check(&api_url, &on_update_found);
        let mut last_check_time = std::time::Instant::now();

        // 后续每 24 小时检查一次
        loop {
            let wait = AUTO_CHECK_INTERVAL.saturating_sub(last_check_time.elapsed());

            match rx.recv_timeout(wait) {
                Ok(UpdateMsg::CheckUpdate) => {
                    // 手动触发：尊重最小间隔，防止频繁触发
                    if last_check_time.elapsed() >= MIN_CHECK_INTERVAL {
                        do_auto_check(&api_url, &on_update_found);
                        last_check_time = std::time::Instant::now();
                    }
                }
                Ok(UpdateMsg::Exit) | Err(RecvTimeoutError::Disconnected) => {
                    log::info!("[updater] 后台检查线程退出");
                    break;
                }
                Err(RecvTimeoutError::Timeout) => {
                    // 24 小时定时触发
                    do_auto_check(&api_url, &on_update_found);
                    last_check_time = std::time::Instant::now();
                }
            }
        }
    });
}

/// 停止后台检查线程。
pub fn stop_auto_update() {
    if let Ok(guard) = TX_MSG.lock() {
        if let Some(tx) = guard.as_ref() {
            let _ = tx.send(UpdateMsg::Exit);
        }
    }
}

fn do_auto_check<F>(api_url: &str, callback: &F)
where
    F: Fn(UpdateInfo),
{
    match check_update(api_url) {
        Ok(Some(info)) => {
            store_update_info(&info);
            callback(info);
        }
        Ok(None) => log::debug!("[updater] 已是最新版本"),
        Err(e) => log::warn!("[updater] 自动检查失败: {e}"),
    }
}

fn store_update_info(info: &UpdateInfo) {
    if let Ok(mut url) = SOFTWARE_UPDATE_URL.lock() {
        *url = info.download_url.clone();
    }
    if let Ok(mut stored) = SOFTWARE_UPDATE_INFO.lock() {
        *stored = Some(info.clone());
    }
}

// ── 版本检查 ──────────────────────────────────────────────────────────────────

/// 向服务端查询最新版本。
/// 返回 `Some(UpdateInfo)` 表示有更新，`None` 表示已是最新。
pub fn check_update(api_url: &str) -> Result<Option<UpdateInfo>, String> {
    let url = format!("{}/api/client/version", api_url.trim_end_matches('/'));
    let client = build_client()?;
    let resp = client
        .get(&url)
        .send()
        .map_err(|e| format!("网络错误: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("服务器返回 {}", resp.status()));
    }
    let json: serde_json::Value = resp.json().map_err(|e| format!("响应解析失败: {e}"))?;

    let latest = json["version"].as_str().unwrap_or("").to_string();
    if latest.is_empty() {
        return Err("服务器未配置客户端版本信息".to_string());
    }
    if !is_newer(&latest, CURRENT_VERSION) {
        return Ok(None);
    }

    let download_url = platform_field(&json, "download_url");
    if download_url.is_empty() {
        return Err("服务器未提供当前平台的下载链接".to_string());
    }

    Ok(Some(UpdateInfo {
        version: latest,
        download_url,
        sha256: platform_field(&json, "sha256"),
        changelog: json["changelog"].as_str().unwrap_or("").to_string(),
    }))
}

fn is_newer(latest: &str, current: &str) -> bool {
    let parse = |s: &str| -> Vec<u64> {
        s.split('.').map(|p| p.parse().unwrap_or(0)).collect()
    };
    parse(latest) > parse(current)
}

fn platform_field(json: &serde_json::Value, prefix: &str) -> String {
    let suffix = if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };
    json[format!("{prefix}_{suffix}")]
        .as_str()
        .unwrap_or("")
        .to_string()
}

// ── 下载 + 校验 + 应用 ───────────────────────────────────────────────────────

/// 下载并安装更新。`progress` 接收 0.0‥1.0 进度（来自后台线程）。
///
/// 成功时当前进程将退出（Windows）或被替换（Unix），不会返回 Ok。
/// 失败时返回 Err，当前进程继续运行。
pub fn download_and_apply<F>(info: &UpdateInfo, progress: F) -> Result<(), String>
where
    F: Fn(f32),
{
    let tmp = get_download_path(&info.download_url);
    log::info!("[updater] 下载目标: {:?}", tmp);

    // 参考 RustDesk：HEAD 请求获取文件大小，已下载则跳过
    let remote_size = head_content_length(&info.download_url)?;
    let already_downloaded = tmp.exists()
        && remote_size.map_or(false, |s| {
            std::fs::metadata(&tmp).map(|m| m.len() == s).unwrap_or(false)
        });

    if already_downloaded {
        log::info!("[updater] 文件已存在且大小一致，跳过下载");
        progress(1.0);
    } else {
        download_to_file(&info.download_url, &tmp, &progress)?;
    }

    if !info.sha256.is_empty() {
        verify_sha256(&tmp, &info.sha256)?;
    }

    apply_update(&tmp)
}

/// 从 URL 末尾提取文件名，拼到临时目录（对应 RustDesk `get_download_file_from_url`）。
pub fn get_download_path(url: &str) -> PathBuf {
    let filename = url.split('/').last().unwrap_or("nat-client-update");
    std::env::temp_dir().join(filename)
}

fn head_content_length(url: &str) -> Result<Option<u64>, String> {
    let client = build_client()?;
    let resp = client
        .head(url)
        .send()
        .map_err(|e| format!("HEAD 请求失败: {e}"))?;
    Ok(resp.content_length())
}

fn download_to_file<F>(url: &str, dest: &Path, progress: &F) -> Result<(), String>
where
    F: Fn(f32),
{
    let client = build_client()?;
    let mut resp = client
        .get(url)
        .send()
        .map_err(|e| format!("下载请求失败: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("下载失败 HTTP {}", resp.status()));
    }

    let total = resp.content_length().unwrap_or(0);
    let mut file = std::fs::File::create(dest)
        .map_err(|e| format!("创建临时文件失败: {e}"))?;

    let mut downloaded = 0u64;
    let mut buf = [0u8; 65536];
    loop {
        let n = resp.read(&mut buf).map_err(|e| format!("读取响应失败: {e}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| format!("写入失败: {e}"))?;
        downloaded += n as u64;
        if total > 0 {
            progress(downloaded as f32 / total as f32);
        }
    }
    progress(1.0);
    Ok(())
}

fn verify_sha256(path: &Path, expected: &str) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    let data = std::fs::read(path).map_err(|e| format!("读取文件失败: {e}"))?;
    let hash = format!("{:x}", Sha256::digest(&data));
    if hash.to_lowercase() != expected.trim().to_lowercase() {
        // 删除损坏的下载文件
        let _ = std::fs::remove_file(path);
        return Err(format!(
            "SHA256 校验不符\n期望: {}\n实际: {}",
            expected, hash
        ));
    }
    Ok(())
}

fn build_client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(300))
        .build()
        .map_err(|e| format!("HTTP 客户端创建失败: {e}"))
}

// ── 平台替换 ─────────────────────────────────────────────────────────────────

fn apply_update(new_binary: &Path) -> Result<(), String> {
    apply_platform(new_binary)
}

/// Windows：参考 RustDesk `update_new_version`。
/// 以 `--apply-update <当前exe路径>` 启动新 exe，然后当前进程退出。
/// 新进程等待旧进程释放文件锁后完成替换。
#[cfg(windows)]
fn apply_platform(new_binary: &Path) -> Result<(), String> {
    let current = std::env::current_exe()
        .map_err(|e| format!("获取当前路径失败: {e}"))?;

    log::info!(
        "[updater] 启动新版本: {:?} --apply-update {:?}",
        new_binary,
        current
    );

    std::process::Command::new(new_binary)
        .args(["--apply-update", current.to_str().unwrap_or("")])
        .spawn()
        .map_err(|e| {
            // 启动失败：删除损坏的下载
            let _ = std::fs::remove_file(new_binary);
            format!("启动更新程序失败: {e}")
        })?;

    std::process::exit(0);
}

/// Unix：直接覆写当前路径（运行中进程持有 inode，rename 不影响），
/// 然后用 `exec` 替换进程镜像（不产生新 PID）。
#[cfg(not(windows))]
fn apply_platform(new_binary: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::CommandExt;

    let current = std::env::current_exe()
        .map_err(|e| format!("获取当前路径失败: {e}"))?;

    std::fs::copy(new_binary, &current)
        .map_err(|e| format!("替换程序文件失败: {e}"))?;
    std::fs::set_permissions(&current, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| format!("设置执行权限失败: {e}"))?;
    let _ = std::fs::remove_file(new_binary);

    let err = std::process::Command::new(&current)
        .args(std::env::args().skip(1))
        .exec();
    Err(format!("exec 失败: {err}"))
}

// ── Windows --apply-update 处理 ──────────────────────────────────────────────

/// 由 `--apply-update <old-exe-path>` 子命令调用（仅 Windows）。
///
/// 当前 exe 位于临时目录，等待旧 exe 进程释放文件锁，
/// 然后将自身复制到旧路径，再从旧路径启动，最后退出。
#[cfg(windows)]
pub fn apply_update_from_cli(old_exe_path: &str) {
    use std::fs::OpenOptions;

    let old_path = Path::new(old_exe_path);
    log::info!("[apply-update] 等待旧进程 {:?} 释放文件锁", old_path);

    // 轮询直到可以独占写入旧路径（旧进程已退出）
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        if std::time::Instant::now() >= deadline {
            eprintln!("[apply-update] 等待旧进程退出超时，放弃更新");
            std::process::exit(1);
        }
        match OpenOptions::new().write(true).open(old_path) {
            Ok(_) => break,
            Err(_) => std::thread::sleep(Duration::from_millis(500)),
        }
    }

    let current = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[apply-update] 获取当前路径失败: {e}");
            std::process::exit(1);
        }
    };

    log::info!("[apply-update] 复制 {:?} → {:?}", current, old_path);
    if let Err(e) = std::fs::copy(&current, old_path) {
        eprintln!("[apply-update] 复制失败: {e}");
        std::process::exit(1);
    }

    log::info!("[apply-update] 从 {:?} 重启", old_path);
    std::process::Command::new(old_path)
        .spawn()
        .expect("重启失败");

    std::process::exit(0);
}

/// Unix 不使用 --apply-update 流程（直接覆写），提供空实现避免 cfg 泄漏到调用处。
#[cfg(not(windows))]
pub fn apply_update_from_cli(_old_exe_path: &str) {
    eprintln!("[apply-update] 此平台不使用 --apply-update 流程");
    std::process::exit(1);
}
