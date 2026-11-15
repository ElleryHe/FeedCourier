# 数据备份与维护

FeedCourier 的所有持久化数据均保存在单一 `DATA_DIR`（容器内默认为 `/data`）目录下，便于日常备份、实例迁移与反向代理部署。

---

## `/data` 持久化目录结构

在运行中的实例上查看 `/data` 目录，包含以下文件：

```text
/data/
├── master.key          # 32 字节 ChaCha20-Poly1305 主密钥（权限 0600，未设置 MASTER_KEY 环境变量时自动生成）
├── feedcourier.db      # SQLite 主数据库文件（表结构、文章指纹、加密后的通道配置、路由与任务队列）
├── feedcourier.db-wal  # SQLite WAL 预写日志文件
└── feedcourier.db-shm  # SQLite WAL 共享内存索引文件
```

### 主密钥（`master.key`）保存须知

> [!IMPORTANT]
> 数据库中保存的 SMTP 密码、Telegram `bot_token` 与 Notion `integration_secret` 均使用 `master.key`（或环境变量 `MASTER_KEY`）加密存储。
> **备份或迁移 `feedcourier.db` 时，必须同时保留对应的 `master.key`（或相同的 `MASTER_KEY` 环境变量）**；若丢失主密钥，已保存的通道凭据将无法解密，需要在 WebUI 中重新填写通道密钥。

---

## 在线备份（`VACUUM INTO`）

由于 SQLite 在 WAL 模式下运行时，最新事务数据同时分布在 `feedcourier.db` 与 `feedcourier.db-wal` 中，直接用 `cp` 复制宿主机文件可能得到未完整合并的副本。

FeedCourier 内置了基于 SQLite `VACUUM INTO` 的在线备份功能，无需停止服务即可导出已合并为单文件的数据库快照：

### 1. 通过 WebUI 下载快照

登录 WebUI，进入 **Settings → Storage & Maintenance**，点击 **Download Online Snapshot (`VACUUM INTO`)**，浏览器将直接下载形如 `feedcourier-snapshot-20261115T093000Z.db` 的单文件 SQLite 备份。

### 2. 通过 REST API 定时备份

也可以携带管理员会话 Cookie 调用 `GET /api/v1/maintenance/backup` 触发在线备份：

```bash
# 1. 登录并保存 fc_session Cookie
curl -s -c cookies.txt \
  -H "Content-Type: application/json" \
  -d '{"password":"your-admin-password"}' \
  http://127.0.0.1:8080/api/v1/auth/login

# 2. 导出并下载单文件备份
curl -fL -b cookies.txt \
  -o "feedcourier-backup-$(date -u +%Y%m%dT%H%M%SZ).db" \
  http://127.0.0.1:8080/api/v1/maintenance/backup
```

### 3. 从备份恢复或迁移实例

1. 停止目标机器上的 FeedCourier 容器或进程；
2. 删除目标 `/data/` 下的旧 `feedcourier.db`、`feedcourier.db-wal`、`feedcourier.db-shm`；
3. 将备份的 `.db` 快照文件复制到 `/data/feedcourier.db`，并确保原实例的 `/data/master.key`（权限 `0600`）已放置在 `/data/` 下；
4. 启动 FeedCourier 服务，即可恢复全部订阅源、去重指纹、推送通道、路由规则与 MCP 令牌。

---

## 自动与手动清理正文缓存（GC）

- **后台定时清理**：每间隔 1 小时自动运行一次：
  1. 清理所有超过 `RETENTION_HOURS`（默认 24 小时）且已无未完成投递任务的文章正文与媒体元数据；
  2. 清理已过期的登录会话记录；
  3. 执行 `PRAGMA optimize` 更新 SQLite 查询统计信息。
- **手动触发清理**：在 WebUI **Settings → Storage & Maintenance** 点击 **Run Payload GC Now**（或调用 `POST /api/v1/maintenance/gc`），可立即执行一次清理并返回本次回收的条目数（`purged_entries`）。

---

## 反向代理与 HTTPS 部署

当通过域名远程访问 WebUI 或连接 `/mcp` 接口时，建议在前面配置 HTTPS 反向代理。

> [!TIP]
> **反向代理配置建议**：
> 1. 传递 `X-Forwarded-Proto` 与 `X-Forwarded-Host` 请求头——WebUI 在生成 MCP 客户端配置示例时，会自动根据这两个请求头拼接完整的 `https://<your-domain>/mcp` 地址。
> 2. 对于 `/mcp` 路径（Streamable HTTP），建议关闭代理缓冲（`proxy_buffering off`），避免流式响应被缓存延迟。

### Caddy 配置示例

```txt
feedcourier.example.com {
    reverse_proxy 127.0.0.1:8080 {
        flush_interval -1
    }
}
```

### Nginx 配置示例

```nginx
server {
    listen 443 ssl http2;
    server_name feedcourier.example.com;

    # SSL 证书配置略...

    location /mcp {
        proxy_pass http://127.0.0.1:8080/mcp;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_set_header X-Forwarded-Host $host;
        proxy_buffering off;
        proxy_cache off;
        proxy_read_timeout 300s;
    }

    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_set_header X-Forwarded-Host $host;
    }
}
```

---

## 常见故障排查

### 1. 订阅源报错 `outbound SSRF policy blocked ...`
- **原因**：目标订阅源 URL（或其重定向跳转地址、全文提取的目标网页地址）解析到了环回地址、局域网私有 IP（如 `10.x`、`192.168.x`）或保留网段（如透明代理返回的 `198.18.0.0/15` Fake-IP）。
- **解决**：
  - 若目标是你自建的内网服务（如 `RSSHub`），前往 WebUI **Settings** 将其 IP/CIDR 或内部主机名加入 **SSRF Allowlist**；
  - 若宿主机运行了透明代理（Fake-IP 模式）导致本地 DNS 返回 `198.18.x.x`，建议配置 `OUTBOUND_PROXY="socks5h://..."` 并为该源勾选 `use_proxy`，使域名由远端代理直接解析。

### 2. 忘记 WebUI 管理员密码如何重置？
- **解决**：在启动容器或进程时设置环境变量 `ADMIN_PASSWORD="your-new-strong-password"` 并重启一次服务。系统会在启动时用新密码的哈希覆盖数据库中的旧记录，随后即可移除该环境变量。

### 3. 失败任务（`dead_letter`）报错 `Entry payload already garbage-collected before delivery`
- **原因**：任务因通道故障停留在死信队列的时间已超过 `RETENTION_HOURS`（默认 24 小时），且在清理过程中文章正文已被回收。
- **解决**：若希望预留更长的故障排查与重投时间，可将环境变量 `RETENTION_HOURS` 调大（如设为 `72` 或 `168` 小时）。
