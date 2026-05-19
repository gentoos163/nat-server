use axum::{
    extract::{Extension, Path},
    http::{HeaderMap, StatusCode},
    response::Json,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::Instant;

use crate::api::{db_user_is_admin, jwt_user_id_from_headers, ApiResponse, ApiState};

const CONFIG_TOML: &str = "config.toml";
const HBBR_TOML: &str = "hbbr.toml";

/// 记录服务器启动时间
static START_TIME: LazyLock<Instant> = LazyLock::new(Instant::now);

/// 初始化启动时间（在 main 里调用一次以触发 LazyLock）
pub fn init_start_time() {
    let _ = *START_TIME;
}

// ─── 系统设置 ─────────────────────────────────────────────────────────────

/// GET /api/admin/settings
pub async fn get_settings(
    Extension(state): Extension<ApiState>,
    headers: HeaderMap,
) -> Result<Json<ApiResponse<HashMap<String, String>>>, StatusCode> {
    let caller = jwt_user_id_from_headers(&state.jwt_secret, &headers)?;
    if !db_user_is_admin(&state.db, caller).await {
        return Err(StatusCode::FORBIDDEN);
    }
    match state.db.get_all_settings().await {
        Ok(map) => Ok(Json(ApiResponse::success(map))),
        Err(e) => Ok(Json(ApiResponse::error(format!("获取设置失败: {}", e)))),
    }
}

#[derive(Debug, Deserialize)]
pub struct UpdateSettingBody {
    pub key: Option<String>,
    pub value: Option<String>,
    pub settings: Option<HashMap<String, String>>,
}

/// PUT /api/admin/settings
pub async fn update_settings(
    Extension(state): Extension<ApiState>,
    headers: HeaderMap,
    Json(body): Json<UpdateSettingBody>,
) -> Result<Json<ApiResponse<()>>, StatusCode> {
    let caller = jwt_user_id_from_headers(&state.jwt_secret, &headers)?;
    if !db_user_is_admin(&state.db, caller).await {
        return Err(StatusCode::FORBIDDEN);
    }

    // 批量更新
    if let Some(map) = body.settings {
        for (k, v) in &map {
            if let Err(e) = state.db.set_setting(k, v).await {
                return Ok(Json(ApiResponse::error(format!("保存设置失败: {}", e))));
            }
        }
        return Ok(Json(ApiResponse::success(())));
    }

    // 单条更新
    if let (Some(key), Some(value)) = (body.key, body.value) {
        match state.db.set_setting(&key, &value).await {
            Ok(_) => Ok(Json(ApiResponse::success(()))),
            Err(e) => Ok(Json(ApiResponse::error(format!("保存设置失败: {}", e)))),
        }
    } else {
        Ok(Json(ApiResponse::error("缺少 key/value 或 settings 字段".to_string())))
    }
}

// ─── 黑名单 / 阻止名单 辅助 ──────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct IpEntry {
    pub ip: String,
    pub comment: String,
}

fn parse_ip_file(content: &str) -> Vec<IpEntry> {
    content
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .map(|l| {
            let mut parts = l.splitn(2, ' ');
            let ip = parts.next().unwrap_or("").trim().to_string();
            let comment = parts.next().unwrap_or("").trim().to_string();
            IpEntry { ip, comment }
        })
        .filter(|e| !e.ip.is_empty())
        .collect()
}

