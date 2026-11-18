---
description: "配置 FeedCourier 环境变量（管理员用户名与口令、2FA 重置、系统时区）、HTTP 与 SOCKS5 出站代理、按订阅源与按通道独立代理开关，以及内网自建服务的 SSRF 白名单。"
---

# 环境配置与代理

FeedCourier 的基础运行参数通过环境变量设置，而网络代理、内网 SSRF 白名单、订阅源策略与通道凭据均支持在 WebUI 中修改并保存到 SQLite 数据库。

---

## 环境变量说明

| 变量名 | 默认值 | 类型 / 格式 | 说明 |
| :--- | :--- | :--- | :--- |
| `DATA_DIR` | `./data`<br>*（Docker 镜像内默认为 `/data`）* | 目录路径 | 持久化数据目录，用于存放 `feedcourier.db`、`master.key` 以及在线备份临时快照。 |
| `LISTEN_ADDR` | `0.0.0.0:8080` | `SocketAddr`（`IP:Port`） | HTTP 服务监听地址，同时提供 WebUI、`/api/v1/*` 接口与 `/mcp` 端点。 |
| `MASTER_KEY` | *（缺省自动生成）* | 64 位 Hex 或 Base64（32 字节） | 用于加密通道配置的 ChaCha20-Poly1305 主密钥。未设置时自动在 `<DATA_DIR>/master.key` 生成（权限 `0600`）。 |
| `ADMIN_USERNAME` | `Courier` | 字符串（1–64 个字符） | WebUI 管理员用户名。设置后会在**每次启动**时覆盖数据库中保存的用户名。 |
| `ADMIN_PASSWORD` | *（首次访问 WebUI 引导设置）* | 字符串（至少 8 个字符） | 启动时预设或重置 WebUI 管理员口令，启动阶段以 Argon2id 哈希写入数据库。 |
| `RESET_2FA` | *（未设置）* | `true` | 一次性应急开关：启动时清除 TOTP 密钥与全部恢复码（并输出 `WARN` 日志）。重启完成后请立即移除该变量。参见[丢失验证器](/zh/guide/operations#lost-authenticator-reset-2fa)。 |
| `TIMEZONE`（或 `TZ`） | `America/Los_Angeles` | IANA 时区名（如 `Asia/Shanghai`） | 用于判定路由投递时间窗口与星期的**系统时区**。仅在数据库尚未保存时区时作为初始值写入，此后以 WebUI **Settings** 中的设置为准。 |
| `OUTBOUND_PROXY` | *（未设置）* | URL（`http://`、`https://`、`socks5://`、`socks5h://`） | 初始全局出站代理地址。当 `OUTBOUND_PROXY` 未设置时，依次回退读取 `ALL_PROXY`、`HTTPS_PROXY`、`HTTP_PROXY`。仅当数据库尚未保存代理配置时作为初始值写入。 |
| `SSRF_ALLOWLIST` | *（空）* | 逗号分隔的 CIDR 或主机名 | 初始内网 SSRF 放行白名单（如 `10.0.0.5/32,rsshub.internal,*.lan`）。仅当数据库尚未保存白名单时作为初始值写入。 |
| `LOCALE` | *（未设置）* | `en` 或 `zh-CN` | 旧版兼容的回退推送语言：仅用于尚未设置自身投递语言的旧路由。新路由各自保存投递语言；WebUI 界面语言只是浏览器本地偏好，不再同步到服务端。 |
| `RETENTION_HOURS` | `24` | 无符号整数（`u64`） | 已完成投递或未命中路由的文章正文与媒体元数据保留小时数，到期后由后台定时任务清理。 |
| `DB_READ_MAX_CONNECTIONS` | `8` | 正整数（`u32 >= 1`） | SQLite WAL 只读连接池的最大连接数（写连接池固定为 `1`）。 |
| `RUST_LOG` | `feedcourier=info,tower_http=info` | 日志过滤指令 | 日志级别控制，排查问题时可设为 `feedcourier=debug,tower_http=debug`。 |

> [!NOTE]
> `TIMEZONE`/`TZ`、`OUTBOUND_PROXY` 与 `SSRF_ALLOWLIST` 会在首次启动时写入数据库的 `system_settings` 表。此后在 WebUI **Settings** 页面修改时区、代理与白名单时，新配置会立即生效并持久化保存，无需重启服务。
>
> `ADMIN_USERNAME` 与 `ADMIN_PASSWORD` 则会在**每次启动**时覆盖已保存的值；设置了 `ADMIN_USERNAME` 时，WebUI 会提示用户名由环境变量管理。`RESET_2FA=true` 只应使用一次，否则每次启动都会清除两步验证。

> [!TIP]
> Docker 部署请通过 `-e TZ=Asia/Shanghai`（或任意 IANA 时区）传入时区。路由的投递时间窗口按系统时区判定，而不是浏览器所在时区。

---

## 全局与按源 / 按通道分流代理

当部分上游订阅源或 Telegram / Notion 接口需要通过代理访问，而其余订阅源或内网服务需要直连时，可以使用“全局代理地址 + 按需勾选”的分流配置：

1. **配置全局出站代理（`outbound_proxy_url`）**：
   - 支持协议：`http://`、`https://`、`socks5://`（本地解析 DNS 后连接 SOCKS5）、`socks5h://`（由远端 SOCKS5 代理执行 DNS 解析，适合需要远端解析域名的环境）。
   - 配置入口：环境变量 `OUTBOUND_PROXY` 或 WebUI **Settings → Outbound Network & SSRF Guard**。
2. **按订阅源独立开关（`feeds.use_proxy`）**：
   - 每个订阅源都有独立的 `use_proxy` 开关（默认关闭）。
   - 开启后，该订阅源的 Feed 拉取、站点图标获取以及网页全文抓取均会通过代理发出。
3. **按推送通道独立开关（`channels.use_proxy`）**：
   - 每个推送通道都有独立的 `use_proxy` 开关（默认关闭）。
   - 开启后，Telegram 接口调用、媒体防盗链中转拉取以及 Notion 接口调用均会通过代理转发。

---

## `SSRF_ALLOWLIST` 内网自建服务放行

FeedCourier 默认启用连接层防 SSRF 保护，会自动拦截指向以下地址的出站请求：
- 环回地址（`127.0.0.0/8`、`::1/128`、`localhost`）
- 私有局域网段（`10.0.0.0/8`、`172.16.0.0/12`、`192.168.0.0/16`、Unique Local `fc00::/7`）
- 链路本地与云实例元数据地址（`169.254.0.0/16`、`169.254.169.254`）

如果你在同一内网或 Docker 网络中自建了 `RSSHub`、FreshRSS 或内部博客，可通过 `SSRF_ALLOWLIST` 放行对应的 IP/CIDR 或内部主机名：

### 支持的白名单格式

- **精确 IP 或 CIDR 网段**：如 `10.0.0.5/32`、`192.168.50.0/24`、`fd00::/64`
- **精确内部主机名**：如 `rsshub.internal`、`rsshub`（Docker Compose 同网络服务名）
- **通配符子域名**：如 `*.internal.example.com`、`*.lan`

### Docker Compose 同网络自建 RSSHub 示例

```yaml
services:
  feedcourier:
    image: elleryhe/feedcourier:latest
    environment:
      # 放行 Docker 内部服务名 rsshub
      SSRF_ALLOWLIST: "rsshub,172.20.0.0/16"
    depends_on:
      - rsshub

  rsshub:
    image: diygod/rsshub:chromium-bundled
    restart: unless-stopped
```

配置后即可在 FeedCourier 中订阅 `http://rsshub:1200/...`。

> [!WARNING]
> 云平台元数据地址 `169.254.169.254` 与 `fe80::/10` 即使写入白名单也会被拦截。关于防 SSRF 的具体校验机制，请参阅架构手册中的 [安全与防注入机制](/zh/reference/security)。

---

## 订阅源策略说明：全文提取与静默初始化

在 WebUI **Feeds** 页面或通过 MCP `manage_feed` 工具添加、编辑订阅源时，可以配置以下两个常用选项：

### 1. 按源全文提取（`fetch_full_content`）

部分网站的 RSS 只输出简短摘要。为该订阅源开启 `fetch_full_content: true` 后：

- 当拉取到需要投递的新文章时，系统会请求原文链接（单页响应上限 5 MiB），提取网页正文与作者信息；
- 全文提取使用独立的并发限制（默认并发数为 2，单页超时 20 秒），避免慢速网页阻塞常规订阅轮询；
- 提取出的 HTML 会自动将相对路径（图片与链接）补全为绝对 URL，并经过安全标签白名单清洗；
- 若源站超时、拒绝访问或提取结果为空，会自动回退使用 Feed 自带的原始摘要继续投递。

### 2. 静默初始化（`silent_init` 与 `push_latest_one`）

首次添加一个包含多篇历史文章的订阅源，或批量导入 OPML 文件时，如果将存量历史文章全部入队发送，会产生大量历史推送并占用通道配额。

- **`silent_init: true`（默认开启）**：首次成功拉取该订阅源时，所有存量文章仅记录去重指纹，不生成投递任务；首次拉取完成后，系统自动将该源的 `silent_init` 设为 `false`，后续新发布的文章正常入队投递。
- **`push_latest_one: true`（按需勾选）**：在添加单个订阅源且开启 `silent_init: true` 时，若勾选“立即试推最新 1 篇”，系统会在静默记录其余历史文章的同时，将发布时间最新的 1 篇文章入队投递，方便立刻查看下游通道的实际显示效果。
