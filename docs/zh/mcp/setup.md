# 客户端接入与安全说明

FeedCourier 在同一个 HTTP 监听端口（默认 `0.0.0.0:8080`）上提供了符合 Model Context Protocol 规范的 **Streamable HTTP 接口（`/mcp`）**。

你无需额外部署中转脚本或独立服务，只需在 WebUI 中生成一个 Bearer 令牌，即可在 **Claude Desktop**、**Cursor**、**Windsurf**、**Antigravity** 等支持 MCP 的客户端中，通过自然语言查看订阅状态、排查失败任务、预览抓取结果或管理订阅源与路由规则。

---

## 1. 在 WebUI 中生成 MCP 令牌

1. 登录 FeedCourier WebUI，进入 **Settings → Native MCP Server (`/mcp`)**。
2. 点击 **Issue Token**，填写令牌备注名称（如 `cursor-macbook` 或 `claude-readonly`），并选择权限范围（Scope）：

| 权限范围（Scope） | 可调用的工具 | 适用场景 |
| :--- | :--- | :--- |
| **`readonly`（只读权限）** | `list_feeds_status`<br>`inspect_failed_deliveries`<br>`preview_feed_latest` | 仅用于查看订阅源状态、查看投递失败诊断日志以及试抓预览新订阅源，无法修改任何配置或数据。 |
| **`admin`（管理权限）** | 全部 `readonly` 工具 **+** <br>`manage_feed`<br>`configure_route`<br>`reset_and_poll_feed`<br>`retry_dead_tasks` | 除查看与预览外，还可通过自然语言添加/修改/暂停/删除订阅源、配置路由过滤规则、立即触发拉取以及重投失败任务。 |

3. 点击生成后，界面会显示以 `fc_mcp_` 开头的明文令牌，以及自动填好当前服务地址的客户端配置 JSON。

> [!IMPORTANT]
> **明文令牌仅在创建时显示一次**：数据库中仅保存令牌的 SHA-256 哈希值与前 15 位前缀（如 `fc_mcp_a1b2c3d4`），不保存明文。如果令牌遗失或不再使用，可在 WebUI 列表中点击 **Revoke（吊销）** 并重新生成。

---

## 2. 客户端连接配置

### Cursor / Windsurf / Antigravity（原生 Streamable HTTP）

在客户端的 MCP 配置文件（如 `~/.cursor/mcp.json` 或 `~/.gemini/antigravity/mcp_config.json`）中添加：

```json
{
  "mcpServers": {
    "feedcourier": {
      "type": "streamable-http",
      "url": "http://127.0.0.1:8080/mcp",
      "headers": {
        "Authorization": "Bearer fc_mcp_your_token_here"
      }
    }
  }
}
```

> 如果你的 FeedCourier 部署在远程服务器并配置了 HTTPS 反向代理，请将 `url` 替换为对应的公网或内网地址（例如 `"https://feedcourier.example.com/mcp"`）。

### Claude Desktop（通过 `mcp-remote` 桥接）

如果所使用的桌面客户端版本仅支持通过本地命令行（`stdio`）方式设置自定义请求头，可以使用 `mcp-remote` 桥接至 `/mcp` 端点：

```json
{
  "mcpServers": {
    "feedcourier": {
      "command": "npx",
      "args": [
        "-y",
        "mcp-remote",
        "http://127.0.0.1:8080/mcp",
        "--header",
        "Authorization: Bearer fc_mcp_your_token_here"
      ]
    }
  }
}
```

### 使用 `curl` 验证连通性

你也可以在终端使用 `curl` 发送 `initialize` 请求，测试令牌与 `/mcp` 端点是否正常工作：

```bash
curl -i -X POST http://127.0.0.1:8080/mcp \
  -H "Authorization: Bearer fc_mcp_your_token_here" \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -d '{
    "jsonrpc": "2.0",
    "id": 1,
    "method": "initialize",
    "params": {
      "protocolVersion": "2025-03-26",
      "capabilities": {},
      "clientInfo": { "name": "curl-check", "version": "1.0.0" }
    }
  }'
```

---

## 3. 安全说明

由于订阅源内容来自外部网站，将 AI 助手接入订阅服务时需要防范恶意文章内容诱导 AI 执行未授权操作。FeedCourier 在 MCP 接口层采取了以下三项隔离措施：

1. **权限分级（`readonly` 与 `admin`）**：
   - `readonly` 令牌仅可调用状态查询、失败任务诊断和试抓预览三个只读工具，任何写操作调用均会被服务端直接拒绝。
   - `admin` 令牌可额外执行订阅源增删改、路由规则配置、重置拉取与重投失败任务。日常如果只需要让 AI 帮忙检查运行情况，建议优先使用 `readonly` 令牌。
2. **通道与收件地址隔离**：
   - 推送通道的密钥（SMTP 密码、Telegram `bot_token`、Notion `integration_secret`）和收件地址（邮箱地址、`chat_id`、Notion 数据库 ID）**只能在 WebUI 中由管理员创建和修改**。
   - MCP 接口不提供任何创建或修改推送通道的工具，也不会向 AI 客户端返回任何密钥或收件地址（AI 只能看到通道的数字 `id`、名称和类型，并通过数字 `channel_id` 将路由绑定到已有通道），从而避免数据被导向未知的外部收件地址。
3. **防提示词注入与内网隔离**：
   - 外部订阅源的标题、作者、摘要和上游报错信息在通过 MCP 返回给 AI 前，都会移除控制字符、限制长度并包裹在 `<untrusted_feed_content>` 标签内，同时不包含原始 HTML，防止网页或文章中的恶意文本诱导 AI 执行非预期操作。
   - 所有通过 MCP 发起的订阅源试抓和添加请求均经过连接层内网地址拦截保护，默认阻止访问本机环回地址、局域网私有网段与云实例元数据地址。

> 关于加密存储、`<untrusted_feed_content>` 净化流程以及连接层防 SSRF 的具体实现细节，请参阅架构手册中的 [安全与防注入机制](/zh/reference/security)。
