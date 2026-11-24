---
description: "FeedCourier 插件协议 v1（Protocol v1）开发手册：HTTP 桥接协议规范、HMAC-SHA256 验签、channel-template 零依赖模板、manifest.schema.json 与插件市场提交指南。"
---

# 插件开发手册（Protocol v1）

FeedCourier 定义了一套稳定的 **插件协议 v1（`api_version: "1"`）**，由第一方内置 Rust 插件、进程外 **HTTP 桥接插件**（`v0.3.1+`）以及官方插件市场仓库（[`ElleryHe/FeedCourier-Plugins`](https://github.com/ElleryHe/FeedCourier-Plugins)）共同遵循。

---

## 1. HTTP 桥接协议 v1（`schema/bridge-protocol.schema.json`）

进程外桥接插件是独立的 HTTP(S) 服务（通常作为 Docker Compose sidecar 部署，支持任意编程语言：Rust、Go、Python、Node.js 等）。

### 请求鉴权（HMAC-SHA256）

FeedCourier 发出的每个请求都附带以下请求头：

| 请求头 | 说明 |
| :--- | :--- |
| `X-FeedCourier-Timestamp` | 签名时的 Unix 秒级时间戳。 |
| `X-FeedCourier-Signature` | `v1=<hex(HMAC-SHA256(BRIDGE_SECRET, "{timestamp}.{raw_body}"))>` |
| `X-FeedCourier-Plugin-Id` | 已登记的插件 ID（首次拉取 `GET /manifest` 时为 `"pending"`）。 |
| `X-FeedCourier-Idempotency-Key` | `/deliver`、`/hooks/on_render` 与 `/hooks/on_delivered` 附带的确定性 SHA-256 幂等键。 |

桥接服务端**必须**使用恒定时间比较（constant-time compare）校验签名，并拒绝时间戳超出 **`±300s`** 重放窗口的请求。

### 端点与边界限制

| 端点 | 适用类型 | 超时 | 职责 |
| :--- | :---: | :---: | :--- |
| `GET /manifest` | 全部 | `10s` | 返回运行时 `PluginManifest` JSON（`id`、`name`、`version`、`api_version: "1"`、`kind: "channel" \| "processor"`、`min_core`、`hooks`、Schema 与 `capabilities`）。 |
| `POST /validate` | 全部 | `10s` | 校验 `{ scope: "account" \| "destination" \| "instance", config }`；返回 `{ ok: true, normalized_config? }` 或 `400 { ok: false, error }`。 |
| `POST /test` | 全部 | `10s` | 对 `{ scope, account?, destination?, config? }` 执行连通性测试，返回 `{ ok: true, message }`。 |
| `POST /deliver` | `channel` | `30s` | 向自定义通道投递已渲染的文章。 |
| `POST /hooks/on_render` | `processor` | `10s` | 通道渲染前改写 `content_html` / `primary_link`（单插件总预算 15s）。 |
| `POST /hooks/on_delivered` | 声明了 `on_delivered` 的插件 | `10s` | 接收持久化、至少一次的投递完成通知。 |

**核心安全与资源边界**：
- 所有端点响应体上限 **`1 MiB`**。
- 严格禁止 HTTP 重定向（`redirect::Policy::none()`），且桥接请求永不经过 `OUTBOUND_PROXY`。
- `POST /hooks/on_render` 返回的 HTML **强制再次经 `ammonia` 清洗**后才送入下游通道。
- `POST /deliver` 返回的 `checkpoint` 序列化体积上限为 **`16 KiB`**。

### `POST /deliver` 契约（`kind: "channel"`）

**请求体**：

```json
{
  "idempotency_key": "<sha256>",
  "task_id": 42,
  "attempt": 1,
  "account": { "server_url": "https://ntfy.sh", "access_token": "tk_..." },
  "destination": { "topic": "feedcourier-alerts", "priority": "default" },
  "entry": {
    "title": "文章标题",
    "url": "https://example.com/post",
    "summary_text": "精简摘要",
    "content_html": "<p>已清洗并经 on_render 处理的 HTML</p>",
    "plain_text": "纯文本回退正文",
    "author": "Alice",
    "published_at": 1732492800,
    "media": []
  },
  "feed": { "id": 1, "title": "Blog", "url": "https://example.com/rss.xml" },
  "route": { "id": 3, "name": "Alerts", "locale": "zh-CN", "timezone": "Asia/Shanghai" },
  "checkpoint": null
}
```

**响应体（`200 OK`）**：

```json
{
  "outcome": "delivered",
  "external_id": "msg-123",
  "checkpoint": { "cursor": "optional-state" },
  "diagnostic": "可选诊断说明"
}
```

- `outcome`：`"delivered"` | `"retry"`（可选 `retry_after_secs`）| `"permanent_failure"` | `"deferred"`（可选 `next_window_at` 或 `retry_after_secs`）。
- 非 2xx HTTP 状态码映射：**`429`** → `retry`（读取 `Retry-After` 并触发凭据组冷却）；**`5xx`** 与网络/超时错误 → `retry`；其他 **`4xx`** → `permanent_failure`。

### `POST /hooks/on_delivered` 契约

当任务投递成功时，FeedCourier 在标记 `delivery_tasks.status = 'delivered'` 的**同一 SQLite 事务**内，为该路由与通道上所有订阅了 `on_delivered` 且已启用的桥接插件写入 `plugin_events`。独立调度循环携带 `X-FeedCourier-Idempotency-Key` 异步投递，失败按指数退避重试（`15s, 30s, 60s, 120s`，最多 **5 次**），绝不影响原任务的已投递状态或通道配额。

---

## 2. 零依赖模板与参考实现（`FeedCourier-Plugins`）

可直接基于 **[`ElleryHe/FeedCourier-Plugins`](https://github.com/ElleryHe/FeedCourier-Plugins)** 快速开发自定义通道插件：

- **`channels/channel-template/`**：零外部依赖的 **Node.js 24** 模板（仅用 `node:http` 与 `node:crypto`），内置 HMAC-SHA256 验签、4 个标准端点、`mapUpstreamStatusToOutcome()`、`Dockerfile` 与 `node:test` 单元测试。
- **`channels/ntfy/`**：面向 `ntfy.sh` 与自托管 ntfy 的完整参考通道插件。

```bash
cd FeedCourier-Plugins/channels/channel-template
node --test server.test.mjs
BRIDGE_SECRET="dev-secret" PORT=8787 node server.mjs
```

---

## 3. 市场清单规范（`schema/manifest.schema.json`）

市场中的插件以 `registry/<id>.json` 声明（JSON Schema Draft 2020-12）：

| 字段 | 类型 | 说明 |
| :--- | :--- | :--- |
| `id`、`name`、`version`、`api_version` | `string` | 唯一短横线 ID（须与文件名一致）、展示名称、SemVer 版本号、固定 `"1"`。 |
| `kind` | `string` | `"builtin"` \| `"processor"` \| `"channel"` \| `"external"` \| `"coming_soon"`。 |
| `runtime` | `string` | 可选 `"builtin"` \| `"bridge"`。 |
| `min_core` | `string` | 最低兼容的 FeedCourier 核心版本（安装前由后端与 WebUI 校验）。 |
| `category`、`icon`、`description` | `string` | 市场分类筛选与卡片简介。 |
| `homepage`、`repository` | `string` | 严格 `https://` 文档与代码仓库地址。 |
| `image`、`compose_snippet` | `string` | 桥接插件可选容器镜像与 Docker Compose 片段。 |
| `hooks` | `string[]` | `"on_render"`、`"on_ingest"`、`"on_delivered"`（`kind: "channel"` 可为空数组 `[]`）。 |
| `permissions` | `string[]` | 权限声明（`["network"]`）。 |
| `account_schema`、`destination_schema`、`capabilities` | `object` | 驱动 WebUI 动态表单的 JSON Schema（`format: "password"` 或 `x-secret: true` 字段自动加密与脱敏）。 |

### 向 `ElleryHe/FeedCourier-Plugins` 提交上架

1. Fork `ElleryHe/FeedCourier-Plugins`，新增 `registry/<id>.json`（可选附带 `channels/<id>/` 源码与单元测试）。
2. 使用 Node.js 24+ 在本地校验：
   ```bash
   node scripts/build-index.mjs --check
   node --test channels/*/*.test.mjs
   ```
3. **请勿在 PR 中手动修改 `index.json`。** PR 阶段由 GitHub Actions 自动校验，合入 `main` 后自动构建并提交 `index.json`。
