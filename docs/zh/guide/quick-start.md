# 快速开始

FeedCourier 将后端网关、SQLite WAL 存储迁移脚本、内嵌 React 19 管理面板与 `/mcp` 服务端打包为**单一可执行文件**，零外部数据库或缓存容器依赖。你可以根据环境偏好选择以下任一方式部署。

---

## 方式一：Docker 一键启动（推荐）

官方多架构容器镜像（原生支持 `linux/amd64` 与 `linux/arm64`）发布于 Docker Hub [`elleryhe/feedcourier`](https://hub.docker.com/r/elleryhe/feedcourier)：

```bash
docker run -d \
  --name feedcourier \
  --restart unless-stopped \
  -p 8080:8080 \
  -v "$(pwd)/data:/data" \
  elleryhe/feedcourier:latest
  # 生产环境也可锁定精确语义化版本号：
  # elleryhe/feedcourier:0.1.0
```

容器启动后会自动在挂载的 `/data` 目录中初始化：
- `/data/master.key`：自动生成的 32 字节高熵主密钥文件（权限严格锁死为 `0600`），用于 ChaCha20-Poly1305 加密落盘的通道凭证；
- `/data/feedcourier.db`：启用 WAL 模式的单文件 SQLite 数据库。

---

## 方式二：Docker Compose 生产编排

创建 `docker-compose.yml` 文件：

```yaml
services:
  feedcourier:
    image: elleryhe/feedcourier:latest # 或锁定版本 elleryhe/feedcourier:0.1.0
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
      DB_READ_MAX_CONNECTIONS: "8"
      RUST_LOG: "feedcourier=info,tower_http=info"
      # 以下为可选预设项（亦可在 WebUI 中可视化配置并持久化）：
      # ADMIN_PASSWORD: "change-me-strong-passphrase"
      # MASTER_KEY: "<64位十六进制或Base64编码的32字节主密钥>"
      # OUTBOUND_PROXY: "socks5h://127.0.0.1:1080"
      # SSRF_ALLOWLIST: "10.0.0.5/32,rsshub.internal"
```

启动并查看运行日志：

```bash
docker compose up -d
docker compose logs -f feedcourier
```

---

## 方式三：下载预编译二进制文件

每次版本发布均在 [GitHub Releases](https://github.com/ElleryHe/FeedCourier/releases) 提供四大主流平台的单文件预编译包与 `SHA256SUMS.txt` 校验文件：

| 操作系统与架构 | 目标三元组（Target Triple） | 压缩包文件名（以 `0.1.0` 为例） |
| :--- | :--- | :--- |
| **Linux x86_64** | `x86_64-unknown-linux-gnu` | `feedcourier-0.1.0-x86_64-unknown-linux-gnu.tar.gz` |
| **Linux ARM64** | `aarch64-unknown-linux-gnu` | `feedcourier-0.1.0-aarch64-unknown-linux-gnu.tar.gz` |
| **macOS Apple Silicon** | `aarch64-apple-darwin` | `feedcourier-0.1.0-aarch64-apple-darwin.tar.gz` |
| **macOS Intel** | `x86_64-apple-darwin` | `feedcourier-0.1.0-x86_64-apple-darwin.tar.gz` |

下载、校验 SHA-256 签名并启动（以 Linux `x86_64` 为例）：

```bash
VERSION="0.1.0"
TARGET="x86_64-unknown-linux-gnu"
ARCHIVE="feedcourier-${VERSION}-${TARGET}.tar.gz"

# 1. 下载二进制包与 SHA256SUMS.txt
curl -fLO "https://github.com/ElleryHe/FeedCourier/releases/download/v${VERSION}/${ARCHIVE}"
curl -fLO "https://github.com/ElleryHe/FeedCourier/releases/download/v${VERSION}/SHA256SUMS.txt"

# 2. 校验文件完整性
grep "${ARCHIVE}" SHA256SUMS.txt | sha256sum -c -

# 3. 解压并运行
tar -xzf "${ARCHIVE}"
mkdir -p ./data
DATA_DIR=./data LISTEN_ADDR=0.0.0.0:8080 ./feedcourier
```

> [!TIP]
> 在 macOS 上执行校验时，若系统未安装 `sha256sum`，可使用 `grep "${ARCHIVE}" SHA256SUMS.txt | shasum -a 256 -c -`。

---

## 方式四：从源码编译构建

如需从源码自行编译，请确保构建环境已安装：

- **Rust 1.94+**（启用 Rust 2024 Edition）
- **Node.js 24+** 与 **`pnpm` 10+**（可通过 `corepack enable` 启用）
- **`cmake`** 与 **`pkg-config`**（用于编译 `aws-lc-rs` 加密原语）

```bash
# 1. 克隆代码仓库
git clone https://github.com/ElleryHe/FeedCourier.git
cd FeedCourier

# 2. 安装前端依赖并构建内嵌静态产物至 web/dist
corepack enable
pnpm --dir web install --frozen-lockfile
pnpm --dir web build

# 3. 编译内嵌 WebUI 的单文件 Release 二进制（telegram-rich 自动从 crates.io 拉取）
cargo build --release --locked

# 4. 启动服务
DATA_DIR=./data LISTEN_ADDR=127.0.0.1:8080 ./target/release/feedcourier
```

你也可以直接在仓库根目录使用内置的多阶段 `Dockerfile` 本地构建镜像：

```bash
docker build -t feedcourier:local .
```

---

## 首次启动与管理员口令初始化

1. **访问控制台**：在浏览器中打开 `http://127.0.0.1:8080`。
2. **初始化管理员口令**：
   - 若启动时**未设置** `ADMIN_PASSWORD` 环境变量，WebUI 将自动呈现首次初始化引导界面（要求口令长度至少为 **8 个字符**）。提交后，后端使用 **Argon2id** 算法将密码哈希持久化至 SQLite `system_settings` 表，并签发有效期 30 天的 `fc_session` `HttpOnly` 会话 Cookie。
   - 若启动时**已设置** `ADMIN_PASSWORD` 环境变量，系统会在启动阶段自动将 Argon2id 哈希写入数据库，直接进入登录界面。
3. **验证健康状态**：你随时可以通过公开健康检查端点验证网关运行状态：

```bash
curl -s http://127.0.0.1:8080/api/v1/health
```

返回示例：

```json
{
  "name": "FeedCourier",
  "version": "0.1.0",
  "license": "AGPL-3.0-only",
  "repo_url": "https://github.com/ElleryHe/FeedCourier",
  "status": "ok"
}
```

---

## 下一步配置引导

完成登录后，建议按以下顺序完成首个投递闭环：

1. **创建授信投递通道**：前往 **Channels** 页面，配置 [SMTP 邮件](/zh/channels/smtp)、[Telegram Bot API 10.4](/zh/channels/telegram) 或 [Notion 数据源](/zh/channels/notion)，并点击 **Test** 按钮发送连通性测试消息。
2. **添加订阅源或导入 OPML**：前往 **Feeds** 页面添加 RSS / Atom / JSON Feed（支持开启 `silent_init` 静默吸收历史文章，或勾选 `push_latest_one` 立即试推最新 1 篇）。
3. **编排路由规则**：前往 **Routes** 页面将指定订阅源或标签（`tags`）绑定至目标通道，按需配置关键词/正则过滤与展现模式。
4. **（可选）接入 AI 助手**：前往 **Settings → Native MCP Server (`/mcp`)** 签发令牌，参阅 [MCP 客户端接入配置](/zh/mcp/setup)。
