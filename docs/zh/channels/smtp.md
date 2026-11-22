---
description: "配置 FeedCourier SMTP 邮件投递通道：单个 SMTP 账号挂载多个收件人目的地、响应式防撑宽邮件排版、本地时区时间，以及便于收件箱自动归档的 List-ID 邮件头。"
---

# SMTP 邮件

SMTP 邮件通道可将新文章自动排版为响应式单栏 HTML 邮件（同时附带纯文本回退版本）发送至收件邮箱，适合在邮件客户端中阅读长文或利用邮件过滤规则自动分类归档。

在 FeedCourier 的双层通道模型下，你只需配置一次 **SMTP 账号**（服务器、认证凭据、发件地址、代理、流速与配额），即可在其下挂载一个或多个**收件目的地**（各自配置独立的收件人组与可选 `Reply-To`）。

---

## 1. 获取 SMTP 服务器信息与授权密码

在配置 SMTP 账号前，请先在你的邮箱服务商后台开启 SMTP 服务，并生成**应用专用密码（App Password）**或**客户端授权码**（大多数邮箱服务商不支持直接使用网页登录密码进行 SMTP 认证）。

### 常见邮箱服务商配置参考

| 邮箱服务商 | `host`（服务器地址） | `port`（端口） | `security`（加密方式） | 获取密码 / 授权码方式 |
| :--- | :--- | :---: | :---: | :--- |
| **Gmail / Google Workspace** | `smtp.gmail.com` | `587` 或 `465` | `start_tls` 或 `tls` | 在 Google 账号开启两步验证后，前往“安全性 → 应用专用密码”生成 16 位专用密码。 |
| **Fastmail** | `smtp.fastmail.com` | `465` | `tls` | 在 Settings → Privacy & Security → Integrations 中生成勾选了 SMTP 权限的 App Password。 |
| **Apple iCloud Mail** | `smtp.mail.me.com` | `587` | `start_tls` | 在 Apple Account 页面生成应用专用密码，`from_address` 需填写 iCloud 邮箱或别名。 |
| **QQ 邮箱 / 腾讯企业邮** | `smtp.qq.com` / `smtp.exmail.qq.com` | `465` | `tls` | 在邮箱“设置 → 账户 → POP3/IMAP/SMTP”中开启服务并生成 16 位授权码。 |
| **163 邮箱** | `smtp.163.com` | `465` | `tls` | 在“设置 → POP3/SMTP/IMAP”中开启 SMTP 服务并新增客户端授权密码。 |
| **飞书企业邮** | `smtp.feishu.cn` | `465` | `tls` | 在邮箱设置中生成第三方客户端专用密码。 |

---

## 2. 在 WebUI 中配置账号与目的地

登录 WebUI，进入 **Channels → New Channel → SMTP**，先配置发件账号并创建首个收件目的地（后续可随时在同一账号下继续新增目的地）。

### SMTP 账号字段

| 字段名 | 必填 | 默认值 | 说明 |
| :--- | :---: | :--- | :--- |
| `name` | 否 | *自动获取* | 账号显示名称。留空时自动填入 **`from_address`** 发件地址。 |
| `host` | 是 | — | SMTP 服务器主机名（如 `smtp.gmail.com`、`smtp.fastmail.com`）。 |
| `port` | 是 | `587` | SMTP 端口（`587` 通常配合 `start_tls`，`465` 通常配合 `tls`）。 |
| `security` | 是 | `start_tls` | 传输加密方式：<br>• `start_tls`：连接后通过 STARTTLS 升级加密（常用于端口 `587`）<br>• `tls`：全程隐式 TLS / SMTPS 加密（常用于端口 `465`）<br>• `plain`：明文连接（仅用于本机或内网自建邮件中继，如端口 `25` / `1025`） |
| `username` | 否 | `null` | SMTP 登录用户名（通常为完整发件邮箱地址）。 |
| `password` | 否 | `null` | SMTP 认证密码或应用专用密码。密码经加密后保存在数据库中，后续编辑账号时若留空则保留原密码；调用 `PATCH /api/v1/channels/{id}` 时显式传入 `"password": null` 可清空可选密码。 |
| `from_address` | 是 | — | 发件人邮箱地址（需与当前认证的 SMTP 账号一致，或具备该地址的发信权限）。 |

### SMTP 目的地字段（单账号可添加多个）

| 字段名 | 必填 | 默认值 | 说明 |
| :--- | :---: | :--- | :--- |
| `name` | 否 | *自动获取* | 目的地备注名。留空时自动填入 `to_addresses` 中的**首个收件地址**。 |
| `to_addresses` | 是 | — | 收件人邮箱地址列表（支持填写一个或多个收件地址）。 |
| `reply_to` | 否 | `null` | 可选的回复地址（`Reply-To`）。 |
| `enabled` | 否 | `true` | 独立控制该收件人组的启用/停用状态，不影响同账号下的其他目的地。 |

