# 构建、发布与验收

## 本地配置与隐私门禁

- 私有配置写 `.env.local`（模板见 [`.env.example`](../.env.example)），本地调试笔记可选写 `.local/debug.md`（均已被忽略，严禁入 Git）。
- 提交前必须运行 `bash scripts/check-public.sh`。

## 本地构建

共用工具链：Rust 1.90+、Node.js 22.12+、pnpm 11。

| 平台 | 基线 | 构建命令与说明 |
| --- | --- | --- |
| Windows | Windows 10/11 (x64) | 需 MSVC、Windows SDK、WebView2；运行 `powershell -File scripts/build-windows.ps1` 生成 NSIS 安装包，安装至 Program Files（首次 UAC 提权）。 |
| macOS | macOS 15+ (Apple Silicon) | 正式签名：配置 `FESEN_TEAM_ID`、`FESEN_SIGN_ID`、`FESEN_HOST_PROFILE`、`FESEN_TUNNEL_PROFILE` 后跑 `bash scripts/build-macos.sh`（加 `--install` 安装到 `/Applications`）。本地测试：`bash scripts/build-macos.sh --unsigned`（ad-hoc 签名，仅可发布）。 |
| Android | Android 13+ (ARM64) | 需 JDK 21/17、Android SDK（Platform 36/37.0、Build Tools 36.1、NDK 29）及 `JAVA_HOME`/`ANDROID_HOME`。调试包：`bash scripts/build-android.sh --debug`；签名包配置 `FESEN_ANDROID_KEYSTORE*` 后跑 `bash scripts/build-android.sh`。 |
| 服务端 | Linux (amd64 / arm64) | Docker：`docker compose -f deploy/compose.yaml build`；原生：`cargo build --locked --release -p fesen-server`。 |

## GitHub Actions

- **CI（`.github/workflows/ci.yml`）**：安全与语法检查、Rust 工作区测试与 Clippy、前端构建、macOS Tauri 原生单测。
- **Release（`.github/workflows/release.yml`）**：推送 `v*` 标签或手动触发，发布 Windows 安装包、macOS 压缩包、Android APK、Linux x64/arm64 服务端二进制、GHCR 多架构镜像及 `SHA256SUMS.txt`。

| 签名 Secrets（可选） | 未配置时的回退 |
| --- | --- |
| `ANDROID_KEYSTORE_BASE64`、`ANDROID_KEYSTORE_PASS`、`ANDROID_KEY_ALIAS`、`ANDROID_KEY_PASS`（可选） | 自动输出 `-debug.apk` |
| `APPLE_TEAM_ID`、`APPLE_SIGN_ID`、`APPLE_CERT_P12_BASE64`、`APPLE_CERT_PASSWORD`、`APPLE_HOST_PROFILE_BASE64`、`APPLE_TUNNEL_PROFILE_BASE64` | 自动输出 `--unsigned` 包 |
| `APPLE_ID`、`APPLE_APP_PASSWORD`（依赖上方签名） | 仅签名，跳过公证 |

## 自动化验收

```sh
bash scripts/check-public.sh
bash -n scripts/*.sh
cargo test --locked --workspace
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --manifest-path apps/desk/src-tauri/Cargo.toml --lib
pnpm -C apps/desk install --frozen-lockfile && pnpm -C apps/desk build
```

- **Linux 网络闭环测试**（真实 SQLite / HTTP / WS / UDP / QUIC / TCP，其中 `publisher_expires_established_flow_with_half_open_control` 跑真实 60s 时钟）：
  ```sh
  docker run --rm -v "$PWD:/repo:ro" -w /work rust:1.90-bookworm sh -ec '
    cp /repo/Cargo.toml /repo/Cargo.lock .
    cp -R /repo/.cargo /repo/packages .
    mkdir apps && cp -R /repo/apps/core /repo/apps/server apps/
    cargo test --locked --workspace
  '
  ```
- **真实 WebView 闭环验收**：在 `.env.local` 配置 `FESEN_LAB_TARGET_IP` / `FESEN_LAB_TARGET_PORT` 后运行 `bash scripts/test-webview.sh`。

## 实机验收清单

1. **三端互访与避让**：原应用直访原地址、同 IP 不同端口共存、冲突拒绝；切至重叠网段确认原地址走本地，虚拟 IP / `.fesen` / `127.0.0.1` 转发访远端。
2. **权限与租约**：活动流中关共享、撤设备、换物理网络立即中断；仅阻断控制面时既有流最迟 60 秒过期；阻断对等 UDP 报无法直连且无中继。
3. **生命周期**：桌面关窗 GUI 退出而流不断、空闲核心退出清路由与 DNS；Android 按需启停 VPN 且释放前台服务。
