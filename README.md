# NAT Server — 自托管内网穿透服务

> 基于 RustDesk 协议的**端对端加密、跨平台、完全自托管**内网穿透服务端。  
> 完整重写业务逻辑，集成用户系统、设备管理、Web 管理后台与支付功能。

---

## 目录

1. [功能特性](#1-功能特性)
2. [架构概述](#2-架构概述)
3. [快速开始](#3-快速开始)
4. [Web 后台功能](#4-web-后台功能)
5. [客户端更新配置](#5-客户端更新配置)
6. [支付配置](#6-支付配置)
7. [项目结构](#7-项目结构)

---

## 1. 功能特性

| 功能 | 说明 |
|---|---|
| **内网穿透** | TCP 打洞 + 中继回落，兼容各类 NAT 环境 |
| **端对端加密** | Ed25519 密钥对，公钥由服务端确认 |
| **用户系统** | 注册 / 登录 / 密码重置，JWT 认证（24 小时有效期） |
| **设备绑定** | 每台 nat-client 实例绑定到用户账户，多设备管理 |
| **Web 管理后台** | 用户管理、设备监控、订阅套餐、系统设置（Askama 模板） |
| **支付集成** | 支付宝当面付（二维码）+ Stripe（在线 Checkout） |
| **客户端下载** | 首页提供 Windows / macOS / Linux 下载链接 |
| **客户端自动更新** | 管理员通过后台配置最新版本号和下载地址，客户端自动检查更新 |
| **订阅套餐** | 灵活配置套餐，管理用户订阅状态与到期时间 |
| **国际化** | i18n 支持多语言 |

---

## 2. 架构概述

### 服务端组件

```
┌─────────────────────────────────────────────────────────────┐
│                        nat-server                           │
│                                                             │
│  ┌──────────────────────────────────────────────────────┐   │
│  │  hbbs（端口 21116）                                   │   │
│  │  ├─ 渲染同端（Rendezvous）：Peer 注册、NAT 打洞协调   │   │
│  │  ├─ 用户 REST API（:8080/api/*）                     │   │
│  │  └─ Web 管理界面（:8080）                            │   │
│  └──────────────────────────────────────────────────────┘   │
│                                                             │
│  ┌──────────────────────────────────────────────────────┐   │
│  │  hbbr（端口 21117）                                   │   │
│  │  └─ 中继转发：对称 NAT / 打洞失败时的数据中转         │   │
│  └──────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────┘
```

### 客户端

| 组件 | 说明 |
|---|---|
| `nat-client` | 跨平台客户端（见 [nat-client/README.md](nat-client/README.md)） |
| GUI | Slint 液态玻璃风格桌面界面 + 系统托盘 |
| 守护进程 | 后台运行，提供本地 IPC 控制接口 |
| 自动更新 | 启动后自动检查版本，一键下载安装 |

### 网络端口

| 端口 | 协议 | 用途 |
|---|---|---|
| **21116** | TCP | hbbs：Peer 注册与打洞协调 |
| **21117** | TCP | hbbr：中继数据转发 |
| **8080** | TCP | Web 界面 + REST API |

---

## 3. 快速开始

### 构建

```bash
# 克隆项目后在根目录执行
cargo build --release
```

构建产物位于 `target/release/`：
- `hbbs` — 渲染同端服务器（含 Web 后台）
- `hbbr` — 中继服务器

### 启动

```bash
# 启动 hbbs（替换 1.2.3.4 为你的公网 IP）
./hbbs -p 21116 -r 1.2.3.4:21117

# 启动 hbbr（独立进程）
./hbbr -p 21117

# Web 管理界面自动启动在 :8080
# 访问 http://1.2.3.4:8080
```

### 命令行选项

| 选项 | 说明 | 默认值 |
|---|---|---|
| `-p <PORT>` | 监听端口 | 21116（hbbs）/ 21117（hbbr） |
| `-r <ADDR>` | 指定 hbbr 中继地址（hbbs 使用） | — |
| `-k <KEY>` | 服务端私钥路径 | 自动生成 |

### 防火墙放行

```bash
# hbbs
ufw allow 21116/tcp

# hbbr
ufw allow 21117/tcp

# Web 后台
ufw allow 8080/tcp
```

---

## 4. Web 后台功能

访问 `http://<服务器IP>:8080`，所有页面均需登录（管理员账户）。

| 路由 | 页面 | 说明 |
|---|---|---|
| `/` | 产品首页 | 项目介绍 + 客户端下载 + 登录/注册入口 |
| `/dashboard` | 用户仪表盘 | 在线设备数、订阅状态、快速操作 |
| `/devices` | 设备管理 | 查看/移除绑定设备，设备在线状态 |
| `/subscription` | 订阅套餐 | 查看套餐、发起支付（支付宝/Stripe） |
| `/admin/users` | 用户管理 | 用户列表、状态管理（管理员专属） |
| `/admin/subscriptions` | 订阅管理 | 所有用户订阅记录（管理员专属） |
| `/monitor` | 系统监控 | 服务器资源、连接数统计（管理员专属） |
| `/admin/settings` | 系统设置 | 客户端更新配置、支付密钥等（管理员专属） |

---

## 5. 客户端更新配置

管理员在 `/admin/settings` 页面配置以下键值，客户端启动后会静默检查并提示更新：

| 设置键 | 说明 | 示例值 |
|---|---|---|
| `client_latest_version` | 最新版本号 | `0.1.1` |
| `client_dl_win` | Windows 下载链接 | `https://example.com/nat-client-0.1.1-windows.exe` |
| `client_dl_mac` | macOS 下载链接 | `https://example.com/nat-client-0.1.1-macos.dmg` |
| `client_dl_linux` | Linux 下载链接 | `https://example.com/nat-client-0.1.1-linux.tar.gz` |
| `client_sha256_win` | Windows SHA256 校验值（可选） | `a1b2c3d4...` |
| `client_sha256_mac` | macOS SHA256 校验值（可选） | `e5f6a7b8...` |
| `client_sha256_linux` | Linux SHA256 校验值（可选） | `c9d0e1f2...` |
| `client_changelog` | 更新日志（Markdown 格式） | `- 修复连接稳定性问题\n- 新增订阅管理页面` |

客户端更新逻辑：
- 启动 30 秒后首次检查，之后每 24 小时一次
- 比对本地版本与 `client_latest_version`，有新版本时提示用户
- 支持 SHA256 文件校验（配置后自动验证）
- 详细实现见 [nat-client/README.md — 自动更新](nat-client/README.md)

对应 REST API 端点：

```
GET  /api/client/version          ← 客户端查询最新版本信息
GET  /api/admin/settings          ← 管理员读取所有设置
POST /api/admin/settings          ← 管理员更新设置（JSON body）
```

---

## 6. 支付配置

支付配置同样通过 `/admin/settings` 或服务器环境变量设置。

### 支付宝（当面付）

| 设置键 | 说明 |
|---|---|
| `alipay_app_id` | 支付宝应用 AppID |
| `alipay_private_key` | 应用私钥（RSA2） |
| `alipay_public_key` | 支付宝公钥 |
| `alipay_notify_url` | 异步通知回调地址（需公网可达） |

支付流程：客户端请求二维码 → 展示扫码 → 每 3 秒轮询支付结果 → 成功后刷新订阅状态。

### Stripe

| 设置键 | 说明 |
|---|---|
| `stripe_secret_key` | Stripe Secret Key（`sk_live_...` 或 `sk_test_...`） |
| `stripe_webhook_secret` | Webhook 签名密钥（`whsec_...`） |
| `stripe_success_url` | 支付成功跳转地址 |
| `stripe_cancel_url` | 取消支付跳转地址 |

支付流程：客户端请求 Checkout Session → 打开系统浏览器 → 完成支付 → Webhook 通知服务端 → 客户端轮询确认。

---

## 7. 项目结构

```
nat-server/
├── src/                          # 服务端源码
│   ├── main.rs                   # hbbs 入口，HTTP 服务器启动
│   ├── hbbr.rs                   # hbbr 中继服务器入口
│   ├── rendezvous_server.rs      # Peer 注册、NAT 打洞协调逻辑
│   ├── relay_server.rs           # hbbr 中继转发逻辑
│   ├── peer.rs                   # Peer 状态管理，JWT 验证绑定
│   ├── web.rs                    # Web 路由（Axum）
│   ├── api.rs                    # 用户 REST API（登录/注册/密码重置）
│   ├── device_api.rs             # 设备管理 API
│   ├── subscription.rs           # 订阅套餐管理
│   ├── payment.rs                # 支付宝 + Stripe 集成
│   ├── settings_api.rs           # 系统设置 API（含客户端更新配置）
│   ├── version.rs                # 客户端版本查询接口
│   ├── database.rs               # SQLite 数据库操作（users/devices/peers/subscriptions）
│   ├── database_v2.rs            # 数据库迁移 v2
│   ├── password_reset.rs         # 密码重置流程
│   ├── i18n.rs                   # 国际化支持
│   ├── common.rs                 # 公共工具函数
│   └── views/                    # 视图模块
│       └── mod.rs
│
├── templates/                    # HTML 模板（Askama）
│   ├── layout.html               # 公共布局（导航、侧边栏）
│   ├── home.html                 # 产品首页（下载 + 介绍）
│   ├── login.html                # 登录页
│   ├── register.html             # 注册页
│   ├── forgot_password.html      # 忘记密码
│   ├── reset_password.html       # 重置密码
│   ├── dashboard.html            # 用户仪表盘
│   ├── devices.html              # 设备管理
│   ├── subscription.html         # 订阅套餐页
│   ├── users.html                # 用户管理（管理员）
│   ├── admin_subscriptions.html  # 订阅管理（管理员）
│   ├── admin_settings.html       # 系统设置（管理员）
│   └── monitor.html              # 系统监控（管理员）
│
├── nat-client/                   # 客户端（独立 Cargo 包）
│   └── README.md                 # 客户端详细文档
│
├── libs/
│   └── core-common/              # 共享协议库（protobuf 定义等）
│
├── db_v2.sqlite3                 # SQLite 数据库（运行时生成）
└── Cargo.toml                    # 工作区配置
```

---

*文档版本：v0.4.0 | 2026 年*
