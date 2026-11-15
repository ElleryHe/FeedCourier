# SQLite 数据库表结构参考

FeedCourier 的全部持久化状态存储于单文件 SQLite WAL 数据库（`<DATA_DIR>/feedcourier.db`）中，编译期迁移脚本位于仓库 `migrations/0001_init.sql`。所有时间戳字段均采用 **UTC Unix 纪元秒（`INTEGER`，`unixepoch()`）** 存储，所有外键关联均启用 **`ON DELETE CASCADE`** 级联清理。

---

## 实体关系概览图（ER Diagram）

```mermaid
erDiagram
    feeds ||--o{ entries : "1 : N (ON DELETE CASCADE)"
    channels ||--o{ routes : "1 : N (ON DELETE CASCADE)"
    routes ||--o{ delivery_tasks : "1 : N (ON DELETE CASCADE)"
    channels ||--o{ delivery_tasks : "1 : N (ON DELETE CASCADE)"
    entries ||--o{ delivery_tasks : "1 : N (ON DELETE CASCADE)"

    feeds {
        INTEGER id PK
        TEXT url UK
        TEXT title
        TEXT tags_json
        INTEGER poll_interval_secs
        INTEGER fetch_full_content
        INTEGER use_proxy
        TEXT health_status
        INTEGER consecutive_failures
        INTEGER next_poll_at
        INTEGER silent_init
    }

    entries {
        INTEGER id PK
        INTEGER feed_id FK
        TEXT fingerprint_sha256
        INTEGER discovered_at
        INTEGER payload_purged_at
        TEXT title
        TEXT content_html
        TEXT media_json
    }

    channels {
        INTEGER id PK
        TEXT name
        TEXT kind
        BLOB encrypted_config
        TEXT credential_group_key
        TEXT capabilities_json
        INTEGER min_interval_secs
        INTEGER jitter_max_secs
        INTEGER hourly_quota
        INTEGER daily_quota
        INTEGER cooldown_until
    }

    routes {
        INTEGER id PK
        TEXT name
        TEXT feed_ids_json
        TEXT match_tags_json
        INTEGER channel_id FK
        TEXT include_keywords_json
        TEXT exclude_keywords_json
        TEXT options_json
    }

    delivery_tasks {
        INTEGER id PK
        INTEGER route_id FK
        INTEGER channel_id FK
        INTEGER entry_id FK
        TEXT status
        INTEGER lease_until
        TEXT checkpoint_json
        INTEGER attempt_count
        INTEGER next_attempt_at
    }
```

---

## 1. `feeds`（订阅源状态与抓取策略表）

记录每一个 RSS / Atom / JSON Feed 订阅源的抓取配置、HTTP 条件请求缓存头以及指数退避/熔断健康状态。

