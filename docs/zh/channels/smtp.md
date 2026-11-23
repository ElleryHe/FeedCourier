---
description: "配置 FeedCourier SMTP 邮件投递通道：v0.3.0 逐收件人发送、手动触发双重确认验证（6 位验证码与 /v/{token}）、RFC 8058 一键退订（/u/{token}）、PUBLIC_URL 反向代理配置与硬退信保护。"
---

# SMTP 邮件

SMTP 邮件通道可将新文章自动排版为响应式单栏 HTML 邮件（附带纯文本回退版本）逐收件人发送，支持手动触发的双重确认（Double Opt-In）验证、RFC 8058 一键退订、CAN-SPAM 发件人身份页脚以及同步硬退信保护。

在 FeedCourier 的双层通道模型下，你只需配置一次 **SMTP 账号**（服务器、认证凭据、发件地址、可选发件人身份、流速与配额），即可在其下挂载一个或多个**收件目的地**（各自配置独立的收件人组与可选 `Reply-To`）。SMTP 通道仅使用原生 TCP/TLS 直连，全局 HTTP/SOCKS 出站代理（`OUTBOUND_PROXY` / `use_proxy`）不适用于 SMTP。

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
| `sender_identity` | 否 | `null` | 可选的发件人身份或联系地址文本（CAN-SPAM 合规信息），渲染在每封投递邮件的页脚中。 |

### SMTP 目的地字段（单账号可添加多个）

| 字段名 | 必填 | 默认值 | 说明 |
| :--- | :---: | :--- | :--- |
| `name` | 否 | *自动获取* | 目的地备注名。留空时自动填入 `to_addresses` 中**打码后的首个收件地址**（如 `a***e@example.com`，避免未加密元数据列泄露明文邮箱）。 |
| `to_addresses` | 是 | — | 收件人邮箱地址列表（支持填写一个或多个收件地址）。新增地址默认为 `unverified` 状态，须完成验证后才会收到订阅投递（详见下文）。 |
| `reply_to` | 否 | `null` | 可选的回复地址（`Reply-To`）。 |
| `enabled` | 否 | `true` | 独立控制该收件人组的启用/停用状态，不影响同账号下的其他目的地。 |

### 账号测试、删除引用保护与共享限流

- **测试账号（未传 `destination_id` 调用 `POST /api/v1/channels/{id}/test`）**：验证 SMTP 服务器连通性与登录认证，并**仅向自身 `from_address`** 发送一封自检邮件（绝不向目的地收件人发信）。
- **测试目的地（`POST /api/v1/destinations/{id}/test`）与预览调试推送（`debug_push`）**：二者均严格**只向 `verified`（已验证）收件人**按单 `To:` 隔离信封逐人发送（附带 RFC 8058 合规退订头，使用所选语言）；若该目的地存在 **`0` 位已验证收件人**，则直接返回明确错误提示，拒绝向未验证地址发信。
- **原生 TCP/TLS 直连与共享限流**：SMTP 仅使用原生 TCP/TLS 直连，不经过 HTTP/SOCKS 出站代理（WebUI 也会对 SMTP 账号隐藏代理开关）。挂载在同一 SMTP 账号下的所有收件目的地共享 `min_interval_secs`、逐条随机抖动、`hourly_quota`、`daily_quota` 以及临时限流冷却。跨账号分组的 `credential_group_key` 纳入密码的 SHA-256 摘要，确保共用通用用户名（如 SendGrid `apikey`）但不同密钥的账号互相隔离。详见[发送频率、配额与过滤](/zh/channels/pacing-and-quotas)。
- **账号与目的地删除保护（`409 Conflict`）**：若通道账号或目的地仍被一条或多条路由规则引用，直接删除会返回 **`409 Conflict`** 并列出 `referencing_routes` 清单；需先解绑或在确认级联删除时携带 `?cascade=true`（`DELETE /api/v1/channels/{id}?cascade=true` 或 `DELETE /api/v1/destinations/{id}?cascade=true`）一并删除关联路由。

---

## 3. SMTP 合规与逐收件人发送（`v0.3.0`）

自 **`v0.3.0`** 起，FeedCourier 在 SQLite 的 `smtp_recipients` 表中**按收件地址粒度**管理验证、授权审计、退订与硬退信状态（表内仅保存经主密钥派生的 HMAC-SHA256 摘要，明文邮箱仍仅存于加密的目的地配置中）。

### 逐收件人单封发送与配额语义说明

