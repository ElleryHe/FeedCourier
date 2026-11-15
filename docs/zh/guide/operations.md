# 数据备份、反向代理与日常运维

FeedCourier 的持久化状态高度收敛在单一 `DATA_DIR`（容器内为 `/data`）目录下，没有任何外部数据库或缓存组件，使日常备份、实例迁移与 HTTPS 反向代理部署极为清爽。

---

## `/data` 持久化目录结构

在运行中的实例上查看 `/data` 目录，结构如下：

```text
/data/
├── master.key          # 32 字节 ChaCha20-Poly1305 主密钥（权限 0600，未设置 MASTER_KEY 环境变量时自动生成）
├── feedcourier.db      # SQLite 主数据库文件（schema + 指纹 + 通道密文 + 路由 + 任务队列）
├── feedcourier.db-wal  # SQLite Write-Ahead Log 预写日志文件（运行期动态增长与检查点回收）
└── feedcourier.db-shm  # SQLite WAL 共享内存索引文件
```

### 主密钥（`master.key`）安全须知

> [!IMPORTANT]
> `channels` 表中的 SMTP 密码、Telegram `bot_token` 与 Notion `integration_secret` 均使用 `master.key`（或环境变量 `MASTER_KEY`）通过 **ChaCha20-Poly1305（AEAD）** 对称加密落盘。
> **备份或迁移 `feedcourier.db` 时，必须同时保留对应的 `master.key`（或相同的 `MASTER_KEY` 环境变量）**；若丢失主密钥，已保存的通道凭证将无法解密（任务会报错 `Failed to decrypt channel credentials`），需在 WebUI 中重新填写通道密钥。

---

## 零停机在线热备份（`VACUUM INTO`）

由于 SQLite 在 WAL 模式下运行时，事务数据分散在 `feedcourier.db` 与 `feedcourier.db-wal` 中，直接用 `cp` 复制宿主机文件可能拷出未合并的不一致快照。

FeedCourier 原生内置了基于 SQLite **`VACUUM INTO`** 的零停机在线热备份机制，无需停止容器或中断后台抓取即可生成单文件、已平展合并且自动紧缩碎片的一致性数据库快照：

### 1. 通过 WebUI 一键下载快照

登录 WebUI，进入 **Settings → Storage & Maintenance**，点击 **Download Online Snapshot (`VACUUM INTO`)**，浏览器将直接下载形如 `feedcourier-snapshot-20261115T093000Z.db` 的单文件 SQLite 备份。

### 2. 通过 REST API 自动化定时备份

你也可以通过携带管理员会话 Cookie 调用 `GET /api/v1/maintenance/backup` 触发在线热备：

```bash
# 1. 登录并保存 fc_session Cookie
curl -s -c cookies.txt \
  -H "Content-Type: application/json" \
  -d '{"password":"your-admin-password"}' \
  http://127.0.0.1:8080/api/v1/auth/login

# 2. 触发 VACUUM INTO 并流式下载一致性单文件备份
curl -fL -b cookies.txt \
  -o "feedcourier-backup-$(date -u +%Y%m%dT%H%M%SZ).db" \
  http://127.0.0.1:8080/api/v1/maintenance/backup
```

### 3. 从备份恢复或迁移实例

1. 停止目标机器上的 FeedCourier 容器或进程；
2. 删除目标 `/data/` 下的旧 `feedcourier.db`、`feedcourier.db-wal`、`feedcourier.db-shm`；
3. 将备份的 `.db` 快照文件重命名为 `/data/feedcourier.db`，并确保原实例的 `/data/master.key`（权限 `0600`）已就位；
4. 启动 FeedCourier 服务即可无缝恢复所有订阅源、历史去重指纹、授信通道、路由规则与 MCP 令牌。

---

## 手动与自动数据库瘦身（GC & `PRAGMA optimize`）

- **后台自动维护协程**：每间隔 `1 小时`（`DEFAULT_MAINTENANCE_TICK`）自动运行一次：
  1. 执行 `NOT EXISTS` 子查询回收所有发现时间超过 `RETENTION_HOURS`（默认 `24` 小时）且无活跃投递任务的条目正文与媒体 JSON；
  2. 清理已过期的 `sessions` 登录会话记录；
  3. 执行 `PRAGMA optimize` 更新 SQLite 查询优化器统计信息。
- **手动立即触发 GC**：在 WebUI **Settings → Storage & Maintenance** 点击 **Run Payload GC Now**（或调用 `POST /api/v1/maintenance/gc`），即可立即执行一次全量回收并返回本次清理的条目数量（`purged_entries`）。

---

## 反向代理与 HTTPS 部署建议

当将 FeedCourier 暴露至公网或内网域名（以便远程访问 WebUI 或供远程 AI 客户端连接 `/mcp`）时，强烈建议前置 HTTPS 反向代理。

> [!TIP]
> **反向代理配置要点**：
> 1. 传递标准的 `X-Forwarded-Proto` 与 `X-Forwarded-Host` 请求头——FeedCourier 在签发 MCP Token 生成客户端一键配置 JSON 时，会自动根据这两个头推导完整的 `https://<your-domain>/mcp` 地址。
> 2. 对于 `/mcp` 路径（Streamable HTTP 协议），建议**关闭代理缓冲（`proxy_buffering off`）**以保障流式通知与长连接响应的低延迟。

### Caddy 配置示例（自动签发 HTTPS 证书）

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

## 常见故障排查（Troubleshooting）

### 1. 订阅源报错 `outbound SSRF policy blocked ...`
- **原因**：目标订阅源 URL（或其在 `30x` 跳转过程中的某一跳，或启用 Readability 全文提取时访问的原文链接）解析到了环回地址、局域网私有 IP（如 `10.x`、`192.168.x`）或被污染的保留网段（如 `198.18.0.0/15` Fake-IP）。
- **解决**：
  - 若目标确实是你自建的内网服务（如 `RSSHub`），前往 WebUI **Settings** 将其 IP/CIDR 或内部主机名加入 **SSRF Allowlist**；
  - 若宿主机运行了透明代理（Fake-IP 模式）导致本地 DNS 返回 `198.18.x.x`，建议配置 `OUTBOUND_PROXY="socks5h://..."` 并为该源勾选 `use_proxy`，使域名由远端代理直接解析。

### 2. 忘记 WebUI 管理员密码如何重置？
- **解决**：在启动容器或进程时设置环境变量 `ADMIN_PASSWORD="your-new-strong-password"` 并重启一次服务。`bootstrap_env_settings` 会在启动时自动用新口令的 Argon2id 哈希覆盖 `system_settings` 表中的旧口令哈希，随后即可移除该环境变量。

### 3. 死信任务（`dead_letter`）报错 `Entry payload already garbage-collected before delivery`
- **原因**：任务因通道故障停留在死信队列的时间已超过 `RETENTION_HOURS`（默认 24 小时），且在某次清理或异常状态下条目正文已被回收。
- **解决**：为了留出更充裕的周末排障窗口，可将环境变量 `RETENTION_HOURS` 调大（如设为 `72` 或 `168` 小时）。
