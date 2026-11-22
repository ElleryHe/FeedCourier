---
description: "配置 FeedCourier 内置「链接替换 (YOURLS)」插件：鉴权模式、局域网/Docker 部署的 SSRF_ALLOWLIST 白名单、SQLite 短链缓存、严格模式及各推送通道行为。"
---

# 链接替换 (YOURLS)

**链接替换（`link-rewrite`）**是 FeedCourier 的首个第一方内置插件（`kind: "builtin"`，`on_render` 钩子）。它在投递渲染阶段通过一个或多个自托管 **[YOURLS](https://yourls.org/)** 服务将外发文章链接转换为短链，同时保持入库原文不变，并支持按路由独立选择服务商（或不启用）。

---

## 1. 准备工作与 YOURLS 配置

1. 部署自托管 YOURLS 实例，确认 API 地址可访问（例如 `https://sho.rt/yourls-api.php` 或内网 / Docker 地址）。
2. 登录 YOURLS 管理后台，进入 **Tools → Secure passwordless API call**，复制 **Signature Token（无密码签名令牌）**（相比账号密码更推荐此方式）。
3. 确认反向代理允许向 `yourls-api.php` 发送 `POST` 请求（`action=shorturl&format=json`）。

::: warning 内网 / Docker 部署的 YOURLS 必须加入 `SSRF_ALLOWLIST`
所有对 YOURLS 的出站请求均经过 FeedCourier 的 `ProtectedHttpClient` 连接层防 SSRF 校验。如果你的 YOURLS 部署在 `127.0.0.1`、私有局域网段（`10.0.0.0/8`、`172.16.0.0/12`、`192.168.0.0/16`）或 Docker Compose 同网络服务名（例如 `http://yourls/yourls-api.php`），必须将其主机名或 IP/CIDR 加入 **`SSRF_ALLOWLIST`**（通过环境变量或 WebUI **设置 → 出站网络与防 SSRF** 热更新），否则请求会被拦截并返回 `403 ssrf_blocked`：

```yaml
services:
  feedcourier:
    image: elleryhe/feedcourier:latest
    environment:
      SSRF_ALLOWLIST: "yourls,172.20.0.0/16"
```
:::

---

## 2. 在 WebUI 中添加 YOURLS 服务商

1. 在左侧菜单进入 **插件**，启用 **Link Rewrite (YOURLS)**（或在 **市场** 标签页点击启用）。
2. 进入插件详情页，点击 **新增服务商**。
3. 填写以下配置（签名令牌与密码均采用 ChaCha20-Poly1305 加密存储，接口与 MCP 返回时一律脱敏）：

| 字段 | 必填 | 默认值 | 说明 |
| :--- | :---: | :--- | :--- |
| `name` | 是 | — | 服务商备注名（例如 `自建短链 sho.rt`）。 |
| `api_url` | 是 | — | 完整的 YOURLS API 地址（如 `https://sho.rt/yourls-api.php`），仅允许 `http://` 或 `https://`，禁止内嵌账号密码。 |
| `auth_mode` | 是 | `signature` | 鉴权模式：<br>• `signature` *（推荐）*：无密码签名令牌（`signature=<token>`）。<br>• `timed_signature`：带时间戳的 MD5 签名（`timestamp=<unix>&signature=md5(timestamp + token)`），降低重放风险。<br>• `password`：传统 `username` + `password` 登录模式。 |
| `signature` | 条件必填 | — | YOURLS 签名令牌（`signature` / `timed_signature` 模式必填）；编辑时留空保留原密钥。 |
| `username` / `password` | 条件必填 | — | YOURLS 用户名与密码（`password` 模式必填）；编辑时密码留空保留原密文。 |
| `exclude_domains` | 否 | `[]` | 不参与短链替换的域名列表，支持精确域名（`github.com`）与通配符子域名（`*.example.com`，同时匹配根域名 `example.com`）。**服务商自身域名会自动跳过**，防止递归缩短。 |
| `strict_mode` | 否 | `false` | 失败策略：<br>• `false` *（默认）*：逐条降级——单条链接缩短失败时保留原始 URL，文章照常投递，并在任务 `diagnostic_log` 记录警告（发件箱队列显示**黄色**「部分替换」）。<br>• `true`：严格模式——任一链接失败即返回 `PluginResult::Retry`，使整个投递任务进入标准退避重试流程。 |
| `pass_title` | 否 | `true` | 缩短文章主链接时附带文章标题（最多 200 字符）作为 `title` 参数传给 YOURLS，避免 YOURLS 服务器再反向回源抓取标题。 |
| `use_proxy` | 否 | `false` | 调用 YOURLS API 时是否经由全局 `OUTBOUND_PROXY` 代理转发。 |

4. 点击 **测试** 验证连通性：系统会实际缩短测试链接 `https://feedcourier.ellery.me/` 并返回生成的短链。
5. 进入 **路由规则** 编辑某条路由，展开可折叠的 **插件** 分区，选择对应的 YOURLS 服务商即可生效。

::: tip 已有短链（`code: "error:url"`）兼容处理
当待缩短的 URL 已存在于 YOURLS 数据库中时，YOURLS 会返回 `status: "fail"` / `code: "error:url"` 并在同一 JSON 响应中附带既有 `shorturl`（部分反向代理下甚至伴随 HTTP 400）。FeedCourier 会自动识别响应中的有效 `shorturl` 并按成功处理。
:::

---

## 3. 性能边界与 SQLite 短链缓存

为防止链接密集型周报拖慢发件箱吞吐或压垮自建 YOURLS 实例，插件内置以下保护与缓存机制：

- **单篇链接数量上限**：单次投递最多缩短 **`50` 条唯一合法链接**（`MAX_REWRITE_URLS_PER_ENTRY = 50`，优先处理文章主链接，随后按文档顺序处理正文 `<a href>`）；超出上限的链接保持原样并记录告警。
- **并发与超时限制**：缓存未命中时以最多 **`4` 路并发**（`REWRITE_CONCURRENCY = 4`）调用 YOURLS，单链接超时 **`5` 秒**（`REWRITE_REQUEST_TIMEOUT = 5s`），单响应体上限 `64 KiB`。
- **SQLite 短链缓存（`plugin_link_cache`）**：
  - 按 `(instance_id, sha256(long_url))` 缓存短链映射，避免任务重试、多路由扇出或 SMTP 多收件人发送时重复请求 YOURLS。
  - 每次命中缓存都会更新 `last_used_at`；后台维护协程自动清理超过 **90 天**未被使用的缓存记录。
  - 可在插件详情页随时手动清空单个服务商的缓存（对应 `DELETE /api/v1/plugins/{id}/instances/{iid}/cache`）。
  - **调试推送（Debug Push）**会完整执行 `on_render` 钩子以预览替换效果，但不会向缓存写入记录。

---

## 4. 各推送通道替换与保留规则

插件仅解析并改写 `http://` 与 `https://` 协议的 `<a href="...">` 属性及文章主链接，自动跳过 `mailto:`、`tel:`、`#` 页内锚点、服务商自身域名及 `exclude_domains`。

| 通道 | 会替换为短链 | 保持原始链接不替换 |
| :--- | :--- | :--- |
| **Telegram** | 富文本正文 `<a href>`、**阅读原文**按钮/链接、快讯卡片链接、媒体模式标题中的链接 | **`link_preview_options.url`**（仍使用原始 `entry.canonical_url`，保证 Telegram 正常生成网页预览卡片）及**所有媒体链接** |
| **SMTP 邮件** | HTML 与纯文本正文中的 `<a href>`、**阅读原文 →** / **继续阅读原文 →** 按钮 | 所有 `<img>` 的 `src`、`List-Unsubscribe` 邮件头、`/u/{token}` 退订链接与 `/v/{token}` 验证链接 |
| **Notion** | 正文 Markdown 链接，以及可选的 **`Short URL`** 数据库属性列（已建列并映射时写入） | **`Link`** 属性列始终保留原始 `entry.canonical_url`；所有外部图片与媒体链接保持原样 |

### 为什么媒体 URL 一律不替换？

媒体元素（`<img>`、`<video>`、`<audio>`、`<source>`）被刻意排除在短链替换之外：

1. **Telegram 媒体代传与嗅探**：FeedCourier 需要直接向源站拉取防盗链图片/视频（合成对应源站 `Referer` 并进行魔数与 MP4 元数据解析）后再上传给 Telegram；将媒体 URL 替换为短链会引入额外跳转、丢失原始域名上下文并破坏类型探测。
2. **邮件内联图片**：Gmail、Apple Mail 与 Outlook 的图片代理对 `<img src>` 非常敏感，短链跳转会抬高垃圾邮件评分且容易导致图片加载失败。
3. **Notion 外部图片块**：Notion 服务端会直接拉取外链图片并校验 Content-Type，经短链重定向的图片链接往往无法在页面中正常渲染。
