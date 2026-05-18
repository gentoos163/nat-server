# nat-server / nat-client 配置参考

## 目录

- [服务端配置（config.toml）](#服务端配置-configtoml)
- [中继服务器配置（hbbr.toml）](#中继服务器配置-hbbrtoml)
- [客户端配置文件](#客户端配置文件)
- [客户端命令行参数](#客户端命令行参数)
- [管理后台运行时设置](#管理后台运行时设置)
- [端口转发规则](#端口转发规则)
- [代理设置](#代理设置)
- [典型部署示例](#典型部署示例)

---

## 服务端配置（config.toml）

hbbs（信令服务器）主配置文件，与可执行文件同目录，启动时自动加载。

```toml
# 监听端口（默认 21116，UDP + TCP 双栈）
port = 21116

# 配置序列号（每次修改后递增，客户端据此决定是否更新本地配置）
serial = 0

# 本服务器自身的 rendezvous 地址（多个用逗号分隔）
rendezvous_servers = "nat.example.com"

# 客户端软件下载页（由 /api/client/version 端点返回）
software_url = "https://nat.example.com/download"

# 默认中继服务器（留空时客户端自动使用 hbbs 同主机的 hbbr）
relay_servers = ""

# UDP 接收缓冲区（字节）；0 = 使用系统默认
# 高并发场景建议：sudo sysctl -w net.core.rmem_max=52428800，然后设为 52428800
rmem = 0

# 局域网判断掩码（填写后，同一 NAT 下的客户端优先内网直连）
# mask = "192.168.0.0/16"

# 连接密钥（客户端与服务端必须相同；留空则不验证）
key = ""

[database]
# SQLite 数据库文件路径（支持相对路径）
url = "db_v2.sqlite3"

[api]
# Web 管理面板 & REST API 监听端口
port = 8080

# JWT 签名密钥（生产环境必须修改为随机长字符串）
jwt_secret = "change-me-in-production-use-random-string"

[log]
# 日志级别：error / warn / info / debug / trace
level = "info"

# 输出到文件（注释掉则输出到 stdout）
# file = "/var/log/hbbs.log"
```

**端口说明**

| 端口 | 协议 | 用途 |
|------|------|------|
| 21115 | TCP | NAT 类型检测 |
| 21116 | TCP/UDP | 信令 / 客户端注册 / 打洞协商 |
| 8080 | TCP | Web 管理面板 & REST API |

---

## 中继服务器配置（hbbr.toml）

hbbr（流量中继服务器）配置文件，打洞失败时所有流量经此中转。

```toml
[server]
# 主监听端口（TCP）
port = 21117

# WebSocket 监听端口（浏览器客户端使用）
websocket_port = 21119

[key]
# 与 hbbs 的 key 保持一致；留空则不验证
secret = ""

[network]
# 绑定地址（"0.0.0.0" = 所有网卡）
bind_address = "0.0.0.0"

[connection]
# 最大并发连接数
max_connections = 1000

# 单条连接超时（秒）
timeout = 300

# 空闲连接超时（秒）
idle_timeout = 600

# 心跳间隔（秒）
heartbeat_interval = 30

[bandwidth]
# 总带宽上限（Mbps），0 = 不限
total_limit = 0

# 单连接带宽上限（Mbps），0 = 不限
single_limit = 0

[security]
# IP 黑名单文件（每行一个 IP 或 CIDR，# 开头为注释）
blacklist_file = "blacklist.txt"

# IP 阻止名单文件
blocklist_file = "blocklist.txt"

[logging]
level = "info"
# file = "/var/log/hbbr.log"
```

**端口说明**

| 端口 | 协议 | 用途 |
|------|------|------|
| 21117 | TCP | 流量中继（主端口） |
| 21119 | TCP | WebSocket 中继 |

---

## 客户端配置文件

### 文件路径

| 平台 | 路径 |
|------|------|
| Linux / macOS | `~/.config/nat-client/config.toml` |
| Windows | `%APPDATA%\nat-client\config.toml` |

首次启动时自动创建，`id` / `uuid` / 密钥对自动生成，**无需手动填写**。

### 完整配置说明

```toml
# ── 设备标识（首次启动自动生成，勿手动修改）────────────────────────────────

# 本机 Peer ID（9 位数字），其他客户端通过此 ID 连接本机
id = ""

# 设备 UUID（base64 编码），用于向服务器注册公钥
uuid = ""

# Ed25519 私钥（base64）
sk = ""

# Ed25519 公钥（base64）
pk = ""

# 公钥是否已由服务器确认（程序自动维护）
key_confirmed = false


# ── 服务器连接 ──────────────────────────────────────────────────────────────

# 信令服务器（hbbs）地址，多个用逗号分隔
# 格式：host 或 host:port（省略端口时默认 21116）
rendezvous_servers = "nat.example.com"

# 与 hbbs 的通信协议
#   proto3  — 默认，与标准 hbbs 兼容
#   capnp   — 实验性，须与服务端配置一致
rendezvous_wire_protocol = "proto3"

# 中继服务器（hbbr）地址（留空 = 服务器自动分配）
# 格式：host 或 host:port（省略端口时默认 21117）
relay_server = ""

# HTTP API 地址（用于登录、注册、更新检查等）
# 留空时自动推导：取 rendezvous_servers 第一个 host + 端口 8080
api_url = ""


# ── 本地网络 ────────────────────────────────────────────────────────────────

# IPC 控制接口端口（CLI 命令通过此端口与后台守护进程通信）
# 同一机器运行多个实例时需设置不同端口
ipc_port = 21114

# 直接访问监听端口（0 = 禁用）
# 非 0 时在该端口等待对端直连，适用于本机有公网 IP 的场景
direct_listen_port = 0

# NAT 类型缓存（程序自动检测写入，无需手动修改）
# 0=未知  1=对称型  2=对称型UDP防火墙
# 3=完全锥形  4=受限锥形  5=端口受限锥形
nat_type = 0


# ── 界面偏好 ────────────────────────────────────────────────────────────────

# 界面语言："zh"（中文）或 "en"（英文）
language = "zh"

# 深色模式：true = 深色，false = 浅色
dark_mode = true


# ── 用户认证（通过 login 命令自动写入，无需手动填写）────────────────────────

auth_token        = ""   # JWT token
auth_token_expires = 0   # token 过期时间（Unix 时间戳，0=未登录）
auth_user_id       = 0
auth_username      = ""
auth_role          = ""  # "user" 或 "admin"
auth_device_row_id = 0


# ── 代理设置 ────────────────────────────────────────────────────────────────

# SOCKS5 代理
socks5_enabled  = false
socks5_port     = 1080
socks5_exit_peer = ""    # 出口节点的 Peer ID（空 = 自动选择）

# HTTP CONNECT 代理
http_proxy_enabled = false
http_proxy_port    = 8118


# ── 端口转发规则（见下方详细说明）──────────────────────────────────────────

# [[forward_rules]]
# id          = "自动生成的UUID"
# name        = "SSH"
# peer_id     = ""          # 空 = 允许任意对端
# target_host = "127.0.0.1"
# target_port = 22
# enabled     = true
```

---

## 客户端命令行参数

### 启动模式

```bash
# GUI 模式（桌面窗口 + 系统托盘）
nat-client gui --server nat.example.com [选项]

# 守护进程模式（无界面，后台运行）
nat-client daemon --server nat.example.com [选项]
```

| 参数 | 说明 | 默认值 |
|------|------|--------|
| `-s, --server` | 信令服务器地址（必填） | — |
| `-r, --relay` | 中继服务器地址 | 与 server 同主机 |
| `--id` | 指定本机 Peer ID | 配置文件中已存的 ID |
| `--ipc-port` | IPC 控制端口 | 21114 |
| `--log-level` | 日志级别 | info |
| `--rendezvous-protocol` | 线路协议（proto3/capnp） | proto3 |

### 常用命令

```bash
# 查看本机 Peer ID
nat-client id

# 查看连接状态
nat-client status

# 扫描局域网内的 nat-client 节点
nat-client discover

# 主动连接到对端（建立本地端口映射）
nat-client connect --peer-id 123456789 --local-port 2222
# 之后 ssh user@127.0.0.1 -p 2222 即可

# 用户管理
nat-client register -u alice -e alice@example.com -p mypassword
nat-client login -u alice -p mypassword
nat-client logout
nat-client auth-status

# 转发规则管理
nat-client list-rules
nat-client add-rule -n SSH -t 22
nat-client add-rule -n RDP -t 3389 --peer-id 123456789   # 仅允许指定对端
nat-client remove-rule -r <rule_id>
nat-client scan-services          # 快速扫描 20 个知名端口（约 200ms）
nat-client scan-services --all    # 全量扫描所有端口，含进程名和绑定地址
```

---

## 管理后台运行时设置

通过 Web 管理面板（`http://server:8080/admin/settings`）或 API 修改，保存在数据库 `settings` 表中，**无需重启服务**。

### 客户端更新配置

| Key | 说明 | 示例值 |
|-----|------|--------|
| `client_latest_version` | 最新客户端版本号 | `0.4.0` |
| `client_dl_win` | Windows 安装包下载地址 | `https://nat.example.com/dl/nat-client-0.4.0-x64.exe` |
| `client_dl_mac` | macOS 安装包下载地址 | `https://nat.example.com/dl/nat-client-0.4.0.dmg` |
| `client_dl_linux` | Linux 安装包下载地址 | `https://nat.example.com/dl/nat-client-0.4.0-linux-x64` |
| `client_sha256_win` | Windows 包 SHA256 | `abc123...` |
| `client_sha256_mac` | macOS 包 SHA256 | `def456...` |
| `client_sha256_linux` | Linux 包 SHA256 | `789ghi...` |
| `client_changelog` | 更新日志（Markdown） | `## v0.4.0\n- 新增自动更新` |

客户端每次启动后 30 秒自动检查，之后每 24 小时检查一次。也可在设置页手动点击"检查更新"。

### API 操作示例

```bash
# 获取所有设置（需管理员 token）
curl -H "Authorization: Bearer $TOKEN" http://nat.example.com:8080/api/admin/settings

# 更新单条
curl -X PUT -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"key":"client_latest_version","value":"0.5.0"}' \
  http://nat.example.com:8080/api/admin/settings

# 批量更新
curl -X PUT -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"settings":{"client_latest_version":"0.5.0","client_changelog":"## v0.5.0\n- 修复若干问题"}}' \
  http://nat.example.com:8080/api/admin/settings
```

### 黑名单 / 阻止名单

黑名单（`blacklist.txt`）：拒绝连接的 IP，整个 TCP 握手被拒。  
阻止名单（`blocklist.txt`）：允许握手但禁止使用中继服务。

```bash
# 添加到黑名单
curl -X POST -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"ip":"1.2.3.4","comment":"恶意扫描"}' \
  http://nat.example.com:8080/api/admin/blacklist

# 也可直接编辑文件（每行一个 IP，# 开头为注释）
echo "1.2.3.4 # 恶意扫描" >> blacklist.txt
```

---

## 端口转发规则

端口转发规则决定：**当其他 Peer 连入本机时，将流量转发到哪个本地服务。**

### 规则字段

| 字段 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `id` | String | 自动 | UUID，程序自动生成 |
| `name` | String | 是 | 规则名称，便于识别 |
| `peer_id` | String | 否 | 限定触发对端；空 = 任意对端 |
| `target_host` | String | 否 | 转发目标主机（默认 `127.0.0.1`） |
| `target_port` | u16 | 是 | 转发目标端口 |
| `enabled` | bool | 否 | 是否启用（默认 `true`） |

### 匹配优先级

```
1. peer_id 与发起方完全匹配的规则（精确匹配）
2. peer_id 为空的通配规则（任意对端）
```

### 配置示例

```toml
# SSH（任意对端均可访问）
[[forward_rules]]
id          = "a1b2c3d4-e5f6-7890-abcd-ef1234567890"
name        = "SSH"
peer_id     = ""
target_host = "127.0.0.1"
target_port = 22
enabled     = true

# RDP（仅允许 ID 为 123456789 的对端）
[[forward_rules]]
id          = "b2c3d4e5-f6a7-8901-bcde-f12345678901"
name        = "RDP"
peer_id     = "123456789"
target_host = "127.0.0.1"
target_port = 3389
enabled     = true

# 转发到内网另一台机器的 MySQL（跨机转发）
[[forward_rules]]
id          = "c3d4e5f6-a7b8-9012-cdef-123456789012"
name        = "MySQL-内网机器"
peer_id     = ""
target_host = "192.168.1.100"
target_port = 3306
enabled     = true
```

---

## 代理设置

nat-client 支持将自身作为 **SOCKS5 代理**或 **HTTP CONNECT 代理**的出口节点，所有流量经由指定的远端 Peer 出站。

```toml
# 启用 SOCKS5 代理
socks5_enabled   = true
socks5_port      = 1080       # 本地监听端口
socks5_exit_peer = "987654321" # 出口节点 Peer ID

# 启用 HTTP 代理
http_proxy_enabled = true
http_proxy_port    = 8118     # 本地监听端口
```

配置后，将浏览器或系统代理指向 `127.0.0.1:1080`，所有流量经由出口 Peer 转发。

---

## 典型部署示例

### 场景一：最简部署（单台公网服务器）

**服务端**（`nat.example.com`）：

```bash
./hbbs   # 使用默认 config.toml
./hbbr   # 使用默认 hbbr.toml
```

**树莓派**（被访问方，家庭内网）：

```bash
# 启动守护进程，注册自身
nat-client daemon --server nat.example.com

# 添加 SSH 转发规则
nat-client add-rule -n SSH -t 22
```

**公司电脑**（访问方）：

```bash
# 连接到树莓派，建立本地端口映射
nat-client connect --peer-id <树莓派ID> --local-port 2222

# SSH 登录
ssh pi@127.0.0.1 -p 2222
```

---

### 场景二：带用户认证的生产部署

1. 修改 `config.toml` 中的 `jwt_secret` 为随机字符串
2. 用户在客户端注册账号：`nat-client register -u alice -e alice@example.com -p pass`
3. 登录后设备自动与账号绑定：`nat-client login -u alice -p pass`
4. 管理员在后台管理页面为用户开通订阅
5. 订阅有效期内，用户的所有设备均可使用完整功能

---

### 场景三：多实例部署（高可用）

在 `config.toml` 中配置多个 rendezvous 服务器：

```toml
rendezvous_servers = "nat1.example.com,nat2.example.com"
```

客户端同样配置多个地址：

```toml
rendezvous_servers = "nat1.example.com,nat2.example.com"
```

客户端会并发连接所有服务器，任意一台在线即可正常工作。

---

## 防火墙放行端口

| 端口 | 协议 | 方向 | 用途 |
|------|------|------|------|
| 21115 | TCP | 入站 | NAT 类型检测 |
| 21116 | TCP+UDP | 入站 | 信令 / 注册 / 打洞 |
| 21117 | TCP | 入站 | 流量中继 |
| 21119 | TCP | 入站 | WebSocket 中继 |
| 8080 | TCP | 入站 | Web 管理面板（可改端口） |
