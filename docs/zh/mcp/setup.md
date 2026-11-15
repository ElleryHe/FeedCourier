# MCP 客户端接入配置

FeedCourier 在同一个 HTTP 监听端口（默认 `0.0.0.0:8080`）上原生挂载了符合最新 Model Context Protocol 规范的 **Streamable HTTP 服务端（`/mcp`）**（基于官方 Rust SDK `rmcp` 构建）。

无需额外部署 Node.js/Python 包装脚本或独立容器，你只需在 WebUI 中签发一枚 Bearer Token，即可在 **Claude Desktop**、**Cursor**、**Windsurf**、**Antigravity** 或任何兼容 Streamable HTTP 的 AI 客户端中，用自然语言接管日常订阅巡检、死信排障、全文提取试抓与路由编排。

---

## 1. 在 WebUI 中签发与管理 MCP 令牌

1. 登录 FeedCourier WebUI，进入 **Settings → Native MCP Server (`/mcp`)**。
2. 点击 **Issue Token**，填写令牌备注名称（如 `cursor-macbook` 或 `claude-readonly`），并选择权限范围（Scope）：

| 权限级别（Scope） | 允许调用的工具集 | 适用场景建议 |
| :--- | :--- | :--- |
| **`readonly`（只读巡检）** | `list_feeds_status`<br>`inspect_failed_deliveries`<br>`preview_feed_latest` | 日常只读状态查询、失败任务诊断、新源 URL 试抓与过滤规则预览。无法修改任何数据库状态。 |
| **`admin`（全权限编排）** | 全部 `readonly` 工具 **+** <br>`manage_feed`<br>`configure_route`<br>`reset_and_poll_feed`<br>`retry_dead_tasks` | 用自然语言新增/暂停/删除订阅源、维护标签、编排路由规则、解除熔断立即拉取以及批量重投死信任务。 |

3. 点击生成后，界面会展示以 `fc_mcp_` 开头的 **55 位明文令牌**及自动填充好当前访问地址的 JSON 配置片段。

> [!IMPORTANT]
> **明文令牌仅在创建瞬间展示一次**：后端数据库（`mcp_tokens` 表）仅存储令牌的 **SHA-256 哈希值（`token_hash`）** 与前 15 位前缀（`token_prefix`，如 `fc_mcp_a1b2c3d4`），绝不保存明文。若令牌遗失或怀疑泄露，请直接在 WebUI 列表中点击 **Revoke（吊销）** 并重新签发。每次 AI 客户端发起请求时，WebUI 列表中的 `last_used_at` 时间戳会自动更新。

---

## 2. 主流 AI 客户端配置示例

### Cursor / Windsurf / Antigravity（原生 Streamable HTTP）

在客户端的 `mcp.json`（或 `~/.cursor/mcp.json`、`~/.gemini/antigravity/mcp_config.json`）中添加：

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

> 若你的 FeedCourier 部署在远程服务器并配置了 HTTPS 反向代理（如 `https://feedcourier.example.com`），请将 `url` 替换为 `"https://feedcourier.example.com/mcp"`。

### Claude Desktop（通过 `mcp-remote` 桥接或原生远端连接）

如果所使用的本地桌面客户端版本仅支持 `stdio` 命令方式配置自定义 Header，可使用社区标准的 `mcp-remote` 零配置桥接至 Streamable HTTP 端点：

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

---

## 3. 连通性验证与 AGPLv3 元数据声明

连接建立时，客户端会向 `/mcp` 发送 JSON-RPC `initialize` 握手请求。FeedCourier 的 `McpServer::get_info` 会返回：

- **`serverInfo`**：`name: "feedcourier"`、`title: "FeedCourier"`、`version: "0.1.0"`、`websiteUrl: "https://github.com/ElleryHe/FeedCourier"`（满足 `AGPL-3.0-only` 第 13 条对网络远程交互服务源码获取途径的原生披露要求）；
- **`instructions`**：向 AI 模型注入强制性的防提示词注入指令与授信终点隔离准则（详见 [MCP 纵深安全边界](/zh/mcp/security)）。

你也可以在终端使用 `curl` 快速验证 `/mcp` 端点的鉴权与连通性：

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

- 当 Token 有效时，服务端返回 `200 OK` 及包含 `serverInfo` 与 `instructions` 的初始化响应；
- 当缺少 `Authorization` 头或 Token 已吊销时，中间件直接返回 `401 Unauthorized` JSON 错误。

---

## 4. 常用自然语言运维提示词（Prompts）

完成接入后，你可以直接对 AI 助手说：

- **日常巡检**：“*帮我查看一下 FeedCourier 当前的运行状态，有没有处于退避（backoff）或熔断（circuit_open）状态的订阅源？*”
- **试抓与全文对比**：“*帮我用 `preview_feed_latest` 试抓一下 `https://blog.rust-lang.org/feed.xml`，对比开启 `fetch_full_content` 前后的效果。*”
- **订阅与路由编排**：“*把上面那个 Rust 官方博客加到订阅源里，打上 `rust` 和 `programming` 标签，开启全文提取并立即试推最新 1 篇；然后检查是否已有匹配 `rust` 标签发往 Telegram 的路由。*”
- **故障自愈**：“*检查一下有没有死信（dead_letter）投递任务，告诉我失败原因，并把它们全部重新入队投递。*”
