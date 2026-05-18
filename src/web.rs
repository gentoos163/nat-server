// Web handlers using loco-rs + Askama
use askama::Template;
use axum::{
    extract::{Extension, Query},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Json},
    routing::{delete, get, post, put},
    Router,
};
use serde::Deserialize;
use std::collections::HashMap;

use crate::{
    api::{ApiResponse, ApiState, LoginRequest, RegisterRequest, UserInfo},
    database::{CreateUserRequest, Database},
    i18n,
    views::{
        AdminSettingsTemplate, AdminSubscriptionsTemplate, DashboardTemplate, DevicesTemplate,
        ForgotPasswordTemplate, HomeTemplate, LoginTemplate, MonitorTemplate, RegisterTemplate,
        ResetPasswordTemplate, SubscriptionTemplate, UsersTemplate,
    },
};

#[derive(Deserialize)]
pub struct ForgotPasswordRequest {
    pub email: String,
}

#[derive(Deserialize)]
pub struct ResetPasswordRequest {
    pub token: String,
    pub password: String,
}

#[derive(Deserialize)]
pub struct SetLangQuery {
    pub lang: Option<String>,
}

/// 从请求头中读取 lang cookie 和 Accept-Language，返回语言代码
fn detect_lang(headers: &HeaderMap) -> String {
    // 解析 Cookie 头中的 lang=zh|en
    let lang_cookie = headers
        .get("cookie")
        .and_then(|v| v.to_str().ok())
        .and_then(|cookies| {
            cookies.split(';').find_map(|part| {
                let part = part.trim();
                if let Some(val) = part.strip_prefix("lang=") {
                    Some(val.trim().to_string())
                } else {
                    None
                }
            })
        });

    let accept_lang = headers
        .get("accept-language")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    i18n::detect(
        lang_cookie.as_deref(),
        accept_lang.as_deref(),
    )
    .to_string()
}

// Web handlers
pub async fn home_page(headers: HeaderMap) -> impl IntoResponse {
    let lang = detect_lang(&headers);
    let t = i18n::get(&lang);
    let template = HomeTemplate {
        title: "NAT Server — 安全内网穿透服务".to_string(),
        current_user: None,
        t,
        lang,
    };
    Html(
        template
            .render()
            .unwrap_or_else(|_| "Template error".to_string()),
    )
}

pub async fn login_page(headers: HeaderMap) -> impl IntoResponse {
    let lang = detect_lang(&headers);
    let t = i18n::get(&lang);
    let template = LoginTemplate {
        title: t.login_title.to_string(),
        current_user: None,
        t,
        lang,
    };
    Html(
        template
            .render()
            .unwrap_or_else(|_| "Template error".to_string()),
    )
}

pub async fn register_page(headers: HeaderMap) -> impl IntoResponse {
    let lang = detect_lang(&headers);
    let t = i18n::get(&lang);
    let template = RegisterTemplate {
        title: t.register_title.to_string(),
        current_user: None,
        t,
        lang,
    };
    Html(
        template
            .render()
            .unwrap_or_else(|_| "Template error".to_string()),
    )
}

pub async fn forgot_password_page(headers: HeaderMap) -> impl IntoResponse {
    let lang = detect_lang(&headers);
    let t = i18n::get(&lang);
    let template = ForgotPasswordTemplate {
        title: t.forgot_password.to_string(),
        current_user: None,
        t,
        lang,
    };
    Html(
        template
            .render()
            .unwrap_or_else(|_| "Template error".to_string()),
    )
}

pub async fn reset_password_page(
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let token = params.get("token").unwrap_or(&String::new()).clone();
    let lang = detect_lang(&headers);
    let t = i18n::get(&lang);
    let template = ResetPasswordTemplate {
        title: "重置密码".to_string(),
        current_user: None,
        token,
        t,
        lang,
    };
    Html(
        template
            .render()
            .unwrap_or_else(|_| "Template error".to_string()),
    )
}