| 字段名 | 类型 | 约束与默认值 | 说明 |
| :--- | :--- | :--- | :--- |
| `id` | `INTEGER` | `PRIMARY KEY AUTOINCREMENT NOT NULL` | 订阅源主键 ID。 |
| `url` | `TEXT` | `NOT NULL UNIQUE` | 规范化后的订阅源 `http(s)` URL（全局唯一）。 |
| `title` | `TEXT` | `NOT NULL DEFAULT ''` | 订阅源名称（留空时由首次抓取自动填充）。 |
| `site_url` | `TEXT` | `NULL` | 订阅源对应的源站主页链接。 |
| `favicon_url` | `TEXT` | `NULL` | 自动探测到的源站图标 URL（用于 WebUI 展示与 Notion 页面图标）。 |
| `tags_json` | `TEXT` | `NOT NULL DEFAULT '[]'` | 自定义字符串标签数组 JSON（如 `["rust","tech"]`），联动路由匹配与 Notion `Tag` 多选列。 |
| `poll_interval_secs` | `INTEGER` | `NOT NULL DEFAULT 1800 CHECK (> 0)` | 基础轮询间隔秒数（默认 30 分钟）。 |
| `fetch_full_content` | `INTEGER` | `NOT NULL DEFAULT 0 CHECK (IN (0, 1))` | 是否按需启用 `dom_smoothie` Readability 网页全文提取。 |
| `use_proxy` | `INTEGER` | `NOT NULL DEFAULT 0 CHECK (IN (0, 1))` | 是否经由全局出站代理拉取该源及其全文网页。 |
| `etag` | `TEXT` | `NULL` | 上次成功拉取缓存的 HTTP `ETag` 响应头（用于发送 `If-None-Match`）。 |
| `last_modified` | `TEXT` | `NULL` | 上次成功拉取缓存的 HTTP `Last-Modified` 响应头（用于发送 `If-Modified-Since`）。 |
| `health_status` | `TEXT` | `NOT NULL DEFAULT 'active'` | 健康状态枚举：`'active'`（正常）、`'backoff'`（指数退避）、`'circuit_open'`（连续失败 $\ge 15$ 次熔断暂停）、`'paused'`（人工暂停）。 |
| `consecutive_failures` | `INTEGER` | `NOT NULL DEFAULT 0 CHECK (>= 0)` | 连续抓取失败次数（成功拉取或 `304 Not Modified` 时归零）。 |
| `next_poll_at` | `INTEGER` | `NULL` | 下次允许调度轮询的 Unix 时间戳（`'circuit_open'` 或 `'paused'` 时为 `NULL`）。 |
| `last_error` | `TEXT` | `NULL` | 最近一次拉取或解析失败的错误诊断信息。 |
| `silent_init` | `INTEGER` | `NOT NULL DEFAULT 1 CHECK (IN (0, 1))` | 首次同步静默标记（首次成功拉取吸收存量指纹后自动置为 `0`）。 |
| `created_at` | `INTEGER` | `NOT NULL DEFAULT (unixepoch())` | 创建时间戳。 |
| `updated_at` | `INTEGER` | `NOT NULL DEFAULT (unixepoch())` | 最后更新时间戳。 |

- **核心索引**：`idx_feeds_poll_schedule ON feeds (health_status, next_poll_at)` —— 加速后台轮询协程按到期时间扫描 `'active'` 与 `'backoff'` 状态的订阅源。

---

## 2. `entries`（永久去重指纹与阅后即焚载荷表）

永久保存每篇文章的 64 位 SHA-256 指纹以防重复投递；正文与媒体大字段在投递闭环并渡过 `RETENTION_HOURS` 窗口后由 `NOT EXISTS` GC 自动置为 `NULL`。

| 字段名 | 类型 | 生命周期 | 约束与默认值 | 说明 |
| :--- | :--- | :---: | :--- | :--- |
| `id` | `INTEGER` | **永久** | `PRIMARY KEY AUTOINCREMENT NOT NULL` | 条目主键 ID。 |
| `feed_id` | `INTEGER` | **永久** | `NOT NULL REFERENCES feeds(id) ON DELETE CASCADE` | 所属订阅源 ID。 |
| `fingerprint_sha256` | `TEXT` | **永久** | `NOT NULL CHECK (length = 64)` | 四级防漂移算法生成的 64 位十六进制 SHA-256 指纹。与 `feed_id` 组成 `UNIQUE (feed_id, fingerprint_sha256)`。 |
| `published_at` | `INTEGER` | **永久** | `NULL` | 源报文显式声明的原始发布时间戳。 |
| `discovered_at` | `INTEGER` | **永久** | `NOT NULL DEFAULT (unixepoch())` | 首次拉取发现该条目的时间戳（作为 GC 保留窗口起算点）。 |
| `payload_purged_at` | `INTEGER` | **永久** | `NULL` | 正文大字段被 GC 回收的时间戳（未回收时为 `NULL`）。 |
| `title` | `TEXT` | *阅后即焚* | `NULL` | 清洗后的纯文本文章标题。 |
| `canonical_url` | `TEXT` | *阅后即焚* | `NULL` | 剥离营销追踪参数并补全相对路径后的规范化原文链接。 |
| `author` | `TEXT` | *阅后即焚* | `NULL` | 文章作者名称。 |
| `content_html` | `TEXT` | *阅后即焚* | `NULL` | 经 `ammonia` 白名单清洗并补全绝对链接后的安全 HTML 正文。 |
| `first_sentence` | `TEXT` | *阅后即焚* | `NULL` | 按 Unicode 句子边界提取的正文首句（服务 Telegram `media_mode` 三行文案）。 |
| `summary_text` | `TEXT` | *阅后即焚* | `NULL` | 精炼纯文本摘要（服务 Telegram `compact_card` 与 MCP 预览）。 |
| `media_json` | `TEXT` | *阅后即焚* | `NULL` | 媒体资源数组 JSON（汇总 `<enclosure>`、`media:content` 与正文图文音视频）。 |