- **单收件人独立信封**：发件箱投递时，仅为每个状态为 **`verified`** 的收件地址单独发送一封邮件（`To:` 头仅包含该收件人），并生成按收件人区分的确定性 `Message-ID`（`<fc-{fp[..24]}-r{route_id}-{hmac[..8]}@{from_domain}>`）与专属 `/u/{token}` 退订链接；同一任务内收件人之间固定间隔 **1 秒** 发送并自动续租任务租约。
- **断点续发**：每成功送达一个收件人，其地址 HMAC 即记录到 `delivery_tasks.checkpoint_json`（`sent_recipients`）；若中途遭遇临时 `4xx` 错误触发重试，将自动跳过已送达的收件人，避免重复发信。

::: warning 配额计数与实际发信封数说明
一篇文章（一个投递任务）在 FeedCourier 的滚动配额流水表（`delivery_quota_ledger`）中**仍只记为 `1` 次**，但底层实际发出的 SMTP 邮件封数等于该目的地下**已验证收件人的数量（`N` 倍）**。若使用的 SMTP 服务商按实际封数计费或限流（如 Gmail、SES、Postmark、SendGrid），请在账号上合理规划 `hourly_quota` 与 `daily_quota`。
:::

### 手动触发双重确认验证（Double Opt-In）

1. **添加地址不自动发信**：向 `to_addresses` 新增收件地址时，系统仅将其记为 **`unverified`（未验证）**，**绝不会自动发送**任何验证邮件。
2. **管理员手动发起验证**：在 WebUI 展开该 SMTP 目的地（卡片显示「`已验证 / 总数`」徽章），点击对应地址后的**发送验证**（`POST /api/v1/destinations/{id}/recipients/{rid}/send-verification`）。验证邮件直接发出（不经过发件箱队列、不占通道配额）：
   - 正文附带 **6 位数字验证码**（服务端仅存 SHA-256 哈希，有效期 **30 分钟**，最多允许输错 **5 次**）；
   - 若已配置 **`PUBLIC_URL`**，邮件中同时附带确认链接 **`/v/{token}`**；
   - 内置防滥用限流：重发冷却 **60 秒**，单个收件地址每小时最多发送 **5 封**验证邮件。
3. **两种完成验证方式**：
   - **WebUI 填写验证码**：管理员将收件人收到的 6 位数字填入 WebUI（`POST /api/v1/destinations/{id}/recipients/{rid}/verify`），记录 `consent_method = "code"`。
   - **点击确认链接（`/v/{token}`）**：收件人打开链接后点击页面上的**确认订阅**按钮完成确认（**`GET /v/{token}` 严格只读**，防止企业邮件网关安全扫描预抓取时误触发确认；仅 **`POST /v/{token}`** 变更状态并记录 `consent_method = "link"`）。
4. **立即唤醒延迟任务**：一旦有收件人验证通过，该目的地下因等待验证而处于 `deferred` 状态的任务会立即被唤醒并进入投递。

### `PUBLIC_URL` 与 RFC 8058 一键退订（`/u/{token}`）

通过环境变量 `PUBLIC_URL` 或 WebUI **设置 → 公网访问地址** 配置绝对地址（推荐使用 `https://`，禁止包含查询参数或片段；配置明文 `http://` 时界面会显示警示）：

- **已配置 `PUBLIC_URL` 时**：
  - 每封邮件自动注入标准 RFC 8058 邮件头：
    ```http
    List-Id: <dest-5.yourdomain.com>
    Auto-Submitted: auto-generated
    List-Unsubscribe: <https://feeds.example.com/u/TOKEN>, <mailto:courier@yourdomain.com?subject=unsubscribe>
    List-Unsubscribe-Post: List-Unsubscribe=One-Click
    ```
  - Gmail、Apple Mail、Yahoo 等客户端会展示原生**退订**按钮，点击后无需登录即可幂等发起 `POST /u/{token}`（携带 `List-Unsubscribe=One-Click`，记录 `revoked_method = "one_click"`）。
  - 点击邮件页脚退订链接会进入 **`GET /u/{token}`** 只读确认页（仅显示打码邮箱如 `a***e@example.com`），点击**确认退订**后发起 `POST /u/{token}`（记录 `revoked_method = "link"`）。
- **未配置 `PUBLIC_URL` 时**：
  - WebUI 在 SMTP 通道显示黄色提示横幅；邮件头降级为仅包含 `mailto:` 退订，页脚提示「回复本邮件即可退订」，收件人验证使用 6 位验证码完成。
