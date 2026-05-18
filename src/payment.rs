// src/payment.rs
//! 收款模块 —— 支付宝当面付（扫码）+ Stripe 在线收款
//!
//! 管理员在后台设置页配置以下 key（system_settings 表）：
//!   payment.alipay_app_id        支付宝 AppID
//!   payment.alipay_private_key   商户 RSA2 私钥（PKCS#8 PEM，可省略 header）
//!   payment.alipay_public_key    支付宝公钥（PKCS#8/X.509 PEM，可省略 header）
//!   payment.alipay_notify_url    异步回调地址（必须公网可访问）
//!   payment.alipay_sandbox       "1" = 使用沙箱
//!   payment.stripe_secret_key    Stripe 密钥（sk_live_... / sk_test_...）
//!   payment.stripe_webhook_secret Stripe Webhook 签名密钥（whsec_...）
//!   payment.stripe_success_url   支付成功跳转地址

use axum::{
    body::Bytes,
    extract::{Extension, Path},
    http::{HeaderMap, StatusCode},
    response::Json,
};
use chrono::Utc;
use core_common::log;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::Row;
use std::collections::BTreeMap;
use uuid::Uuid;

use crate::api::{jwt_user_id_from_headers, ApiResponse, ApiState};
use crate::database::Database;
use core_common::ResultType;

// ─── 订单数据结构 ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub id: i64,
    pub order_no: String,
    pub user_id: i64,
    pub plan_id: i64,
    pub plan_name: String,
    pub amount: String,       // 如 "29.90"
    pub currency: String,     // "CNY" | "USD"
    pub payment_method: String, // "alipay" | "stripe"
    pub status: String,       // "pending" | "paid" | "failed" | "cancelled"
    pub gateway_order_id: Option<String>,
    pub gateway_qr: Option<String>,         // 支付宝二维码内容
    pub stripe_checkout_url: Option<String>,
    pub paid_at: Option<chrono::DateTime<Utc>>,
    pub created_at: chrono::DateTime<Utc>,
}

// ─── 数据库方法 ─────────────────────────────────────────────────────────────────