- **核心索引**：
  - `UNIQUE (feed_id, fingerprint_sha256)` —— 幂等插入冲突检测（`INSERT INTO entries ... ON CONFLICT DO NOTHING`）。
  - `idx_entries_feed_discovered ON entries (feed_id, discovered_at DESC)` —— 按源检索最新条目。
  - `idx_entries_gc_candidates ON entries (discovered_at) WHERE payload_purged_at IS NULL` —— **部分索引（Partial Index）**，使后台 GC 仅扫描尚未回收正文的活跃行，即使累积百万条历史指纹也保持零扫描开销。

---

## 3. `channels`（授信投递通道与流控护栏表）

保存由管理员在 WebUI 中独占配置的下游通道加密凭证、能力探测快照以及步进流控/配额参数。

| 字段名 | 类型 | 约束与默认值 | 说明 |
| :--- | :--- | :--- | :--- |
| `id` | `INTEGER` | `PRIMARY KEY AUTOINCREMENT NOT NULL` | 授信通道主键 ID（MCP 仅通过此整数 ID 引用通道）。 |
| `name` | `TEXT` | `NOT NULL` | 通道备注名称（如 `"主力通知电报群"`）。 |
| `kind` | `TEXT` | `NOT NULL CHECK (IN ('smtp', 'telegram', 'notion'))` | 通道协议类型。 |
| `encrypted_config` | `BLOB` | `NOT NULL` | 使用 `MasterKey` 经 ChaCha20-Poly1305 加密后的 `ChannelConfig` JSON 密文。 |
| `credential_group_key` | `TEXT` | `NOT NULL` | 基于底层发信身份计算的 SHA-256 分组标识，使同账号/同 Token/同 Secret 通道自动共享步进与冷却锁。 |
| `capabilities_json` | `TEXT` | `NOT NULL DEFAULT '{}'` | 探测到的平台能力快照（如 Telegram `supports_rich_message`、`bot_api_version`、`fallback_reason`）。 |
| `min_interval_secs` | `INTEGER` | `NOT NULL DEFAULT 15 CHECK (>= 0)` | 最小发信间隔秒数。 |
| `jitter_max_secs` | `INTEGER` | `NOT NULL DEFAULT 10 CHECK (>= 0)` | 随机抖动上限秒数。 |
| `hourly_quota` | `INTEGER` | `NULL CHECK (IS NULL OR > 0)` | 可选的滚动每小时最大投递篇数上限。 |
| `daily_quota` | `INTEGER` | `NULL CHECK (IS NULL OR > 0)` | 可选的滚动每日最大投递篇数上限。 |
| `use_proxy` | `INTEGER` | `NOT NULL DEFAULT 0 CHECK (IN (0, 1))` | 该通道出站请求是否经由全局代理转发。 |
| `cooldown_until` | `INTEGER` | `NULL` | 触发下游 `429` / `retry_after` 时的通道冷却截止时间戳。 |
| `enabled` | `INTEGER` | `NOT NULL DEFAULT 1 CHECK (IN (0, 1))` | 通道启用开关。 |
| `created_at` / `updated_at` | `INTEGER` | `NOT NULL DEFAULT (unixepoch())` | 创建与更新时间戳。 |

- **核心索引**：`idx_channels_credential_group ON channels (credential_group_key)` —— 支持按共享凭据组批量设置冷却时间与查询最近发信时间。

---

