# SMTP 邮件通道

FeedCourier 的 SMTP 通道直接传承自经典工具 `rss2email` 的单篇邮件投递哲学，并针对现代邮件服务商（Gmail、Fastmail、iCloud Mail、Proton Mail、自建 Postfix/Stalwart 及各类企业邮）的反垃圾安全策略与移动端阅读体验进行了全面重构。

---

## 核心特性概览

- **响应式单栏内联 CSS 阅读信纸**：基于 `minijinja` 渲染精调内联样式的现代无衬线单栏信纸，同时在 `multipart/alternative` 中附带 `html2text` 生成的 **78 列宽纯文本回退**。
- **严控 `<102 KB` 防 Gmail 折叠截断**：HTML 正文部分严格限制在 **`95 KiB`（`MAX_EMAIL_HTML_BYTES`）** 以内，确保整封 MIME 编码邮件远离 Gmail 的 `102 KB` 强行截断红线；超长文章自动在安全标签边界截断并附上“继续阅读原文”引导卡片。
- **`Force-From` 强制对齐认证发件人**：信封与 `From` 头邮箱地址始终固定为你配置的已认证发件地址（`from_address`），仅将显示名称动态拼装为 `"<订阅源标题> (via FeedCourier)"`，完美通过 SPF、DKIM 与 DMARC 严苛校验。
- **确定性 `Message-ID` 与原生邮件归档头**：基于条目指纹与通道标识生成确定性 `Message-ID` 辅助客户端去重，自动注入 `Auto-Submitted: auto-generated`、`X-Mailer` 以及按订阅源生成的标准 `List-Id` 头。

---

## 配置参数说明

在 WebUI **Channels → New Channel → SMTP** 中，可配置以下字段（对应 `SmtpChannelConfig`）：

| 字段名 | 必填 | 默认值 | 说明 |
| :--- | :---: | :--- | :--- |
| `host` | 是 | — | SMTP 服务器主机名（如 `smtp.gmail.com`、`smtp.fastmail.com`、`smtp.feishu.cn`）。 |
| `port` | 是 | `587` | SMTP 端口（`587` 通常配合 `start_tls`，`465` 通常配合 `tls`）。 |
| `security` | 是 | `start_tls` | 传输加密方式：<br>• `start_tls`：显式 STARTTLS 升级（推荐端口 `587`）<br>• `tls`：隐式 TLS / SMTPS 加密（推荐端口 `465`）<br>• `plain`：明文连接（仅限本机或受信任内网中继，如端口 `25`/`1025`） |
| `username` | 否 | `null` | SMTP 认证用户名（通常为发件邮箱完整地址）。 |
| `password` | 否 | `null` | SMTP 认证密码或应用专用密码（App Password）。使用 ChaCha20-Poly1305 加密落盘，WebUI 二次编辑留空时自动保留原密码。 |
| `from_address` | 是 | — | **`Force-From` 认证发件人邮箱地址**（必须与你的 SMTP 账号具备发信授权关系）。 |
| `to_addresses` | 是 | — | 收件人邮箱地址列表（支持配置 1 个或多个收件邮箱）。 |
| `reply_to` | 否 | `null` | 可选的 `Reply-To` 回复地址。 |

### 主流邮箱服务商配置参考

| 服务商 | `host` | `port` | `security` | 认证注意事项 |
| :--- | :--- | :---: | :---: | :--- |
| **Gmail / Google Workspace** | `smtp.gmail.com` | `587` 或 `465` | `start_tls` 或 `tls` | 需开启两步验证并生成 16 位**应用专用密码（App Password）**作为 `password`。 |
| **Fastmail** | `smtp.fastmail.com` | `465` | `tls` | 在 Fastmail Settings → Privacy & Security 中生成具有 SMTP 权限的 App Password。 |
| **Apple iCloud Mail** | `smtp.mail.me.com` | `587` | `start_tls` | 在 Apple Account 页面生成应用专用密码，`from_address` 需与 iCloud 主邮箱或别名一致。 |
| **腾讯企业邮 / 飞书企业邮** | `smtp.exmail.qq.com` / `smtp.feishu.cn` | `465` | `tls` | 使用客户端专用密码，`from_address` 必须与登录账号完全一致。 |