- **CAN-SPAM 合规页脚**：每封邮件底部均展示订阅来源说明（「你收到本邮件是因为订阅了 …」）、退订方式以及账号上可选配置的 `sender_identity` 发件人身份/联系地址。

### 反向代理配置示例：仅对外暴露 `/u/` 与 `/v/`

开启 RFC 8058 一键退订与确认链接**不需要**将 FeedCourier 的 WebUI、`/api/v1/*` 或 `/mcp` 暴露到公网。可在公网入口通过 Nginx 或 Caddy **仅反代 `/u/` 与 `/v/` 路径**，其余管理路径直接拒绝访问（仅保留在局域网或 VPN 内访问）。

> [!IMPORTANT]
> **在反向代理后务必开启 `TRUST_PROXY=true`**
> 默认情况下（`TRUST_PROXY=false`），FeedCourier 仅信任底层 TCP 连接的对端地址（`ConnectInfo<SocketAddr>`）对 `/u/` 与 `/v/` 实施按 IP 限流，忽略客户端自带的代理头，防止外部伪造 `X-Forwarded-For` 绕过限流。部署在 Nginx 或 Caddy 后方时，请在 FeedCourier 环境中设置 **`TRUST_PROXY=true`**，并由反向代理覆写 `X-Real-IP`（`$remote_addr`）或向 `X-Forwarded-For` 追加客户端 IP。开启 `TRUST_PROXY=true` 后，FeedCourier 仅读取 `X-Real-IP` 或 `X-Forwarded-For` **最右侧**由代理追加的一跳，有效抵御客户端伪造左侧头部。确认页表单采用相对路径 `action=""` 以兼容子路径 `PUBLIC_URL`，空路径 `/u/`、`/v/` 直接返回公开 `404` 而不会穿透至 WebUI SPA。

#### Nginx 示例

```nginx
# 请同步在 FeedCourier 容器或进程中设置 TRUST_PROXY=true
server {
    listen 443 ssl http2;
    server_name feeds.example.com;

    # TLS 证书配置略

    # 仅放行公开 SMTP 合规端点（RFC 8058 退订 + 双重确认）
    location ~ ^/(u|v)/ {
        proxy_pass http://127.0.0.1:8080;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
    }

    # 隐藏 WebUI、/api/v1/* 与 /mcp，禁止公网访问
    location / {
        return 404;
    }
}
```

#### Caddy 示例

```text
# 请同步在 FeedCourier 容器或进程中设置 TRUST_PROXY=true
feeds.example.com {
    @public_smtp path /u/* /v/*
    handle @public_smtp {
        reverse_proxy 127.0.0.1:8080 {
            header_up X-Real-IP {remote_host}
        }
    }

    handle {
        respond 404
    }
}
```

### 同步硬退信分类、自动暂停与重验证恢复、旧版升级回填

- **收件人级 5xx 硬退信 vs. 账号/配额/认证/内容级 5xx**：
  - **收件人级硬退信**：仅当 SMTP 服务器明确返回收件人永久拒收错误——增强状态码 `5.1.1`、`5.1.2`、`5.1.3`、`5.1.6`、`5.1.10` 或指向收件人邮箱不存在/拒收的 `550`/`551`/`553`——才会将该收件人标记为 **`bounced`**（`revoked_method = "hard_bounce"`，`bounce_diagnostic` 与 `diagnostic_log` 中的邮箱均自动脱敏）并继续投递其余已验证收件人。
    - 若同目的地下其他收件人发送成功，任务记为 **`delivered`** 并在 `diagnostic_log` 附带脱敏警告；
    - 若**全部**已验证收件人均硬退信，任务转入 **`dead_letter`**。
  - **账号 / 认证 / 配额 / 内容级 5xx（`530`、`535`、`552`、`554`、`5.4.5`、`5.7.*`）**：因登录认证失败、邮件体积超限、发件配额耗尽或 SPF/DKIM/DMARC/内容策略拒收导致的 5xx 会立即中止本次投递并作为通道级错误返回（保留 `checkpoint_json.sent_recipients` 中已送达记录），**绝不会误伤已验证订阅者的状态**。
  - 临时 `4xx` 错误仍按常规指数退避重试，并基于 checkpoint 跳过已成功送达的地址。
