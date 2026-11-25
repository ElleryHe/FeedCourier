---
description: "在 FeedCourier (v0.3.1+) 中通过 WebUI 插件市场一键安装进程内沙箱 JS 插件，涵盖官方 ntfy 与 Bark 推送通道以及自定义单文件 .js 插件导入，零 sidecar 容器。"
---

# JS 插件（`ntfy`、`Bark` 与自定义 `.js`）

自 **`v0.3.1`** 起，FeedCourier 内置了**纯 Rust 进程内 JavaScript 沙箱引擎（`boa_engine`）**，可直接在单一主程序二进制中运行单文件 `plugin.js` 插件——**无需额外部署 Docker Compose sidecar、无需暴露新端口，也不依赖 Node.js 或 Python 运行时**。

管理员可以在 **WebUI 插件市场一键安装** 官方维护的 **ntfy** 与 **Bark** 推送通道，随时在线审计已安装插件的完整源码，或直接上传自定义的 `.js` 单文件插件。

> [!IMPORTANT]
> JS 插件会接收渲染后的文章内容（标题、摘要、清洗后的 HTML/纯文本与原文链接），并在隔离的 `boa_engine` 沙箱内执行（无本地文件系统、Shell 或环境变量权限）。请仅安装来自可信仓库的插件，或在确认安装前核对其源码与 `allowed_hosts` 域名白名单。

---

## 沙箱与网络安全边界

- **纯 Rust 进程内沙箱（`boa_engine`）**：每次调用在独立沙箱上下文中执行，完全隔离本地文件系统、环境变量、子进程与原始 Socket。
- **显式 `allowed_hosts` + 管理员显式提交 `format: "uri"` 策略**：`fc.fetch()` 仅允许请求以下两类目标：
  1. 插件在 `meta.allowed_hosts` 中显式声明的公网域名（例如 `ntfy.sh` 或 `api.day.app`），这些域名会直接展示在插件市场卡片与安装确认弹窗中，并全程经过连接层防 DNS Rebinding SSRF 的 `ProtectedHttpClient`。
  2. **仅由管理员显式提交的非空 `format: "uri"` 字段**所对应的精确 `(scheme, host, port)`（如自建 LAN / Docker 的 `ntfy` 或 `Bark` `server_url`），可精确放行直连而**无需放宽订阅源的全局 `SSRF_ALLOWLIST`**。`format: "uri"` 的 Schema `default` **仅当**其主机名是插件自身 `meta.allowed_hosts` 中声明的公网域名（如 `ntfy.sh`、`api.day.app`）时才允许前后端自动填充，且始终走公网 SSRF 防护（不赋予私网直连豁免）；凡不在 `meta.allowed_hosts` 内的 URI `default` 一律不自动填充（前端仅作灰色 `placeholder` 提示）。同时，插件 `validate()` 返回的 `normalized_config` 不得新增 `format: "uri"` 字段，也不得篡改已有 `format: "uri"` 字段的 `(scheme, host, port)` 源站。
- **云元数据硬拦截、代理 DNS 边界与 `use_proxy` 支持**：
  - 云元数据字面量 IP（`169.254.169.254`、`169.254.170.2`、`100.100.100.200`、`fd00:ec2::254` 及 IPv4 映射 IPv6 / NAT64 形式）、未指定地址（`0.0.0.0`、`::`）、云元数据主机名（`metadata.google.internal`、`metadata.goog`、`instance-data`）以及 HTTP `3xx` 重定向在所有 `fc.fetch()` 调用中（无论直连还是走代理）**永久硬拦截**。
  - 直连与本地 DNS 解析代理（`socks5://`）还会在建立连接前解析 DNS 并拦截指向云元数据 IP 的记录；远端 DNS 解析代理（`http://`、`https://`、`socks5h://`）由于 DNS 解析发生在代理服务端，客户端侧强制执行主机名与字面量 IP 校验。
  - 插件通道账号（`ChannelKind::Plugin`）完整支持账号级 **使用出站代理**（`use_proxy`）开关（未配置全局 `OUTBOUND_PROXY` 时 fail-closed 报错）。
- **并发上限、超时取消与连续超时熔断**：
  - 全局最多同时运行 **4** 个 JS 沙箱执行任务；每个钩子均带有墙钟超时预算，且在每次 `fc.*` 宿主调用前检查取消标记（避免超时后仍发起 HTTP 请求），单次 `fc.fetch()` 的 `timeoutMs` 也会自动截断至剩余钩子预算内。
  - 若某个 JS 插件连续触发 **3 次钩子总预算超时**（与等待共享沙箱槽位超时及单次 `fc.fetch()` 的短 `timeoutMs` 超时严格区分，避免误杀健康插件），自动熔断器会将其全局禁用并在 `plugins.disabled_reason` 中记录原因，WebUI 插件卡片与详情页顶部会以醒目的红色 **已自动禁用** 警告横幅提示管理员排查。
  - 边界说明：`boa_engine` 0.22 尚不提供针对纯 CPU 嵌套循环的外部抢占中断，因此恶意纯 CPU 死循环可能会占用 4 个沙箱槽位中的 1 个，直至 Boa 自身的单循环迭代上限触发退出。
- **SHA-256 完整性校验与凭据静态加密**：从市场下载的插件脚本在预览与安装前均强制校验 `index.json` 中声明的 SHA-256 摘要；`account_schema` / `destination_schema` 中标记为敏感的字段（`format: "password"` 或 `x-secret: true`）均使用 `MasterKey`（ChaCha20-Poly1305）加密落盘并在所有接口中脱敏。

---

## 在 WebUI 插件市场一键安装

