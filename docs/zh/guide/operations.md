---
description: "FeedCourier 在线备份与快照恢复 SQLite WAL 数据库及 master.key 主密钥、全局投递队列暂停与恢复、清除订阅源缓存、丢失 2FA 验证器的恢复、正文保留期清理与反向代理配置说明。"
---

# 数据备份与维护

FeedCourier 的所有持久化数据均保存在单一 `DATA_DIR`（容器内默认为 `/data`）目录下，便于日常备份、实例迁移与反向代理部署。

---

## `/data` 持久化目录结构

在运行中的实例上查看 `/data` 目录，包含以下文件：

```text
/data/
├── master.key            # 32 字节 ChaCha20-Poly1305 主密钥（权限 0600，未设置 MASTER_KEY 环境变量时自动生成）
├── feedcourier.db        # SQLite 主数据库文件（表结构、文章指纹、加密后的账号与目的地配置、路由与任务队列）
├── feedcourier.db-wal    # SQLite WAL 预写日志文件
├── feedcourier.db-shm    # SQLite WAL 共享内存索引文件
└── pre-v0.2.0-backup.db  # 从 v0.1.x 升级到 v0.2.0 时自动生成的升级前在线快照备份
```

### 主密钥（`master.key`）保存须知

> [!IMPORTANT]
> 数据库中保存的通道账号凭据（SMTP 密码、Telegram `bot_token`、Notion `integration_secret`）与目的地配置（收件人列表、`chat_id`、`data_source_id`）均使用 `master.key`（或环境变量 `MASTER_KEY`）加密存储。
> **备份或迁移 `feedcourier.db` 时，必须同时保留对应的 `master.key`（或相同的 `MASTER_KEY` 环境变量）**；若丢失主密钥，已保存的账号凭据与目的地配置将无法解密，需要在 WebUI 中重新填写。

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
  -d '{"username":"Courier","password":"your-admin-password"}' \
  http://127.0.0.1:8080/api/v1/auth/login
# 启用 2FA 后，响应包含 "mfa_required": true 与 "mfa_token"，需再完成：
#   curl -s -c cookies.txt -H "Content-Type: application/json" \
#     -d '{"mfa_token":"<mfa_token>","code":"123456"}' \
#     http://127.0.0.1:8080/api/v1/auth/login/2fa

# 2. 导出并下载单文件备份
curl -fL -b cookies.txt \
  -o "feedcourier-backup-$(date -u +%Y%m%dT%H%M%SZ).db" \
  http://127.0.0.1:8080/api/v1/maintenance/backup
