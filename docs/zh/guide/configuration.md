# 配置说明与环境变量

FeedCourier 遵循**零配置文件开箱即用**原则：基础运行参数通过环境变量注入，而网络代理、内网 SSRF 白名单、订阅源策略与通道凭证均支持在 WebUI 中可视化修改并实时热重载落盘至 SQLite。

---

## 环境变量速查表

源码定于 `src/config.rs`（`AppConfig::from_env`）：

| 变量名 | 默认值 | 类型 / 格式 | 详细说明 |
| :--- | :--- | :--- | :--- |
| `DATA_DIR` | `./data`<br>*（Docker 镜像内默认为 `/data`）* | 目录路径 | 持久化数据目录，用于存放 `feedcourier.db`（SQLite WAL）、`master.key` 以及在线热备份临时快照。 |
| `LISTEN_ADDR` | `0.0.0.0:8080` | `SocketAddr`（`IP:Port`） | HTTP 服务监听地址，统一承载内嵌 React 19 WebUI、`/api/v1/*` 管理接口与 `/mcp` 服务端。 |
| `MASTER_KEY` | *（缺省自动生成）* | 64 位 Hex 或 Base64（32 字节） | 用于加密 `channels.encrypted_config` 的 ChaCha20-Poly1305 主密钥。未设置时自动在 `<DATA_DIR>/master.key` 生成（权限 `0600`）。 |
| `ADMIN_PASSWORD` | *（首次访问 WebUI 引导设置）* | 字符串（建议 $\ge 8$ 字符） | 启动时预设或覆盖 WebUI 管理员口令。启动阶段自动以 Argon2id PHC 格式哈希写入 `system_settings` 表。 |
| `OUTBOUND_PROXY` | *（未设置）* | URL（`http://`、`https://`、`socks5://`、`socks5h://`） | 初始全局出站代理地址。当 `OUTBOUND_PROXY` 未设置时，依次回退读取 `ALL_PROXY`、`HTTPS_PROXY`、`HTTP_PROXY`。仅当 `system_settings` 表尚未保存代理配置时作为初始种子写入。 |
| `SSRF_ALLOWLIST` | *（空）* | 逗号分隔的 CIDR 或主机名 | 初始内网 SSRF 放行白名单（如 `10.0.0.5/32,rsshub.internal,*.lan`）。仅当 `system_settings` 表尚未保存白名单时作为初始种子写入。 |
| `RETENTION_HOURS` | `24` | 无符号整数（`u64`） | 已闭环或未命中路由条目的正文（`content_html`）与媒体元数据暂存保留小时数，到期后由后台 `NOT EXISTS` GC 清理。 |
| `DB_READ_MAX_CONNECTIONS` | `8` | 正整数（`u32 >= 1`） | SQLite WAL 并发只读连接池（`PRAGMA query_only = ON`）的最大连接数。写连接池固定为 `1` 以保证串行无锁竞争。 |
| `RUST_LOG` | `feedcourier=info,tower_http=info` | `tracing_subscriber` 指令 | 结构化日志级别控制，排障时可设为 `feedcourier=debug,tower_http=debug`。 |

> [!NOTE]
> `OUTBOUND_PROXY` 与 `SSRF_ALLOWLIST` 会在首次启动时自动同步至 SQLite `system_settings` 表。此后你在 WebUI **Settings** 页面或通过 `PUT /api/v1/settings` 修改代理与白名单时，新配置会立即在内存中**无重启热重载**并持久化至数据库。

---

## 全局与按源 / 按通道分流代理机制

在跨国网络或受限网络环境中，部分上游 RSS 源或 Telegram / Notion API 需要通过代理访问，而境内订阅源或内网自建服务则需要直连。FeedCourier 提供了**全局代理地址 + 细粒度按需勾选**的双层分流机制：

1. **配置全局出站代理（`outbound_proxy_url`）**：
   - 支持协议：`http://`、`https://`、`socks5://`（本地 DNS 解析后连接 SOCKS5）、`socks5h://`（由远端 SOCKS5 代理执行 DNS 解析，推荐用于防 DNS 污染场景）。
   - 配置入口：环境变量 `OUTBOUND_PROXY` 或 WebUI **Settings → Outbound Network & SSRF Guard**。
2. **按订阅源独立开关（`feeds.use_proxy`）**：
   - 每个订阅源拥有独立的 `use_proxy` 布尔开关（默认关闭）。
   - 开启后，该订阅源的 **Feed XML/JSON 拉取**、**Favicon 探测**以及 **`fetch_full_content` Readability 网页全文抓取**均自动经由代理客户端发出。