1. 进入 FeedCourier WebUI 的 **插件中心 → 插件市场** 标签页。
2. 核对插件卡片上的中文简介、版本号以及 **允许访问域名（`allowed_hosts`）** 标签。
3. 点击 **一键安装**（或有新版本时点击 **更新至 vX.Y.Z**）：
   - 后端会拉取对应版本的 `plugin.js`（`POST /api/v1/plugins/install`，`dry_run: true`），执行 **SHA-256 校验**并在沙箱内预编译 `export const meta`，随后在弹窗中展示 **已校验 SHA-256**、**允许访问的外部域名** 与版本变更日志。
   - 若插件发布了多个版本，可在下拉框中自由选择目标版本。
4. 点击 **确认安装** 即可持久化写入 SQLite 并立即启用。

---

## 官方通道快速上手

### 1. ntfy 推送（`ntfy`）

向 [`ntfy.sh`](https://ntfy.sh) 或自建 ntfy 服务器推送 RSS / Atom / JSON Feed 文章，支持自定义 Topic、优先级、标签与 Bearer Token 鉴权。

1. 进入 **插件中心 → 插件市场** → 找到 **ntfy Push** → 点击 **一键安装** → **确认安装**。
2. 进入 **投递通道 → 新建账号** → 选择 **ntfy Push**。
3. 填写 **账号配置**：
   - **服务器地址（`server_url`）**：填写 `https://ntfy.sh` 或自建地址（如 `https://ntfy.example.com`、`http://ntfy:80`，内网/Docker 地址开箱直连，无需修改 `SSRF_ALLOWLIST`）。
   - **访问令牌（`access_token`，可选）**：若 Topic 开启鉴权，填写 `tk_...` 令牌。
   - **使用出站代理（可选）**：若需要经全局 HTTP/SOCKS5 代理访问 `ntfy.sh`，直接打开开关即可。
4. 填写 **首个目的地配置**：
   - **Topic（`topic`）**：目标 ntfy 主题名（例如 `feedcourier-alerts`）。
   - 可选配置 **Priority（优先级）** 与 **Tags（标签）**。
5. 点击 **验证账号** 或 **发送测试** 确认送达，即可在路由规则中绑定投递。

### 2. Bark 推送（`bark`）

向 [`api.day.app`](https://bark.day.app/) 或自建 Bark 服务推送简洁优雅的 iOS / macOS 通知，内置 **Telegram 快讯卡片风格的摘要排版**（精炼摘要、来源标识与点击直达原文链接），并按 UTF-8 字节预算自动安全截断以契合 APNs 载荷上限。

1. 进入 **插件中心 → 插件市场** → 找到 **Bark Push** → 点击 **一键安装** → **确认安装**。
2. 进入 **投递通道 → 新建账号** → 选择 **Bark Push**。
3. 填写 **账号配置**：
   - **服务器地址（`server_url`）**：填写 `https://api.day.app` 或自建 Bark 服务地址（内网/Docker 地址开箱直连，无需修改 `SSRF_ALLOWLIST`）。
   - **使用出站代理（可选）**：若需经由全局代理访问 `api.day.app`，可直接开启。
4. 填写 **首个目的地配置**：
   - **设备密钥（`device_key`）**：填写 Bark App 中的 Device Key（落盘时经 ChaCha20-Poly1305 加密，读取接口自动脱敏）。
   - 按需填写 **分组（`group`）**、**铃声（`sound`）**、**时效级别（`level`）** 或 **图标（`icon`）** 等可选参数。
5. 点击 **发送测试** 在设备上查收测试通知，随后在 **路由规则** 中绑定即可。

---

## 导入自定义单文件 `.js` 插件

如需接入私有 Webhook、内部网关或在本地调试新开发的插件：

1. 在 **插件中心** 右上角点击 **导入自定义插件 (.js)**。
2. 点击 **选择 .js 文件** 上传本地 `plugin.js` / `.mjs` 文件，或直接在等宽代码框中粘贴 ES 模块源码。
3. 点击 **校验并预览**（`POST /api/v1/plugins/upload`，`dry_run: true`），后端将在 `boa_engine` 沙箱内预编译并展示解析出的 `meta`、SHA-256 摘要、`allowed_hosts` 与核心版本兼容性。
4. 若目标插件 ID 已存在且希望原位更新，勾选 **若插件 ID 已安装则直接覆盖更新**。
5. 点击 **确认安装**；自定义导入的插件会标注 **`自定义`** 徽章以便区分。

---

## 生命周期管理

- **随时审计源码**：点击任意已安装 JS 插件卡片或详情页上的 **查看源码**（`GET /api/v1/plugins/{id}/source`），即可查看完整 `plugin.js` 源码与 SHA-256 校验值并一键复制。
- **一键更新、版本切换与自定义插件覆盖保护**：当市场索引中出现新版本时，已安装卡片会直接展示 **更新至 vX.Y.Z** 按钮；原位更新会同步刷新脚本与字段 Schema，同时完整保留已有通道账号、目的地与路由绑定。若目标插件 ID 当前为**自定义导入**版本（`source: "custom"`），市场安装默认返回 **`409 Conflict`** 防止误覆盖，需显式确认覆盖（`replace_custom: true`，WebUI 弹窗会展示红色警告与必填勾选框）。
- **停用保护与自动禁用恢复**：全局停用某个通道插件时，发往该插件的待投递任务会自动顺延（`deferred`），不消耗重试次数也不会误入死信。因连续 3 次超时触发熔断被自动禁用的插件会展示红色 **已自动禁用** 警告（`disabled_reason`）；管理员排查后手动重新启用即可清除原因并重置计数。
- **级联卸载保护**：卸载仍被通道账号或路由引用的插件会返回 **`409 Conflict`** 并列出关联的通道与路由清单，需显式确认级联卸载（`?cascade=true`）。
