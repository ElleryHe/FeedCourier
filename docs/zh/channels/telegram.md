---
description: "配置 FeedCourier Telegram 投递通道：单个 Bot 账号挂载多个私聊、群组、超级群组话题与频道目的地，支持升级后的富文本排版（含原生页脚与连图拼贴/幻灯片）、快讯与媒体模式。"
---

# Telegram

Telegram 通道支持将订阅文章推送到**个人私聊**、**普通群组**、**开启话题（Topics）的超级群组指定话题**以及**频道（Channel）**。在双层通道模型下，你只需添加一次 **Telegram Bot 账号**，即可在其下挂载多个不同的会话**目的地**；每条路由指向具体目的地后，可独立选择**富文本模式**、**快讯模式**或**媒体模式**，以及独立的投递语言与路由时区。

---

## 1. 获取凭据与会话 ID

### 第一步：通过 `@BotFather` 创建机器人并获取 `bot_token`

1. 在 Telegram 中打开 [@BotFather](https://t.me/BotFather)，发送 `/newbot` 命令。
2. 按提示设置机器人的显示名称和以 `bot` 结尾的用户名。
3. 创建成功后，`@BotFather` 会返回形如 `123456789:AAH...` 的 **`bot_token`**，请妥善保存。

### 第二步：将机器人加入目标会话并获取 `chat_id`

根据你希望接收推送的目标位置，完成对应设置（同一 Bot 可加入多个不同群组、频道或话题作为多个目的地）：

- **推送到个人私聊**：
  1. 在 Telegram 中搜索你的机器人并点击 **Start** 发送任意一条消息；
  2. 填写你自己的 Telegram 数字 User ID 作为 `chat_id`（可通过 `@userinfobot` 查询，或调用 `https://api.telegram.org/bot<TOKEN>/getUpdates` 查看返回结果中的 `chat.id`）。
- **推送到公开频道或公开群组**：
  1. 将机器人添加为频道或群组成员（如果是频道，需要授予机器人“发布消息 / Post Messages”管理员权限）；
  2. `chat_id` 可直接填写带 `@` 的公开用户名（例如 `@my_tech_channel`）。
- **推送到私密频道或私密群组**：
  1. 将机器人添加进频道或群组（频道需授予发布消息权限）；
  2. `chat_id` 填写以 `-100` 开头的数字 ID（例如 `-1001234567890`）。你可以通过将该频道/群组的一条消息转发给查询机器人，或在群内发送一条消息后查询 `getUpdates` 接口获取该 ID。
- **推送到超级群组的指定话题（Topic）**：
  1. 如果目标群组开启了多话题（Topics）功能，除了填写群组的 `chat_id`（`-100...`）外，还可以填写目标话题的整数 ID（**`message_thread_id`**）；
  2. 在 Telegram 桌面端或移动端复制该话题内的任意一条消息链接（形如 `https://t.me/c/1234567890/42/108`），中间代表话题编号的数字（如 `42`）即为 `message_thread_id`。

---

## 2. 在 WebUI 中配置 Bot 账号与目的地

进入 WebUI **Channels → New Channel → Telegram**，配置机器人账号并添加首个目的地（创建后可随时在同一 Bot 账号下继续添加更多会话或话题目的地）。

### Telegram 账号（Bot）字段

| 字段名 | 必填 | 默认值 | 说明 |
| :--- | :---: | :--- | :--- |
| `name` | 否 | *自动获取* | 账号显示名称。留空时自动调用 Telegram `getMe` 获取机器人的 `@username`。 |
| `bot_token` | 是 | — | 从 `@BotFather` 获取的机器人令牌（加密保存在数据库中，WebUI 仅展示脱敏提示）。 |
| `api_base_url` | 否 | `null` | 自定义 Bot API 服务地址（留空默认使用官方 `https://api.telegram.org`）。仅允许不含 userinfo、查询参数（query）或片段（fragment）的 `http://` / `https://` URL，且所有请求均强制执行连接期 `SsrfPolicy` 校验。 |

### Telegram 目的地字段（单 Bot 可添加多个）

| 字段名 | 必填 | 默认值 | 说明 |
| :--- | :---: | :--- | :--- |
| `name` | 否 | *自动获取* | 目的地备注名。留空时自动调用 Telegram `getChat` 获取群组、频道或对话标题。 |
| `chat_id` | 是 | — | 目标私聊 ID、群组 ID（`-100...`）或公开频道用户名（`@...`）。 |
| `message_thread_id` | 否 | `null` | 超级群组下的指定话题（Topic）整数 ID。留空则发送到默认主聊天流。 |
| `enabled` | 否 | `true` | 独立控制该群组/话题的启用或停用状态，不影响同一 Bot 下的其他目的地。 |

### 账号测试、目的地测试与共享限流

- **测试账号**：调用 `getMe` 验证 Token 并探测服务端是否支持 `sendRichMessage`，不会向任何群组发消息。
- **测试目的地**：向指定群组、频道或话题发送一条测试消息（使用当前 WebUI 语言）。
- **共享速率与配额**：同一 Bot 账号下的全部目的地自动共享 `min_interval_secs`、逐条随机抖动、`hourly_quota`、`daily_quota`、`use_proxy` 以及 Telegram `429 retry_after` 冷却。详见[发送频率、配额与过滤](/zh/channels/pacing-and-quotas)。

---

## 3. 三种路由投递模式及选择建议

在 **Routes** 页面创建或编辑指向 Telegram 目的地的路由时，可以在路由选项（`telegram_mode`）中选择以下三种投递模式之一：

| 模式名称 | 配置值 | 展示内容与特点 | 适用场景 |
| :--- | :--- | :--- | :--- |
| **富文本模式**（默认） | `rich_message` | 基于 `telegram-rich 0.0.2` 排版：纯白 `h3` 标题、原生灰字 `<footer>` 元信息、`<hr>` 分隔线、正文标题自动降级（`offset=3`）、连续多图自动合并为拼贴/幻灯片，并保留列表、紧凑表格、可折叠引用与代码块，文末附“阅读原文”按钮。 | 博客长文、技术文章、深度报道等需要完整排版的订阅源。 |
| **快讯模式** | `compact_card` | 采用社论卡片布局：顶部加粗标题 + `<blockquote>`（超长自动 `<blockquote expandable>` 折叠）摘要块 + 底部斜体「来源 · 作者 · 时间」+ 底部原生内联“阅读原文”按钮，并支持网页链接预览开关（`link_preview`）。 | 高频新闻快讯、GitHub Releases 更新、状态通知或群组简要提醒。 |
| **媒体模式** | `media_mode` | 提取文章中的图片、视频或音频附件，搭配固定三行简短说明发送。 | 摄影、插画、漫画、社交媒体动态、播客或视频订阅源。 |

### 各模式补充说明

- **富文本模式（`rich_message`）**：
  - **克制的层次化排版**：文章主标题采用无超链接的纯白 **`<h3>`**（避免 `<h1>`/`<h2>` 衬线字号过大及满屏蓝字刺眼）；标题下方通过 Telegram 原生小号灰字 `<footer>` 展示「来源 · 作者 · 按路由时区格式化的紧凑发布时间（如 `2026-11-08 11:00`）」；头部与正文之间以 `<hr>` 分隔线区隔，底部保留全宽“阅读原文”胶囊按钮。
  - **正文标题自动降级与连图合并**：借助 `telegram-rich 0.0.2`，正文内标题统一偏移 3 级降级（`h1→h4`、`h2→h5`、`h3+→h6`）；正文中连续出现的 2 张及以上图片会自动合并为 Telegram **拼贴（collage）**，超出拼贴数量上限时自动切换为**幻灯片（slideshow）**。
  - **超长截断与旧服务端降级**：当文章超过单条消息长度上限时，会在段落边界处自动截断并提示前往原文阅读。若 `api_base_url` 指向不支持 `sendRichMessage` 的旧版自建 Local Bot API Server，系统检测后会自动回退为**快讯模式**完成发送。
- **快讯模式（`compact_card`）**：
  - **社论摘要块与智能折叠**：正文摘要包裹在原生 `<blockquote>` 引用块中与顶部加粗标题清晰区隔；摘要与标题重复时自动省略；长度超过 `300` UTF-16 字符（上限 `800` 字符、按句/词边界优雅截断）时自动升级为 Telegram 原生可展开 `<blockquote expandable>`；底部以斜体展示「来源 · 作者 · 紧凑本地时间」，并附加与富文本视觉一致的原生内联 URL 按钮（“阅读原文” / “Read original”）。
  - **无痕防误解析与链接预览**：自动抑制摘要中 `/命令`、`@包名` 以及 `README.md`、`sku.go` 等文件名被 Telegram 误标蓝（真实 `http(s)://` 链接不受影响）；可在路由设置中通过 `link_preview` 决定是否展开原链接网页预览卡片。
- **媒体模式（`media_mode`）**：
  - **三行简短说明**：附带在媒体下方的文字固定为三行——第一行为作者名称（无作者时显示订阅源名称），第二行为提取出的正文第一句话，第三行为指向原文的 **`链接`**（英文环境下显示 **`Link`**，由路由的投递语言决定）超链接。若某篇文章不包含任何可用媒体，会自动回退发送同样三行格式的 HTML 文本消息。
  - **多图视自动分批**：当单篇文章包含超过 10 张图片或视频时，系统会自动按每组最多 10 项分批发送媒体组（Media Group）；若中途遇到平台限流，冷却结束后会从中断批次继续发送剩余媒体，不会重复发送已成功的批次。
  - **视频在线预览与防盗链内存代传**：图片与音频优先由 Telegram 直接拉取外链，遇源站防盗链（`Referer` 校验）时自动在内存中代拉并上传（图片上限 `10 MiB`，音视频上限 `50 MiB`）；视频及无明确后缀的 `<enclosure>` 附件会直接在内存中完成魔数嗅探与 MP4 宽高（含竖屏旋转矩阵校正）、时长元数据提取，携带 `supports_streaming: true` 上传，确保视频可在 Telegram 内直接在线播放而不会降级为文件附件。

> [!TIP]
> 想在保存路由前试看某种模式的效果？在**订阅源预览**窗口的 **Debug push** 栏选择 Telegram 目的地、模式、链接预览、语言与可选时区，即可把真实条目推送过去，且不会影响队列。参见[调试推送](/zh/channels/pacing-and-quotas#debug-push-from-the-feed-preview)。

---

## 4. 自建 Local Bot API Server 与 SSRF 校验 {#self-hosted-local-bot-api-server}

**所有** Telegram Bot API 请求（包括官方 `https://api.telegram.org` 以及自定义的 `api_base_url` / 自建 Local Bot API Server）均强制经过连接期 `SsrfPolicy` DNS IP 校验（共享可热更新的 `SSRF_ALLOWLIST`），并禁用自动 HTTP 重定向：

- `api_base_url` 仅允许 `http://` 或 `https://` 协议，禁止携带 userinfo（`user:pass@`）、查询字符串（`?…`）或片段（`#…`）；
- 创建、编辑或探测 Telegram 账号时，若目标地址被 SSRF 策略拦截，服务端将返回 HTTP `403`（`ssrf_blocked`）。

> [!WARNING]
> **升级注意：存量内网 Local Bot API 不会自动放行**
> 从 `v0.1.x` 升级至 `v0.2.0` 后，已配置的内网/私网 Local Bot API 地址**不会**被自动加入白名单。如果你的 `api_base_url` 指向环回地址、Docker 内部服务或私网地址（例如 `http://telegram-bot-api.internal:8081`），请务必通过环境变量或 WebUI **Settings** 将其主机名或 IP/CIDR 手动添加到 [`SSRF_ALLOWLIST`](/zh/guide/configuration#allowing-trusted-internal-services-ssrf-allowlist)。

---

## 延伸阅读

- 关于底层 Bot API 协议适配、`telegram-rich 0.0.2` 转换选项、长度与块数截断规则、防盗链流式上传限制以及分批发送断点恢复的技术细节，请参阅架构手册中的 [通道协议与投递实现](/zh/reference/channel-protocols)。