3. **按投递通道独立开关（`channels.use_proxy`）**：
   - 每个授信通道拥有独立的 `use_proxy` 布尔开关（默认关闭）。
   - 开启后，Telegram Bot API 调用、**Telegram `media_mode` 防盗链媒体流式中转拉取**以及 Notion API 调用均自动经由代理转发。

---

## `SSRF_ALLOWLIST` 内网自建服务精准放行

FeedCourier 默认启用严格的**连接层 DNS 防 SSRF 保护**（`src/net/ssrf.rs`），自动拦截指向以下目标的一切未授权出站请求：
- 环回地址（`127.0.0.0/8`、`::1/128`、`localhost`）
- 私有局域网段（`10.0.0.0/8`、`172.16.0.0/12`、`192.168.0.0/16`、Unique Local `fc00::/7`）
- 链路本地与云厂商实例元数据服务（`169.254.0.0/16`、**`169.254.169.254` 永远硬拦截不可绕过**）

如果你在同一内网或 Docker 网络中自建了 **`RSSHub`**、**FreshRSS 输出源**或内部博客，请通过 `SSRF_ALLOWLIST` 精细放行对应 IP/CIDR 或内部域名，而无需关闭全局防护：

### 支持的白名单语法

- **精确 IP 或 CIDR 网段**：如 `10.0.0.5/32`、`192.168.50.0/24`、`fd00::/64`
- **精确内部主机名**：如 `rsshub.internal`、`rsshub`（Docker Compose 同网络服务名）
- **通配符子域名**：如 `*.internal.example.com`、`*.lan`

### Docker Compose 同网络自建 RSSHub 示例

```yaml
services:
  feedcourier:
    image: elleryhe/feedcourier:latest
    environment:
      # 精准放行 Docker 内部服务名 rsshub
      SSRF_ALLOWLIST: "rsshub,172.20.0.0/16"
    depends_on:
      - rsshub

  rsshub:
    image: diygod/rsshub:chromium-bundled
    restart: unless-stopped
```

此时在 FeedCourier 中即可安全订阅 `http://rsshub:1200/...` 而不会被防 SSRF 策略拦截。

> [!WARNING]
> 出于云环境纵深安全考量，云平台元数据地址 `169.254.169.254` 与 `fe80::/10` 等敏感特殊地址即使被写入白名单也会被底层守卫拒绝；Telegram 通道配置的 `api_base_url`（如内网自建 Local Bot API Server）属于管理员在 WebUI 显式锁死的授信端点，由 `trusted_client` 承载，但媒体防盗链回退拉取源站图片时仍严格执行 SSRF 校验。

---

## 按源配置项详解：Readability 全文提取与静默初始化

在 WebUI **Feeds** 页面或通过 MCP `manage_feed` 工具管理订阅源时，有两个关键策略开关直接影响投递体验：

### 1. 按源全文提取（`fetch_full_content`）

许多网站输出的 RSS 仅包含寥寥数十字的截断摘要。开启该源的 `fetch_full_content: true` 后：

- 当发现**需要投递的新条目**时，FeedCourier 会使用受保护 HTTP 客户端请求条目的 `canonical_url`（单页响应体上限 **5 MiB**）；
- 通过纯 Rust 实现的 **`dom_smoothie`（Readability 算法）** 提取网页核心正文、作者与站点图标，无需外部 Headless Chrome 容器；
- 全文提取受**独立并发信号量（默认并发上限 `2`，单页超时 `20s`）**隔离保护，绝不会因为个别慢速源站阻塞常规 Feed 轮询；
- 提取出的 HTML 会自动以原文 URL 为基准将所有相对路径（`<img src="/...">`、`<a href="?... ">`）补全为绝对 URL，再经 `ammonia` 白名单清洗；
- **无损降级**：若源站开启反爬校验、网络超时或提取正文为空，系统自动平滑回退至 Feed 原生摘要，绝不阻断投递任务。

### 2. 静默初始化（`silent_init` 与 `push_latest_one`）

当你首次添加一个包含 50 篇历史文章的订阅源，或批量导入一份包含上百个源的 OPML 文件时，如果将存量历史文章全部入队推送，会瞬间塞满收件箱并耗尽通道配额。

- **`silent_init: true`（默认开启）**：首次成功拉取该订阅源时，所有存量条目仅计算 SHA-256 指纹写入 `entries` 表并立即清空正文大字段（标记 `payload_purged_at`），**不生成任何投递任务**；首次拉取完成后，系统自动将该源的 `silent_init` 翻转为 `false`，后续新发布的文章正常实时入队投递。
- **`push_latest_one: true`（按需勾选）**：在新增单个订阅源且开启 `silent_init: true` 时，若勾选“立即试推最新 1 篇”，系统会在静默吸收其余历史条目的同时，**挑选发布时间最新（`published_at` 最大）的 1 篇文章入队投递**，方便你在添加源的第一时间检验下游通道排版效果。