- **无可用收件人与 `auto_paused` 自动恢复**：
  - 若无 `verified` 地址但仍存在 `unverified` / `pending` 地址，任务进入 **`deferred`**（原因「等待收件人验证」，1 小时后复查；一旦有地址验证通过即刻唤醒）；
  - 若**全部**地址均为 `unsubscribed` 或 `bounced`，系统会自动将目的地置为 **`enabled = false, auto_paused = true`** 并顺延剩余任务，停止无效发送；
  - 此后只要有任一收件人重新完成验证（6 位验证码或 `/v/{token}`），处于 **`auto_paused = true`** 的目的地会自动恢复为 **`enabled = true, auto_paused = false`** 并唤醒延迟任务；而管理员手动停用目的地（或手动切换 `enabled` 时清除 `auto_paused = false`）时，系统不会擅自恢复启用。
- **重新启用须重新验证**：管理员对已退订或已退信地址点击**重新启用**（`POST .../reactivate`）时，其状态重置为 **`unverified`**，必须再次完成验证后才会恢复投递。验证码累计输错 5 次会自动清空当前有效验证码，使此前发出的 `/v/{token}` 链接立即返回 `410 Gone`。
- **从 `v0.2.x` 升级的旧地址回填（Legacy Backfill）**：升级至 `v0.3.0` 首次启动时，只要存量数据库已执行迁移 `0002` 且任意业务表（`feeds`、`channels`、`channel_destinations`）非空，`Database::connect` 会先在执行迁移 `0003` 前自动生成在线热备 `<DATA_DIR>/pre-v0.3.0-backup.db`；随后 `backfill_legacy_smtp_recipients` 自动将存量 SMTP 目的地中的既有收件地址回填为 `status = 'verified', consent_method = 'legacy'`（通过 `system_settings.smtp_recipient_model_version = 1` 保证幂等），确保已有个人订阅平滑续跑不中断。

---

## 4. 发件人地址对齐（`Force-From`）与标准邮件头

FeedCourier 始终使用 **Force-From** 发件人对齐策略，确保通过 SPF、DKIM 与 DMARC 校验：
- **发件人邮箱地址固定**：`From:` 地址始终为你配置的 `from_address`；
- **动态显示名称与合规头**：`From:` 显示名为 `"<订阅源标题> via FeedCourier"`，并携带完整标准邮件头：

```http
From: "Rust Blog via FeedCourier" <courier@yourdomain.com>
To: reader@example.com
Subject: Announcing Rust 1.94.0
List-Id: <dest-5.yourdomain.com>
Auto-Submitted: auto-generated
List-Unsubscribe: <https://feeds.example.com/u/TOKEN>, <mailto:courier@yourdomain.com?subject=unsubscribe>
List-Unsubscribe-Post: List-Unsubscribe=One-Click
```

---

## 5. 投递邮件效果与客户端归档

- **响应式单栏 HTML 与纯文本回退**：最大宽度 `640px` 的单栏卡片配合 78 列纯文本版本（`multipart/alternative`），展示文章标题、来源、作者、按**路由时区**转换的本地发布时间（如 `2026-11-08 11:00 (UTC+08:00)`）、原文链接与合规页脚。
- **图片、图注、表格与代码块自适应**：大尺寸固定像素宽度的 `<img>` 自动改写为适配 `592px` 内宽的响应式样式（Outlook 兼容的 `width="592"`，保留本身更窄的像素宽度，移除固定 `height` 并内联 `max-width:100%;height:auto`）。对于百分比宽度，小数百分比保留在内联 CSS 中（例如 `style="width:33.3%;max-width:100%;height:auto;..."`），同时 HTML `width` 属性输出按 `592px` 内宽折算的整数像素（如 `width="197"`）；`0%` 保持为 `0` 不拉伸，正数且小于 `1%` 的宽度（如 `0.004%`）下限钳制为 `1%` 以防图片消失。属性解码遵循分号感知的 HTML5 规范，同时规范 `<figure>`/`<figcaption>` 边距，并对宽 `<table>` 与 `<pre>` 启用 `max-width:100%` 滚动与折行兜底。
- **防邮件截断折叠**：超长 HTML 正文自动在安全边界处精简并附上「继续阅读原文 →」链接，避免触发 Gmail `102 KB` 折叠阈值。
- **收件箱自动归档（`List-Id`）**：可在 Gmail、Apple Mail、Fastmail 或 Thunderbird 中按 `List-Id` 或发件人名称自动归档。

---

## 延伸阅读

- 关于 MIME 组装、HTML 属性改写、HMAC 令牌构造及 SMTP 错误分类细节，请参阅 [通道协议与投递实现](/zh/reference/channel-protocols) 与 [安全与防注入机制](/zh/reference/security)。
