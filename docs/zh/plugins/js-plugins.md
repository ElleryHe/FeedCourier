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
- **显式 `allowed_hosts` + 管理员配置 URI 边界**：`fc.fetch()` 仅允许请求以下两类目标：
  1. 插件在 `meta.allowed_hosts` 中显式声明的公网域名（例如 `ntfy.sh` 或 `api.day.app`），这些域名会直接展示在插件市场卡片与安装确认弹窗中。
  2. 管理员在通道账号或目的地 `format: "uri"` 字段中显式填写的主机地址（如自建服务器的 `server_url`）。
- **双层网络白名单、云元数据硬拦截与 `use_proxy` 支持**：
  - 云元数据与未指定地址（`169.254.169.254`、`100.100.100.200`、`fd00:ec2::254`、`0.0.0.0`、`::`、`metadata.google.internal`）以及 HTTP `3xx` 重定向在所有 `fc.fetch()` 调用中**永久硬拦截**。
  - 管理员在账号/目的地/实例 `format: "uri"` 字段中显式填写的精确 `(scheme, host, port)`（例如自建 LAN / Docker 的 `ntfy` 或 `Bark` 地址）自动精确放行直连，**无需放宽订阅源的全局 `SSRF_ALLOWLIST`**。
  - 插件声明的 `meta.allowed_hosts` 域名均经过 FeedCourier 连接层防 DNS Rebinding SSRF 的 `ProtectedHttpClient`。
  - 插件通道账号（`ChannelKind::Plugin`）完整支持账号级 **使用出站代理**（`use_proxy`）开关（未配置全局 `OUTBOUND_PROXY` 时 fail-closed 报错）。
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
- **一键更新与版本切换**：当市场索引中出现新版本时，已安装卡片会直接展示 **更新至 vX.Y.Z** 按钮；原位更新会同步刷新脚本与字段 Schema，同时完整保留已有通道账号、目的地与路由绑定。
- **停用保护**：全局停用某个通道插件时，发往该插件的待投递任务会自动顺延（`deferred`），不消耗重试次数也不会误入死信。
- **级联卸载保护**：卸载仍被通道账号或路由引用的插件会返回 **`409 Conflict`** 并列出关联的通道与路由清单，需显式确认级联卸载（`?cascade=true`）。
