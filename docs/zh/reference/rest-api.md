---
description: "FeedCourier REST API 接口参考（/api/v1/*）：涵盖鉴权与 2FA、订阅源、通道与目的地（409 保护）、SMTP 收件人验证、插件与市场、路由规则、任务队列、系统设置（PUBLIC_URL）、MCP 令牌以及公开 /u/{token} 与 /v/{token} 合规端点。"
---

# REST API 接口参考（`/api/v1/*`）

FeedCourier 的内嵌 React 19 管理面板通过挂载于 `/api/v1/*` 的 JSON REST API 与后端通信（路由与处理器源码位于 `src/api/mod.rs`、`src/api/auth.rs`、`src/api/handlers.rs` 与 `src/api/dto.rs`）。运维脚本或外部程序也可直接通过 `curl` 调用这些接口。

---

## 鉴权机制与统一错误格式

### 1. Argon2id 口令与 `fc_session` `HttpOnly` Cookie

除公开探针与登录引导端点（`/api/v1/health`、`/api/v1/auth/*`）以及挂载在根路径的公开 SMTP 合规端点（`/u/{token}`、`/v/{token}`，注册在 SPA 回退之前）外，所有 `/api/v1/*` 端点均受 `require_session` 中间件保护：
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
    "version": "0.3.1",
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
    "version": "0.3.1",
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
| `POST` | `/api/v1/feeds` | 返回 **`201 Created`**。新增订阅源并立即触发首次拉取（支持 `silent_init` 与 `push_latest_one`；若初始拉取触发 `SsrfBlocked` 则自动回滚删除该源）。 |
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

将单条订阅源文章投递到指定授信目的地用于测试。该接口是**无状态**的：不写入指纹、不进入发件箱，也不应用路由过滤、时间窗口与配额；Notion 页面的 `FC-ID` 带 `test-` 前缀。针对 SMTP 目的地，调试推送严格**仅向 `verified` 收件人**按单 `To:` 隔离信封附带 RFC 8058 合规头逐人发送（0 位已验证收件人时返回错误提示）。

```json
{
  "url": "https://example.com/feed.xml",
  "feed_id": 4,
  "fingerprint_sha256": "…",
  "fetch_full_content": false,
  "use_proxy": false,
  "destination_id": 5,
  "options": {
    "telegram_mode": "rich_message",
    "link_preview": true,
    "locale": "zh-CN",
    "timezone": "Asia/Shanghai",
    "plugins": {
      "link-rewrite": { "provider_id": 1 }
    }
  }
}
```

`url` 与 `destination_id` 必填（兼容旧调用方传入 `channel_id`：仅在对应账号恰好只有 **1 个目的地** 时自动解析，多目的地时返回校验错误）；`feed_id` 可选（预览已订阅的订阅源时传入）；省略 `fingerprint_sha256` 表示推送最新一篇；`options` 可选。响应（`200 OK`）：`{ "ok": true, "message": "...", "entry_title": "...", "entry_url": "https://..." | null }`。下游投递失败时返回 `ok: false`，诊断信息在 `message` 中（HTTP 状态码仍为 `200`）。

---

## 三、授信投递通道（账号）、目的地与 Notion 向导（`/api/v1/channels*`、`/api/v1/destinations*`）

