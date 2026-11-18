---
description: "FeedCourier REST API 接口参考（/api/v1/*）：涵盖用户名/密码与两步验证登录、账号安全、概览统计、订阅源（清除缓存、调试推送）与 OPML 导入导出、投递通道、路由规则（投递语言、时间窗口）、任务队列与全局队列控制、系统设置（时区）以及在线备份。"
---

# REST API 接口参考（`/api/v1/*`）

FeedCourier 的内嵌 React 19 管理面板通过挂载于 `/api/v1/*` 的 JSON REST API 与后端通信（路由与处理器源码位于 `src/api/mod.rs`、`src/api/auth.rs`、`src/api/handlers.rs` 与 `src/api/dto.rs`）。运维脚本或外部程序也可直接通过 `curl` 调用这些接口。

---

## 鉴权机制与统一错误格式

### 1. Argon2id 口令与 `fc_session` `HttpOnly` Cookie

除公开探针与登录引导端点（`/api/v1/health`、`/api/v1/auth/*`）外，所有 `/api/v1/*` 端点均受 `require_session` 中间件保护：
- 调用 `POST /api/v1/auth/setup`（首次初始化）、`POST /api/v1/auth/login`（未启用 2FA 时）或 `POST /api/v1/auth/login/2fa` 成功后，服务端通过 `Set-Cookie` 响应头下发名为 **`fc_session`** 的会话凭证：
  ```http
  Set-Cookie: fc_session=<token>; HttpOnly; SameSite=Lax; Path=/; Max-Age=2592000
  ```
- 服务端仅在 SQLite `sessions` 表中存储该令牌的 **SHA-256 哈希值**，有效期为 **30 天（`2,592,000` 秒，`SESSION_TTL_SECS`）**。

### 2. 统一错误响应格式

所有 `4xx` / `5xx` 响应（包括未知的 `/api/*` 路径 `404`）均返回结构化 JSON：

```json
{
  "error": "validation_error",
  "message": "poll_interval_secs must be greater than 0"
}
```

常见 `error` 代码包括：`unauthorized`（`401`）、`not_found`（`404`）、`conflict`（`409`）、`validation_error` / `bad_request`（`400`）、`ssrf_blocked`（`403`）、`upstream_error`（`502`）。

---

## 一、系统健康、鉴权与运行概览

### 1. 公开健康检查与版本合规信息
- **路径**：`GET /api/v1/health`（无需鉴权；用于容器健康检查探针）
- **响应（`200 OK`）**：
  ```json
  {
    "name": "FeedCourier",
    "version": "0.1.5",
    "license": "AGPL-3.0",
    "repo_url": "https://github.com/ElleryHe/FeedCourier",
    "status": "ok"
  }
  ```

### 2. 查询鉴权与初始化状态
- **路径**：`GET /api/v1/auth/status`（无需鉴权）
- **响应（`AuthStatusResponse`）**：
  ```json
  {
    "setup_required": false,
    "authenticated": true,
    "version": "0.1.5",
    "repo_url": "https://github.com/ElleryHe/FeedCourier",
    "license": "AGPL-3.0"
  }
  ```

### 3. 首次初始化管理员账号
- **路径**：`POST /api/v1/auth/setup`
- **请求体**：`{ "username": "Courier", "password": "your-strong-password" }`（`username` 可选，默认 `Courier`，1–64 个字符，自动去除首尾空白且不得含控制字符；口令要求长度 $\ge 8$ 个字符；若已初始化过则返回 `409 Conflict`）
- **响应**：`{ "ok": true }` 并设置 `fc_session` Cookie。
- `GET /api/v1/auth/status` 不会返回用户名。

