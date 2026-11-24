---
description: "FeedCourier 插件框架概览与实时插件市场索引：支持官方内置 Rust 插件与进程内纯 Rust JS 沙箱插件（boa_engine，一键安装 ntfy / Bark 等通道与处理器）。"
---

# 插件概览与市场

FeedCourier 提供基于 **Protocol v1（`api_version: "1"`）** 的统一插件框架，同时支持**官方内置 Rust 插件**（`v0.3.0+`）与**进程内沙箱 JavaScript 插件（`boa_engine`，`v0.3.1+`）**，涵盖自定义投递通道（`kind: "channel"`）、渲染前处理（`kind: "processor"`，`on_render`）以及投递后持久化回调（`on_delivered`）——全部运行在单一主程序二进制内，**无需任何 sidecar 容器**。

```mermaid
flowchart LR
    Entry[("入库原文 Entry\n(canonical_url 与 HTML 保持原样)")] --> Hook["on_render 钩子链\n(按 RouteOptions.plugin_order 排序)"]
    Hook --> Overrides["RenderOverrides\n(经 ammonia 清洗的 HTML / 链接 / 告警)"]
    Overrides --> Channels["通道适配器\n(SMTP / Telegram / Notion / JS 插件通道)"]
    Channels --> Delivered["持久化 plugin_events\n(on_delivered 至少一次推送)"]
```

---

## 插件架构概览

- **双运行时共用 Protocol v1**：
  - **内置插件（`runtime: "builtin"`）**：第一方 Rust 插件直接编译进单文件二进制（例如 [链接替换 (YOURLS)](/zh/plugins/link-rewrite)）。
  - **进程内 JS 沙箱插件（`runtime: "js"`，`v0.3.1+`）**：单文件 `plugin.js` ES 模块在纯 Rust `boa_engine` 沙箱中执行，支持在 WebUI 插件市场**一键安装与更新**（强制 SHA-256 校验）或手动上传自定义 `.js` 文件。可实现自定义**投递通道**（`deliver`，如官方 **ntfy** 与 **Bark**）、**渲染处理插件**（`onRender`，返回 HTML 强制经 `ammonia` 清洗）或**投递完成回调**（`onDelivered`）。
- **严格沙箱与网络隔离**：JS 插件无文件系统、Shell 或环境变量权限；通过 `fc.fetch()` 发起的 HTTP 请求严格限制在插件声明的 `allowed_hosts` 及管理员填写的 URI 主机范围内，统一经过连接层防 SSRF 校验，并支持在插件通道账号上开启 `use_proxy` 走全局出站代理访问公网域名。
- **路由级配置与执行顺序（`RouteOptions.plugins` 与 `plugin_order`）**：在路由编辑器中按需绑定处理插件，并通过上下箭头调整多插件执行顺序。每个插件独立计时（单插件 15 秒超时），非严格模式报错仅记告警、不阻断投递。
- **持久化 `on_delivered` 回调（`plugin_events`）**：在任务标记为已投递的同一 SQLite 事务内写入事件，由独立调度循环携带幂等键推送，支持最多 5 次指数退避重试。
- **兼容性校验与卸载保护**：
  - 市场条目与自定义上传脚本均按 `api_version` 与 `min_core` 自动校验版本兼容性。
  - 卸载仍被引用的插件或删除被路由引用的服务商实例均返回 **`409 Conflict`** 并列出引用它的通道与路由清单（卸载插件支持 `?cascade=true` 级联删除）。

---

## 插件市场

下方目录在文档站构建时直接拉取官方 **[`ElleryHe/FeedCourier-Plugins`](https://github.com/ElleryHe/FeedCourier-Plugins)** 仓库的 `index.json` 生成；若处于离线环境或远端不可达，则自动回退到主仓库内置快照 `src/plugins/market_snapshot.json`。

<PluginMarketplace locale="zh" />

### 运行时市场索引加载机制

在 WebUI 打开 **插件中心 → 插件市场** 标签页（`GET /api/v1/plugins/market`）时：

- 后端通过连接层防 SSRF HTTP 客户端拉取 `https://raw.githubusercontent.com/ElleryHe/FeedCourier-Plugins/main/index.json`（上限 `1 MiB`，超时 `8s`），并在内存中缓存 **6 小时**。
- 可通过环境变量 [`PLUGIN_INDEX_URL`](/zh/guide/configuration#环境变量说明) 指定自托管镜像或内网索引地址。
- 在离线或无法访问 GitHub 的环境中，系统会自动回退到编译内置快照（`source: "snapshot"`）。
- 内置插件（`kind: "builtin"`）可一键启用；JS 插件（`runtime: "js"`）会展示声明的 **允许访问域名（`allowed_hosts`）** 标签，点击即可进入带 SHA-256 校验、版本切换与变更日志的 **一键安装 / 更新** 弹窗；不兼容当前核心版本的条目会标明最低版本要求（`min_core`）并禁用安装。

---

## 下一步阅读

- **[JS 插件（ntfy 与 Bark）](/zh/plugins/js-plugins)**：一键安装并配置 **ntfy** 与 **Bark** 推送通道、导入自定义 `.js` 插件、查看源码与了解沙箱安全边界。
- **[链接替换 (YOURLS)](/zh/plugins/link-rewrite)**：配置自托管 YOURLS 短链服务商、鉴权模式、`SSRF_ALLOWLIST` 内网放行、缓存与各渠道行为说明。
- **[插件开发手册](/zh/plugins/development)**：Plugin API v1（`plugin.js`）模块导出规范、`globalThis.fc` 宿主工具函数、沙箱限制、`node --test` 单元测试与向 [`ElleryHe/FeedCourier-Plugins`](https://github.com/ElleryHe/FeedCourier-Plugins) 提交 PR 指南。