## 4. `routes`（路由编排与展现模式规则表）

定义“哪些订阅源或标签”在通过“哪些关键词/正则过滤规则”后，以何种展现模式投递至哪个授信 `channel_id`。

| 字段名 | 类型 | 约束与默认值 | 说明 |
| :--- | :--- | :--- | :--- |
| `id` | `INTEGER` | `PRIMARY KEY AUTOINCREMENT NOT NULL` | 路由主键 ID。 |
| `name` | `TEXT` | `NOT NULL` | 路由规则名称。 |
| `feed_ids_json` | `TEXT` | `NOT NULL DEFAULT '[]'` | 显式匹配的 `feed_id` 整数数组 JSON（与 `match_tags_json` 均为空时匹配所有订阅源）。 |
| `match_tags_json` | `TEXT` | `NOT NULL DEFAULT '[]'` | 按标签匹配的标签字符串数组 JSON。 |
| `channel_id` | `INTEGER` | `NOT NULL REFERENCES channels(id) ON DELETE CASCADE` | 目标授信通道 ID。 |
| `include_keywords_json` | `TEXT` | `NOT NULL DEFAULT '[]'` | 包含关键词数组 JSON（非空时标题或正文至少命中其一）。 |
| `exclude_keywords_json` | `TEXT` | `NOT NULL DEFAULT '[]'` | 排除关键词数组 JSON（命中任意一词即拦截）。 |
| `include_regex` | `TEXT` | `NULL` | 包含正则表达式（非空时必须匹配）。 |
| `exclude_regex` | `TEXT` | `NULL` | 排除正则表达式（匹配即拦截）。 |
| `options_json` | `TEXT` | `NOT NULL DEFAULT '{}'` | 通道展现选项 JSON（`RouteOptions`：`telegram_mode` 与 `link_preview`）。 |
| `enabled` | `INTEGER` | `NOT NULL DEFAULT 1 CHECK (IN (0, 1))` | 路由启用开关。 |
| `created_at` / `updated_at` | `INTEGER` | `NOT NULL DEFAULT (unixepoch())` | 创建与更新时间戳。 |

---

## 5. `delivery_tasks`（单篇幂等事务发件箱与审计日志表）

承载 `1 Task = 1 Entry × 1 Route` 的持久化发件箱状态机、租约截止时间（`lease_until`）与跨崩溃断点（`checkpoint_json`）。

| 字段名 | 类型 | 约束与默认值 | 说明 |
| :--- | :--- | :--- | :--- |
| `id` | `INTEGER` | `PRIMARY KEY AUTOINCREMENT NOT NULL` | 投递任务主键 ID。 |
| `route_id` | `INTEGER` | `NOT NULL REFERENCES routes(id) ON DELETE CASCADE` | 关联的路由 ID。 |
| `channel_id` | `INTEGER` | `NOT NULL REFERENCES channels(id) ON DELETE CASCADE` | 目标通道 ID。 |
| `entry_id` | `INTEGER` | `NOT NULL REFERENCES entries(id) ON DELETE CASCADE` | 关联的条目 ID。受 `UNIQUE (route_id, entry_id)` 唯一约束保护。 |
| `status` | `TEXT` | `NOT NULL DEFAULT 'queued'` | 状态机枚举：`'queued'`、`'delivering'`、`'delivered'`、`'retrying'`、`'deferred'`、`'dead_letter'`。 |
| `lease_until` | `INTEGER` | `NULL` | 当 `status = 'delivering'` 时的 Worker 租约过期时间戳（过期即允许被自动重新接管）。 |
| `checkpoint_json` | `TEXT` | `NULL` | `DeliveryCheckpoint` 序列化 JSON（保存 SMTP `message_id`、Telegram `next_batch_offset` 或 Notion `create_initiated` / `async_task_id` / `page_id`）。 |
| `attempt_count` | `INTEGER` | `NOT NULL DEFAULT 0 CHECK (>= 0)` | 已尝试发送次数（默认达到 6 次临时错误后转入 `'dead_letter'`）。 |
| `next_attempt_at` | `INTEGER` | `NOT NULL DEFAULT (unixepoch())` | 下次可被调度领取的时间戳。 |
| `last_status_code` | `INTEGER` | `NULL` | 下游最近一次返回的 HTTP 状态码或 SMTP 响应码。 |
| `diagnostic_log` | `TEXT` | `NULL` | 最近一次投递结果或错误诊断摘要。 |
| `completed_at` | `INTEGER` | `NULL` | 任务进入 `'delivered'` 或 `'dead_letter'` 终态的时间戳。 |
| `created_at` | `INTEGER` | `NOT NULL DEFAULT (unixepoch())` | 任务入队时间戳。 |