| 方法 | 路径 | 说明 |
| :---: | :--- | :--- |
| `GET` | `/api/v1/channels` | 获取全部授信通道账号列表（`ChannelDto[]`，敏感密码/Token 自动脱敏为 `has_password` / `bot_token_hint` / `secret_hint`，对于 `plugin` 通道则按插件 Schema 自动脱敏为 `has_<field>` / `<field>_hint`，并内嵌 `destinations: ChannelDestinationDto[]`，每个目的地包含 `enabled` 与 `auto_paused: bool`，SMTP 目的地另附带 `smtp_summary` 收件人验证计数，详见[安全与防注入机制](/zh/reference/security)）。 |
| `POST` | `/api/v1/channels` | 返回 **`201 Created`**。创建新通道账号（`smtp` / `telegram` / `notion` / `plugin`，SMTP 支持可选 `sender_identity` 且使用原生 TCP/TLS 直连，不适用 HTTP/SOCKS 代理；`name` 可省略以自动获取，可选传入 `initial_destination` 一并创建首个目的地）；Telegram 账号创建时自动执行 `probe_capabilities` 探测（`api_base_url` 强制走 SSRF 校验，命中禁止目标时返回 `403 ssrf_blocked`）；`kind = "plugin"` 时通过沙箱 JS 插件的 `validate` 函数（`scope: "account"`）校验账号配置，且支持 `use_proxy` 开关（针对公网 `allowed_hosts` 走全局代理，自建内网地址受 `SSRF_ALLOWLIST` 约束直连）。 |
| `PATCH` | `/api/v1/channels/{id}` | 更新账号凭据、`sender_identity`、代理（Telegram / Notion / 插件通道）、共享步进间隔或配额（**密钥保留语义**：当 `password` / `bot_token` / `integration_secret` 或 `plugin` 通道 Schema 中的密钥字段省略或留空时，后端自动保留原加密密钥不变；对于 SMTP 账号，显式传入 `"password": null` 可清空可选的 SMTP 密码；将 `hourly_quota` 或 `daily_quota` 显式传 `null` 可清空配额限制）。 |
| `DELETE` | `/api/v1/channels/{id}` | 在单事务内原子删除通道账号。若其下目的地仍被任意路由引用且未传 `?cascade=true`，返回 **`409 Conflict`** 及 `{ "error", "code": "conflict", "message", "referencing_routes": [{ "id", "name" }] }`（与 `DELETE /api/v1/destinations/{id}` 对齐）；显式传入 **`?cascade=true`** 才会连同所有目的地、关联路由与任务一并删除。 |
| `POST` | `/api/v1/channels/{id}/test` | 未传 `destination_id` 时，校验账号级凭据（SMTP 校验连通性与认证并**仅向自身 `from_address`** 发送自检邮件；Telegram 执行 `getMe` + `sendRichMessage` 探测；Notion 调用 `GET /v1/users/me`；`plugin` 通道执行沙箱 JS 插件的 `test` 函数且 `scope: "account"`）；传入 `{ "destination_id": 5, "locale"?: "en" \| "zh-CN" }` 时执行目的地级测试（规则同 `POST /api/v1/destinations/{id}/test`）。 |
| `GET` | `/api/v1/channels/{id}/destinations` | 列出通道账号 `{id}` 下的全部目的地（`ChannelDestinationDto[]`，含 `auto_paused: bool`，`plugin` 目的地自动按 Schema 脱敏密钥字段，SMTP 目的地附带 `smtp_summary`）。 |
| `POST` | `/api/v1/channels/{id}/destinations` | 返回 **`201 Created`**。在通道账号 `{id}` 下新增目的地（`CreateDestinationRequest`；省略 `name` 时自动取 Telegram `getChat`、打码后的 SMTP 首个非空收件人如 `a***e@example.com`、Notion 数据库标题或 `plugin` 目的地主标识字段）；`plugin` 通道会自动执行 JS 插件 `validate` 函数（`scope: "destination"`）校验。 |
| `PATCH` | `/api/v1/destinations/{id}` | 局部更新目的地备注名、目标配置（`DestinationPatch`）或启用状态（`enabled`；手动修改 `enabled` 时自动清零 `auto_paused = false`；`plugin` 目的地中的密钥字段在省略或传空串时保持不变）。修改 SMTP `to_addresses` 时自动同步 `smtp_recipients`（新地址初始为 `unverified`，已移除地址同步删除，保留地址维持原状态）。 |
| `DELETE` | `/api/v1/destinations/{id}` | 在单事务内原子删除指定目的地。若仍有路由引用该目的地且未传 `?cascade=true`，返回 **`409 Conflict`** 及 `{ "error", "code": "conflict", "message", "referencing_routes": [{ "id", "name" }] }`；显式传入 **`?cascade=true`** 才会连同关联路由与任务一起删除。 |
| `POST` | `/api/v1/destinations/{id}/test` | 向指定目的地发送实时连通性测试邮件/消息/页面，或对 `plugin` 目的地执行沙箱 JS 插件 `test`（`scope: "destination"`）。可选请求体 `{ "locale"?: "en" \| "zh-CN" }` 指定测试消息语言。针对 SMTP 目的地，严格**仅向 `verified` 收件人**按单 `To:` 隔离信封附带 RFC 8058 合规头逐人发送，0 位已验证收件人时返回明确错误。 |
| `POST` | `/api/v1/channels/notion/init-db` | 在授权父页面下创建包含 8 个标准属性列（含可选 `Short URL`）的 Notion 数据库（`Notion-Version: 2026-03-11`，`initial_data_source.properties`）。 |
| `POST` | `/api/v1/channels/notion/inspect-schema` | 检查已有 Notion 数据源属性列匹配情况，并在 `patch_missing: true` 时自动补齐缺失列。 |

