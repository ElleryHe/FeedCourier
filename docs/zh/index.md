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
  - title: 发送频率控制与配额管理
    details: 按 1 Task = 1 Entry × 1 Route 独立入队，支持配置最小发信间隔、随机抖动以及每小时/每日配额；共用同一发信账号、Bot Token 或 Notion 密钥的通道共享限流状态。
    link: /zh/channels/pacing-and-quotas
    linkText: 查看频率、配额与过滤
  - title: SMTP 邮件投递
    details: 使用响应式内联 CSS 邮件模板渲染正文，支持 Force-From 发件人对齐以匹配 SPF/DKIM/DMARC 校验，并提供确定性 Message-ID 与按订阅源分类的 List-Id。
    link: /zh/channels/smtp
    linkText: 配置 SMTP 邮件
  - title: Telegram 多模式投递
    details: 支持按路由选择富文本消息、摘要卡片或媒体消息模式，支持防盗链图片内存转发、多图分组续传以及自建 Local Bot API 兼容降级。
    link: /zh/channels/telegram
    linkText: 配置 Telegram 通道
  - title: Notion 数据库同步
    details: 支持在父页面下自动初始化标准属性列或绑定已有数据源，内置 HTML 转 Notion Markdown、长文异步创建及重复页面校验。
    link: /zh/channels/notion
    linkText: 配置 Notion 通道
  - title: 内置 Web 控制台与 /mcp 接口
    details: 单二进制内嵌 React 管理界面与 Streamable HTTP /mcp 端点，支持 readonly 与 admin 两级令牌；通道密钥加密存储，出站请求在连接层统一拦截内网 SSRF 地址。
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
