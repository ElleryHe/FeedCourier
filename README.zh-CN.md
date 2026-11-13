# FeedCourier

[English](./README.md) | **简体中文**

[![CI](https://github.com/ElleryHe/FeedCourier/actions/workflows/ci.yml/badge.svg)](https://github.com/ElleryHe/FeedCourier/actions/workflows/ci.yml)
[![Release](https://github.com/ElleryHe/FeedCourier/actions/workflows/release.yml/badge.svg)](https://github.com/ElleryHe/FeedCourier/actions/workflows/release.yml)
[![License: AGPL-3.0-only](https://img.shields.io/badge/License-AGPL--3.0--only-blue.svg)](./LICENSE)
[![Rust 2024](https://img.shields.io/badge/Rust-2024_Edition-orange.svg)](https://www.rust-lang.org/)

**FeedCourier** 是一款面向个人与极客小团队（10 人以内）的纯血 Rust 单二进制 **RSS / Atom / JSON Feed 流式订阅投递网关**（空闲内存 `<20 MB`，仅需挂载单一 `/data` SQLite WAL 数据目录）。

项目传承经典工具 `rss2email` 稳定可靠的单篇投递哲学，砍掉重型多租户与在线阅读器囤积包袱，将订阅流实时桥接至 **SMTP 精品阅读邮件**、**Telegram Bot API 10.4**（基于 [`telegram-rich`](https://crates.io/crates/telegram-rich) 的 `sendRichMessage` 富文档、快讯卡片、媒体模式）与 **Notion（`2026-03-11` 数据源 & Markdown v2）**，并在同一进程内原生内嵌 **React 19 可视化管理面板**与 **Streamable HTTP MCP 服务端（`/mcp`）**。

---

## 核心设计边界

1. **做投递网关，不做在线阅读器** —— 不在本地永久囤积海量文章正文与媒体缓存，将深度阅读与知识沉淀交给最擅长的终端（邮箱客户端、Telegram 即时流、Notion 结构化数据库）。
2. **指纹永久保留，正文阅后即焚** —— 单文件 SQLite 永久保存定长 64 位 SHA-256 指纹以防重发；清洗后的 HTML 正文与媒体元数据仅在排队流控或重试窗口期（默认 `RETENTION_HOURS=24`）暂存，闭环后自动回收；防盗链媒体中转全程内存流式转发、即传即毁绝不落盘。
3. **全通道单篇流式，防风控匀速控速** —— 不做死板的跨天定时拼版，全通道统一按 `1 Task = 1 Entry × 1 Route` 实时入队，通过持久化发件箱执行“**最小发信间隔 + 随机抖动 + 小时/日配额护栏**”削峰填谷（共用同一发信账号 / Bot Token / Notion Secret 的通道自动共享限流锁）。
4. **人类面板与 AI 协议同源驱动、权限分级隔离** —— WebUI 与 `/mcp` 共享同一套业务引擎；敏感凭证与授信投递终点独占锁死在 WebUI 中，MCP 在严格安全边界内以自然语言完成巡检、试抓与路由编排。

---

## 核心架构亮点

```mermaid
flowchart LR
    subgraph Ingest["受保护抓取与清洗管线"]
        F["RSS / Atom / JSON Feed"] --> HC["连接层防 SSRF 客户端"]
        HC --> R["按需 Readability 全文提取 (dom_smoothie)"]
        R --> D["四级防漂移 SHA-256 去重 & Ammonia 清洗"]
    end

    subgraph Storage["SQLite WAL 单卷存储 (/data)"]
        D --> E[("entries（永久指纹 + 阅后即焚正文）")]
        E --> T[("delivery_tasks（租约 + Checkpoint 发件箱）")]
        GC["零引用计数 NOT EXISTS GC"] -.-> E
    end

    subgraph Targets["匀速步进投递通道"]
        T --> SMTP["SMTP 邮件（Force-From / 响应式内联信纸）"]
        T --> TG["Telegram Bot API 10.4（富文档 / 快讯 / 媒体）"]
        T --> NT["Notion 2026-03-11（数据源 + Markdown v2）"]
    end

    UI["内嵌 React 19 WebUI"] <--> Storage
    MCP["Streamable HTTP /mcp (readonly | admin)"] <--> Storage
```

### 1. 四级防漂移 SHA-256 去重与零引用计数 `NOT EXISTS` 正文回收

针对野生订阅源缺失 GUID 或动态追加追踪参数导致的重复刷屏，FeedCourier 按四级优先级生成与 `feed_id` 联合唯一的 SHA-256 指纹：

1. **一级优先（标准唯一标识）**：Atom `<id>` / RSS `<guid>` / JSON Feed `id`。
2. **二级回退（规范化永久链接）**：剥离 `utm_*`、`fbclid`、`gclid`、`spm` 等营销追踪参数并统一协议头。
3. **三级回退（带原始时间戳标题）**：**仅当源报文明确携带发布时间字段时**采用 `标题 + 原始发布时间戳`（杜绝缺失时间戳的源注入当前时间导致指纹漂移刷屏）。
4. **四级兜底（正文摘要前缀）**：`标题 + 清洗后纯文本前 512 字节`。

新增订阅源或批量导入 OPML 默认开启**静默初始化**（`silent_init`，存量历史仅写指纹不发通知，可选仅推送最新 1 篇验证连通性）。后台 GC 协程**不维护任何脆弱的引用计数**：凡首次发现时间超过 `RETENTION_HOURS`（默认 `24` 小时）且 **`NOT EXISTS`** 任何未终态（`queued` / `delivering` / `retrying` / `deferred`）关联投递任务的条目，自动清空 HTML 正文与媒体列表大字段，自然覆盖未命中路由、中途删改路由或禁用通道等全部边界场景，并支持基于 `VACUUM INTO` 的零停机在线热备份。

### 2. 租约制事务发件箱（`lease_until`）与崩溃安全断点续投（`checkpoint_json`）

```mermaid
stateDiagram-v2
    [*] --> queued
    queued --> delivering: "原子领取（写入 lease_until）"
    deferred --> delivering: "配额窗口到达"
    retrying --> delivering: "到达 next_attempt_at"
    delivering --> delivering: "租约过期自动接管（先读 checkpoint_json 对账续投）"
    delivering --> delivered: "下游确认成功"
    delivering --> retrying: "临时错误 / 429 Retry-After 冷却"
    delivering --> deferred: "触达小时 / 每日配额上限"
    delivering --> dead_letter: "超过最大重试次数或永久 4xx 错误"
    dead_letter --> queued: "WebUI / MCP 人工重投"
    delivered --> [*]
```

存储层隔离为“多连接并发只读池（`query_only = ON`）+ 单连接串行写池（`max_connections = 1`）”，杜绝 `SQLITE_BUSY` 锁竞争。调度器将 `lease_until` 已过期的 `delivering` 任务直接视为可接管任务，覆盖进程强杀、容器重启与协程卡死，并在重投前读取 `checkpoint_json` 执行断点对账：

| 投递通道 | 投递语义保障 | 崩溃恢复与限流续投机制 |
| :--- | :--- | :--- |
| **Notion（`2026-03-11`）** | **有效一次（Effective-once）** | 发起 `POST /v1/pages` 前落盘 `create_initiated`；收到 `202 Accepted`（`allow_async: true`）立即持久化 `async_task_id` 并轮询任务；若请求发出后未落盘响应即崩溃，重领时先按 **`FC-ID`** 查询数据源防重对账。 |
| **SMTP 邮件** | **至少一次**（支持客户端去重） | 持久化基于条目指纹生成的确定性 `Message-ID`，固定认证发件人 `Force-From`（保障 SPF/DKIM/DMARC 通过率），注入 `Auto-Submitted: auto-generated` 与按源归类的 `List-Id`。 |
| **Telegram（`10.4`）** | **至少一次**（收敛至单批次内） | 媒体模式超过 `sendMediaGroup` 单组 10 项上限时分批发送，每批成功后立即将 `next_batch_offset` 与 `sent_message_ids` 写入 `checkpoint_json`，遭遇 `retry_after` 或崩溃重启后从断点批次无损续发。 |

### 3. 连接层防 SSRF 出站客户端、按需 Readability 与分流代理

拉取订阅源、全文提取、防盗链媒体中转、Favicon 探测及 MCP `preview_feed_latest` 实时试抓**全部**统一经由受保护 HTTP 客户端：

- **连接层 DNS SSRF 防护**：自定义 DNS 解析器在 TCP 建连阶段逐一校验解析出的 IPv4/IPv6 地址，拦截环回地址、链路本地、云元数据（`169.254.169.254`）与私有网段，彻底防御 DNS Rebinding。
- **逐跳 `30x` 重定向重校验**：禁用自动跟随重定向，对每一跳目标地址重新执行协议与私网校验并限制最大跳转跳数。
- **精细内网白名单放行**：通过 `SSRF_ALLOWLIST`（CIDR 网段或内部主机名）精准放行同内网自建 `RSSHub` 或自建 Local Bot API Server，无需全局关闭安全防护。
- **严格流式体积护栏与可选全文提取**：设置响应体字节硬上限（Feed `10 MB`、网页全文 `5 MB`、Telegram 中转图片 `10 MB` / 其他媒体 `50 MB`），超限立即中断读取；按源开启 `fetch_full_content` 时使用纯 Rust `dom_smoothie`（独立并发信号量隔离）提取正文并补全相对路径，异常时平滑回退至原生摘要。
- **按源 / 按通道 HTTP & SOCKS5 代理**：支持全局配置 `OUTBOUND_PROXY`，在每个订阅源与每个投递通道上独立勾选 `use_proxy`。

### 4. 三大精选推送通道工程细节

- **SMTP 邮件通道**：基于 `minijinja` 渲染精调内联 CSS 的单栏响应式阅读信纸，构造标准 `multipart/alternative`（附 `html2text` 纯文本回退），严控体积远离 Gmail `102 KB` 截断红线。
- **Telegram 通道（对齐 Bot API 10.4，路由级三模切换）**：
  - **富文档模式（`rich_message`）**：通过自研 [`telegram-rich`](https://crates.io/crates/telegram-rich)（[源码仓库](https://github.com/ElleryHe/telegram-rich)）将 HTML 规范化为 Telegram Rich HTML（原生覆盖 `<table compact>`、`<blockquote expandable>`、`<tg-button-row>`、代码块语言保留与独立媒体块提升），并由 `Document::fit` 按官方上限（`32,768` 字符 / `500` 块 / `16` 层嵌套 / `50` 媒体 / `20` 列）在块边界优雅截断。支持通道级 Bot API 能力探测，面对版本滞后的自建 Local Bot API Server 自动降级为快讯模式并告警。
  - **快讯模式（`compact_card`）**：调用 `sendMessage`（`4,096` 字符安全截断），组装“加粗标题 + 来源标签 + 精炼摘要 + 原文链接”，支持按路由开关链接预览。
  - **媒体模式（`media_mode`）**：提取 `<enclosure>`、`media:content` 与正文图文音视频，挂载三行精炼文案（*作者/源名* + *Unicode 句子边界首句* + *规范化原文链接*），支持防盗链零落盘流式中转回退与多图断点续投。
- **Notion 数据源通道（全面对齐 `Notion-Version: 2026-03-11`）**：
  - **零门槛自动建表或绑定已有数据源**：在父页面下通过 `initial_data_source.properties` 一键创建标准 7 列表结构（`Title`、`Link`、`Source`、`Published`、`State`、`Tag`、`FC-ID`），或自动检测已有 `data_source_id` 的属性列匹配状态并按需补齐缺失列。
  - **Markdown v2 规范化器**：`htmd` 转换后动态扫描代码块内嵌反引号并扩充外层围栏（`4+` 反引号），按块边界实施 `5,000` 块超长截断兜底，调用 `POST /v1/pages` 以 `markdown_version: "v2"` 一次性写入属性与纯净正文（超长文自动启用 `allow_async: true`）。

### 5. 原生 Streamable HTTP MCP（`/mcp`）与纵深安全护栏

`/mcp` 端点基于官方 `rmcp` SDK 构建，采用双级 Bearer Token 鉴权（仅落盘 SHA-256 哈希）：

| 令牌权限级 | 暴露的 MCP 工具（Tools） | 安全职责边界 |
| :--- | :--- | :--- |
| **`readonly`** | `list_feeds_status`、`inspect_failed_deliveries`、`preview_feed_latest` | 只读健康度巡检、死信/重试队列诊断、受防 SSRF 保护的实时抓取与过滤规则试跑。 |
| **`admin`** | 全部 `readonly` 工具 + `manage_feed`、`configure_route`、`reset_and_poll_feed`、`retry_dead_tasks` | 新增/调优订阅源、编排标签与路由规则、解除熔断立即拉取、死信批量重投及通道连通性测试。 |

- **凭证黑盒与授信终点锁死**：通道密钥（ChaCha20-Poly1305 对称加密落盘）与收件终点（收件邮箱、Telegram `chat_id`、Notion `data_source_id`）**仅允许在 WebUI 中管理**；MCP 仅暴露通道 ID 与脱敏备注名，彻底杜绝提示词注入诱导外发敏感数据。
- **不可信外部内容隔离包裹**：MCP 返回的订阅标题、摘要与预览正文统一截断并包裹显式的“不可信外部内容”安全边界标记，降低恶意订阅源实施间接提示词注入的风险。

---

## 快速开始

### 方式一：Docker 一键启动

每次版本标签发布都会同步构建多架构（`linux/amd64`、`linux/arm64`）容器镜像并推送至 **GitHub Container Registry（`ghcr.io/elleryhe/feedcourier:<version>`）** 与 **Docker Hub（`<dockerhub-user>/feedcourier:<version>`）**（支持 `0.1.0`、`0.1`、`0`、`latest` 等语义化标签）：

```bash
docker run -d \
  --name feedcourier \
  --restart unless-stopped \
  -p 8080:8080 \
  -v "$(pwd)/data:/data" \
  ghcr.io/elleryhe/feedcourier:latest
  # 也可锁定精确版本号或从 Docker Hub 拉取：
  # ghcr.io/elleryhe/feedcourier:0.1.0
  # <dockerhub-user>/feedcourier:0.1.0
```

浏览器访问 `http://127.0.0.1:8080` 完成首次管理员口令初始化（也可通过环境变量 `ADMIN_PASSWORD` 预设）。容器首次启动将在 `/data` 下自动生成 `0600` 权限主密钥 `/data/master.key` 并初始化 SQLite WAL 数据库 `/data/feedcourier.db`。

### 方式二：Docker Compose 部署

```yaml
services:
  feedcourier:
    image: ghcr.io/elleryhe/feedcourier:latest # 或 ghcr.io/elleryhe/feedcourier:0.1.0 / <dockerhub-user>/feedcourier:0.1.0
    container_name: feedcourier
    restart: unless-stopped
    ports:
      - "8080:8080"
    volumes:
      - ./data:/data
    environment:
      DATA_DIR: /data
      LISTEN_ADDR: 0.0.0.0:8080
      RETENTION_HOURS: "24"
      # 以下为可选预设项（亦可在 WebUI 中可视化配置）：
      # ADMIN_PASSWORD: "change-me-strong-passphrase"
      # MASTER_KEY: "<64位Hex或Base64编码的32字节主密钥>"
      # OUTBOUND_PROXY: "socks5h://127.0.0.1:1080"
      # SSRF_ALLOWLIST: "10.0.0.5/32,rsshub.internal"
```

```bash
docker compose up -d
```

### 方式三：预编译二进制或从源码编译构建

各主流平台（`x86_64-unknown-linux-gnu`、`aarch64-unknown-linux-gnu`、`x86_64-apple-darwin`、`aarch64-apple-darwin`）的单文件预编译包及 `SHA256SUMS.txt` 校验文件均可在 [GitHub Releases](https://github.com/ElleryHe/FeedCourier/releases) 直接下载。

如需从源码自行编译，请安装 **Rust 1.88+（2024 Edition）**、**Node.js 24+**、**`pnpm`** 以及 **`cmake`**（用于编译 `aws-lc-rs`）：

```bash
# 1. 克隆仓库
git clone https://github.com/ElleryHe/FeedCourier.git
cd FeedCourier

# 2. 构建前端静态产物至 web/dist
corepack enable
pnpm --dir web install --frozen-lockfile
pnpm --dir web build

# 3. 编译内嵌 WebUI 的单文件 Release 二进制（telegram-rich 直接从 crates.io 拉取）
cargo build --release --locked

# 4. 启动服务
DATA_DIR=./data LISTEN_ADDR=127.0.0.1:8080 ./target/release/feedcourier
```

也可直接在仓库根目录执行本地多阶段 Docker 镜像构建：

```bash
docker build -t feedcourier:local .
```

### 自动化版本发布与多架构镜像推送（`.github/workflows/release.yml`）

推送语义化版本 Git 标签（`v*.*.*`，例如 `git tag v0.1.0 && git push origin v0.1.0`）将自动触发 `.github/workflows/release.yml` 流水线：

1. **版本一致性校验与多平台二进制打包**：自动校验 `Cargo.toml` 版本号与 `${GITHUB_REF_NAME#v}` 完全一致，构建 Linux（`x86_64`、`aarch64`）与 macOS（`x86_64`、`aarch64`）四平台锁定二进制压缩包并生成 `SHA256SUMS.txt`，自动创建 GitHub Release（带 `-rc` / `-beta` / `-alpha` 后缀自动标记为预发布）。
2. **GHCR + Docker Hub 多架构（`linux/amd64`、`linux/arm64`）同步推送**：
   - **GHCR（`ghcr.io/elleryhe/feedcourier:<version>`）**：使用内置 `GITHUB_TOKEN` 免配置自动推送。
   - **Docker Hub（`<dockerhub-user>/feedcourier:<version>`）**：当仓库（或 Fork 仓库）在 GitHub Actions Secrets 中配置了以下 Docker Hub API Token 凭据时自动登录并同步推送：
     - `DOCKERHUB_USERNAME` —— Docker Hub 账号用户名。
     - `DOCKERHUB_TOKEN` —— Docker Hub Personal Access Token（具备 `Read, Write, Delete` 权限）。
     - `DOCKERHUB_REPO` *（可选 Repository Variable）* —— 自定义目标镜像名（默认使用 `<DOCKERHUB_USERNAME>/feedcourier`）。
   - 自动生成的镜像标签涵盖：`<version>`（如 `0.1.0`）、`<major>.<minor>`（`0.1`）、`<major>`（`0`）、`latest`（仅正式版）以及 `sha-<short>`。

---

## 环境变量完整说明

| 变量名 | 默认值 | 说明 |
| :--- | :--- | :--- |
| `DATA_DIR` | `./data`（Docker 镜像中为 `/data`） | 持久化目录，存放 `feedcourier.db`（SQLite WAL）、`master.key` 与在线备份快照。 |
| `LISTEN_ADDR` | `0.0.0.0:8080` | HTTP 监听地址，统一承载内嵌 WebUI、`/api/v1/*` 管理接口与 `/mcp` 端点。 |
| `MASTER_KEY` | *（自动生成）* | 显式指定 32 字节 ChaCha20-Poly1305 主密钥（64 位十六进制或 Base64）；未设置时自动初始化 `<DATA_DIR>/master.key`（权限 `0600`）。 |
| `ADMIN_PASSWORD` | *（首次访问引导设置）* | 启动时预设或重置单管理员口令（以 Argon2id PHC 哈希落盘）。 |
| `OUTBOUND_PROXY` | *（未设置）* | 初始全局出站代理地址（支持 `http://`、`https://`、`socks5://`、`socks5h://`）；未设置时依次回退读取 `ALL_PROXY`、`HTTPS_PROXY`、`HTTP_PROXY`。 |
| `SSRF_ALLOWLIST` | *（空）* | 逗号分隔的内网放行 CIDR 网段或主机名白名单（如 `10.0.0.5/32,rsshub.internal`）。 |
| `RETENTION_HOURS` | `24` | 条目暂存正文与媒体元数据在闭环后的保留小时数，到期由 `NOT EXISTS` GC 自动回收。 |
| `DB_READ_MAX_CONNECTIONS` | `8` | SQLite WAL 并发只读连接池最大连接数（最小为 `1`）。 |
| `RUST_LOG` | `feedcourier=info,tower_http=info` | 结构化日志过滤级别指令。 |

---

## 接入 MCP 客户端配置

1. 登录 FeedCourier WebUI，进入 **Settings → Native MCP Server (`/mcp`)**。
2. 按需签发 **`readonly`**（只读巡检）或 **`admin`**（全权限编排）Bearer Token，并复制仅展示一次的明文密钥。
3. 将以下配置粘贴至任意支持 Streamable HTTP 的 MCP 客户端（Claude Desktop、Cursor、Windsurf 等）：

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

---

## 开源协议与合规声明

[AGPL-3.0](./LICENSE)

部署与使用 FeedCourier 须遵守 Telegram 服务条款（[Telegram Bot Developer Terms](https://telegram.org/tos/bot-developers)）、Notion 服务条款（[Notion Developer Terms](https://developers.notion.com/)）、SMTP 反垃圾邮件策略以及上游站点的抓取与内容版权规范；请勿将本软件用于垃圾信息发送、API 滥用或未经授权的内容分发。