pub async fn dashboard_page(
    headers: HeaderMap,
    Extension(_state): Extension<ApiState>,
) -> Result<impl IntoResponse, StatusCode> {
    let lang = detect_lang(&headers);
    let t = i18n::get(&lang);
    let template = DashboardTemplate {
        title: t.dashboard_title.to_string(),
        current_user: None,
        t,
        lang,
    };
    Ok(Html(
        template
            .render()
            .unwrap_or_else(|_| "Template error".to_string()),
    ))
}

pub async fn devices_page(
    headers: HeaderMap,
    Extension(_state): Extension<ApiState>,
) -> Result<impl IntoResponse, StatusCode> {
    let lang = detect_lang(&headers);
    let t = i18n::get(&lang);
    let template = DevicesTemplate {
        title: t.devices_title.to_string(),
        current_user: None,
        t,
        lang,
    };
    Ok(Html(
        template
            .render()
            .unwrap_or_else(|_| "Template error".to_string()),
    ))
}

pub async fn users_page(
    headers: HeaderMap,
    Extension(_state): Extension<ApiState>,
) -> Result<impl IntoResponse, StatusCode> {
    let lang = detect_lang(&headers);
    let t = i18n::get(&lang);
    let template = UsersTemplate {
        title: t.users_title.to_string(),
        current_user: None,
        t,
        lang,
    };
    Ok(Html(
        template
            .render()
            .unwrap_or_else(|_| "Template error".to_string()),
    ))
}

pub async fn monitor_page(headers: HeaderMap) -> impl IntoResponse {
    let lang = detect_lang(&headers);
    let t = i18n::get(&lang);
    let template = MonitorTemplate {
        title: t.monitor_title.to_string(),
        current_user: None,
        t,
        lang,
    };
    Html(
        template
            .render()
            .unwrap_or_else(|_| "Template error".to_string()),
    )
}

pub async fn subscription_page(headers: HeaderMap) -> impl IntoResponse {
    let lang = detect_lang(&headers);
    let t = i18n::get(&lang);
    let template = SubscriptionTemplate {
        title: t.subscription_title.to_string(),
        current_user: None,
        t,
        lang,
    };
    Html(
        template
            .render()
            .unwrap_or_else(|_| "Template error".to_string()),
    )
}

pub async fn admin_subscriptions_page(headers: HeaderMap) -> impl IntoResponse {
    let lang = detect_lang(&headers);
    let t = i18n::get(&lang);
    let template = AdminSubscriptionsTemplate {
        title: t.admin_sub_title.to_string(),
        current_user: None,
        t,
        lang,
    };
    Html(
        template
            .render()
            .unwrap_or_else(|_| "Template error".to_string()),
    )
}

pub async fn admin_settings_page(headers: HeaderMap) -> impl IntoResponse {
    let lang = detect_lang(&headers);
    let t = i18n::get(&lang);
    let template = AdminSettingsTemplate {
        title: t.settings_title.to_string(),
        current_user: None,
        t,
        lang,
    };
    Html(
        template
            .render()
            .unwrap_or_else(|_| "Template error".to_string()),
    )
}

/// 语言切换：设置 lang cookie，重定向回 Referer 或 /dashboard
pub async fn set_lang(
    headers: HeaderMap,
    Query(query): Query<SetLangQuery>,
) -> impl IntoResponse {
    let lang = match query.lang.as_deref() {
        Some("en") => "en",
        _ => "zh",
    };

    let redirect_to = headers
        .get("referer")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("/dashboard")
        .to_string();

    // 构建响应：设置 cookie 并重定向
    axum::response::Response::builder()
        .status(302)
        .header("Location", redirect_to)
        .header(
            "Set-Cookie",
            format!("lang={}; Path=/; SameSite=Lax; Max-Age=31536000", lang),
        )
        .body(axum::body::Body::empty())
        .unwrap()
}