### 账号测试、凭据隔离与共享限流

- **测试账号**：仅验证 SMTP 服务器连通性与登录认证（`EHLO`/`AUTH`），不会发出真实邮件。
- **测试目的地**：向该目的地的 `to_addresses` 发送一封验证邮件（使用当前 WebUI 语言）。
- **共享速率与密码摘要隔离**：挂载在同一 SMTP 账号下的所有收件目的地共享 `min_interval_secs`、逐条随机抖动、`hourly_quota`、`daily_quota`、`use_proxy` 以及临时限流冷却。在跨账号限流分组中，SMTP 的 `credential_group_key` 纳入了密码的 SHA-256 摘要（不存储明文），因此即便多个账号共用同一通用用户名（例如 SendGrid 均使用 `apikey` 作为用户名但密码不同），彼此的配额与限流仍完全隔离。详见[发送频率、配额与过滤](/zh/channels/pacing-and-quotas)。
- **从 `v0.1.x` 升级至 `v0.2.0` 的自动合并规则**：自动迁移仅在 `host`、`port`、`username`、`password`、`security` 与 `from_address` 六项字段**完全一致**时才将旧 SMTP 通道合并为同一个 SMTP 账号；合并后的配额、间隔、冷却与代理仅统计组内**已启用（`enabled = true`）**的旧通道（仅当全组均被禁用时才回退统计全部）。详见[从 `v0.1.x` 升级至 `v0.2.0`](/zh/guide/operations#upgrading-from-v0-1-x-to-v0-2-0)。

---

## 3. 发件人地址对齐（`Force-From`）说明

现代邮件服务商普遍启用了 SPF、DKIM 与 DMARC 校验。如果邮件的 `From` 发件地址填写了非当前 SMTP 账号所属域名的外部地址（例如直接使用文章原作者邮箱），邮件会被 Gmail、QQ 邮箱或企业邮网关拒收（如返回 `550 5.7.1`）或归入垃圾邮件箱。

因此，FeedCourier 默认采用发件人对齐策略（`Force-From`）：
- **发件人邮箱地址固定**：邮件 `From` 头的邮箱部分始终使用你在账号中配置的 `from_address`（确保与 SMTP 认证账号对齐，正常通过 SPF/DKIM/DMARC 校验）；
- **发件人显示名称动态展示**：邮件 `From` 头的显示名称会自动设置为 `"<订阅源标题> via FeedCourier"`，方便你在收件箱列表中直接分辨每封邮件来自哪个订阅源。

---

## 4. 投递邮件效果与客户端归档

每篇推送到邮箱的文章具有以下排版与邮件头特性：

- **响应式单栏 HTML 与纯文本回退**：邮件正文采用最大宽度 `640px` 的单栏卡片样式，展示文章标题、来源、作者、按**路由时区**转换的本地发布时间（如 `2026-11-08 11:00 (UTC+08:00)`）以及正文与原文链接；同时附带纯文本（`text/plain`）回退内容。
- **图片、图注、表格与代码块自适应屏幕**：针对订阅源正文中自带大尺寸固定宽高属性（如 `<img width="1200" height="630">`）的图片，渲染时会自动改写为适配卡片可用宽度的响应式样式（Outlook 兼容的 `width="592"`，保留本身更窄的小图宽度，移除固定 `height` 并内联 `max-width:100%;height:auto`），同时规范 `<figure>`/`<figcaption>` 边距与样式，对宽 `<table>` 与 `<pre>` 限制 `max-width:100%` 并允许横向滚动或自动换行，并在 `<head>` 中提供兼容 Gmail、Apple Mail 与 Outlook.com 的样式兜底，避免撑破手机或桌面端邮件卡片。
- **超长文章自动截断**：当单篇文章的 HTML 内容过长时，系统会自动在段落或标签边界处截断，并在文末附上原文链接，避免触发 Gmail 等客户端对超过约 `102 KB` 邮件的正文折叠。
- **支持按订阅源自动归档**：每封邮件均携带标准的 `List-Id`（按订阅源生成）与 `Auto-Submitted: auto-generated` 邮件头。你可以在 Gmail、Apple Mail、Fastmail 或 Thunderbird 中根据 `List-Id` 或发件人名称创建过滤规则，将不同订阅源的邮件自动归档到对应文件夹或标签中。

---

## 延伸阅读

- 关于 SMTP 连接复用、邮件 HTML 响应式改写规则、体积截断阈值及标准 RFC 邮件头生成的内部实现细节，请参阅架构手册中的 [通道协议与投递实现](/zh/reference/channel-protocols)。
