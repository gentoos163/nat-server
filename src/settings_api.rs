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
