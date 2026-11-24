---
description: "FeedCourier 插件框架概览与实时插件市场索引：支持内置 Rust 插件与进程外 HTTP 桥接（通道/处理/on_delivered）扩展。"
---

# 插件概览与市场

FeedCourier 提供基于 **Protocol v1（`api_version: "1"`）** 的统一插件框架，同时支持**官方内置 Rust 插件**（`v0.3.0+`）与**进程外 HTTP 桥接插件**（`v0.3.1+`），涵盖自定义投递通道（`kind: "channel"`）、渲染前处理（`kind: "processor"`，`on_render`）以及投递后持久化回调（`on_delivered`）。

```mermaid
flowchart LR
    Entry[("入库原文 Entry\n(canonical_url 与 HTML 保持原样)")] --> Hook["on_render 钩子链\n(按 RouteOptions.plugin_order 排序)"]
    Hook --> Overrides["RenderOverrides\n(经 ammonia 清洗的 HTML / 链接 / 告警)"]
    Overrides --> Channels["通道适配器\n(SMTP / Telegram / Notion / 桥接通道)"]
    Channels --> Delivered["持久化 plugin_events\n(on_delivered 至少一次推送)"]
```

---

## 插件架构概览

- **双运行时共用 Protocol v1**：
  - **内置插件（`runtime: "builtin"`）**：第一方 Rust 插件直接编译进单文件二进制（例如 [链接替换 (YOURLS)](/zh/plugins/link-rewrite)）。
  - **HTTP 桥接插件（`runtime: "bridge"`，`v0.3.1+`）**：由管理员登记的独立 sidecar HTTP 服务，采用 HMAC-SHA256（`±300s` 时间窗）签名认证，可实现自定义**投递通道**（`POST /deliver`）、**渲染处理插件**（`POST /hooks/on_render`，返回 HTML 强制经 `ammonia` 清洗）或**投递完成回调**（`POST /hooks/on_delivered`）。
- **路由级配置与执行顺序（`RouteOptions.plugins` 与 `plugin_order`）**：在路由编辑器中按需绑定处理插件，并通过上下箭头调整多插件执行顺序。每个插件独立计时（单插件 15 秒超时），非严格模式报错仅记告警、不阻断投递。
- **持久化 `on_delivered` 回调（`plugin_events`）**：在任务标记为已投递的同一 SQLite 事务内写入事件，由独立调度循环携带幂等键推送，支持最多 5 次指数退避重试。
- **兼容性校验与删除保护**：
  - 市场条目与桥接注册均按 `api_version` 与 `min_core` 自动校验版本兼容性。
  - 删除仍被引用的桥接插件或服务商实例返回 **`409 Conflict`** 并列出引用它的通道与路由清单（桥接插件支持 `?cascade=true` 级联删除）。

---

## 插件市场

下方目录在文档站构建时直接拉取官方 **[`ElleryHe/FeedCourier-Plugins`](https://github.com/ElleryHe/FeedCourier-Plugins)** 仓库的 `index.json` 生成；若处于离线环境或远端不可达，则自动回退到主仓库内置快照 `src/plugins/market_snapshot.json`。

<PluginMarketplace locale="zh" />

### 运行时市场索引加载机制

在 WebUI 打开 **插件中心 → 插件市场** 标签页（`GET /api/v1/plugins/market`）时：

- 后端通过连接层防 SSRF HTTP 客户端拉取 `https://raw.githubusercontent.com/ElleryHe/FeedCourier-Plugins/main/index.json`（上限 `1 MiB`，超时 `8s`），并在内存中缓存 **6 小时**。
- 可通过环境变量 [`PLUGIN_INDEX_URL`](/zh/guide/configuration#环境变量说明) 指定自托管镜像或内网索引地址。
- 在离线或无法访问 GitHub 的环境中，系统会自动回退到编译内置快照（`source: "snapshot"`）。
- 内置插件（`kind: "builtin"`）可一键启用；HTTP 桥接插件（`runtime: "bridge"`）展示容器镜像与 Docker Compose 片段，并可直接打开「登记桥接」弹窗；不兼容当前核心版本的条目会标明最低版本要求（`min_core`）并禁用安装。

---

## 下一步阅读

- **[桥接通道插件](/zh/plugins/bridge-channels)**：部署并登记进程外 HTTP 桥接投递通道（含零依赖 `ntfy` 参考插件）。
- **[链接替换 (YOURLS)](/zh/plugins/link-rewrite)**：配置自托管 YOURLS 短链服务商、鉴权模式、`SSRF_ALLOWLIST` 内网放行、缓存与各渠道行为说明。
- **[插件开发手册](/zh/plugins/development)**：Protocol v1（`api_version: "1"`）清单与 HTTP 桥接协议规范、HMAC-SHA256 签名、`channel-template` 模板与向 [`ElleryHe/FeedCourier-Plugins`](https://github.com/ElleryHe/FeedCourier-Plugins) 提交 PR 指南。
