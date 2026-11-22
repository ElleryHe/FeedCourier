---
layout: home
title: "FeedCourier"
titleTemplate: ":title — 自托管 RSS / Atom / JSON Feed 订阅投递服务"
description: "基于 Rust、SQLite 与 React 构建的自托管 RSS / Atom / JSON Feed 订阅投递服务，支持过滤并投递文章至 SMTP 邮件、Telegram 与 Notion，内置 Web 管理控制台与 MCP 服务端。"

hero:
  name: "FeedCourier"
  text: "RSS / Atom / JSON Feed 订阅投递服务"
  tagline: "基于 Rust、SQLite 与 React 构建，定时抓取并过滤订阅源文章，投递至 SMTP 邮件、Telegram 与 Notion，内置 Web 管理控制台与 /mcp 服务端。"
  image:
    light: /logo-light.svg
    dark: /logo-dark.svg
    alt: FeedCourier
  actions:
    - theme: brand
      text: 快速开始
      link: /zh/guide/quick-start
    - theme: alt
      text: 使用手册
      link: /zh/guide/introduction
    - theme: alt
      text: 架构手册
      link: /zh/reference/architecture

features:
  - title: 文章去重与定期清理
    details: 在 SQLite 中长期保留文章 SHA-256 指纹以避免重复投递；已完成投递的正文与媒体元数据在保留期（默认 24 小时）过后自动清理。
    link: /zh/guide/introduction
    linkText: 查看项目简介
  - title: 发送频率控制与路由级时区
    details: 按 1 Task = 1 Entry × 1 Route 独立入队，同一通道账号下的多个目的地共享最小间隔、逐条独立随机抖动、小时/日配额与 429 冷却，并支持按路由自定义 IANA 时区。
    link: /zh/channels/pacing-and-quotas
    linkText: 查看频率、配额与过滤
  - title: SMTP 邮件投递
    details: 使用响应式内联 CSS 邮件模板渲染正文（自动适配图片/表格宽度与本地时区时间），支持单发件账号挂载多组收件目的地、Force-From 发件人对齐与按源分类的 List-Id。
    link: /zh/channels/smtp
    linkText: 配置 SMTP 邮件
  - title: Telegram 多模式投递
    details: 单个 Bot 账号可挂载多个私聊、群组、话题或频道目的地；按路由选择富文本（原生页脚、标题降级、连图拼贴/幻灯片）、快讯或媒体模式，支持防盗链代传与 Local Bot API 降级。
    link: /zh/channels/telegram
    linkText: 配置 Telegram 通道
  - title: Notion 数据库同步
    details: 单个 Notion 集成可同步至多个数据库目的地，支持一键初始化 7 列标准数据库或绑定已有数据源，内置 Markdown v2 转换、长文异步创建及重复页面校验。
    link: /zh/channels/notion
    linkText: 配置 Notion 通道
  - title: 内置 Web 控制台与 /mcp 接口
    details: 单二进制内嵌 React 管理界面（支持订阅源标签筛选、排序记忆与分页、路由级联筛选、发件箱语义配色）与 Streamable HTTP /mcp 端点，支持双级令牌、凭据与目的地隔离加密及连接层防 SSRF。
    link: /zh/mcp/setup
    linkText: 配置 MCP 客户端
---

## 整体数据流向

```mermaid
flowchart LR
    subgraph Ingest["抓取与解析管线"]
        F["RSS / Atom / JSON Feed"] --> HC["连接层防 SSRF 客户端"]
        HC --> R["按需 Readability 全文提取"]
        R --> D["SHA-256 去重与 HTML 清洗"]
    end

    subgraph Storage["SQLite 单卷存储 (/data)"]
        D --> E[("文章记录与去重指纹")]
        E --> T[("投递任务队列")]
        GC["过期正文定期清理"] -.-> E
    end

    subgraph Targets["投递通道"]
        T --> SMTP["SMTP 邮件"]
        T --> TG["Telegram"]
        T --> NT["Notion"]
    end

    UI["内置 Web 控制台"] <--> Storage
    MCP["Streamable HTTP /mcp"] <--> Storage
```

## Docker 快速部署

```bash
docker run -d \
  --name feedcourier \
  --restart unless-stopped \
  -p 8080:8080 \
  -v "$(pwd)/data:/data" \
  -e TZ=Asia/Shanghai \
  elleryhe/feedcourier:latest
```

启动后在浏览器访问 `http://127.0.0.1:8080` 设置管理员用户名与密码，即可在 Web 控制台中配置推送通道、导入 OPML 订阅源或签发 `/mcp` 令牌。更多部署方式请参阅[快速开始](/zh/guide/quick-start)。