### 插件通道配置结构（`kind = "plugin"`）

创建或更新 JS 插件通道账号（`ChannelKind::Plugin`）时，在账号侧指定 `plugin_id`、按需设置 `use_proxy`（用于公网 `allowed_hosts`），并在 `config` 中传入由插件 Schema 定义的配置对象；目的地侧同样嵌套在 `config` 内（自动归属父账号的 `plugin_id`）：

```json
{
  "name": "ntfy 推送",
  "kind": "plugin",
  "use_proxy": false,
  "config": {
    "kind": "plugin",
    "plugin_id": "ntfy",
    "config": {
      "server_url": "https://ntfy.sh",
      "access_token": "tk_..."
    }
  },
  "initial_destination": {
    "name": "发布通知",
    "config": {
      "kind": "plugin",
      "config": {
        "topic": "feedcourier-alerts",
        "priority": "high"
      }
    }
  }
}
```

插件 Manifest 的 `account_schema` / `destination_schema` 中标注为敏感凭据的字段（如 `"format": "password"` 或 `"x-secret": true`）落盘时采用 ChaCha20-Poly1305 加密，读取接口（`GET`）中自动脱敏为 `has_<field>: true|false` 与 `<field>_hint: "tk_••••"`，不返回明文。

### SMTP 收件人合规管理（`/api/v1/destinations/{id}/recipients*`）

| 方法 | 路径 | 说明 |
| :---: | :--- | :--- |
| `GET` | `/api/v1/destinations/{id}/recipients` | 解密 SMTP 目的地配置并返回各收件人状态与审计信息（`SmtpRecipientDto[]`：含 `email`、`status`、`code_expires_at`、`code_attempts`、`last_code_sent_at`、`consent_method`/`consented_at`、`revoked_method`/`revoked_at`、已脱敏的 `bounce_diagnostic`）。 |
| `POST` | `/api/v1/destinations/{id}/recipients/{rid}/send-verification` | 先在 `write_pool` 事务内原子检查并预占限流槽位，再立即发送验证邮件（不入发件箱、不占配额；若发信失败自动回滚预占），内含 6 位验证码（30 分钟有效、最多输错 5 次），配置 `PUBLIC_URL` 时附带 `/v/{token}` 确认链接。受 60 秒冷却与每小时 5 封上限保护，超限时返回 **`429 Too Many Requests`** 并附带 **`Retry-After`** 响应头（剩余秒数）。 |
| `POST` | `/api/v1/destinations/{id}/recipients/{rid}/verify` | 请求体 `{ "code": "123456" }`。常量时间校验 6 位验证码，将收件人标记为 `verified`（`consent_method: "code"`），若目的地处于 `auto_paused = true` 则同事务自动恢复 `enabled = true, auto_paused = false`，并立即唤醒该目的地下处于 `deferred` 的任务。累计输错 5 次时自动清空当前验证码（旧 `/v/{token}` 链接立即返回 `410 Gone`）。 |
| `POST` | `/api/v1/destinations/{id}/recipients/{rid}/reactivate` | 将 `unsubscribed` 或 `bounced` 状态的收件人重置为 `unverified`，须重新完成验证后才会恢复投递。 |
| `POST` | `/api/v1/destinations/{id}/recipients/{rid}/unsubscribe` | 管理员手动退订该收件人（`status: "unsubscribed"`，`revoked_method: "admin"`）。 |

