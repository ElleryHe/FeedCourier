---
description: "FeedCourier REST API 接口参考（/api/v1/*）：涵盖会话认证、概览统计、订阅源与 OPML 导入导出、投递通道、路由规则、任务队列与在线备份。"
---

# REST API 接口参考（`/api/v1/*`）

FeedCourier 的内嵌 React 19 管理面板通过挂载于 `/api/v1/*` 的 JSON REST API 与后端通信（路由与处理器源码位于 `src/api/mod.rs`、`src/api/auth.rs`、`src/api/handlers.rs` 与 `src/api/dto.rs`）。运维脚本或外部程序也可直接通过 `curl` 调用这些接口。

---

## 鉴权机制与统一错误格式

### 1. Argon2id 口令与 `fc_session` `HttpOnly` Cookie

除公开探针与登录引导端点（`/api/v1/health`、`/api/v1/auth/*`）外，所有 `/api/v1/*` 端点均受 `require_session` 中间件保护：
- 调用 `POST /api/v1/auth/setup`（首次初始化）或 `POST /api/v1/auth/login` 成功后，服务端通过 `Set-Cookie` 响应头下发名为 **`fc_session`** 的会话凭证：
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

### 3. 首次初始化管理员口令
- **路径**：`POST /api/v1/auth/setup`
- **请求体**：`{ "password": "your-strong-password" }`（要求长度 $\ge 8$ 个字符；若已设置过口令则返回 `409 Conflict`）
- **响应**：`{ "ok": true }` 并设置 `fc_session` Cookie。

### 4. 管理员登录与登出
- **登录**：`POST /api/v1/auth/login`，请求体 `{ "password": "..." }`，校验 Argon2id 哈希通过后返回 `{ "ok": true }` 并下发 `fc_session`。
- **登出**：`POST /api/v1/auth/logout`，从数据库删除当前会话哈希并清除 Cookie。
- **重置密码说明**：管理员口令可通过启动时注入环境变量 `ADMIN_PASSWORD` 覆盖更新 `system_settings` 表中的 Argon2id 哈希。

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

---

## 二、订阅源与 OPML 导入导出（`/api/v1/feeds*`）

| 方法 | 路径 | 说明 |
| :---: | :--- | :--- |
| `GET` | `/api/v1/feeds` | 获取全部订阅源列表（`FeedDto[]`）。 |
| `POST` | `/api/v1/feeds` | 新增订阅源并立即触发首次拉取（支持 `silent_init` 与 `push_latest_one`；若初始拉取触发 `SsrfBlocked` 则自动回滚删除该源）。 |
| `POST` | `/api/v1/feeds/preview` | 在不写入数据库的情况下经防 SSRF 客户端试抓指定 URL 最新 10 篇条目，支持试跑 Readability 与过滤规则。 |
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

---

## 三、授信投递通道与 Notion 向导（`/api/v1/channels*`）

| 方法 | 路径 | 说明 |
| :---: | :--- | :--- |
| `GET` | `/api/v1/channels` | 获取全部授信通道列表（敏感密码/Token 自动掩码脱敏为 `has_password` / `bot_token_hint` / `secret_hint`，详见[安全与防注入机制](/zh/reference/security)）。 |
| `POST` | `/api/v1/channels` | 创建新通道（`smtp` / `telegram` / `notion`）；Telegram 通道创建时自动执行 `probe_capabilities` 探测。 |
| `PATCH` | `/api/v1/channels/{id}` | 更新通道配置、步进间隔或配额（**密钥保留语义**：当 `password` / `bot_token` / `integration_secret` 省略或留空时，后端自动保留原加密密钥不变；将 `hourly_quota` 或 `daily_quota` 显式传 `null` 可清空配额限制）。 |
| `DELETE` | `/api/v1/channels/{id}` | 删除指定通道（级联删除绑定该通道的路由与任务）。 |
| `POST` | `/api/v1/channels/{id}/test` | 向该通道发送实时连通性测试邮件/消息/页面（Telegram 同时刷新 `capabilities_json`）。 |
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
    "link_preview": true
  },
  "enabled": true
}
```

---

## 五、发件箱任务与死信重投（`/api/v1/tasks*`）

| 方法 | 路径 | 说明 |
| :---: | :--- | :--- |
| `GET` | `/api/v1/tasks` | 查询发件箱投递任务列表。支持 Query 参数 `status`（`queued` / `delivering` / `retrying` / `deferred` / `dead_letter` / `delivered`）与 `limit`（默认 `100`，最大 `500`）。 |
| `POST` | `/api/v1/tasks/retry` | 批量重投失败任务：请求体传入 `{ "task_ids": [12, 15] }` 或 `{ "all_dead_letter": true }`，唤醒发件箱 Worker 并返回 `{ "requeued_count": 2 }`。 |

---

## 六、网络设置、MCP 令牌与在线备份维护

| 方法 | 路径 | 说明 |
| :---: | :--- | :--- |
| `GET` | `/api/v1/settings` | 获取当前生效的全局出站代理（`outbound_proxy_url`）、内网 SSRF 白名单（`ssrf_allowlist`）及推送文案语言（`locale`）。 |
| `PUT` | `/api/v1/settings` | 修改并热重载 `outbound_proxy_url`、`ssrf_allowlist` 或 `locale`（`"en"` \| `"zh-CN"`），支持局部更新并持久化至 `system_settings` 表。 |
| `GET` | `/api/v1/mcp-tokens` | 获取已签发的 MCP Bearer Token 元数据列表（仅含 `id`、`name`、`token_prefix`、`scope`、`last_used_at`，不含明文）。 |
| `POST` | `/api/v1/mcp-tokens` | 签发新 MCP 令牌（请求体 `{ "name": "cursor", "scope": "readonly" \| "admin" }`），返回仅展示一次的 `plaintext_token` 与 `client_config_json`。 |
| `DELETE` | `/api/v1/mcp-tokens/{id}` | 立即吊销并删除指定 MCP 令牌。 |
| `POST` | `/api/v1/maintenance/gc` | 立即触发一次后台维护（执行 `NOT EXISTS` 正文回收、清理过期会话并运行 `PRAGMA optimize`），返回 `{ "purged_entries": N }`。 |
| `GET` | `/api/v1/maintenance/backup` | 执行 SQLite `VACUUM INTO` 在线热备份，并以二进制流返回数据库快照附件。 |