fn serialize_ip_file(entries: &[IpEntry]) -> String {
    entries
        .iter()
        .map(|e| {
            if e.comment.is_empty() {
                e.ip.clone()
            } else {
                format!("{} {}", e.ip, e.comment)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

fn read_ip_file(path: &str) -> Vec<IpEntry> {
    match std::fs::read_to_string(path) {
        Ok(content) => parse_ip_file(&content),
        Err(_) => vec![],
    }
}

fn write_ip_file(path: &str, entries: &[IpEntry]) -> std::io::Result<()> {
    std::fs::write(path, serialize_ip_file(entries))
}

#[derive(Debug, Deserialize)]
pub struct AddIpBody {
    pub ip: String,
    #[serde(default)]
    pub comment: String,
}

// ─── 黑名单 API ──────────────────────────────────────────────────────────

const BLACKLIST_FILE: &str = "blacklist.txt";

/// GET /api/admin/blacklist
pub async fn get_blacklist(
    Extension(state): Extension<ApiState>,
    headers: HeaderMap,
) -> Result<Json<ApiResponse<serde_json::Value>>, StatusCode> {
    let caller = jwt_user_id_from_headers(&state.jwt_secret, &headers)?;
    if !db_user_is_admin(&state.db, caller).await {
        return Err(StatusCode::FORBIDDEN);
    }
    let entries = read_ip_file(BLACKLIST_FILE);
    Ok(Json(ApiResponse::success(serde_json::json!({ "blacklist": entries }))))
}

/// POST /api/admin/blacklist
pub async fn add_blacklist(
    Extension(state): Extension<ApiState>,
    headers: HeaderMap,
    Json(body): Json<AddIpBody>,
) -> Result<Json<ApiResponse<()>>, StatusCode> {
    let caller = jwt_user_id_from_headers(&state.jwt_secret, &headers)?;
    if !db_user_is_admin(&state.db, caller).await {
        return Err(StatusCode::FORBIDDEN);
    }
    let ip = body.ip.trim().to_string();
    if ip.is_empty() {
        return Ok(Json(ApiResponse::error("IP 地址不能为空".to_string())));
    }
    let mut entries = read_ip_file(BLACKLIST_FILE);
    if entries.iter().any(|e| e.ip == ip) {
        return Ok(Json(ApiResponse::error("IP 已在黑名单中".to_string())));
    }
    entries.push(IpEntry { ip, comment: body.comment });
    match write_ip_file(BLACKLIST_FILE, &entries) {
        Ok(_) => Ok(Json(ApiResponse::success(()))),
        Err(e) => Ok(Json(ApiResponse::error(format!("写入文件失败: {}", e)))),
    }
}

/// DELETE /api/admin/blacklist/:ip
pub async fn delete_blacklist(
    Extension(state): Extension<ApiState>,
    headers: HeaderMap,
    Path(ip): Path<String>,
) -> Result<Json<ApiResponse<()>>, StatusCode> {
    let caller = jwt_user_id_from_headers(&state.jwt_secret, &headers)?;
    if !db_user_is_admin(&state.db, caller).await {
        return Err(StatusCode::FORBIDDEN);
    }
    let mut entries = read_ip_file(BLACKLIST_FILE);
    let before = entries.len();
    entries.retain(|e| e.ip != ip);
    if entries.len() == before {
        return Ok(Json(ApiResponse::error("未找到该 IP".to_string())));
    }
    match write_ip_file(BLACKLIST_FILE, &entries) {
        Ok(_) => Ok(Json(ApiResponse::success(()))),
        Err(e) => Ok(Json(ApiResponse::error(format!("写入文件失败: {}", e)))),
    }
}

// ─── 阻止名单 API ─────────────────────────────────────────────────────────

const BLOCKLIST_FILE: &str = "blocklist.txt";

/// GET /api/admin/blocklist
pub async fn get_blocklist(
    Extension(state): Extension<ApiState>,
    headers: HeaderMap,
) -> Result<Json<ApiResponse<serde_json::Value>>, StatusCode> {
    let caller = jwt_user_id_from_headers(&state.jwt_secret, &headers)?;
    if !db_user_is_admin(&state.db, caller).await {
        return Err(StatusCode::FORBIDDEN);
    }
    let entries = read_ip_file(BLOCKLIST_FILE);
    Ok(Json(ApiResponse::success(serde_json::json!({ "blocklist": entries }))))
}

/// POST /api/admin/blocklist
pub async fn add_blocklist(
    Extension(state): Extension<ApiState>,
    headers: HeaderMap,
    Json(body): Json<AddIpBody>,
) -> Result<Json<ApiResponse<()>>, StatusCode> {
    let caller = jwt_user_id_from_headers(&state.jwt_secret, &headers)?;
    if !db_user_is_admin(&state.db, caller).await {
        return Err(StatusCode::FORBIDDEN);
    }
    let ip = body.ip.trim().to_string();
    if ip.is_empty() {
        return Ok(Json(ApiResponse::error("IP 地址不能为空".to_string())));
    }
    let mut entries = read_ip_file(BLOCKLIST_FILE);
    if entries.iter().any(|e| e.ip == ip) {
        return Ok(Json(ApiResponse::error("IP 已在阻止名单中".to_string())));
    }
    entries.push(IpEntry { ip, comment: body.comment });
    match write_ip_file(BLOCKLIST_FILE, &entries) {
        Ok(_) => Ok(Json(ApiResponse::success(()))),
        Err(e) => Ok(Json(ApiResponse::error(format!("写入文件失败: {}", e)))),
    }
}

/// DELETE /api/admin/blocklist/:ip
pub async fn delete_blocklist(
    Extension(state): Extension<ApiState>,
    headers: HeaderMap,
    Path(ip): Path<String>,
) -> Result<Json<ApiResponse<()>>, StatusCode> {
    let caller = jwt_user_id_from_headers(&state.jwt_secret, &headers)?;
    if !db_user_is_admin(&state.db, caller).await {
        return Err(StatusCode::FORBIDDEN);
    }
    let mut entries = read_ip_file(BLOCKLIST_FILE);
    let before = entries.len();
    entries.retain(|e| e.ip != ip);
    if entries.len() == before {
        return Ok(Json(ApiResponse::error("未找到该 IP".to_string())));
    }
    match write_ip_file(BLOCKLIST_FILE, &entries) {
        Ok(_) => Ok(Json(ApiResponse::success(()))),
        Err(e) => Ok(Json(ApiResponse::error(format!("写入文件失败: {}", e)))),
    }
}

// ─── 系统信息 ─────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct SysInfo {
    pub version: String,
    pub uptime_secs: u64,
    pub os: String,
    pub arch: String,
    pub db_path: String,
}

/// GET /api/admin/sysinfo
pub async fn get_sysinfo(
    Extension(state): Extension<ApiState>,
    headers: HeaderMap,
) -> Result<Json<ApiResponse<SysInfo>>, StatusCode> {
    let caller = jwt_user_id_from_headers(&state.jwt_secret, &headers)?;
    if !db_user_is_admin(&state.db, caller).await {
        return Err(StatusCode::FORBIDDEN);
    }

    let uptime_secs = START_TIME.elapsed().as_secs();

    let info = SysInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        uptime_secs,
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        db_path: "db_v2.sqlite3".to_string(),
    };
    Ok(Json(ApiResponse::success(info)))
}

// ─── 服务器参数（config.toml / hbbr.toml 读写）────────────────────────────

/// 从 config.toml 中读取 hbbs 相关的顶层键值（不属于任何 [section]）。
/// 映射关系：
///   port            → hbbs_port
///   serial          → hbbs_serial
///   key             → hbbs_key
///   mask            → hbbs_lan_mask
///   relay_servers   → hbbr_relay_host
fn read_config_toml() -> HashMap<String, String> {
    let mut map = HashMap::new();
    let content = match std::fs::read_to_string(CONFIG_TOML) {
        Ok(c) => c,
        Err(_) => return map,
    };
    let mut in_section = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_section = true;
            continue;
        }
        if in_section || trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        if let Some((k, v)) = split_ini_kv(trimmed) {
            let api_key = match k {
                "port"          => "hbbs_port",
                "serial"        => "hbbs_serial",
                "key"           => "hbbs_key",
                "mask"          => "hbbs_lan_mask",
                "relay_servers" => "hbbr_relay_host",
                _ => continue,
            };
            map.insert(api_key.to_string(), v.to_string());
        }
    }
    map
}