// API handlers for password reset
pub async fn forgot_password(
    Extension(_state): Extension<ApiState>,
    Json(request): Json<ForgotPasswordRequest>,
) -> Result<Json<ApiResponse<String>>, StatusCode> {
    // 检查用户是否存在
    match _state.db.get_user_by_email(&request.email).await {
        Ok(Some(_user)) => {
            // 在实际应用中，这里应该发送邮件
            // 为了演示，我们只是返回成功消息
            // TODO: 实现邮件发送功能
            Ok(Json(ApiResponse {
                success: true,
                message: "重置密码链接已发送到您的邮箱".to_string(),
                data: Some("success".to_string()),
            }))
        }
        Ok(None) => {
            // 为了安全，即使用户不存在也返回成功消息
            Ok(Json(ApiResponse {
                success: true,
                message: "如果邮箱存在，重置链接已发送".to_string(),
                data: Some("success".to_string()),
            }))
        }
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

pub async fn reset_password(
    Extension(_state): Extension<ApiState>,
    Json(request): Json<ResetPasswordRequest>,
) -> Result<Json<ApiResponse<String>>, StatusCode> {
    // 在实际应用中，这里应该验证token的有效性
    // 为了演示，我们只是检查token是否为空
    if request.token.is_empty() {
        return Ok(Json(ApiResponse {
            success: false,
            message: "无效的重置令牌".to_string(),
            data: None,
        }));
    }

    // TODO: 实现真正的token验证和密码重置逻辑
    // 这里应该：
    // 1. 验证token是否有效且未过期
    // 2. 从token中获取用户ID
    // 3. 更新用户密码

    // 为了演示，我们假设token有效并返回成功
    Ok(Json(ApiResponse {
        success: true,
        message: "密码重置成功".to_string(),
        data: Some("success".to_string()),
    }))
}

// API handlers (保持原有逻辑)
pub async fn login(
    Extension(_state): Extension<ApiState>,
    Json(request): Json<LoginRequest>,
) -> Result<Json<ApiResponse<crate::api::LoginResponse>>, StatusCode> {
    match _state.db.get_user_by_username(&request.username).await {
        Ok(Some(user)) => {
            if !user.is_active {
                return Ok(Json(ApiResponse::<crate::api::LoginResponse>::error(
                    "用户账户已被禁用".to_string(),
                )));
            }
            match _state
                .db
                .verify_password(&request.password, &user.password_hash)
                .await
            {
                Ok(true) => {
                    let udid = match request
                        .device_id
                        .as_ref()
                        .map(|s| s.trim())
                        .filter(|s| !s.is_empty())
                    {
                        None => None,
                        Some(did) => match _state.db.get_user_device_row_id(user.id, did).await {
                            Ok(Some(id)) => Some(id),
                            Ok(None) => {
                                return Ok(Json(ApiResponse::error(
                                    "指定的设备不属于当前用户或未激活".to_string(),
                                )));
                            }
                            Err(_) => {
                                return Ok(Json(ApiResponse::error("查询设备失败".to_string())));
                            }
                        },
                    };
                    let now = chrono::Utc::now();
                    let claims = crate::api::Claims {
                        sub: user.id.to_string(),
                        username: user.username.clone(),
                        exp: (now + chrono::Duration::hours(24)).timestamp(),
                        iat: now.timestamp(),
                        udid,
                        role: user.role.clone(),
                    };

                    let token = match jsonwebtoken::encode(
                        &jsonwebtoken::Header::default(),
                        &claims,
                        &jsonwebtoken::EncodingKey::from_secret(_state.jwt_secret.as_ref()),
                    ) {
                        Ok(token) => token,
                        Err(_) => {
                            return Ok(Json(ApiResponse::<crate::api::LoginResponse>::error(
                                "生成令牌失败".to_string(),
                            )));
                        }
                    };

                    let user_id_for_sub = user.id;
                    let sub_info =
                        crate::api::build_subscription_info(&_state.db, user_id_for_sub).await;
                    let response = crate::api::LoginResponse {
                        token,
                        user: user.into(),
                        subscription: sub_info,
                    };

                    Ok(Json(ApiResponse::success(response)))
                }
                Ok(false) => Ok(Json(ApiResponse::<crate::api::LoginResponse>::error(
                    "用户名或密码错误".to_string(),
                ))),
                Err(_) => Ok(Json(ApiResponse::<crate::api::LoginResponse>::error(
                    "验证密码失败".to_string(),
                ))),
            }
        }
        Ok(None) => Ok(Json(ApiResponse::<crate::api::LoginResponse>::error(
            "用户不存在".to_string(),
        ))),
        Err(_) => Ok(Json(ApiResponse::<crate::api::LoginResponse>::error(
            "查询用户失败".to_string(),
        ))),
    }
}

pub async fn register(
    Extension(_state): Extension<ApiState>,
    Json(request): Json<RegisterRequest>,
) -> Result<Json<ApiResponse<UserInfo>>, StatusCode> {
    // 验证密码确认
    if request.password != request.confirm_password {
        return Ok(Json(ApiResponse::<UserInfo>::error(
            "两次输入的密码不一致".to_string(),
        )));
    }

    // 检查用户名是否已存在
    match _state.db.get_user_by_username(&request.username).await {
        Ok(Some(_)) => {
            return Ok(Json(ApiResponse::<UserInfo>::error(
                "用户名已存在".to_string(),
            )));
        }
        Ok(None) => {}
        Err(_) => {
            return Ok(Json(ApiResponse::<UserInfo>::error(
                "检查用户名失败".to_string(),
            )));
        }
    }

    // 检查邮箱是否已存在
    match _state.db.get_user_by_email(&request.email).await {
        Ok(Some(_)) => {
            return Ok(Json(ApiResponse::<UserInfo>::error(
                "邮箱已存在".to_string(),
            )));
        }
        Ok(None) => {}
        Err(_) => {
            return Ok(Json(ApiResponse::<UserInfo>::error(
                "检查邮箱失败".to_string(),
            )));
        }
    }

    let create_request = CreateUserRequest {
        username: request.username.clone(),
        email: request.email.clone(),
        password: request.password.clone(),
    };

    match _state.db.create_user(&create_request).await {
        Ok(user_id) => {
            // 获取刚创建的用户信息
            match _state.db.get_user_by_id(user_id).await {
                Ok(Some(user)) => Ok(Json(ApiResponse::success(user.into()))),
                Ok(None) => Ok(Json(ApiResponse::error("创建用户后查询失败".to_string()))),
                Err(_) => Ok(Json(ApiResponse::error("查询用户信息失败".to_string()))),
            }
        }
        Err(_) => Ok(Json(ApiResponse::error("注册失败".to_string()))),
    }
}

/// 返回最新客户端版本信息（管理员通过 /api/admin/settings 设置下列 key）：
///   client_latest_version, client_dl_win, client_dl_mac, client_dl_linux,
///   client_sha256_win, client_sha256_mac, client_sha256_linux, client_changelog
async fn client_version(Extension(state): Extension<ApiState>) -> impl IntoResponse {
    let db = &state.db;

    async fn get(db: &Database, key: &str) -> String {
        db.get_setting(key).await.ok().flatten().unwrap_or_default()
    }

    let version = get(db, "client_latest_version").await;
    let dl_win = get(db, "client_dl_win").await;
    let dl_mac = get(db, "client_dl_mac").await;
    let dl_linux = get(db, "client_dl_linux").await;
    let sha256_win = get(db, "client_sha256_win").await;
    let sha256_mac = get(db, "client_sha256_mac").await;
    let sha256_linux = get(db, "client_sha256_linux").await;
    let changelog = get(db, "client_changelog").await;

    Json(serde_json::json!({
        "ok": true,
        "version": version,
        "download_url_windows": dl_win,
        "download_url_macos":   dl_mac,
        "download_url_linux":   dl_linux,
        "sha256_windows":       sha256_win,
        "sha256_macos":         sha256_mac,
        "sha256_linux":         sha256_linux,
        "changelog":            changelog,
    }))
}

// 创建Web路由
pub fn create_web_router(db: Database, jwt_secret: String) -> Router {
    let state = ApiState { db, jwt_secret };

    Router::new()
        // 页面路由
        .route("/", get(home_page))
        .route("/login", get(login_page))
        .route("/register", get(register_page))
        .route("/forgot-password", get(forgot_password_page))
        .route("/reset-password", get(reset_password_page))
        .route("/dashboard", get(dashboard_page))
        .route("/devices", get(devices_page))
        .route("/users", get(users_page))
        .route("/monitor", get(monitor_page))
        .route("/subscription", get(subscription_page))
        .route("/admin/subscriptions", get(admin_subscriptions_page))
        .route("/admin/settings", get(admin_settings_page))
        // 语言切换路由
        .route("/set-lang", get(set_lang))
        // API路由
        .route("/api/login", post(login))
        .route("/api/register", post(register))
        .route("/api/forgot-password", post(forgot_password))
        .route("/api/reset-password", post(reset_password))
        .route(
            "/api/users",
            get(crate::api::list_users).post(crate::api::admin_create_user),
        )
        .route(
            "/api/users/:id",
            get(crate::api::get_user)
                .put(crate::api::update_user)
                .delete(crate::api::delete_user),
        )
        .route("/api/users/:id/role", put(crate::api::update_user_role))
        .route("/api/users/:id/devices", get(crate::api::get_user_devices))
        .route(
            "/api/users/:id/devices/:device_id",
            delete(crate::api::remove_device),
        )
        .route(
            "/api/devices/:device_id/owner",
            get(crate::api::get_device_owner),
        )
        .route("/api/devices", post(crate::device_api::add_device))
        .route(
            "/api/devices/:device_id",
            delete(crate::device_api::remove_device_by_id),
        )
        .route(
            "/api/monitor/connections",
            get(crate::api::monitor_connections),
        )
        .route("/api/monitor/stats", get(crate::api::monitor_stats))
        .route("/api/change-password", post(crate::password_reset::change_password))
        .route("/api/subscription/plans", get(crate::api::list_subscription_plans))
        .route("/api/subscription/my", get(crate::api::get_my_subscription))
        .route(
            "/api/admin/subscriptions",
            get(crate::api::admin_list_subscriptions).post(crate::api::admin_create_subscription),
        )
        .route(
            "/api/admin/subscriptions/:id",
            put(crate::api::admin_update_subscription)
                .delete(crate::api::admin_deactivate_subscription),
        )
        // 系统设置 API
        .route(
            "/api/admin/settings",
            get(crate::settings_api::get_settings).put(crate::settings_api::update_settings),
        )
        // 黑名单 API
        .route(
            "/api/admin/blacklist",
            get(crate::settings_api::get_blacklist).post(crate::settings_api::add_blacklist),
        )
        .route(
            "/api/admin/blacklist/:ip",
            delete(crate::settings_api::delete_blacklist),
        )
        // 阻止名单 API
        .route(
            "/api/admin/blocklist",
            get(crate::settings_api::get_blocklist).post(crate::settings_api::add_blocklist),
        )
        .route(
            "/api/admin/blocklist/:ip",
            delete(crate::settings_api::delete_blocklist),
        )
        // 系统信息 API
        .route("/api/admin/sysinfo", get(crate::settings_api::get_sysinfo))
        // 客户端更新（公开，无需认证）
        .route("/api/client/version", get(client_version))
        // ── 收款 API ──────────────────────────────────────────────────────────
        .route("/api/payment/create", post(crate::payment::create_payment))
        .route("/api/payment/order/:order_no", get(crate::payment::query_order))
        .route("/api/payment/alipay/notify", post(crate::payment::alipay_notify))
        .route("/api/payment/stripe/webhook", post(crate::payment::stripe_webhook))
        .layer(axum::middleware::from_fn(cors_middleware))
        .layer(axum::Extension(state))
}

// CORS中间件
async fn cors_middleware<B>(
    request: axum::http::Request<B>,
    next: axum::middleware::Next<B>,
) -> Result<axum::response::Response, StatusCode> {
    let mut response = next.run(request).await;

    let headers = response.headers_mut();
    headers.insert("Access-Control-Allow-Origin", "*".parse().unwrap());
    headers.insert(
        "Access-Control-Allow-Methods",
        "GET, POST, PUT, DELETE, OPTIONS".parse().unwrap(),
    );
    headers.insert(
        "Access-Control-Allow-Headers",
        "Content-Type, Authorization".parse().unwrap(),
    );

    Ok(response)
}