### 4. 管理员登录、两步验证与登出
- **登录**：`POST /api/v1/auth/login`，请求体 `{ "username": "Courier", "password": "..." }`。
  - 未启用 2FA：返回 `{ "ok": true, "mfa_required": false }` 并下发 `fc_session`。
  - 已启用 2FA 且凭据正确：返回 `{ "ok": false, "mfa_required": true, "mfa_token": "..." }`，不下发 Cookie；`mfa_token` 有效期 5 分钟。
  - 用户名或密码错误：`401`，`{ "error": "unauthorized", "code": "unauthorized", "message": "..." }`（提示信息是通用的，不会透露具体是哪一项错误）；连续失败过多时返回 `429` 并带 `Retry-After` 响应头。
- **两步验证登录**：`POST /api/v1/auth/login/2fa`，请求体 `{ "mfa_token": "...", "code": "123456" }`。`code` 可以是 6 位 TOTP 验证码，也可以是恢复码（`XXXX-XXXX-XXXX`，不区分大小写，连字符可省略；每个恢复码仅可使用一次）。成功后返回 `{ "ok": true, ... }` 并下发 `fc_session`。
- **登出**：`POST /api/v1/auth/logout`，从数据库删除当前会话哈希并清除 Cookie。
- **重置说明**：用户名与口令可通过启动时注入环境变量 `ADMIN_USERNAME` / `ADMIN_PASSWORD` 在每次启动时覆盖；验证器与恢复码全部丢失时，可一次性设置 `RESET_2FA=true` 重启（参见[丢失验证器](/zh/guide/operations#lost-authenticator-reset-2fa)）。

### 5. 全局运行指标概览
- **路径**：`GET /api/v1/overview`
- **响应（`OverviewResponse`）**：
  ```json
  {
    "feeds": { "active": 10, "backoff": 1, "circuit_open": 0, "paused": 0 },
    "entries": { "total_fingerprints": 1520, "active_payloads": 8 },
    "tasks": {
      "queued": 0,
      "delivering": 0,
      "retrying": 0,
      "deferred": 1,
      "dead_letter": 0,
      "delivered": 1980
    },
    "active_cooldowns": []
  }
  ```

### 6. 账号与安全（`/api/v1/account*`）

以下端点均需已登录会话。

| 方法 | 路径 | 说明 |
| :---: | :--- | :--- |
| `GET` | `/api/v1/account` | 返回 `AccountDto`：`{ "username": "Courier", "username_env_managed": false, "totp_enabled": false, "recovery_codes_remaining": 0 }`。`username_env_managed: true` 表示用户名由环境变量 `ADMIN_USERNAME` 在每次重启时覆盖。 |
| `PATCH` | `/api/v1/account` | 修改用户名和/或口令。请求体 `{ "current_password": "...", "new_username"?: "...", "new_password"?: "...", "code"?: "..." }`。启用 2FA 且设置 `new_password` 时，`code`（TOTP 或恢复码）**必填**；修改口令会使其他所有会话失效。返回 `AccountDto`。 |
| `POST` | `/api/v1/account/2fa/setup` | 请求体 `{ "current_password": "..." }`。生成尚未生效的待确认 TOTP 密钥，返回 `{ "secret": "<base32>", "otpauth_url": "otpauth://...", "qr_svg": "<svg ...>" }`；若已启用 2FA 则返回 `409`。 |
| `POST` | `/api/v1/account/2fa/enable` | 请求体 `{ "code": "123456" }`（来自待确认密钥的 TOTP 验证码）。启用 2FA 并返回仅展示一次的 `{ "recovery_codes": [ ...10 个恢复码 ] }`。 |
| `POST` | `/api/v1/account/2fa/disable` | 请求体 `{ "current_password": "...", "code": "..." }`。关闭 2FA 并删除密钥与恢复码，返回 `AccountDto`。 |
| `POST` | `/api/v1/account/2fa/recovery-codes` | 请求体 `{ "current_password": "..." }`。仅在已启用 2FA 时可用，用 10 个新恢复码替换旧恢复码，返回 `{ "recovery_codes": [...] }`。 |

以上接口的二次验证失败（`current_password` 或 `code` 错误）返回 `403` 而非 `401`，避免客户端误判为会话过期；连续失败过多时返回 `429`。

---

## 二、订阅源与 OPML 导入导出（`/api/v1/feeds*`）

| 方法 | 路径 | 说明 |
| :---: | :--- | :--- |
| `GET` | `/api/v1/feeds` | 获取全部订阅源列表（`FeedDto[]`）。 |
| `POST` | `/api/v1/feeds` | 新增订阅源并立即触发首次拉取（支持 `silent_init` 与 `push_latest_one`；若初始拉取触发 `SsrfBlocked` 则自动回滚删除该源）。 |
| `POST` | `/api/v1/feeds/{id}/clear-cache` | 删除该订阅源的全部文章记录、指纹与任务，清除 `ETag` / `Last-Modified` 与失败状态，并设置 `silent_init = false`。请求体 `{ "poll_now"?: true }`（默认 `true`），返回 `{ "deleted_entries": 42, "poll": PollFeedResponse \| null }`。`poll_now` 为真时立即按从旧到新的顺序将全部积压入队，否则在下一次定时拉取时入队。 |
| `POST` | `/api/v1/feeds/preview` | 在不写入数据库的情况下经防 SSRF 客户端试抓指定 URL 最新 10 篇条目，支持试跑 Readability 与过滤规则。 |
| `POST` | `/api/v1/feeds/preview/push` | 将一条文章调试推送到指定通道（见下文）。 |
| `PATCH` | `/api/v1/feeds/{id}` | 修改订阅源 URL、标题、标签、轮询间隔、`fetch_full_content`、`use_proxy` 或暂停/恢复（`health_status: "active" \| "paused"`）。 |
| `DELETE` | `/api/v1/feeds/{id}` | 删除订阅源（级联清理其 `entries` 与关联 `delivery_tasks`）。 |
| `POST` | `/api/v1/feeds/{id}/poll` | 解除该源的熔断/退避状态并立即触发一次增量拉取（返回 `PollFeedResponse`）。 |
| `POST` | `/api/v1/feeds/opml/import` | 导入 OPML 2.0 XML 文档（请求体 `{ "opml_xml": "...", "silent_init": true }`，自动跳过已订阅 URL）。 |
| `GET` | `/api/v1/feeds/opml/export` | 将当前全部订阅源导出为标准 OPML 2.0 XML 附件下载。 |

### 新增订阅源请求示例（`POST /api/v1/feeds`）

```json
{
  "url": "https://blog.rust-lang.org/feed.xml",
  "title": "Rust Official Blog",
  "tags": ["rust", "Compiler"],
  "poll_interval_secs": 1800,
  "fetch_full_content": true,
  "use_proxy": false,
  "silent_init": true,
  "push_latest_one": true
}
```

### 调试推送（`POST /api/v1/feeds/preview/push`）

将单条订阅源文章投递到指定通道用于测试。该接口是**无状态**的：不写入指纹、不进入发件箱，也不应用路由过滤、时间窗口与配额；Notion 页面的 `FC-ID` 带 `test-` 前缀。

```json
{
  "url": "https://example.com/feed.xml",
  "feed_id": 4,
  "fingerprint_sha256": "…",
  "fetch_full_content": false,
  "use_proxy": false,
  "channel_id": 2,
  "options": { "telegram_mode": "rich_message", "link_preview": true, "locale": "zh-CN" }
}
```

`url` 与 `channel_id` 必填；`feed_id` 可选（预览已订阅的订阅源时传入）；省略 `fingerprint_sha256` 表示推送最新一篇；`options` 可选。响应（`200 OK`）：`{ "ok": true, "message": "...", "entry_title": "...", "entry_url": "https://..." \| null }`。下游投递失败时返回 `ok: false`，诊断信息在 `message` 中（HTTP 状态码仍为 `200`）。

---

## 三、授信投递通道与 Notion 向导（`/api/v1/channels*`）

| 方法 | 路径 | 说明 |
| :---: | :--- | :--- |
| `GET` | `/api/v1/channels` | 获取全部授信通道列表（敏感密码/Token 自动掩码脱敏为 `has_password` / `bot_token_hint` / `secret_hint`，详见[安全与防注入机制](/zh/reference/security)）。 |
| `POST` | `/api/v1/channels` | 创建新通道（`smtp` / `telegram` / `notion`）；Telegram 通道创建时自动执行 `probe_capabilities` 探测。 |
| `PATCH` | `/api/v1/channels/{id}` | 更新通道配置、步进间隔或配额（**密钥保留语义**：当 `password` / `bot_token` / `integration_secret` 省略或留空时，后端自动保留原加密密钥不变；将 `hourly_quota` 或 `daily_quota` 显式传 `null` 可清空配额限制）。 |
| `DELETE` | `/api/v1/channels/{id}` | 删除指定通道（级联删除绑定该通道的路由与任务）。 |
| `POST` | `/api/v1/channels/{id}/test` | 向该通道发送实时连通性测试邮件/消息/页面（Telegram 同时刷新 `capabilities_json`）。可选请求体 `{ "locale"?: "en" \| "zh-CN" }` 指定测试消息语言（WebUI 会发送当前界面语言）。 |
| `POST` | `/api/v1/channels/notion/init-db` | 在授权父页面下创建包含 7 个标准属性列的 Notion 数据库（`Notion-Version: 2026-03-11`，`initial_data_source.properties`）。 |
| `POST` | `/api/v1/channels/notion/inspect-schema` | 检查已有 Notion 数据源属性列匹配情况，并在 `patch_missing: true` 时自动补齐缺失列。 |

### 创建通道请求结构（`POST /api/v1/channels`）

```json
{
  "name": "Telegram 主频道",
  "kind": "telegram",
  "config": {
    "kind": "telegram",
    "bot_token": "123456789:AAH...",
    "chat_id": "@my_tech_feed",
    "message_thread_id": null,
    "api_base_url": null
  },
  "min_interval_secs": 15,
  "jitter_max_secs": 10,
  "hourly_quota": 30,
  "daily_quota": 200,
  "use_proxy": false,
  "enabled": true
}
```

---

## 四、路由编排（`/api/v1/routes*`）

| 方法 | 路径 | 说明 |
| :---: | :--- | :--- |
| `GET` | `/api/v1/routes` | 获取全部路由规则列表（`RouteDto[]`）。 |
| `POST` | `/api/v1/routes` | 创建路由规则（创建前编译校验 `include_regex` / `exclude_regex` 合法性）。 |
| `PATCH` | `/api/v1/routes/{id}` | 更新路由规则（正则字段传 `null` 或 `""` 可清除）。 |
| `DELETE` | `/api/v1/routes/{id}` | 删除指定路由规则。 |

### 创建路由请求示例（`POST /api/v1/routes`）

```json
{
  "name": "Rust 动态推至 Telegram",
  "feed_ids": [],
  "match_tags": ["rust"],
  "channel_id": 1,
  "include_keywords": [],
  "exclude_keywords": ["sponsored"],
  "include_regex": null,
  "exclude_regex": null,
  "options": {
    "telegram_mode": "rich_message",
    "link_preview": true,
    "locale": "zh-CN",
    "active_windows": [{ "start": "08:00", "end": "22:00" }],
    "active_weekdays": [1, 2, 3, 4, 5]
  },
  "enabled": true
}
```

### 路由 `options` 字段

| 字段 | 说明 |
| :--- | :--- |
| `telegram_mode` | `"rich_message"`（默认）、`"compact_card"` 或 `"media_mode"`。 |
| `link_preview` | Telegram 网页链接预览开关。 |
| `locale` | 该路由的投递语言（`"en"` 或 `"zh-CN"`）。WebUI 默认填入当前界面语言；未设置的旧路由回退到旧版 `settings.locale`。 |
| `active_windows` | 系统时区下的每日投递窗口（`HH:MM`）。`end` 早于 `start` 表示跨午夜，`start == end` 表示全天，空列表表示不限制。 |
| `active_weekdays` | `1`（周一）到 `7`（周日），空列表表示每天；跨午夜的窗口按开始那一天的星期判定。 |

窗口或星期之外的任务**仍会入队**：状态保持 `queued` / `deferred`，`next_attempt_at` 被设为下一个窗口的开始时间。

---

## 五、发件箱任务、全局队列与死信重投（`/api/v1/tasks*`）

| 方法 | 路径 | 说明 |
| :---: | :--- | :--- |
| `GET` | `/api/v1/tasks` | 查询发件箱投递任务列表。支持 Query 参数 `status`（`queued` / `delivering` / `retrying` / `deferred` / `dead_letter` / `delivered`）与 `limit`（默认 `100`，最大 `500`）。 |
| `POST` | `/api/v1/tasks/retry` | 批量重投失败任务：请求体传入 `{ "task_ids": [12, 15] }` 或 `{ "all_dead_letter": true }`，唤醒发件箱 Worker 并返回 `{ "requeued_count": 2 }`。 |
| `GET` | `/api/v1/tasks/queue` | 查询全局发件箱状态：`{ "paused": false, "resume_at": null }`。已自动恢复的暂停会返回 `paused: false`。 |
| `PUT` | `/api/v1/tasks/queue` | 请求体 `{ "action": "pause", "resume_in_secs"?: 3600, "resume_at"?: "2026-01-01T08:00:00Z" }` 或 `{ "action": "resume" }`，返回与 `GET` 相同的结构。暂停期间订阅源仍会轮询、任务持续累积；恢复后所有订阅源中到期的任务按 `COALESCE(published_at, discovered_at)` 从旧到新依次投递。 |

---

## 六、系统设置、MCP 令牌与在线备份维护

| 方法 | 路径 | 说明 |
| :---: | :--- | :--- |
| `GET` | `/api/v1/settings` | 获取当前生效的设置：`{ "outbound_proxy_url", "ssrf_allowlist", "timezone": "America/Los_Angeles", "locale" }`。`timezone` 为 IANA 时区名（默认取自 `TIMEZONE` / `TZ`）；`locale` 仅是未设置 `options.locale` 的旧路由的**兼容回退**投递语言，WebUI 不再写入它，也不再用它决定界面语言。 |
| `PUT` | `/api/v1/settings` | 局部更新 `timezone`、`outbound_proxy_url`、`ssrf_allowlist` 的任意子集并持久化至 `system_settings` 表；仅当请求中包含网络相关字段时才会应用并热重载运行时客户端。无效的 IANA 时区返回 `400`。 |
| `GET` | `/api/v1/mcp-tokens` | 获取已签发的 MCP Bearer Token 元数据列表（仅含 `id`、`name`、`token_prefix`、`scope`、`last_used_at`，不含明文）。 |
| `POST` | `/api/v1/mcp-tokens` | 签发新 MCP 令牌（请求体 `{ "name": "cursor", "scope": "readonly" \| "admin" }`），返回仅展示一次的 `plaintext_token` 与 `client_config_json`。 |
| `DELETE` | `/api/v1/mcp-tokens/{id}` | 立即吊销并删除指定 MCP 令牌。 |
| `POST` | `/api/v1/maintenance/gc` | 立即触发一次后台维护（执行 `NOT EXISTS` 正文回收、清理过期会话并运行 `PRAGMA optimize`），返回 `{ "purged_entries": N }`。 |
| `GET` | `/api/v1/maintenance/backup` | 执行 SQLite `VACUUM INTO` 在线热备份，并以二进制流返回数据库快照附件。 |