/// 从 hbbr.toml 中读取 hbbr 参数（有 [section]）。
/// 映射关系：
///   [server].port             → hbbr_port
///   [server].websocket_port   → hbbr_websocket_port
///   [key].secret              → hbbr_key
///   [connection].max_connections → hbbr_max_connections
///   [bandwidth].total_limit   → hbbr_total_bandwidth
///   [bandwidth].single_limit  → hbbr_single_bandwidth
fn read_hbbr_toml() -> HashMap<String, String> {
    let mut map = HashMap::new();
    let content = match std::fs::read_to_string(HBBR_TOML) {
        Ok(c) => c,
        Err(_) => return map,
    };
    let mut section = String::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            section = trimmed[1..trimmed.len() - 1].to_string();
            continue;
        }
        if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        if let Some((k, v)) = split_ini_kv(trimmed) {
            let api_key = match (section.as_str(), k) {
                ("server",     "port")             => "hbbr_port",
                ("server",     "websocket_port")   => "hbbr_websocket_port",
                ("key",        "secret")           => "hbbr_key",
                ("connection", "max_connections")  => "hbbr_max_connections",
                ("bandwidth",  "total_limit")      => "hbbr_total_bandwidth",
                ("bandwidth",  "single_limit")     => "hbbr_single_bandwidth",
                _ => continue,
            };
            map.insert(api_key.to_string(), v.to_string());
        }
    }
    map
}

