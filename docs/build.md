# 构建、发布与验收

## 本地配置与调试文档（不入 Git）

- **配置模板**：[`.env.example`](../.env.example)。本机联调目标、签名路径和服务器参数统一写入 `.env.local`（已被 `.gitignore` 忽略）。
- **私有调试文档**：可选放在 `.local/debug.md`（已被 `.gitignore` 和 `.dockerignore` 忽略，**允许不存在**）。存在时可记录本机拓扑、测试设备与实机验收笔记供联调参考。
- **防泄露门禁**：禁止将真实内网/公网 IP、域名、端口、密码、证书、Keystore 或实机笔记写入受版本控制的文件；提交前运行 `bash scripts/check-public.sh`。

## 目标平台与本地构建

共用工具链：Rust 1.90+、Node.js 22.12+、pnpm 11。服务端部署见 [README](../README.md)。

| 平台 | 基线 | 本地构建方式 |
| --- | --- | --- |
| Windows | Windows 10/11 (x64) | 安装 MSVC C++ 构建工具、Windows SDK 和 WebView2。运行 `powershell -File scripts/build-windows.ps1`，用生成的 NSIS 安装包安装到 Program Files；首次启动需管理员确认 UAC。 |
| macOS | macOS 14+ (Apple Silicon) | 正式签名：在环境变量或 `.env.local` 中设置 `FESEN_TEAM_ID`、`FESEN_SIGN_ID`、`FESEN_HOST_PROFILE`、`FESEN_TUNNEL_PROFILE`（分别对应 `io.fesen.client` 与 `io.fesen.client.tunnel` 的 Packet Tunnel System Extension profile），运行 `bash scripts/build-macos.sh`（加 `--install` 可安装到 `/Applications`）；对外分发另做公证。本地测试包运行 `bash scripts/build-macos.sh --unsigned`（ad-hoc 签名，仅可发布，不激活网络扩展，不修改 `/Applications`）。 |
| Android | Android 13+ (ARM64) | 安装 JDK 21（或 17）、Android SDK（Platform 36 与 37.0、Build Tools 36.1、NDK 29），设置 `JAVA_HOME`、`ANDROID_HOME`。调试包运行 `bash scripts/build-android.sh --debug`；发布签名包设置 `FESEN_ANDROID_KEYSTORE`、`FESEN_ANDROID_KEYSTORE_PASS`、`FESEN_ANDROID_KEY_ALIAS`（及可选 `FESEN_ANDROID_KEY_PASS`）后运行 `bash scripts/build-android.sh`。 |
| 服务端 | Linux (amd64 / arm64) | 推荐 Docker：`docker compose -f deploy/compose.yaml build`。原生编译（各平台通用）：`cargo build --locked --release -p fesen-server`。 |

macOS 首次打开（含接入链接）仅检查配置，用户点击「启用网络扩展」后才提交系统申请；未签名包无扩展申请。

## GitHub Actions 自动打包

- **CI（`.github/workflows/ci.yml`）**：每次 push / PR 自动执行防泄露与语法检查、Rust 工作区测试与 Clippy、前端构建及 macOS 客户端原生单测。
- **Release（`.github/workflows/release.yml`）**：推送 `v*` 标签（如 `v0.1.0`，纯语义化标签会自动同步 `tauri.conf.json` 应用版本号）或手动触发 `workflow_dispatch`，自动构建并发布：
  - Windows x64 客户端安装包（`*_windows-x64-setup.exe`）
  - macOS Apple Silicon 客户端（`*_macos-arm64.zip` 或 `-unsigned.zip`）
  - Android ARM64 APK（`*_android-arm64.apk` 或 `-debug.apk`）
  - Linux 多架构 Docker 镜像（`ghcr.io/<owner>/<repo>-server`，`linux/amd64` + `linux/arm64`）
  - Linux x64 / arm64 服务端独立二进制（`*-server_*_linux-*.tar.gz`）
  - `SHA256SUMS.txt` 校验文件

**可选签名 Secrets（不配也可先完成全平台测试版打包）：**

