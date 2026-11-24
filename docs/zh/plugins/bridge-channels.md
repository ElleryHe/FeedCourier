---
description: "在 FeedCourier (v0.3.1+) 中安装与配置进程外 HTTP 桥接通道插件，含零依赖 ntfy 参考插件部署教程。"
---

# 桥接通道插件（HTTP Bridge）

自 **`v0.3.1`** 起，FeedCourier 支持**进程外 HTTP 桥接插件**（`runtime: "bridge"`，基于 Plugin Protocol v1 增量扩展）。管理员与开发者无需修改核心二进制，即可按需接入自定义投递通道（如 **ntfy**、Pushover、Slack、Matrix、Gotify 或内部 Webhook）。

> [!IMPORTANT]
> FeedCourier 会将渲染后的文章内容（标题、摘要、HTML/纯文本正文与链接）直接发送至你登记的桥接 sidecar 服务。桥接服务的可信度由登记它的管理员负责；FeedCourier 负责 HMAC-SHA256 请求签名、精确 `host:port` 放行范围、禁止重定向与硬超时边界。

---

## 安全与网络隔离边界

- **HMAC-SHA256 双向签名校验**：FeedCourier 发出的每个请求均携带 `X-FeedCourier-Timestamp` 与 `X-FeedCourier-Signature: v1=<hex(HMAC-SHA256(secret, "{ts}.{body}"))>`，重放时间窗为 `±300s`。
- **精确 `host:port` 直连隔离**：桥接客户端使用独立的 HTTP 客户端（不走全局代理、`redirect::Policy::none()`、响应上限 `1 MiB`），仅直连你登记的那一个 `(scheme, host, port)`。无需把 Docker sidecar 加入 `SSRF_ALLOWLIST`，其他未登记的内网/私网地址对订阅源抓取仍然严格拦截。
- **凭据静态加密**：桥接共享密钥（`plugins.encrypted_bridge_secret`）与通道账号、目的地中的敏感字段均使用 `MasterKey`（ChaCha20-Poly1305）加密落盘。

---

## 快速上手：部署官方参考 `ntfy` 通道插件

官方仓库 **[`ElleryHe/FeedCourier-Plugins`](https://github.com/ElleryHe/FeedCourier-Plugins)** 在 `channels/ntfy/` 下提供了一个基于 Node.js 24、零外部依赖的 `ntfy` 通道参考实现。

### 1. 通过 Docker Compose 启动桥接 Sidecar

将 `feedcourier-ntfy` 与 `feedcourier` 放在同一个 Compose 网络中：

```yaml
services:
  feedcourier:
    image: elleryhe/feedcourier:0.3.1
    ports:
      - "8080:8080"
    volumes:
      - feedcourier_data:/data
    stop_grace_period: 30s
    restart: unless-stopped

  feedcourier-ntfy:
    build: https://github.com/ElleryHe/FeedCourier-Plugins.git#main:channels/ntfy
    environment:
      BRIDGE_SECRET: "replace-with-a-long-random-secret"
      PORT: "8787"
    restart: unless-stopped
```

### 2. 在 WebUI 中登记桥接插件

1. 进入 **插件中心** → 点击 **添加桥接插件**。
2. 填写：
   - **桥接服务地址（Base URL）**：`http://feedcourier-ntfy:8787`
   - **共享 HMAC 密钥**：与 sidecar 容器中配置的 `BRIDGE_SECRET` 保持一致。
3. 点击 **拉取并预览 Manifest**（`POST /api/v1/plugins/bridges`，`dry_run: true`），确认签名校验通过、协议兼容（`api_version: "1"` 与 `min_core`）及声明的字段 Schema。
4. 点击 **确认登记**。

### 3. 新建插件通道账号与目的地

1. 进入 **投递渠道** → 点击 **新建账号**。
2. 在渠道类型中选择 **ntfy Push**（启用后自动与 Telegram / SMTP / Notion 并列展示）。
3. 填写由 Schema 动态生成的**账号字段**（`server_url`、可选 `access_token`）与**初始目的地字段**（`topic`、可选 `priority` 与 `tags`）。FeedCourier 会同时执行本地 Schema 校验与远端 `POST /validate` 校验。
4. 点击账号或目的地上的 **测试** 按钮验证连通性，随后即可在路由中绑定投递。

---

## 生命周期管理

- **刷新 Manifest**：升级 sidecar 镜像后，点击插件卡片上的 **刷新桥接 Manifest**（`POST /api/v1/plugins/{id}/refresh-manifest`）即可同步最新版本与字段 Schema，无需重新输入密钥。
- **停用保护**：全局停用某个通道插件时，发往该插件的待投递任务会自动顺延（`deferred`），不消耗重试次数也不会误入死信。
- **级联删除保护**：删除仍被通道账号或路由引用的桥接插件会返回 **`409 Conflict`** 并列出引用的通道与路由清单，需显式确认级联删除（`?cascade=true`）。