---

## 三（续）、插件与插件市场（`/api/v1/plugins*`）

| 方法 | 路径 | 说明 |
| :---: | :--- | :--- |
| `GET` | `/api/v1/plugins` | 获取已安装插件列表（`PluginDto[]`），含 `source`（`builtin` \| `market` \| `custom`）、`runtime`（`builtin` \| `js`）、`kind`（`processor` \| `channel`）、`script_sha256`、`allowed_hosts`、`compatible` / `compatibility_reason`、`manifest`（内嵌 `description_i18n`、`allowed_hosts`、`instance_schema`、`route_option_schema`、`account_schema`、`destination_schema`、`capabilities`）、`enabled`、`disabled_reason`（连续 3 次超时触发自动熔断禁用时非空）、`installed_version`、`settings`、`instance_count` 及脱敏后的 `instances: PluginInstanceDto[]`。 |
| `POST` | `/api/v1/plugins/install` | 从插件市场一键安装或更新沙箱 JS 插件（请求体 `{ "plugin_id", "version"?, "enabled"?: true, "dry_run"?: false, "replace_custom"?: false }`）。后端通过 `ProtectedHttpClient` 拉取对应版本的 `plugin.js`、强制校验注册表记录的 SHA-256 摘要，并在 `boa_engine` 中编译提取 `meta` 与校验 Protocol v1 / `min_core` 兼容性：`dry_run: true` 时返回 **`200 OK`**（`JsPluginPreviewResponse`，含 `script_sha256`、`allowed_hosts`、`already_installed`、`installed_version`、`installed_source` 与 `changelog`）不落盘；否则持久化脚本、同步关联通道能力并返回 **`201 Created`**（原位更新返回 **`200 OK`**）及 `PluginDto`。若目标插件 ID 当前以 `source: "custom"` 安装且未传 `replace_custom: true`，返回 **`409 Conflict`** 防止误覆盖自定义脚本。 |
| `POST` | `/api/v1/plugins/upload` | 导入自定义单文件 JS 插件（请求体 `{ "script_js", "enabled"?: true, "dry_run"?: false, "allow_overwrite"?: false }`）。在 `boa_engine` 沙箱中编译校验 `script_js` 并提取 `meta`、`allowed_hosts` 与 `script_sha256`：`dry_run: true` 时返回 **`200 OK`**（`JsPluginPreviewResponse`）；否则以 `source: "custom"` 持久化并返回 **`201 Created`** / **`200 OK`**（`PluginDto`；若目标 ID 已安装且未传 `allow_overwrite: true` 返回 **`409 Conflict`**，若与内置插件 ID 冲突返回 **`400 Bad Request`**）。 |
| `GET` | `/api/v1/plugins/{id}/source` | 查看已安装 JS 插件的持久化源码（`PluginSourceResponse`：`{ "id", "source", "version", "script_sha256", "script_js" }`）；对内置 Rust 插件返回 **`400 Bad Request`**。 |
| `PATCH` | `/api/v1/plugins/{id}` | 请求体 `{ "enabled"?: bool, "settings"?: object }`。全局启用/禁用插件或更新插件级配置（禁用时保留路由配置与通道账号；手动启用 `enabled: true` 会自动清除熔断写入的 `disabled_reason`）。 |
| `DELETE` | `/api/v1/plugins/{id}` | 卸载已安装的 JS 插件。内置插件拒绝卸载并返回 **`400 Bad Request`**；若仍被任意 `kind = 'plugin'` 通道账号或路由（`options.plugins` / `options.plugin_order`）引用且未传 `?cascade=true`，返回 **`409 Conflict`** 并列出 `referencing_channels: [{ "id", "name" }]` 与 `referencing_routes: [{ "id", "name" }]`；显式传入 **`?cascade=true`** 将原子卸载该插件及其关联通道账号（含目的地、路由与任务）并从其余路由中剥离处理器引用。 |
| `GET` | `/api/v1/plugins/{id}/instances` | 列出插件 `{id}` 下已配置的服务商实例（敏感凭据脱敏为 `has_signature`、`signature_hint`、`has_password` 或按 Schema 生成的 `has_<field>` / `<field>_hint`，并附带 `cache_entries` 缓存条数）。 |
| `POST` | `/api/v1/plugins/{id}/instances` | 返回 **`201 Created`**。校验、加密（ChaCha20-Poly1305）并保存新服务商实例（`CreatePluginInstanceRequest`：`{ "name", "config", "enabled"?: true }`）。 |
| `PATCH` | `/api/v1/plugins/{id}/instances/{iid}` | 密钥保留式局部更新服务商实例（敏感字段省略或传 `""` 时保留原密钥）。 |
| `DELETE` | `/api/v1/plugins/{id}/instances/{iid}` | 删除服务商实例；若仍被路由 `options.plugins` 引用，返回 **`409 Conflict`** 并列出 `referencing_routes: [{ "id", "name" }]`。 |
| `POST` | `/api/v1/plugins/{id}/instances/{iid}/test` | 执行连通性测试（`link-rewrite` 经由 `ProtectedHttpClient` 实际缩短 `https://feedcourier.ellery.me/`；JS 处理器执行插件的 `test` 函数且 `scope: "instance"`）。返回 `{ "ok": true, "message": "..." }`。 |
| `DELETE` | `/api/v1/plugins/{id}/instances/{iid}/cache` | 清空服务商 `{iid}` 在 `plugin_link_cache` 中的全部短链缓存，返回 `{ "cleared_entries": N }`。 |
| `GET` | `/api/v1/plugins/market` | 后端通过防 SSRF 客户端拉取插件市场索引（6 小时内存缓存；回退编译内置快照时负缓存 TTL 缩短为 60 秒，拉取瞬时失败时不覆盖已有有效远端缓存；传 `?refresh=true` 强制刷新），返回 `{ "source": "remote" \| "snapshot", "index_url", "generated_at", "plugins": MarketPluginItemDto[] }` 并标注 `runtime`、`allowed_hosts`、`versions`、`latest`、`description_i18n`、`installed`、`installed_version`、`update_available` 与 `enabled`（`homepage`/`repository` 严格限制为 `http://` 或 `https://` URL）。 |

