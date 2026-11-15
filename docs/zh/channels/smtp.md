# SMTP 邮件

SMTP 邮件通道可将新文章自动排版为 HTML 邮件（同时附带纯文本回退版本）发送到一个或多个收件邮箱，适合在邮件客户端中阅读长文或利用邮件过滤规则自动分类归档。

---

## 1. 获取 SMTP 服务器信息与授权密码

在配置 SMTP 通道前，请先在你的邮箱服务商后台开启 SMTP 服务，并生成**应用专用密码（App Password）**或**客户端授权码**（大多数邮箱服务商不支持直接使用网页登录密码进行 SMTP 认证）。

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

## 2. 在 WebUI 中填写通道配置

登录 WebUI，进入 **Channels → New Channel → SMTP**，填写以下配置项：

| 字段名 | 必填 | 默认值 | 说明 |
| :--- | :---: | :--- | :--- |
| `host` | 是 | — | SMTP 服务器主机名（如 `smtp.gmail.com`、`smtp.fastmail.com`）。 |
| `port` | 是 | `587` | SMTP 端口（`587` 通常配合 `start_tls`，`465` 通常配合 `tls`）。 |
| `security` | 是 | `start_tls` | 传输加密方式：<br>• `start_tls`：连接后通过 STARTTLS 升级加密（常用于端口 `587`）<br>• `tls`：全程隐式 TLS / SMTPS 加密（常用于端口 `465`）<br>• `plain`：明文连接（仅用于本机或内网自建邮件中继，如端口 `25` / `1025`） |
| `username` | 否 | `null` | SMTP 登录用户名（通常为完整发件邮箱地址）。 |
| `password` | 否 | `null` | SMTP 认证密码或应用专用密码。密码经加密后保存在数据库中，后续编辑通道时若留空则保留原密码。 |
| `from_address` | 是 | — | 发件人邮箱地址（需与当前认证的 SMTP 账号一致，或具备该地址的发信权限）。 |
| `to_addresses` | 是 | — | 收件人邮箱地址列表（支持填写一个或多个收件地址）。 |
| `reply_to` | 否 | `null` | 可选的回复地址（`Reply-To`）。 |

填写完成后，点击 **Test** 按钮可向收件邮箱发送一封测试邮件，确认连接和认证配置无误。

---

## 3. 发件人地址对齐（`Force-From`）说明

现代邮件服务商普遍启用了 SPF、DKIM 与 DMARC 校验。如果邮件的 `From` 发件地址填写了非当前 SMTP 账号所属域名的外部地址（例如直接使用文章原作者邮箱），邮件会被 Gmail、QQ 邮箱或企业邮网关拒收（如返回 `550 5.7.1`）或归入垃圾邮件箱。

因此，FeedCourier 默认采用发件人对齐策略（`Force-From`）：
- **发件人邮箱地址固定**：邮件 `From` 头的邮箱部分始终使用你在通道中配置的 `from_address`（确保与 SMTP 认证账号对齐，正常通过 SPF/DKIM/DMARC 校验）；
- **发件人显示名称动态展示**：邮件 `From` 头的显示名称会自动设置为 `"<订阅源标题> (via FeedCourier)"`，方便你在收件箱列表中直接分辨每封邮件来自哪个订阅源。

---

## 4. 投递邮件效果与客户端归档

每篇推送到邮箱的文章具有以下排版与邮件头特性：

- **单栏 HTML 排版与纯文本回退**：邮件正文采用适合桌面端和手机端阅读的单栏 HTML 样式，展示文章标题、来源、作者、发布时间与清洗后的正文，并在底部附带跳转到原文网页的按钮；同时邮件内包含纯文本（`text/plain`）回退内容，兼容纯文本邮件客户端。
- **超长文章自动截断**：当单篇文章的 HTML 内容过长时，系统会自动在段落或标签边界处截断，并在文末附上原文链接，避免触发 Gmail 等客户端对超过约 `102 KB` 邮件的正文折叠。
- **支持按订阅源自动归档**：每封邮件均携带标准的 `List-Id`（按订阅源生成）与 `Auto-Submitted: auto-generated` 邮件头。你可以在 Gmail、Apple Mail、Fastmail 或 Thunderbird 中根据 `List-Id` 或发件人名称创建过滤规则，将不同订阅源的邮件自动归档到对应文件夹或标签中。

---

## 延伸阅读

- 关于 SMTP 连接复用、邮件模板渲染、体积截断阈值及标准 RFC 邮件头生成的内部实现细节，请参阅架构手册中的 [通道协议与投递实现](/zh/reference/channel-protocols)。