impl Database {
    pub async fn migrate_orders(&self) -> ResultType<()> {
        let mut conn = self.pool.get().await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS orders (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                order_no TEXT UNIQUE NOT NULL,
                user_id INTEGER NOT NULL,
                plan_id INTEGER NOT NULL,
                amount TEXT NOT NULL,
                currency TEXT NOT NULL DEFAULT 'CNY',
                payment_method TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending',
                gateway_order_id TEXT,
                gateway_qr TEXT,
                stripe_checkout_url TEXT,
                paid_at DATETIME,
                created_at DATETIME NOT NULL DEFAULT (current_timestamp),
                updated_at DATETIME NOT NULL DEFAULT (current_timestamp),
                FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE,
                FOREIGN KEY (plan_id) REFERENCES subscription_plans(id)
            )",
        )
        .execute(&mut *conn)
        .await?;
        for sql in [
            "CREATE INDEX IF NOT EXISTS idx_orders_user_id ON orders(user_id)",
            "CREATE INDEX IF NOT EXISTS idx_orders_order_no ON orders(order_no)",
            "CREATE INDEX IF NOT EXISTS idx_orders_status ON orders(status)",
        ] {
            let _ = sqlx::query(sql).execute(&mut *conn).await;
        }
        Ok(())
    }

    pub async fn create_order(
        &self,
        user_id: i64,
        plan_id: i64,
        amount: &str,
        currency: &str,
        payment_method: &str,
    ) -> ResultType<String> {
        let suffix: String = Uuid::new_v4()
            .to_string()
            .replace('-', "")
            .chars()
            .take(6)
            .collect::<String>()
            .to_uppercase();
        let order_no = format!("ORD{}{}", Utc::now().format("%Y%m%d%H%M%S"), suffix);
        let mut conn = self.pool.get().await?;
        sqlx::query(
            "INSERT INTO orders (order_no,user_id,plan_id,amount,currency,payment_method,status)
             VALUES (?,?,?,?,?,?,'pending')",
        )
        .bind(&order_no)
        .bind(user_id)
        .bind(plan_id)
        .bind(amount)
        .bind(currency)
        .bind(payment_method)
        .execute(&mut *conn)
        .await?;
        Ok(order_no)
    }

    pub async fn set_order_gateway(
        &self,
        order_no: &str,
        qr: Option<&str>,
        stripe_url: Option<&str>,
    ) -> ResultType<()> {
        let mut conn = self.pool.get().await?;
        sqlx::query(
            "UPDATE orders SET gateway_qr=?,stripe_checkout_url=?,updated_at=datetime('now')
             WHERE order_no=?",
        )
        .bind(qr)
        .bind(stripe_url)
        .bind(order_no)
        .execute(&mut *conn)
        .await?;
        Ok(())
    }

    /// 标记订单已支付（幂等）。返回 Some((user_id, plan_id)) 表示首次成功，None 表示已处理。
    pub async fn mark_order_paid(
        &self,
        order_no: &str,
        gateway_order_id: &str,
    ) -> ResultType<Option<(i64, i64)>> {
        let mut conn = self.pool.get().await?;
        let existing: Option<(i64, i64, String)> = sqlx::query_as(
            "SELECT user_id,plan_id,status FROM orders WHERE order_no=?",
        )
        .bind(order_no)
        .fetch_optional(&mut *conn)
        .await?;

        let (user_id, plan_id, status) = match existing {
            None => return Ok(None),
            Some(r) => r,
        };
        if status == "paid" {
            return Ok(None); // 幂等
        }
        sqlx::query(
            "UPDATE orders SET status='paid',gateway_order_id=?,paid_at=datetime('now'),
             updated_at=datetime('now') WHERE order_no=? AND status='pending'",
        )
        .bind(gateway_order_id)
        .bind(order_no)
        .execute(&mut *conn)
        .await?;
        Ok(Some((user_id, plan_id)))
    }

    pub async fn get_order_by_no(
        &self,
        order_no: &str,
        user_id: i64,
    ) -> ResultType<Option<Order>> {
        let mut conn = self.pool.get().await?;
        let row = sqlx::query(
            "SELECT o.id,o.order_no,o.user_id,o.plan_id,sp.name as plan_name,
                    o.amount,o.currency,o.payment_method,o.status,
                    o.gateway_order_id,o.gateway_qr,o.stripe_checkout_url,
                    o.paid_at,o.created_at
             FROM orders o
             JOIN subscription_plans sp ON o.plan_id=sp.id
             WHERE o.order_no=? AND o.user_id=?",
        )
        .bind(order_no)
        .bind(user_id)
        .fetch_optional(&mut *conn)
        .await?;

        Ok(row.map(|r| Order {
            id: r.get("id"),
            order_no: r.get("order_no"),
            user_id: r.get("user_id"),
            plan_id: r.get("plan_id"),
            plan_name: r.get("plan_name"),
            amount: r.get("amount"),
            currency: r.get("currency"),
            payment_method: r.get("payment_method"),
            status: r.get("status"),
            gateway_order_id: r.get("gateway_order_id"),
            gateway_qr: r.get("gateway_qr"),
            stripe_checkout_url: r.get("stripe_checkout_url"),
            paid_at: r.get("paid_at"),
            created_at: r.get("created_at"),
        }))
    }
}

// ─── 支付宝工具函数 ──────────────────────────────────────────────────────────────

