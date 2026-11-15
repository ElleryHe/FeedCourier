---
layout: home

hero:
  name: "FeedCourier"
  text: "轻量级纯血 Rust 订阅流投递网关"
  tagline: "空闲内存 <20 MB · 单文件 SQLite WAL · 实时桥接 SMTP 邮件、Telegram Bot API 10.4 与 Notion 2026-03-11 · 原生内嵌 React 19 控制台与 /mcp 服务端"
  image:
    light: /logo-light.svg
    dark: /logo-dark.svg
    alt: FeedCourier
  actions:
    - theme: brand
      text: 快速开始
      link: /zh/guide/quick-start
    - theme: alt
      text: 设计理念与架构
      link: /zh/guide/introduction
    - theme: alt
      text: GitHub 仓库
      link: https://github.com/ElleryHe/FeedCourier

features:
  - title: 阅后即焚投递网关
    details: 传承 rss2email 单篇投递哲学，不做臃肿在线阅读器。SQLite WAL 永久保留 64 位 SHA-256 指纹防重，正文与媒体元数据在保留窗口（默认 24h）后由零引用计数 NOT EXISTS GC 自动回收。
    link: /zh/guide/introduction
    linkText: 了解四大设计边界
  - title: 防风控匀速步进发件箱
    details: 全通道统一按 1 Task = 1 Entry × 1 Route 实时入队，通过最小发信间隔、随机抖动与小时/日配额护栏削峰填谷；共用同一发信账号、Bot Token 或 Notion Secret 的通道自动共享限流锁。
    link: /zh/channels/pacing-and-quotas
    linkText: 查看步进流控与配额
  - title: SMTP 精品单栏阅读信纸
    details: 基于 minijinja 渲染精调内联 CSS 单栏响应式信纸（严控 <95 KiB 远离 Gmail 102 KB 截断线），保留 Force-From 保障 SPF/DKIM/DMARC 送达率，内置确定性 Message-ID 与按源 List-Id 归类。
    link: /zh/channels/smtp
    linkText: 配置 SMTP 邮件通道
  - title: Telegram Bot API 10.4 三模投递
    details: 路由级自由切换富文档模式（基于自研 telegram-rich 的 sendRichMessage）、快讯卡片模式与三行精炼媒体模式，支持防盗链内存零落盘流式中转、多图断点续投及旧版自建服务端自动降级。
    link: /zh/channels/telegram
    linkText: 配置 Telegram 通道
  - title: Notion 2026-03-11 & Markdown v2
    details: 全面对齐 Notion-Version 2026-03-11 数据源架构，支持父页面一键初始化 7 列标准表或绑定补齐已有 data_source_id，内置代码块动态反引号扩充、allow_async 异步长文写入与 FC-ID 有效一次防重对账。
    link: /zh/channels/notion
    linkText: 配置 Notion 数据源
  - title: 原生 /mcp 服务端与纵深防御
    details: 内嵌基于 rmcp 的 Streamable HTTP /mcp 端点与 readonly / admin 双级令牌；通道密钥与收件终点硬锁死在 WebUI 中，外部正文包裹 <untrusted_feed_content> 防提示词注入，出站请求全链路连接层防 SSRF。
    link: /zh/mcp/setup
    linkText: 接入 AI 客户端
---

## 整体数据流向速览

```mermaid
flowchart LR
    subgraph Ingest["受保护抓取与清洗管线"]
        F["RSS / Atom / JSON Feed"] --> HC["连接层防 SSRF 客户端"]
        HC --> R["按需 Readability 全文提取 (dom_smoothie)"]
        R --> D["四级防漂移 SHA-256 去重 & Ammonia 清洗"]
    end

    subgraph Storage["SQLite WAL 单卷存储 (/data)"]
        D --> E[("entries (永久指纹 + 阅后即焚正文)")]
        E --> T[("delivery_tasks (租约 + Checkpoint 发件箱)")]
        GC["零引用计数 NOT EXISTS GC"] -.-> E
    end

    subgraph Targets["匀速步进投递通道"]
        T --> SMTP["SMTP 邮件 (Force-From / 响应式内联信纸)"]
        T --> TG["Telegram Bot API 10.4 (富文档 / 快讯 / 媒体)"]
        T --> NT["Notion 2026-03-11 (数据源 + Markdown v2)"]
    end

    UI["内嵌 React 19 WebUI"] <--> Storage
    MCP["Streamable HTTP /mcp (readonly | admin)"] <--> Storage
```

## 30 秒 Docker 一键部署

```bash
docker run -d \
  --name feedcourier \
  --restart unless-stopped \
  -p 8080:8080 \
  -v "$(pwd)/data:/data" \
  elleryhe/feedcourier:latest
```

启动后浏览器访问 `http://127.0.0.1:8080` 即可引导设置管理员口令，进入内嵌 React 19 控制台配置授信通道、导入 OPML 订阅源或签发 `/mcp` 令牌。更多部署方式请查阅[快速开始](/zh/guide/quick-start)。