| 平台 | GitHub Repository Secrets | 未配置时的行为 |
| --- | --- | --- |
| Android 正式签名 | `ANDROID_KEYSTORE_BASE64`、`ANDROID_KEYSTORE_PASS`、`ANDROID_KEY_ALIAS`、`ANDROID_KEY_PASS`（可选） | 自动回退构建 `-debug.apk`（可直接真机安装；配置固定 Keystore 后支持跨版本无缝覆盖升级） |
| macOS 正式签名 | `APPLE_TEAM_ID`、`APPLE_SIGN_ID`、`APPLE_CERT_P12_BASE64`、`APPLE_CERT_PASSWORD`、`APPLE_HOST_PROFILE_BASE64`、`APPLE_TUNNEL_PROFILE_BASE64` | 自动回退为 `--unsigned`（ad-hoc 签名，仅可发布） |
| macOS 公证（可选） | `APPLE_ID`、`APPLE_APP_PASSWORD`（依赖上面的签名 Secrets） | 仅签名，跳过 Apple 公证 |

一次性生成自用 Android Keystore 并转 Base64（仅本地保存，切勿提交 `.jks`）：

```sh
keytool -genkeypair -v -keystore release.jks -keyalg RSA -keysize 2048 -validity 10000 -alias fesen
base64 < release.jks | tr -d '\n'
```

## 服务端规范化部署与平滑迁移

若此前使用旧镜像或旧环境变量部署过中心服务，可按以下最小步骤规范化迁移到 `fesen-server`（完整保留原 SQLite 账号与设备数据）：

1. **备份状态卷**：停止旧容器后，先备份持久化目录（`/state/fesen.sqlite` 或旧库文件）到宿主机备份路径；若原数据文件名为旧名称，在挂载卷中将其重命名/同步至新 `FESEN_DB` 指定路径（默认 `/state/fesen.sqlite`）。
2. **统一环境变量与镜像**：将 `deploy/compose.yaml` 或宿主机环境变量统一切换为 `FESEN_IMAGE`、`FESEN_ORIGIN`、`FESEN_DB`（可选）、`FESEN_STUN`（可选）与 `FESEN_UDP_PORT`（默认不监听 UDP），移除旧容器名称与旧别名。
3. **启动并核验**：拉取或构建 `fesen-server` 镜像启动容器，验证 `GET /healthz` 返回 `ok`、`/admin/` 管理员登录正常且原有成员与设备记录仍在，`/join` 页面生成 `fesen://join?server=...` 链接。
4. **客户端升级**：macOS、Windows、Android 客户端按需安装新版 Fesen 包后，使用同一组地址重新接入即可。

## 自动化验收

```sh
bash scripts/check-public.sh
cargo test --locked --workspace
cargo clippy --locked --workspace --all-targets -- -D warnings
pnpm -C apps/desk install --frozen-lockfile
pnpm -C apps/desk build
```

Linux 网络闭环测试在 Linux 环境或本机 Docker 中运行（真实 SQLite、HTTP/WS、UDP、双向认证 QUIC、目标 TCP；进程内包设备代替宿主机网卡）：

```sh
docker run --rm -v "$PWD:/repo:ro" -w /work rust:1.90-bookworm sh -ec '
  cp /repo/Cargo.toml /repo/Cargo.lock .
  cp -R /repo/.cargo /repo/packages .
  mkdir apps
  cp -R /repo/apps/core /repo/apps/server apps/
  cargo test --locked --workspace
'
```

其中失联租约用例 `publisher_expires_established_flow_with_half_open_control` 使用真实时钟运行约一分钟。

本地真实网页渲染验收：在 `.env.local` 中设置 `FESEN_LAB_TARGET_IP` 与 `FESEN_LAB_TARGET_PORT`（指向 Docker 可直达的局域网 HTTP 服务），然后运行 `bash scripts/test-webview.sh`。脚本使用一次性 Linux 容器、独立测试组和无缓存 WebKitGTK，不修改宿主路由或端口。

## 实机验收清单

源码编译和容器模拟通过不计作实机通过。三端签名、虚拟网卡、系统 DNS、托盘/通知和跨真实 NAT 须按以下流程实测：

1. **三端互访与冲突隔离**：三端各登录设备并发布 TCP/HTTP 服务，原应用按原地址访问；切换至存在地址重叠的网络，确认原地址继续访问本地，虚拟 IPv4 与 `.fesen` 别名访问远端；验证同 IP 不同端口共存、重叠规则拒绝。
2. **权限、撤权与租约**：维持活动流，分别关闭共享、撤销设备、切换物理网络，确认对应流中断且其他共享不受影响；阻断共享端控制面而保留对等 UDP，确认既有流最迟 60 秒关闭；阻断对等 UDP 时提示「无法直连」且无中继流量。
3. **生命周期与清理**：Windows/macOS 关窗后进程列表中 GUI/WebView 退出、传输不中断；无任务后核心退出并清除路由与 DNS 规则。Android 13+ 按需建立与停止系统 VPN，释放 TUN 路由和前台服务。