```

### 3. 从备份恢复或迁移实例

1. 停止目标机器上的 FeedCourier 容器或进程；
2. 删除目标 `/data/` 下的旧 `feedcourier.db`、`feedcourier.db-wal`、`feedcourier.db-shm`；
3. 将备份的 `.db` 快照文件复制到 `/data/feedcourier.db`，并确保原实例的 `/data/master.key`（权限 `0600`）已放置在 `/data/` 下；
4. 启动 FeedCourier 服务，即可恢复全部订阅源、去重指纹、推送通道与目的地、路由规则与 MCP 令牌。

---

## 从 `v0.1.x` 升级至 `v0.2.0` {#upgrading-from-v0-1-x-to-v0-2-0}

FeedCourier `v0.2.0` 引入了**双层投递通道模型（通道账号 + 多个目的地）**。从 `v0.1.x` 升级时，服务会在首次启动阶段**自动、原子地**完成旧数据迁移，无需手动执行 SQL：

1. **升级前自动备份（`pre-v0.2.0-backup.db`）**：在修改任何旧通道记录前，系统会自动通过 `VACUUM INTO` 在数据目录下生成一致性快照 `<DATA_DIR>/pre-v0.2.0-backup.db`。若后续需要回退至 `v0.1.6`，将该备份与原 `master.key` 一同还原即可。
2. **表结构变更与单事务拆分**：`migrations/0002_channel_destinations.sql` 先创建 `channel_destinations` 表并为 `routes` / `delivery_tasks` 增加 `destination_id` 列；随后启动钩子 `Database::migrate_channels_to_v2` 使用 `MasterKey` 在**单个 SQLite 事务**内解密每条旧通道配置，拆分为账号凭据（`channels`）与目的地配置（`channel_destinations`）。
3. **按 `credential_group_key` 自动合并重复账号**：共享同一底层凭据的旧通道（相同 `credential_group_key`，例如同一 Telegram Bot 分别推送至两个群组，或同一 SMTP 账号发送至两组收件人）会自动合并为同一个通道账号，每个旧通道转化为其下一个独立目的地并沿用旧名称作为备注：
   - **流速与配额（取最严格值）**：`min_interval_secs` 取最大值、`jitter_max_secs` 取最大值、`hourly_quota` 与 `daily_quota` 取非空最小值、`cooldown_until` 取最晚截止时间。
   - **代理与启用状态（取并集）**：只要任一被合并的旧通道开启了代理，合并后的账号即开启 `use_proxy`；只要任一旧通道处于启用状态，合并后的账号即保持启用（每个目的地仍各自保留原有的启用/停用状态）。
   - **账号名称**：SMTP 账号优先使用 `from_address`，Telegram 账号优先使用已缓存的 `@username`，否则沿用主通道原名。
4. **路由与历史任务重指向**：所有 `routes` 与 `delivery_tasks`（含历史 `delivered` 与 `dead_letter` 任务）会自动更新指向新的 `destination_id` 与合并后的 `channel_id`。
5. **幂等标记**：事务提交后写入 `system_settings.channel_model_version = 2`，后续重启不会重复迁移。

> [!WARNING]
> **API 与 MCP 调用方兼容提示**
> 升级后，仍使用 `channel_id` 调用 `POST`/`PATCH /api/v1/routes`、`POST /api/v1/feeds/preview/push` 或 MCP `configure_route` / `test_push_feed_entry` 的脚本，在目标账号仅拥有 **1 个目的地** 时仍可自动解析；若账号下存在**多个目的地**（包括旧通道合并后产生多个目的地的情况），服务端会返回校验错误并提示改用 `destination_id`。

---

## 暂停与恢复全局投递队列 {#pausing-resuming-the-delivery-queue}

所有路由共用一个全局发件箱。需要暂时停止全部投递时（例如维护下游服务），可使用 **Tasks** 页面顶部的控制栏：

- **立即暂停**、**暂停 15 分钟 / 30 分钟 / 1 小时 / 3 小时 / 6 小时**，或**暂停至**自定义日期时间；
- **立即恢复**可提前结束暂停。

队列暂停期间，订阅源仍会继续轮询，新任务持续累积，但不会发送任何消息；**Overview** 页面会显示暂停横幅。暂停结束（到点自动恢复或手动恢复）后，所有订阅源中已到期的任务会按文章发布时间（缺失时按发现时间）从旧到新依次投递，形成一条统一的时间顺序流。该功能也可通过 [`PUT /api/v1/tasks/queue`](/zh/reference/rest-api) 与 MCP 工具 [`manage_outbox_queue`](/zh/mcp/tools) 使用。

> [!TIP]
> 路由自身的投递时间窗口独立生效：窗口外的任务仍会入队，并在下一个窗口开始时投递。参见[发送频率、配额与时间窗口](/zh/channels/pacing-and-quotas#route-delivery-time-windows)。

### 发件箱队列状态配色

在 **Tasks** 页面中，任务状态标签、HTTP/SMTP 状态码、尝试次数与诊断日志采用统一的语义配色，便于一眼区分正常投递、临时等待与永久错误：

| 状态 / 信号 | 颜色 |
| :--- | :--- |
| **已送达** / `2xx`（不含 `202`） | **绿色** |
| **重试中** / **延后** / `202`（异步任务） / `429` 等可恢复情况 | **黄色** |
| **死信** / 永久性 `4xx` / `5xx` | **红色** |
| **排队中** | **灰色** |
| **尝试次数**：1 次 / ≥ 2 次 / 达到上限（6 次） | **中性色** / **黄色** / **红色** |

诊断日志的文字颜色跟随对应任务的状态变化，成功送达的提示日志以绿色显示，不再显示为红色。

---

## 清除订阅源缓存

希望某个订阅源像全新订阅一样重新开始时，可在订阅源上点击 **Clear cache**（需确认）。它会删除该订阅源的全部文章记录、内容指纹与投递任务，清除 `ETag` / `Last-Modified` 与失败状态，并关闭静默初始化。开启开关**立即重新抓取并将全部积压入队**（默认开启）时，会立即轮询并按从旧到新的顺序将全部积压文章入队。

> [!WARNING]
> 由于指纹已被删除，曾经投递过的文章会被视为新文章而再次投递。如需先检查再发送，请先暂停队列，或关闭立即重新抓取开关。

REST：`POST /api/v1/feeds/{id}/clear-cache`；MCP：`manage_feed` 的 `action: "clear_cache"`。

---

## 丢失验证器（`RESET_2FA`） {#lost-authenticator-reset-2fa}

如果验证器设备与恢复码都已丢失，登录会被 2FA 拦住。请带环境变量 `RESET_2FA=true` 重启一次 FeedCourier：

```bash
docker run -d --name feedcourier -p 8080:8080 -v "$(pwd)/data:/data" \
  -e TZ=Asia/Shanghai \
  -e RESET_2FA=true \
  elleryhe/feedcourier:latest
```

启动时 FeedCourier 会删除 TOTP 密钥与全部恢复码，并输出一条 `WARN` 日志。随后使用用户名和密码登录，并**移除 `RESET_2FA` 后再重启一次**，否则每次启动都会清除 2FA。之后可在 **Settings → 账号与安全** 中重新启用。若同时忘记了密码或用户名，可用同样方式设置 `ADMIN_PASSWORD` / `ADMIN_USERNAME`。

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

### 2. 忘记 WebUI 管理员用户名、密码或丢失 2FA 验证器如何找回？
- **解决**：在启动容器或进程时设置环境变量 `ADMIN_PASSWORD="your-new-strong-password"`（需要时同时设置 `ADMIN_USERNAME`）并重启一次服务。系统会在启动时用新值覆盖数据库中的旧记录，随后即可移除这些环境变量。若验证器与恢复码都已丢失，请按[丢失验证器](#lost-authenticator-reset-2fa)一节使用一次性的 `RESET_2FA=true`。

### 3. 失败任务（`dead_letter`）报错 `Entry payload already garbage-collected before delivery`
- **原因**：任务因通道故障停留在死信队列的时间已超过 `RETENTION_HOURS`（默认 24 小时），且在清理过程中文章正文已被回收。
- **解决**：若希望预留更长的故障排查与重投时间，可将环境变量 `RETENTION_HOURS` 调大（如设为 `72` 或 `168` 小时）。