/// 将 api_key→value 映射写入 config.toml 的顶层键（不属于任何 section）。
fn write_config_toml(updates: &HashMap<String, String>) -> std::io::Result<()> {
    // 反映射：api_key → toml_key
    let key_map: &[(&str, &str)] = &[
        ("hbbs_port",        "port"),
        ("hbbs_serial",      "serial"),
        ("hbbs_key",         "key"),
        ("hbbs_lan_mask",    "mask"),
        ("hbbr_relay_host",  "relay_servers"),
    ];
    let toml_updates: HashMap<&str, &str> = key_map
        .iter()
        .filter_map(|(api_k, toml_k)| {
            updates.get(*api_k).map(|v| (*toml_k, v.as_str()))
        })
        .collect();
    if toml_updates.is_empty() {
        return Ok(());
    }
    update_flat_keys(CONFIG_TOML, &toml_updates)
}

/// 将 api_key→value 映射写入 hbbr.toml 的对应 [section] 中的键。
fn write_hbbr_toml(updates: &HashMap<String, String>) -> std::io::Result<()> {
    // api_key → (section, toml_key)
    let key_map: &[(&str, &str, &str)] = &[
        ("hbbr_port",             "server",     "port"),
        ("hbbr_websocket_port",   "server",     "websocket_port"),
        ("hbbr_key",              "key",        "secret"),
        ("hbbr_max_connections",  "connection", "max_connections"),
        ("hbbr_total_bandwidth",  "bandwidth",  "total_limit"),
        ("hbbr_single_bandwidth", "bandwidth",  "single_limit"),
    ];
    let mut sectioned: Vec<(&str, &str, &str)> = Vec::new();
    for (api_k, sec, toml_k) in key_map {
        if let Some(v) = updates.get(*api_k) {
            sectioned.push((sec, toml_k, v.as_str()));
        }
    }
    if sectioned.is_empty() {
        return Ok(());
    }
    update_sectioned_keys(HBBR_TOML, &sectioned)
}

/// 在不破坏注释和结构的前提下，更新文件中**顶层**（无 [section]）的 key = value 行。
fn update_flat_keys(path: &str, updates: &HashMap<&str, &str>) -> std::io::Result<()> {
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let mut lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();
    let mut updated: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut in_section = false;

    for line in &mut lines {
        let trimmed = line.trim().to_string();
        if trimmed.starts_with('[') {
            in_section = true;
            continue;
        }
        if in_section || trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        if let Some((k, _)) = split_ini_kv(&trimmed) {
            if let Some(&new_val) = updates.get(k) {
                let new_line = format!("{} = {}", k, new_val);
                updated.insert(k.to_string());
                *line = new_line;
            }
        }
    }
    // 对于 config.toml 中不存在的键，在第一个 [section] 前插入
    let mut extra: Vec<String> = updates
        .iter()
        .filter(|(k, _)| !updated.contains(k as &str))
        .map(|(k, v)| format!("{} = {}", k, v))
        .collect();
    if !extra.is_empty() {
        let insert_pos = lines
            .iter()
            .position(|l| l.trim().starts_with('['))
            .unwrap_or(lines.len());
        extra.reverse();
        for s in extra {
            lines.insert(insert_pos, s);
        }
    }
    std::fs::write(path, lines.join("\n") + "\n")
}

