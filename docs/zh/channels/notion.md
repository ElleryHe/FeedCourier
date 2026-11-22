---
description: "配置 FeedCourier Notion 投递通道：单个 Notion 集成账号挂载多个数据库目的地，将 RSS、Atom 与 JSON Feed 新文章写入页面属性并转换为 Notion 内容块。"
---

# Notion

Notion 通道可将订阅源的新文章自动同步到 Notion 数据库中，每篇文章生成一条数据库页面记录，同时写入文章元数据属性并将完整正文转换为 Notion 页面内容块。

在双层通道模型下，你只需保存一次 **Notion 集成账号**（`integration_secret`、代理、流速与配额），即可在其下挂载一个或多个**数据库目的地**（各自绑定不同的 `data_source_id` 与列映射，并支持单独启停与测试）。

---

## 1. 创建 Notion 内部集成并授权页面

在 WebUI 中配置 Notion 账号前，请先在 Notion 中完成以下两步准备：

1. **创建内部集成并获取密钥**：
   - 访问 [Notion Integrations 页面](https://www.notion.so/profile/integrations)，点击 **New integration** 创建一个内部集成（Internal Integration）。
   - 在 **Capabilities（权限）** 设置中，确认已勾选 **Read content**、**Update content** 与 **Insert content**。
   - 复制生成的 **`Internal Integration Secret`**（通常以 `ntn_` 或 `secret_` 开头）。
2. **在目标页面或数据库中添加集成连接**：
   - 在 Notion 中打开每个准备存放订阅文章的**父页面**或**已有数据库页面**。
   - 点击页面右上角的 `•••` 菜单 → 选择 **Connections（连接）** → 搜索并添加你刚才创建的集成。

> [!IMPORTANT]
> 如果未在目标页面或数据库的 `Connections` 菜单中添加该集成，Notion 接口会返回 `404 Could not find page/database` 错误。

---

## 2. 在 WebUI 中配置集成账号与数据库目的地

登录 WebUI，进入 **Channels → New Channel → Notion**。

### Notion 账号（集成）字段

| 字段名 | 必填 | 默认值 | 说明 |
| :--- | :---: | :--- | :--- |
| `name` | 否 | *自动获取* | 账号显示名称。留空时自动调用 Notion `/v1/users/me` 获取集成名称。 |
| `integration_secret` | 是 | — | Notion Internal Integration Secret（`ntn_...` 或 `secret_...`，加密保存，WebUI 仅展示脱敏提示）。 |

### 添加一个或多个数据库目的地

在 Notion 账号下可挂载多个**目的地**（留空目的地备注名时，自动获取对应的 **Notion 数据库标题**）。每个目的地支持以下两种配置方式：

#### 方式一：在父页面下一键新建数据库（推荐）

如果你还没有现成的阅读数据库，只需填入已授权的**父页面链接（或 32 位页面 ID）**，点击 **Initialize Standard Database**：

- 系统会在该父页面下自动创建一个包含 **7 个标准属性列**的数据库，并自动绑定其 `data_source_id` 到该目的地。
- 自动创建的 7 个标准属性列说明如下：

| 属性列名 | Notion 属性类型 | 是否必填 | 同步内容说明 |
| :--- | :---: | :---: | :--- |
| **`Title`** | `title` | **必填** | 文章标题。 |
| **`FC-ID`** | `rich_text` | **必填** | 文章的去重指纹哈希，用于在网络超时重试时检查页面是否已创建，避免产生重复页面。 |
| **`Link`** | `url` | 可选 | 去除追踪参数后的原文链接。 |
| **`Source`** | `select` | 可选 | 文章所属的订阅源名称。 |
| **`Published`** | `date` | 可选 | 文章原始发布时间。 |
| **`State`** | `select` | 可选 | 阅读状态（预设 `UnRead` 与 `Read` 两个选项，新同步的文章默认标记为 `UnRead`）。 |
| **`Tag`** | `multi_select` | 可选 | 自动同步该订阅源在 FeedCourier 中设置的标签（`tags`）。 |

#### 方式二：绑定已有数据库

如果你希望将文章同步到自己正在使用的现有 Notion 数据库中：

1. 选择 **Bind Existing Data Source**，填入已有数据库或数据源的 ID（也可以直接粘贴 Notion 数据库页面链接，系统会自动解析对应的数据源 ID）。
2. 点击 **Inspect & Repair Schema** 检查表结构：
   - **字段检查与映射**：系统会检查现有数据库的属性列。其中 **`Title`（标题类型）** 和用于防重的 **`FC-ID`（富文本类型）** 为必填项；其余 5 个属性（`Link`、`Source`、`Published`、`State`、`Tag`）均为可选，你可以将它们映射到现有的自定义列名，或留空跳过写入。
   - **一键补齐缺失列**：如果现有数据库中还没有 `FC-ID` 或其他标准列，可以直接在向导中勾选自动补齐，由系统自动在数据库中添加缺失的属性列。

### 账号测试、目的地测试与共享限流

- **测试账号**：调用 `GET /v1/users/me` 验证集成密钥有效性，不会创建页面。
- **测试目的地**：在目标数据库中创建一条验证页面，确认写入与列映射正常。
- **共享速率与配额**：同一 Notion 集成账号下的所有数据库目的地共享 `min_interval_secs`、逐条随机抖动、`hourly_quota`、`daily_quota`、`use_proxy` 以及 Notion `429` 冷却。详见[发送频率、配额与过滤](/zh/channels/pacing-and-quotas)。

---

## 3. 同步内容与页面排版说明

配置完成并绑定路由后，每当有符合条件的新文章到达时，FeedCourier 会向目标 Notion 数据库写入一条新记录：

- **页面属性与图标**：自动填入上述映射的属性字段，并将订阅源站点的图标（Favicon）设置为 Notion 页面图标。
- **页面正文转换**：将清洗后的文章 HTML 正文（若订阅源开启了 `fetch_full_content`，则为提取出的完整网页正文）转换为 Notion 页面块，保留多级标题、段落、有序/无序列表、引用块、代码块与超链接。
- **超长文章与防重处理**：当遇到块数过多或篇幅较长的文章时，系统会自动通过异步写入方式同步，并在超出 Notion 单页块数上限时在段落边界截断并附上原文链接；若投递途中遇到网络中断并触发重试，系统会先根据 `FC-ID` 检查是否已创建成功，不会产生重复页面。

---

## 延伸阅读

- 关于底层 Notion API 版本适配、HTML 转 Markdown 规范化处理、长文异步创建流程以及 `FC-ID` 防重对账的实现细节，请参阅架构手册中的 [通道协议与投递实现](/zh/reference/channel-protocols)。

> [!NOTE]
> 通过[调试推送](/zh/channels/pacing-and-quotas#debug-push-from-the-feed-preview)在 Notion 创建的页面，`FC-ID` 会写入带 `test-` 前缀的值，而不是文章内容哈希；这类页面不参与去重与重试对账。