### 创建通道账号请求结构（`POST /api/v1/channels`）

```json
{
  "name": null,
  "kind": "telegram",
  "config": {
    "kind": "telegram",
    "bot_token": "123456789:AAH...",
    "api_base_url": null
  },
  "initial_destination": {
    "name": "Telegram 主频道",
    "config": {
      "kind": "telegram",
      "chat_id": "@my_tech_feed",
      "message_thread_id": null
    },
    "enabled": true
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
| `GET` | `/api/v1/routes` | 获取全部路由规则列表（`RouteDto[]`，含 `channel_id` 与 `destination_id`）。 |
| `POST` | `/api/v1/routes` | 返回 **`201 Created`**。创建路由规则（以 `destination_id` 为主；仅在目标账号恰好只有 1 个目的地时兼容旧版 `channel_id`；创建前编译校验 `include_regex` / `exclude_regex`、`options.timezone` 与 `active_windows` 合法性）。 |
| `PATCH` | `/api/v1/routes/{id}` | 更新路由规则（正则字段传 `null` 或 `""` 可清除；`options.timezone` 传 `""` 可恢复继承系统时区）。更换目标目的地时，会在同一事务内将 `queued` / `deferred` / `retrying` 在途任务重指向新目的地并清空 `checkpoint_json`、重置 `attempt_count = 0`（进行中的 `delivering` 任务不受影响）；修改 `active_windows`、`active_weekdays` 或 `timezone` 时会立即将 `queued` / `deferred` 任务的 `next_attempt_at` 重置为当前时间并唤醒发件箱。 |
| `DELETE` | `/api/v1/routes/{id}` | 删除指定路由规则。 |

### 创建路由请求示例（`POST /api/v1/routes`）

```json
{
  "name": "Rust 动态推至 Telegram",
  "feed_ids": [],
  "match_tags": ["rust"],
  "destination_id": 5,
  "include_keywords": [],
  "exclude_keywords": ["sponsored"],
  "include_regex": null,
  "exclude_regex": null,
  "options": {
    "telegram_mode": "rich_message",
    "link_preview": true,
    "locale": "zh-CN",
    "timezone": "Asia/Shanghai",
    "active_windows": [{ "start": "08:00", "end": "22:00" }],
    "active_weekdays": [1, 2, 3, 4, 5],
    "plugin_order": ["link-rewrite", "ai-summary"],
    "plugins": {
      "link-rewrite": { "provider_id": 1 },
      "ai-summary": {},
      "optional-tagger": { "enabled": false }
    }
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
| `timezone` | 可选的路由级 IANA 时区（如 `"Asia/Shanghai"`；省略或传 `""` 继承 `settings.timezone`），同时控制 `active_windows` / `active_weekdays`、SMTP 邮件文章时间与 Telegram 富文本 `<footer>` 时间（`YYYY-MM-DD HH:MM (UTC±HH:MM)`）。 |
| `active_windows` | **路由时区**（默认继承系统时区）下的每日投递窗口（`HH:MM`），支持夏令时（DST）安全边界计算。`end` 早于 `start` 表示跨午夜；仅 `00:00`–`00:00`（或空列表 `[]`）表示全天，其余 `start == end` 的非零等值窗口将返回 **`400 Bad Request`**。 |
| `active_weekdays` | `1`（周一）到 `7`（周日），空列表表示每天；跨午夜的窗口按开始那一天的星期判定。 |
| `plugin_order` | 可选的已启用处理器插件 ID 有序数组，用于指定 `on_render` 管线的执行顺序（最多 **64** 项）。在 `plugins` 中已配置但未列入 `plugin_order` 的插件会按 ID 字典序依次在后执行。 |
| `plugins` | 可选的路由级插件配置映射，按插件 ID 索引（如 `{"link-rewrite": {"provider_id": 1}}`）。支持对无参处理器插件传入空对象 `{}`，以及通过 `"enabled": false` 在单条路由上显式禁用某个插件。创建/更新路由时会校验处理器插件与服务商实例存在。 |

窗口或星期之外的任务**仍会入队**：状态保持 `queued` / `deferred`，`next_attempt_at` 被设为下一个窗口的开始时间。

---

## 五、发件箱任务、全局队列与死信重投（`/api/v1/tasks*`）

| 方法 | 路径 | 说明 |
| :---: | :--- | :--- |
| `GET` | `/api/v1/tasks` | 查询发件箱投递任务列表（`DeliveryTaskDto[]`，含 `destination_id`、`destination_name` 以及标识已送达任务是否存在非致命告警的 `has_warnings: bool`）。支持 Query 参数 `status`（`queued` / `delivering` / `retrying` / `deferred` / `dead_letter` / `delivered`）与 `limit`（默认 `100`，最大 `500`）。 |
| `POST` | `/api/v1/tasks/retry` | 批量重投失败任务：请求体传入 `{ "task_ids": [12, 15] }` 或 `{ "all_dead_letter": true }`，唤醒发件箱 Worker 并返回 `{ "requeued_count": 2 }`。 |
| `GET` | `/api/v1/tasks/queue` | 查询全局发件箱状态：`{ "paused": false, "resume_at": null }`。已自动恢复的暂停会返回 `paused: false`。 |
| `PUT` | `/api/v1/tasks/queue` | 请求体 `{ "action": "pause", "resume_in_secs"?: 3600, "resume_at"?: "2026-01-01T08:00:00Z" }` 或 `{ "action": "resume" }`，返回与 `GET` 相同的结构。暂停期间订阅源仍会轮询、任务持续累积；恢复后所有订阅源中到期的任务按 `COALESCE(published_at, discovered_at)` 从旧到新依次投递。 |

---

## 六、系统设置、MCP 令牌与在线备份维护

| 方法 | 路径 | 说明 |
| :---: | :--- | :--- |
| `GET` | `/api/v1/settings` | 获取当前生效的设置：`{ "outbound_proxy_url", "ssrf_allowlist", "timezone": "America/Los_Angeles", "locale", "public_url", "public_url_env_managed" }`。`timezone` 为 IANA 时区名；`public_url_env_managed: true` 表示由环境变量 `PUBLIC_URL` 锁定；`locale` 仅是未设置 `options.locale` 的旧路由兼容回退语言。 |
| `PUT` | `/api/v1/settings` | 局部更新 `timezone`、`outbound_proxy_url`、`ssrf_allowlist` 与 `public_url` 的任意子集并持久化至 `system_settings` 表（传 `null` 或 `""` 可清空 `public_url`，若由环境变量 `PUBLIC_URL` 管理则返回 `400`）；仅当请求中包含网络相关字段时才热重载运行时客户端。无效时区或非法 `public_url` 返回 `400`。 |
| `GET` | `/api/v1/mcp-tokens` | 获取已签发的 MCP Bearer Token 元数据列表（仅含 `id`、`name`、`token_prefix`、`scope`、`last_used_at`，不含明文）。 |
| `POST` | `/api/v1/mcp-tokens` | 返回 **`201 Created`**。签发新 MCP 令牌（请求体 `{ "name": "cursor", "scope": "readonly" \| "admin" }`），返回仅展示一次的 `plaintext_token` 与 `client_config_json`。 |
| `DELETE` | `/api/v1/mcp-tokens/{id}` | 立即吊销并删除指定 MCP 令牌。 |
| `POST` | `/api/v1/maintenance/gc` | 立即触发一次后台维护（执行 `NOT EXISTS` 正文回收、过期会话清理、90 天过期短链缓存清理并运行 `PRAGMA optimize`），返回 `{ "purged_entries": N }`。 |
| `GET` | `/api/v1/maintenance/backup` | 执行 SQLite `VACUUM INTO` 在线热备份，并以二进制流返回数据库快照附件。 |

---

## 七、公开 SMTP 合规端点（`/u/{token}`、`/v/{token}`）

挂载在根路径且注册于 SPA 回退路由之前（并对空路径 `/u`、`/u/`、`/v`、`/v/` 显式返回公开 `404` 而绝不穿透至 WebUI SPA），**无需登录**，请求体大小限制为 **`4 KiB`（`4,096` 字节）**，按 IP 实施内存限流（**`30` 次 / `60` 秒**，最大 **`8,192` 桶硬容量上限**；默认 `TRUST_PROXY=false` 仅取 `ConnectInfo<SocketAddr>` 对端 IP，开启 `TRUST_PROXY=true` 时取 `X-Real-IP` 或 `X-Forwarded-For` 最右侧一跳），统一返回 `X-Robots-Tag: noindex, nofollow`、严格 CSP、`Referrer-Policy: no-referrer` 与 `Cache-Control: no-store`：

| 方法 | 路径 | 行为说明 |
| :---: | :--- | :--- |
| `GET` | `/u/{token}` | **严格只读**的本地化退订确认页，仅展示目的地名称与打码邮箱（`a***e@example.com`）及相对路径 `action=""` 的 `POST` 表单按钮（兼容子路径 `PUBLIC_URL`）；无效或篡改的令牌返回 `404`。 |
| `POST` | `/u/{token}` | 幂等执行退订并标记 `unsubscribed`（请求体含 `List-Unsubscribe=One-Click` 时记为 `revoked_method: "one_click"`，否则记为 `"link"`）；若该目的地下全部收件人均转为 `unsubscribed` 或 `bounced`，自动将目的地置为 `enabled = false, auto_paused = true`。兼容 RFC 8058 一键 POST。 |
| `GET` | `/v/{token}` | **严格只读**的双重确认订阅页，展示打码邮箱与相对路径 `action=""` 的 `POST` 确认按钮（防止安全网关预抓取误确认）；篡改令牌返回 `404`，过期、输错 5 次锁定或被新验证码取代返回 `410 Gone`。 |
| `POST` | `/v/{token}` | 将收件人标记为 `verified`（`consent_method: "link"`），若目的地处于 `auto_paused = true` 则自动恢复 `enabled = true, auto_paused = false`，并立即唤醒该目的地下处于 `deferred` 的投递任务；已过期、锁定或被取代时返回 `410 Gone`。 |