---

## 工程细节详解

### 1. 为什么必须采用 `Force-From` 机制？

早期的订阅转邮件工具曾尝试将原作者邮箱或伪造的 `author@blog-domain.com` 直接填入邮件 `From` 头。在当今普遍部署 **SPF、DKIM 与 DMARC（`p=quarantine` / `p=reject`）** 的邮件生态下，使用非自有域名的 `From` 地址会立即导致邮件被 Gmail 或企业邮网关拒收（`550 5.7.1`）或直接投入垃圾箱。

FeedCourier 严格执行 `Force-From` 规范：
- **发件地址固定**：`From` 头的邮箱部分始终为你配置的 `from_address`（例如 `courier@yourdomain.com`）；
- **显示名称动态化**：`From` 头的 Display Name 自动格式化为 `"<订阅源标题> (via FeedCourier)"`（当文章含作者名时，在信纸副标题栏清晰展示作者与原始发布时间）。

这样既能在收件箱列表中一眼分辨文章来自哪个订阅源，又能保证 **100% 的 SPF/DKIM/DMARC 对齐通过率**。

### 2. 注入的标准 RFC 邮件头与客户端过滤归档

每封由 FeedCourier 投递的邮件均包含以下规范化邮件头（源码见 `src/channels/smtp.rs`）：

| 邮件头 | 生成规则示例 | 作用 |
| :--- | :--- | :--- |
| `Message-ID` | `<fc.<32位确定性哈希>@<from_domain>>` | 由 `feed_id + fingerprint_sha256 + route_id + channel_id` 确定性派生，并持久化在 `delivery_tasks.checkpoint_json` 中。即使网络抖动触发重试，主流邮件客户端也能依据相同 `Message-ID` 自动去重。 |
| `List-Id` | `"<Feed Title>" <feed-<feed_id>.feedcourier.<from_domain>>` | 符合 RFC 2919 标准。Gmail、Apple Mail、Thunderbird 与 Fastmail 可直接识别该头，支持按订阅源一键创建“自动归档至专属文件夹/标签”规则。 |
| `Auto-Submitted` | `auto-generated` | 符合 RFC 3834 标准，明确告知收件端自动回复程序（Vacation Auto-Responder）不要对此邮件触发自动回信，防止邮件死循环。 |
| `X-FeedCourier-Feed-Id` | `<feed_id>` | 供高级邮件篩选规则（Sieve 脚本）精确匹配订阅源数字 ID。 |
| `X-FeedCourier-Entry-SHA256` | `<fingerprint_sha256>` | 携带 64 位条目指纹，便于审计与排障。 |

### 3. 防 Gmail `102 KB` 截断与连接池复用

- **双模正文（`multipart/alternative`）**：每封邮件同时包含 `text/plain; charset=utf-8`（由 `html2text` 按 `78` 列宽排版）与 `text/html; charset=utf-8`（由 `minijinja` 渲染的内联 CSS 模板）。
- **95 KiB 字节预算保护**：当遇到超长万字长文导致渲染后的 HTML 超过 `95 KiB`（`MAX_EMAIL_HTML_BYTES`）时，渲染器会自动按安全字符边界截断正文，并在邮件底部渲染高亮提示框与“Continue reading original article →”直达按钮，避免 Gmail 因总体积超过 `102 KB` 而隐藏文末结构。
- **异步连接池复用**：`SmtpSender` 内部按 `(host, port, security, username, password)` 维护 `lettre::AsyncSmtpTransport` 连接池，避免连续步进发信时频繁重复 TCP/TLS 握手。
