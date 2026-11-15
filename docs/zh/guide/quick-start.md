# 快速开始

FeedCourier 将后端服务、SQLite 数据库迁移脚本、Web 管理界面与 `/mcp` 接口打包为单一可执行文件，无需额外部署外部数据库或缓存服务。你可以根据环境选择以下任一方式启动。

---

## 方式一：Docker 启动（推荐）

官方多架构容器镜像（支持 `linux/amd64` 与 `linux/arm64`）发布于 Docker Hub [`elleryhe/feedcourier`](https://hub.docker.com/r/elleryhe/feedcourier)：

```bash
docker run -d \
  --name feedcourier \
  --restart unless-stopped \
  -p 8080:8080 \
  -v "$(pwd)/data:/data" \
  elleryhe/feedcourier:latest
  # 也可指定具体版本号：
  # elleryhe/feedcourier:0.1.1
```

容器启动后会自动在挂载的 `/data` 目录中初始化：
- `/data/master.key`：自动生成的 32 字节主密钥文件（文件权限为 `0600`），用于加密保存在数据库中的通道密钥；
- `/data/feedcourier.db`：启用 WAL 模式的单文件 SQLite 数据库。

---

## 方式二：Docker Compose 部署

创建 `docker-compose.yml` 文件：

```yaml
services:
  feedcourier:
    image: elleryhe/feedcourier:latest # 或指定版本 elleryhe/feedcourier:0.1.1
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
      # 以下为可选配置项（也可在启动后通过 WebUI 设置并保存）：
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

每次版本发布均在 [GitHub Releases](https://github.com/ElleryHe/FeedCourier/releases) 提供四个主流平台的预编译压缩包与 `SHA256SUMS.txt` 校验文件：

| 操作系统与架构 | 目标三元组（Target Triple） | 压缩包文件名（以 `0.1.1` 为例） |
| :--- | :--- | :--- |
| **Linux x86_64** | `x86_64-unknown-linux-gnu` | `feedcourier-0.1.1-x86_64-unknown-linux-gnu.tar.gz` |
| **Linux ARM64** | `aarch64-unknown-linux-gnu` | `feedcourier-0.1.1-aarch64-unknown-linux-gnu.tar.gz` |
| **macOS Apple Silicon** | `aarch64-apple-darwin` | `feedcourier-0.1.1-aarch64-apple-darwin.tar.gz` |
| **macOS Intel** | `x86_64-apple-darwin` | `feedcourier-0.1.1-x86_64-apple-darwin.tar.gz` |

下载、校验 SHA-256 并启动（以 Linux `x86_64` 为例）：

```bash
VERSION="0.1.1"
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

如需从源码编译，请确保本地已安装：

- **Rust 1.94+**（使用 Rust 2024 Edition）
- **Node.js 24+** 与 **`pnpm` 10+**（可通过 `corepack enable` 启用）
- **`cmake`** 与 **`pkg-config`**（用于编译 `aws-lc-rs` 依赖）

```bash
# 1. 克隆代码仓库
git clone https://github.com/ElleryHe/FeedCourier.git
cd FeedCourier

# 2. 安装前端依赖并构建静态资源至 web/dist
corepack enable
pnpm --dir web install --frozen-lockfile
pnpm --dir web build

# 3. 编译内嵌 WebUI 的 Release 二进制文件
cargo build --release --locked

# 4. 启动服务
DATA_DIR=./data LISTEN_ADDR=127.0.0.1:8080 ./target/release/feedcourier
```

你也可以在仓库根目录使用内置的多阶段 `Dockerfile` 本地构建镜像：

```bash
docker build -t feedcourier:local .
```

---

## 首次启动与管理员口令初始化

1. **访问管理界面**：在浏览器中打开 `http://127.0.0.1:8080`。
2. **初始化管理员口令**：
   - 若启动时**未设置** `ADMIN_PASSWORD` 环境变量，WebUI 会展示首次初始化界面（要求口令长度至少为 **8 个字符**）。提交后，后端使用 Argon2id 算法将密码哈希保存至 SQLite 数据库，并签发有效期 30 天的 `fc_session` `HttpOnly` 会话 Cookie。
   - 若启动时**已设置** `ADMIN_PASSWORD` 环境变量，系统会在启动阶段自动写入密码哈希，直接进入登录界面。
3. **验证健康状态**：可以通过健康检查接口确认服务运行状态：

```bash
curl -s http://127.0.0.1:8080/api/v1/health
```

返回示例：

```json
{
  "name": "FeedCourier",
  "version": "0.1.1",
  "license": "AGPL-3.0",
  "repo_url": "https://github.com/ElleryHe/FeedCourier",
  "status": "ok"
}
```

---

## 下一步配置引导

完成登录后，可按以下顺序完成基本配置：

1. **创建推送通道**：前往 **Channels** 页面，配置 [SMTP 邮件](/zh/channels/smtp)、[Telegram](/zh/channels/telegram) 或 [Notion](/zh/channels/notion)，并点击 **Test** 按钮发送测试消息。
2. **添加订阅源或导入 OPML**：前往 **Feeds** 页面添加 RSS / Atom / JSON Feed（支持开启 `silent_init` 静默记录存量历史文章，或勾选 `push_latest_one` 立即试推最新 1 篇）。
3. **配置路由规则**：前往 **Routes** 页面将指定订阅源或标签（`tags`）绑定到目标通道，按需设置关键词/正则过滤与投递模式。
4. **（可选）接入 MCP 客户端**：前往 **Settings → Native MCP Server (`/mcp`)** 签发令牌，参阅 [客户端接入与安全说明](/zh/mcp/setup)。