/// 将可能缺少 PEM header 的裸 Base64 密钥包装成 PEM
fn wrap_pem(raw: &str, label: &str) -> String {
    let raw = raw.trim();
    if raw.contains("-----BEGIN") {
        return raw.to_string();
    }
    // 每 64 字符换行（PEM 规范）
    let body: String = raw
        .chars()
        .collect::<Vec<_>>()
        .chunks(64)
        .map(|c| c.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");
    format!("-----BEGIN {label}-----\n{body}\n-----END {label}-----")
}

/// RSA2（PKCS#1 v1.5 + SHA-256）签名，返回 Base64 字符串
fn rsa2_sign(private_key_raw: &str, content: &str) -> ResultType<String> {
    use rsa::pkcs8::DecodePrivateKey;
    use rsa::pkcs1v15::SigningKey;
    use sha2::Sha256;
    use signature::RandomizedSigner;
    use signature::SignatureEncoding;

    let pem = wrap_pem(private_key_raw, "PRIVATE KEY");
    let private_key = rsa::RsaPrivateKey::from_pkcs8_pem(&pem)
        .map_err(|e| anyhow::anyhow!("RSA 私钥解析失败: {}", e))?;

    let signing_key = SigningKey::<Sha256>::new(private_key);
    let sig = signing_key.sign_with_rng(&mut rand::thread_rng(), content.as_bytes());
    Ok(base64::encode(sig.to_bytes().as_ref()))
}

/// 验证支付宝异步通知签名（RSA2）
fn rsa2_verify(public_key_raw: &str, content: &str, sig_b64: &str) -> ResultType<bool> {
    use rsa::pkcs8::DecodePublicKey;
    use rsa::pkcs1v15::VerifyingKey;
    use sha2::Sha256;
    use signature::Verifier;

    let pem = wrap_pem(public_key_raw, "PUBLIC KEY");
    let public_key = rsa::RsaPublicKey::from_public_key_pem(&pem)
        .map_err(|e| anyhow::anyhow!("支付宝公钥解析失败: {}", e))?;

    let sig_bytes = base64::decode(sig_b64)
        .map_err(|e| anyhow::anyhow!("签名 Base64 解码失败: {}", e))?;

    let verifying_key = VerifyingKey::<Sha256>::new(public_key);
    let sig = rsa::pkcs1v15::Signature::try_from(sig_bytes.as_slice())
        .map_err(|e| anyhow::anyhow!("签名格式错误: {}", e))?;

    verifying_key
        .verify(content.as_bytes(), &sig)
        .map_err(|e| anyhow::anyhow!("签名验证失败: {}", e))?;
    Ok(true)
}

/// 构建支付宝签名字符串：按 key 升序排列，去掉 sign/sign_type，拼接 key=value&
fn alipay_build_sign_str(params: &BTreeMap<String, String>) -> String {
    params
        .iter()
        .filter(|(k, v)| *k != "sign" && *k != "sign_type" && !v.is_empty())
        .map(|(k, v)| format!("{}={}", k, v))
        .collect::<Vec<_>>()
        .join("&")
}

/// 调用支付宝 precreate 接口，返回二维码内容（如 "https://qr.alipay.com/..."）
async fn alipay_precreate(
    app_id: &str,
    private_key: &str,
    notify_url: &str,
    sandbox: bool,
    order_no: &str,
    amount: &str,
    subject: &str,
) -> ResultType<String> {
    let gateway = if sandbox {
        "https://openapi-sandbox.dl.alipaydev.com/gateway.do"
    } else {
        "https://openapi.alipay.com/gateway.do"
    };

    let biz = serde_json::json!({
        "out_trade_no": order_no,
        "total_amount": amount,
        "subject": subject,
        "timeout_express": "30m",
    })
    .to_string();

    let ts = Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();

    let mut params: BTreeMap<String, String> = BTreeMap::new();
    params.insert("app_id".into(), app_id.into());
    params.insert("method".into(), "alipay.trade.precreate".into());
    params.insert("charset".into(), "utf-8".into());
    params.insert("sign_type".into(), "RSA2".into());
    params.insert("timestamp".into(), ts);
    params.insert("version".into(), "1.0".into());
    params.insert("notify_url".into(), notify_url.into());
    params.insert("biz_content".into(), biz);

    let sign_str = alipay_build_sign_str(&params);
    let sign = rsa2_sign(private_key, &sign_str)?;
    params.insert("sign".into(), sign);

    // POST form-urlencoded
    let client = reqwest::blocking::Client::new();
    let form: Vec<(String, String)> = params.into_iter().collect();
    let resp = client
        .post(gateway)
        .form(&form)
        .send()
        .map_err(|e| anyhow::anyhow!("支付宝请求失败: {}", e))?;

    let body: Value = resp
        .json()
        .map_err(|e| anyhow::anyhow!("支付宝响应解析失败: {}", e))?;

    let resp_obj = body
        .get("alipay_trade_precreate_response")
        .ok_or_else(|| anyhow::anyhow!("支付宝响应格式异常: {}", body))?;

    let code = resp_obj["code"].as_str().unwrap_or("");
    if code != "10000" {
        let msg = resp_obj["sub_msg"].as_str().unwrap_or(resp_obj["msg"].as_str().unwrap_or("未知错误"));
        return Err(anyhow::anyhow!("支付宝下单失败({}): {}", code, msg));
    }

    resp_obj["qr_code"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("支付宝未返回二维码"))
}

// ─── Stripe 工具函数 ─────────────────────────────────────────────────────────────

/// 创建 Stripe Checkout Session，返回 (session_id, checkout_url)
async fn stripe_create_session(
    secret_key: &str,
    success_url: &str,
    cancel_url: &str,
    order_no: &str,
    amount_cents: u64, // USD cents
    description: &str,
) -> ResultType<(String, String)> {
    let client = reqwest::blocking::Client::new();
    let params = [
        ("mode", "payment"),
        ("success_url", success_url),
        ("cancel_url", cancel_url),
        ("line_items[0][price_data][currency]", "usd"),
        ("line_items[0][price_data][product_data][name]", description),
        ("line_items[0][quantity]", "1"),
    ];
    let amount_str = amount_cents.to_string();
    let mut form: Vec<(&str, &str)> = params.to_vec();
    form.push(("line_items[0][price_data][unit_amount]", &amount_str));
    form.push(("metadata[order_no]", order_no));

    let resp = client
        .post("https://api.stripe.com/v1/checkout/sessions")
        .basic_auth(secret_key, Option::<&str>::None)
        .form(&form)
        .send()
        .map_err(|e| anyhow::anyhow!("Stripe 请求失败: {}", e))?;

    let body: Value = resp
        .json()
        .map_err(|e| anyhow::anyhow!("Stripe 响应解析失败: {}", e))?;

    if let Some(err) = body.get("error") {
        return Err(anyhow::anyhow!(
            "Stripe 下单失败: {}",
            err["message"].as_str().unwrap_or("未知错误")
        ));
    }

    let id = body["id"].as_str().unwrap_or("").to_string();
    let url = body["url"].as_str().unwrap_or("").to_string();
    if url.is_empty() {
        return Err(anyhow::anyhow!("Stripe 未返回 checkout URL"));
    }
    Ok((id, url))
}

/// 验证 Stripe Webhook 签名（HMAC-SHA256）
fn stripe_verify_webhook(secret: &str, payload: &[u8], sig_header: &str) -> ResultType<()> {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    // 解析 Stripe-Signature: t=...,v1=...,v0=...
    let mut ts = "";
    let mut v1 = "";
    for part in sig_header.split(',') {
        if let Some(val) = part.trim().strip_prefix("t=") {
            ts = val;
        } else if let Some(val) = part.trim().strip_prefix("v1=") {
            v1 = val;
        }
    }
    if ts.is_empty() || v1.is_empty() {
        return Err(anyhow::anyhow!("Stripe 签名头格式异常"));
    }

    let signed_payload = format!("{}.{}", ts, String::from_utf8_lossy(payload));
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .map_err(|e| anyhow::anyhow!("HMAC key error: {}", e))?;
    mac.update(signed_payload.as_bytes());
    let computed = hex::encode(mac.finalize().into_bytes());

    if computed != v1 {
        return Err(anyhow::anyhow!("Stripe Webhook 签名不匹配"));
    }
    Ok(())
}

// ─── 支付完成后激活订阅 ─────────────────────────────────────────────────────────

async fn activate_subscription_for_order(db: &Database, user_id: i64, plan_id: i64) {
    // 停用旧订阅
    if let Ok(Some(sub)) = db.get_user_active_subscription(user_id).await {
        if let Err(e) = db.deactivate_user_subscription(sub.id).await {
            log::warn!("[payment] 停用旧订阅失败: {}", e);
        }
    }
    // 创建新订阅（30天有效期）
    let expires = Utc::now() + chrono::Duration::days(30);
    if let Err(e) = db
        .create_user_subscription(user_id, plan_id, Some(expires), None)
        .await
    {
        log::error!("[payment] 创建订阅失败 user={} plan={}: {}", user_id, plan_id, e);
    } else {
        log::info!("[payment] 订阅已激活 user={} plan={}", user_id, plan_id);
    }
}

// ─── HTTP Handler 请求/响应结构体 ──────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct CreatePaymentReq {
    pub plan_id: i64,
    pub method: String, // "alipay" | "stripe"
}