- **核心复合索引**：
  - `idx_delivery_tasks_status_next_attempt ON delivery_tasks (status, next_attempt_at)` —— 支撑 Worker 毫秒级领取到期任务。
  - `idx_delivery_tasks_status_lease ON delivery_tasks (status, lease_until)` —— 支撑过期租约任务的自动发现与回收。
  - `idx_delivery_tasks_entry_status ON delivery_tasks (entry_id, status)` —— 支撑 `entries` 正文 GC 的 `NOT EXISTS` 子查询。
  - `idx_delivery_tasks_channel_status_completed ON delivery_tasks (channel_id, status, completed_at)` —— 支撑滚动小时/每日配额（`hourly_quota` / `daily_quota`）水位统计。

---

## 6. 系统设置、MCP 令牌与会话表（`system_settings`、`mcp_tokens`、`sessions`）

### `system_settings`（系统配置键值表，亦称 `settings`）

| 字段名 | 类型 | 约束与默认值 | 说明 |
| :--- | :--- | :--- | :--- |
| `key` | `TEXT` | `PRIMARY KEY NOT NULL` | 配置项键名：<br>• `'admin_password_hash'`：Argon2id PHC 管理员密码哈希<br>• `'outbound_proxy_url'`：全局出站代理地址<br>• `'ssrf_allowlist'`：内网 SSRF 白名单字符串数组 |
| `value_json` | `TEXT` | `NOT NULL` | JSON 序列化后的配置值。 |
| `updated_at` | `INTEGER` | `NOT NULL DEFAULT (unixepoch())` | 最后修改时间戳。 |

### `mcp_tokens`（双级 MCP 鉴权令牌表）

| 字段名 | 类型 | 约束与默认值 | 说明 |
| :--- | :--- | :--- | :--- |
| `id` | `INTEGER` | `PRIMARY KEY AUTOINCREMENT NOT NULL` | 令牌主键 ID。 |
| `name` | `TEXT` | `NOT NULL` | 令牌备注名称（如 `"Cursor Desktop"`）。 |
| `token_hash` | `TEXT` | `NOT NULL UNIQUE` | `fc_mcp_...` 明文令牌的 64 位 SHA-256 十六进制哈希（数据库不存明文）。 |
| `token_prefix` | `TEXT` | `NOT NULL` | 令牌前 15 个字符前缀（用于 WebUI 列表辨识）。 |
| `scope` | `TEXT` | `NOT NULL CHECK (IN ('readonly', 'admin'))` | 权限范围枚举：`'readonly'` 或 `'admin'`。 |
| `last_used_at` | `INTEGER` | `NULL` | 最近一次通过 `/mcp` 鉴权调用的时间戳。 |
| `created_at` | `INTEGER` | `NOT NULL DEFAULT (unixepoch())` | 签发时间戳。 |

### `sessions`（WebUI 管理员登录会话表）

| 字段名 | 类型 | 约束与默认值 | 说明 |
| :--- | :--- | :--- | :--- |
| `token_hash` | `TEXT` | `PRIMARY KEY NOT NULL` | `fc_session` Cookie 随机令牌的 SHA-256 哈希。 |
| `expires_at` | `INTEGER` | `NOT NULL` | 会话过期时间戳（默认签发后 30 天，由后台维护协程定期清理过期行）。 |
| `created_at` | `INTEGER` | `NOT NULL DEFAULT (unixepoch())` | 登录时间戳。 |
