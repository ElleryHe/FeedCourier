---
description: "FeedCourier Plugin API v1（plugin.js）开发手册：单文件 ES 模块导出契约、globalThis.fc 宿主工具函数、boa_engine 沙箱边界、node --test 单元测试与插件市场提交指南。"
---

# 插件开发手册（Plugin API v1 / `plugin.js`）

FeedCourier 定义了一套稳定的 **Plugin API v1（`api_version: "1"`）**，由第一方内置 Rust 插件、进程内沙箱 **JS 插件**（`runtime: "js"`，`v0.3.1+` 基于纯 Rust `boa_engine`）以及官方插件市场仓库（[`ElleryHe/FeedCourier-Plugins`](https://github.com/ElleryHe/FeedCourier-Plugins)）共同遵循。

每个 JS 插件都是一个**零外部运行时依赖的单文件 ES 模块（`plugin.js`）**，管理员既可在 WebUI 插件市场一键安装，也可直接上传自定义 `.js` 文件。

---

## 1. 单文件 `plugin.js` 导出契约

一个标准 `plugin.js` 需导出静态 `meta` 元数据对象，以及与其 `kind` 和 `hooks` 对应的处理函数：

```js
export const meta = {
  id: "my-channel",
  name: "My Custom Channel",
  version: "0.1.0",
  api_version: "1",
  kind: "channel", // "channel" | "processor"
  runtime: "js",
  min_core: "0.3.1",
  category: "delivery",
  icon: "bell",
  description: "Delivers FeedCourier articles to My Custom Channel.",
  description_i18n: {
    "zh-CN": "将 FeedCourier 文章推送至自定义通道。"
  },
  homepage: "https://feedcourier.ellery.me/plugins/js-plugins",
  hooks: [], // 例如 ["on_render"] 或 ["on_delivered"]
  permissions: ["network"],
  allowed_hosts: ["api.example.com"],
  account_schema: {
    type: "object",
    required: ["server_url"],
    properties: {
      server_url: {
        type: "string",
        format: "uri",
        title: "Server URL",
        default: "https://api.example.com"
      },
      access_token: {
        type: "string",
        format: "password",
        "x-secret": true,
        title: "Access Token"
      }
    }
  },
  destination_schema: {
    type: "object",
    required: ["target"],
    properties: {
      target: { type: "string", title: "Target Room / Topic" }
    }
  }
};

export function validate(input) {
  // input: { scope: "account" | "destination" | "instance", config }
  return { ok: true, normalized_config: input.config };
}

export function test(input) {
  // input: { scope, account?, destination?, instance?, config?, locale? }
  const res = fc.fetch({ method: "GET", url: "https://api.example.com/ping", timeoutMs: 8000 });
  return { ok: res.ok, message: res.ok ? "Connected" : `HTTP ${res.status}` };
}

export function deliver(input) {
  // input: { idempotency_key, task_id, attempt, account, destination, entry, feed, route, checkpoint }
  const body = fc.truncateUtf8Bytes(input.entry.summary_text || input.entry.title || "", 3500);
  const res = fc.fetch({
    method: "POST",
    url: `${input.account.server_url.replace(/\/+$/, "")}/send`,
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ target: input.destination.target, title: input.entry.title, body }),
    timeoutMs: 15000
  });
  if (res.status === 429) return { outcome: "retry", status_code: 429, retry_after_secs: 60 };
  if (res.status >= 500) return { outcome: "retry", status_code: res.status, diagnostic: `Upstream HTTP ${res.status}` };
  if (!res.ok) return { outcome: "permanent_failure", status_code: res.status, diagnostic: `HTTP ${res.status}` };
  return { outcome: "delivered", status_code: res.status };
}
```

### 支持的导出函数

| 导出项 | 适用类型 / 钩子 | 超时预算 | 职责与返回值 |
| :--- | :---: | :---: | :--- |
| `meta` | 全部（必填） | 编译期 | 静态清单对象，声明 `id`、`name`、`version`、`api_version: "1"`、`kind`、`min_core`、`allowed_hosts`、字段 Schema 与 `hooks`。 |
| `validate(input)` | 全部（可选） | `10s` | 校验 `{ scope: "account" \| "destination" \| "instance", config }`，返回 `{ ok: true, normalized_config? }` 或 `{ ok: false, error }`。 |
| `test(input)` | 全部（可选） | `10s` | 对 `{ scope, account?, destination?, instance?, locale? }` 执行实时连通性测试，返回 `{ ok: true \| false, message?, error? }`。 |
| `deliver(input)` | `kind: "channel"` | `30s` | 向自定义通道投递渲染后的文章，返回 `{ outcome, status_code?, external_id?, checkpoint?, retry_after_secs?, next_window_at?, diagnostic? }`。 |
| `onRender(input)` | `hooks: ["on_render"]` | `15s` | 在通道渲染前改写 `content_html` / `primary_link`；返回的 HTML **强制再次经 `ammonia` 清洗**。 |
| `onDelivered(input)` | `hooks: ["on_delivered"]` | `10s` | 接收来自持久化 `plugin_events` 队列的至少一次投递完成回调（最多 5 次指数退避重试）。 |

### `deliver(input)` 参数与结果枚举

- **输入字段**：
  - `idempotency_key`：确定性 SHA-256 幂等键（调试推送每次独立生成）
  - `task_id`、`attempt`：当前任务 ID 与重试次数
  - `account`、`destination`：已解密的账号与目的地配置对象
  - `entry`：`{ title, url, summary_text, plain_text, content_html, author, published_at, published_local, media }`（`published_local` 为路由时区 `YYYY-MM-DD HH:MM`）
  - `feed`：`{ id, title, url, site_url, favicon_url }`
  - `route`：`{ id, name, locale, timezone }`
  - `checkpoint`：上次投递记录的断点 JSON 或 `null`
- **返回 `outcome`**：
  - `"delivered"`：投递成功（可选附带 `status_code`、`external_id`、`checkpoint`、`diagnostic`）
  - `"retry"`：临时错误，支持返回 `retry_after_secs`（上游 `429` 或指定 `retry_after_secs` 会触发账号凭据组冷却）
  - `"permanent_failure"`：不可重试的永久错误（任务转入 `dead_letter`）
  - `"deferred"`：不消耗尝试次数的延后投递（可选 `next_window_at` 或 `retry_after_secs`）

---

## 2. 宿主工具函数（`globalThis.fc`）

FeedCourier 会向每次沙箱执行注入只读的 `globalThis.fc` 宿主对象（插件须仅使用纯 ECMAScript 与 `globalThis.fc`，不可依赖 `import`、`fetch`、`URL`、`Buffer` 或 `TextEncoder`）：

| 函数 | 签名 | 说明 |
| :--- | :--- | :--- |
| `fc.fetch` | `fc.fetch({ method?, url, headers?, body?, timeoutMs? }) -> { status, ok, headers, body }` | 同步 HTTP(S) 请求（通道/校验/测试/onDelivered 每次最多 4 次，`onRender` 最多 10 次；`timeoutMs` 限制 `100..=15000`；响应上限 `1 MiB`；禁止重定向；响应头键名统一小写）。执行双层白名单（`meta.allowed_hosts` 走 `ProtectedHttpClient` 防 SSRF + 管理员配置的 `format: "uri"` 精确 `(scheme, host, port)` 直连），永久硬拦截云元数据 IP，完整遵循通道/实例的 `use_proxy`。 |
| `fc.sha256` | `fc.sha256(input: string, encoding?: "hex" \| "base64" \| "base64url") -> string` | 计算 UTF-8 字符串的 SHA-256 摘要（默认小写 `"hex"`）。 |
| `fc.hmacSha256` | `fc.hmacSha256(message: string, secret: string, encoding?: "hex" \| "base64" \| "base64url") -> string` | 计算 HMAC-SHA256 签名（默认小写 `"hex"`）。 |
| `fc.md5` | `fc.md5(input: string) -> string` | 计算小写十六进制 MD5 摘要（适配传统 API 签名）。 |
| `fc.base64Encode` | `fc.base64Encode(input: string) -> string` | UTF-8 文本的标准 Base64 编码（用于 `Basic` 认证头或 RFC 2047 标题编码）。 |
| `fc.base64Decode` | `fc.base64Decode(b64: string) -> string` | 解码标准或 URL-safe Base64 为 UTF-8 字符串。 |
| `fc.base64UrlEncode` | `fc.base64UrlEncode(input: string) -> string` | 无填充 URL-safe Base64 编码（RFC 4648）。 |
| `fc.utf8ByteLength` | `fc.utf8ByteLength(str: string) -> number` | 计算字符串的精确 UTF-8 字节长度。 |
| `fc.truncateUtf8Bytes` | `fc.truncateUtf8Bytes(str: string, maxBytes: number) -> string` | 按 UTF-8 字节上限在合法 Unicode 码点边界安全截断文本（适合 ntfy / APNs / Bark 4 KB 载荷限制）。 |
| `fc.htmlToText` | `fc.htmlToText(html: string) -> string` | 将 HTML 片段转换为干净的纯文本。 |
| `fc.unixNow` | `fc.unixNow() -> number` | 返回当前 UTC Unix 秒级时间戳。 |
| `fc.uuid` | `fc.uuid() -> string` | 生成随机 RFC 4122 UUID v4 字符串。 |

---

## 3. 沙箱与核心安全边界

- **纯 Rust 隔离沙箱（`boa_engine`）**：沙箱内不暴露文件系统、`process`、环境变量、原生扩展、`import` 语句（`IdleModuleLoader`）或原始 Socket。所有钩子必须同步返回（返回 `Promise` 会被拒绝）。
- **双层域名白名单校验**：向未列入 `meta.allowed_hosts` 且非管理员配置的 `format: "uri"` 精确 `(scheme, host, port)` 的目标发起 `fc.fetch()` 会在 DNS 解析前立即拒绝（`403 ssrf_blocked`）；云元数据地址（`169.254.169.254`、`100.100.100.200`、`fd00:ec2::254`、`0.0.0.0`、`::`、`metadata.google.internal`）与 HTTP `3xx` 重定向永久硬拦截。
- **资源与载荷限制**：
  - 单个插件脚本上限 **`512 KiB`**，单次 HTTP 响应上限 **`1 MiB`**。
  - 每次调用受硬性 VM 预算（`loop_iteration_limit = 200,000`、`recursion_limit = 128`、`stack_size_limit = 64 KiB`）与超时约束（编译 `5s`，`validate`/`test`/`onDelivered` `10s`，`onRender` `15s`，`deliver` `30s`）。
  - `onRender` 返回的 HTML 强制再次通过 `ammonia` 清洗。
  - `deliver` 返回的序列化 `checkpoint` 上限为 **`16 KiB`**。
- **持久化 `on_delivered` 出队表**：任务标记为已投递时，在同一 SQLite 事务内向 `plugin_events` 写入事件，由独立后台协程按指数退避重试（`15s, 30s, 60s, 120s`，最多 **5 次**），不影响原任务状态与通道配额。

---

## 4. 插件市场与单元测试（`ElleryHe/FeedCourier-Plugins`）

官方与社区 JS 插件统一托管在 **[`ElleryHe/FeedCourier-Plugins`](https://github.com/ElleryHe/FeedCourier-Plugins)**（内置 **`ntfy`** 与 **`bark`** 官方通道插件），支持使用 Node.js 24+ 直接运行零依赖单元测试：

```bash
cd FeedCourier-Plugins
node --test tests/*.test.mjs
node scripts/build-index.mjs --check --verify-index
```

### 注册表条目与 `index.json` 字段

| 字段 | 类型 | 说明 |
| :--- | :--- | :--- |
| `id`、`name`、`version`、`api_version` | `string` | 唯一短横线 ID、展示名称、最新 SemVer 版本号、固定 `"1"`。 |
| `kind` | `string` | `"builtin"` \| `"channel"` \| `"processor"` \| `"coming_soon"`。 |
| `runtime` | `string` | `"builtin"` \| `"js"`。 |
| `min_core` | `string` | 最低兼容的 FeedCourier 核心版本（安装前由后端与 WebUI 自动校验）。 |
| `category`、`icon`、`description`、`description_i18n` | `string` / `object` | 市场分类筛选、图标与多语言简介（如 `{ "en": "...", "zh": "..." }`）。 |
| `homepage`、`repository` | `string` | 严格 `https://` 文档与源码仓库地址。 |
| `allowed_hosts` | `string[]` | 插件允许通过 `fc.fetch()` 访问的公网域名列表（在 WebUI 卡片与安装弹窗中显式展示）。 |
| `latest`、`versions` | `string` / `object[]` | 版本列表（`{ version, min_core, path, sha256, changelog_path?, changelog? }`），一键安装时强制执行 SHA-256 校验。 |
| `hooks`、`permissions` | `string[]` | 钩子声明（`on_render`、`on_delivered`）与权限声明（`["network"]`）。 |
| `account_schema`、`destination_schema`、`instance_schema`、`route_option_schema` | `object` | 驱动 WebUI 动态表单的 JSON Schema（`format: "password"` 或 `x-secret: true` 字段自动加密与脱敏）。 |

### 向 `ElleryHe/FeedCourier-Plugins` 提交上架

1. Fork `ElleryHe/FeedCourier-Plugins`，新增 `plugins/<channels|processors>/<id>/<version>/plugin.js`、`CHANGELOG.md`、`plugins/<channels|processors>/<id>/README.md`（及 `README.zh-CN.md`）与 `tests/<id>.test.mjs`。
2. 在本地运行 `node --test tests/*.test.mjs` 与 `node scripts/build-index.mjs --check` 验证通过。
3. **请勿在 PR 中手动修改 `index.json`。** PR 阶段由 GitHub Actions 自动校验，合入 `main` 后自动重新生成 `index.json` 与所有版本的 `sha256` 校验值。