#[derive(Debug, Serialize)]
pub struct CreatePaymentResp {
    pub order_no: String,
    pub method: String,
    pub qr_content: Option<String>,  // 支付宝二维码内容
    pub checkout_url: Option<String>, // Stripe checkout URL
}

// ─── POST /api/payment/create ─────────────────────────────────────────────────

pub async fn create_payment(
    Extension(state): Extension<ApiState>,
    headers: HeaderMap,
    Json(body): Json<CreatePaymentReq>,
) -> Result<Json<ApiResponse<CreatePaymentResp>>, StatusCode> {
    let user_id = jwt_user_id_from_headers(&state.jwt_secret, &headers)?;

    // 获取套餐信息
    let plans = state
        .db
        .get_subscription_plans()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let plan = plans
        .into_iter()
        .find(|p| p.id == body.plan_id)
        .ok_or(StatusCode::NOT_FOUND)?;

    if plan.price_monthly <= 0.0 {
        return Ok(Json(ApiResponse::error("免费套餐无需付款".to_string())));
    }

    // 获取支付配置
    let settings = state
        .db
        .get_all_settings()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let amount = format!("{:.2}", plan.price_monthly);
    let currency = if body.method == "stripe" { "USD" } else { "CNY" };

    let order_no = state
        .db
        .create_order(user_id, plan.id, &amount, currency, &body.method)
        .await
        .map_err(|e| {
            log::error!("[payment] 创建订单失败: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    match body.method.as_str() {
        "alipay" => {
            let app_id = settings.get("payment.alipay_app_id").cloned().unwrap_or_default();
            let private_key = settings.get("payment.alipay_private_key").cloned().unwrap_or_default();
            let notify_url = settings.get("payment.alipay_notify_url").cloned().unwrap_or_default();
            let sandbox = settings.get("payment.alipay_sandbox").map(|v| v == "1").unwrap_or(false);

            if app_id.is_empty() || private_key.is_empty() || notify_url.is_empty() {
                return Ok(Json(ApiResponse::error("支付宝未配置，请联系管理员".to_string())));
            }

            let subject = format!("NAT穿透 - {}", plan.display_name);
            match alipay_precreate(&app_id, &private_key, &notify_url, sandbox, &order_no, &amount, &subject).await {
                Ok(qr) => {
                    if let Err(e) = state.db.set_order_gateway(&order_no, Some(&qr), None).await {
                        log::warn!("[payment] 更新订单 QR 失败: {}", e);
                    }
                    Ok(Json(ApiResponse::success(CreatePaymentResp {
                        order_no,
                        method: "alipay".into(),
                        qr_content: Some(qr),
                        checkout_url: None,
                    })))
                }
                Err(e) => Ok(Json(ApiResponse::error(format!("支付宝下单失败: {}", e)))),
            }
        }
        "stripe" => {
            let secret_key = settings.get("payment.stripe_secret_key").cloned().unwrap_or_default();
            let success_url = settings
                .get("payment.stripe_success_url")
                .cloned()
                .unwrap_or_else(|| {
                    settings
                        .get("payment.base_url")
                        .map(|b| format!("{}/subscription?payment=success", b))
                        .unwrap_or_else(|| "/subscription?payment=success".into())
                });
            let cancel_url = settings
                .get("payment.base_url")
                .map(|b| format!("{}/subscription", b))
                .unwrap_or_else(|| "/subscription".into());

            if secret_key.is_empty() {
                return Ok(Json(ApiResponse::error("Stripe 未配置，请联系管理员".to_string())));
            }

            // Stripe 金额以美分计（价格直接乘 100 转整数）
            let amount_cents = (plan.price_monthly * 100.0).round() as u64;
            let description = format!("NAT穿透 - {}", plan.display_name);

            match stripe_create_session(
                &secret_key,
                &success_url,
                &cancel_url,
                &order_no,
                amount_cents,
                &description,
            )
            .await
            {
                Ok((_session_id, url)) => {
                    if let Err(e) = state.db.set_order_gateway(&order_no, None, Some(&url)).await {
                        log::warn!("[payment] 更新 Stripe URL 失败: {}", e);
                    }
                    Ok(Json(ApiResponse::success(CreatePaymentResp {
                        order_no,
                        method: "stripe".into(),
                        qr_content: None,
                        checkout_url: Some(url),
                    })))
                }
                Err(e) => Ok(Json(ApiResponse::error(format!("Stripe 下单失败: {}", e)))),
            }
        }
        _ => Ok(Json(ApiResponse::error("不支持的支付方式".to_string()))),
    }
}

// ─── GET /api/payment/order/:order_no ────────────────────────────────────────

pub async fn query_order(
    Extension(state): Extension<ApiState>,
    headers: HeaderMap,
    Path(order_no): Path<String>,
) -> Result<Json<ApiResponse<Value>>, StatusCode> {
    let user_id = jwt_user_id_from_headers(&state.jwt_secret, &headers)?;
    match state.db.get_order_by_no(&order_no, user_id).await {
        Ok(Some(order)) => Ok(Json(ApiResponse::success(json!({
            "order_no": order.order_no,
            "status": order.status,
            "amount": order.amount,
            "currency": order.currency,
            "payment_method": order.payment_method,
            "plan_name": order.plan_name,
            "paid_at": order.paid_at,
        })))),
        Ok(None) => Ok(Json(ApiResponse::error("订单不存在".to_string()))),
        Err(e) => {
            log::error!("[payment] 查询订单失败: {}", e);
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

// ─── POST /api/payment/alipay/notify ────────────────────────────────────────

pub async fn alipay_notify(
    Extension(state): Extension<ApiState>,
    body: axum::extract::Form<std::collections::HashMap<String, String>>,
) -> impl axum::response::IntoResponse {
    let params = body.0;
    let trade_status = params.get("trade_status").map(|s| s.as_str()).unwrap_or("");
    let out_trade_no = params.get("out_trade_no").cloned().unwrap_or_default();
    let trade_no = params.get("trade_no").cloned().unwrap_or_default();
    let sig_b64 = params.get("sign").cloned().unwrap_or_default();

    // 重组签名字符串（排除 sign/sign_type，按 key 升序）
    let mut sorted: BTreeMap<&str, &str> = BTreeMap::new();
    for (k, v) in &params {
        if k != "sign" && k != "sign_type" && !v.is_empty() {
            sorted.insert(k.as_str(), v.as_str());
        }
    }
    let sign_str: String = sorted
        .iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .collect::<Vec<_>>()
        .join("&");

    // 验证签名
    let settings = match state.db.get_all_settings().await {
        Ok(s) => s,
        Err(_) => return "fail".to_string(),
    };
    let public_key = settings.get("payment.alipay_public_key").cloned().unwrap_or_default();
    if public_key.is_empty() {
        log::error!("[alipay] 未配置支付宝公钥，无法验签");
        return "fail".to_string();
    }

    if let Err(e) = rsa2_verify(&public_key, &sign_str, &sig_b64) {
        log::error!("[alipay] 验签失败: {}", e);
        return "fail".to_string();
    }

    // 仅处理支付成功状态
    if trade_status != "TRADE_SUCCESS" && trade_status != "TRADE_FINISHED" {
        return "success".to_string(); // 告知支付宝不再重发此通知
    }

    match state.db.mark_order_paid(&out_trade_no, &trade_no).await {
        Ok(Some((user_id, plan_id))) => {
            activate_subscription_for_order(&state.db, user_id, plan_id).await;
            log::info!("[alipay] 订单 {} 支付成功，已激活订阅", out_trade_no);
        }
        Ok(None) => {
            log::info!("[alipay] 订单 {} 已处理（幂等）", out_trade_no);
        }
        Err(e) => {
            log::error!("[alipay] 标记支付失败: {}", e);
            return "fail".to_string();
        }
    }

    "success".to_string()
}

// ─── POST /api/payment/stripe/webhook ────────────────────────────────────────

pub async fn stripe_webhook(
    Extension(state): Extension<ApiState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl axum::response::IntoResponse {
    let sig_header = headers
        .get("stripe-signature")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let settings = match state.db.get_all_settings().await {
        Ok(s) => s,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR,
    };
    let webhook_secret = settings
        .get("payment.stripe_webhook_secret")
        .cloned()
        .unwrap_or_default();

    if webhook_secret.is_empty() {
        log::error!("[stripe] 未配置 Webhook 密钥");
        return StatusCode::INTERNAL_SERVER_ERROR;
    }

    if let Err(e) = stripe_verify_webhook(&webhook_secret, &body, sig_header) {
        log::warn!("[stripe] Webhook 验签失败: {}", e);
        return StatusCode::BAD_REQUEST;
    }

    let event: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return StatusCode::BAD_REQUEST,
    };

    if event["type"].as_str() == Some("checkout.session.completed") {
        let session = &event["data"]["object"];
        let order_no = session["metadata"]["order_no"].as_str().unwrap_or("");
        let payment_intent = session["payment_intent"].as_str().unwrap_or("");

        if !order_no.is_empty() {
            match state.db.mark_order_paid(order_no, payment_intent).await {
                Ok(Some((user_id, plan_id))) => {
                    activate_subscription_for_order(&state.db, user_id, plan_id).await;
                    log::info!("[stripe] 订单 {} 支付成功，已激活订阅", order_no);
                }
                Ok(None) => log::info!("[stripe] 订单 {} 已处理（幂等）", order_no),
                Err(e) => log::error!("[stripe] 标记支付失败: {}", e),
            }
        }
    }

    StatusCode::OK
}