/// 在不破坏注释和结构的前提下，更新文件中指定 [section] 内的 key = value 行。
fn update_sectioned_keys(path: &str, updates: &[(&str, &str, &str)]) -> std::io::Result<()> {
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let mut lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();
    let mut current_section = String::new();
    // track which (section,key) pairs were found and updated
    let mut updated: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();

    for line in &mut lines {
        let trimmed = line.trim().to_string();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            current_section = trimmed[1..trimmed.len() - 1].to_string();
            continue;
        }
        if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        if let Some((k, _)) = split_ini_kv(&trimmed) {
            for (sec, toml_k, new_val) in updates.iter() {
                if *sec == current_section && *toml_k == k {
                    let new_line = format!("{} = {}", toml_k, new_val);
                    updated.insert((sec.to_string(), toml_k.to_string()));
                    *line = new_line;
                    break;
                }
            }
        }
    }
    // 追加未找到的键到对应 section 末尾
    for (sec, toml_k, new_val) in updates {
        if !updated.contains(&(sec.to_string(), toml_k.to_string())) {
            // 找到该 section 的最后一行，在其后插入
            let section_header = format!("[{}]", sec);
            if let Some(start) = lines.iter().position(|l| l.trim() == section_header) {
                let end = lines[start + 1..]
                    .iter()
                    .position(|l| l.trim().starts_with('['))
                    .map(|i| start + 1 + i)
                    .unwrap_or(lines.len());
                lines.insert(end, format!("{} = {}", toml_k, new_val));
            } else {
                lines.push(format!("\n[{}]", sec));
                lines.push(format!("{} = {}", toml_k, new_val));
            }
        }
    }
    std::fs::write(path, lines.join("\n") + "\n")
}

/// 分割 `key = value` 或 `key=value`，去掉两端空格和值的引号。
fn split_ini_kv(line: &str) -> Option<(&str, &str)> {
    let pos = line.find('=')?;
    let k = line[..pos].trim();
    let v = line[pos + 1..].trim().trim_matches('"');
    if k.is_empty() { None } else { Some((k, v)) }
}

#[derive(Debug, Deserialize)]
pub struct UpdateServerParamsBody {
    pub params: HashMap<String, String>,
}

/// GET /api/admin/server-params
pub async fn get_server_params(
    Extension(state): Extension<ApiState>,
    headers: HeaderMap,
) -> Result<Json<ApiResponse<HashMap<String, String>>>, StatusCode> {
    let caller = jwt_user_id_from_headers(&state.jwt_secret, &headers)?;
    if !db_user_is_admin(&state.db, caller).await {
        return Err(StatusCode::FORBIDDEN);
    }
    let mut params = read_config_toml();
    params.extend(read_hbbr_toml());
    Ok(Json(ApiResponse::success(params)))
}

/// PUT /api/admin/server-params
pub async fn update_server_params(
    Extension(state): Extension<ApiState>,
    headers: HeaderMap,
    Json(body): Json<UpdateServerParamsBody>,
) -> Result<Json<ApiResponse<()>>, StatusCode> {
    let caller = jwt_user_id_from_headers(&state.jwt_secret, &headers)?;
    if !db_user_is_admin(&state.db, caller).await {
        return Err(StatusCode::FORBIDDEN);
    }
    if let Err(e) = write_config_toml(&body.params) {
        return Ok(Json(ApiResponse::error(format!("写入 config.toml 失败: {}", e))));
    }
    if let Err(e) = write_hbbr_toml(&body.params) {
        return Ok(Json(ApiResponse::error(format!("写入 hbbr.toml 失败: {}", e))));
    }
    Ok(Json(ApiResponse::success(())))
}
