# 飞弦（Fesen）工程规则

一台服务器就是一个组。客户端支持 Windows 10/11 (x64)、macOS 15+ (Apple Silicon)、Android 13+ (ARM64)，设备间以双向认证 QUIC 直连共享 IPv4/TCP 服务；服务端通过 Linux Docker（`amd64` / `arm64`）交付。

## 目录职责（禁止跨层堆砌）

- `apps/server`：控制面（HTTPS/WSS）、SQLite、组目录、直连信令与 Web `/admin/`；不监听或转发业务流量。
- `apps/core`：独立 Rust 核心（`node/` 状态机与隧道、`direct/` QUIC、`probe/` 探测、`routing.rs` 分流、`platform/` 三端适配）。
- `apps/desk`：Tauri 2 客户端（`src/{app,model,ui,dialogs,views,styles}` 单向分层，`src-tauri/` 仅负责 IPC 与预览壳）；关窗后 GUI 退出，传输不依赖 WebView。
- `packages/acl`、`packages/proto`：全端共用的地址/端口规则与协议定义。
- `packages/ui`：全端唯一设计系统与样式真源（`styles/tokens.css` 三层令牌 + 零依赖 ESM 组件），客户端与 Web `/admin/`、`/join` 直接共用。
- `deploy/`、`scripts/`：Docker 部署清单、三端打包与防泄露门禁脚本。

## 核心铁律

1. **产品第一性原理**：仅交付 IPv4/TCP 端到端直连；不开注册、不建业务中继、不扫网段、不改默认路由。保护本地非默认路由，冲突交由使用方虚拟 IPv4（`198.18.x.y`）、`.fesen` 别名或 `127.0.0.1:<端口>` 转换解决。
2. **权限与安全边界**：每组仅允许 **1 个超级管理员**（支持 `FESEN_ADMIN_USER` / `FESEN_ADMIN_PASSWORD[_FILE]` 首次幂等初始化、CLI `admin` / `passwd` 与后台改密）；登录与改密具备内存双维防爆破限流；直连 HTTPS 自动下发 HSTS；每次建流在发布端复验设备证书、最长 60s 授权租约及 `地址 × 端口` 规则，停用/撤销/换网立即断流。
3. **品牌与样式主导权**：严禁在 `packages/ui/styles/tokens.css` 之外硬编码色值、透明度或阴影，严禁各端私自复制组件变体；只呈现当前平台真实生效的控件。
4. **工程精简与卫生**：以最小范围完成改动，不添加未要求的抽象、依赖、文档或兼容壳；删除功能时同步清掉对应 CSS、JS 与文案，不留死代码，新增逻辑按既定目录归位。
5. **配置与隐私隔离**：私有配置仅入 `.env.local` 或 GitHub Secrets，本地笔记仅入 `.local/debug.md`（均不入 Git）；代码、测试与文档只用 RFC 5737 网段（`192.0.2.0/24`、`198.51.100.0/24`、`203.0.113.0/24`）及 `example.com`。提交前必须通过 `bash scripts/check-public.sh`。
6. **诚实验收**：严格区分编译、模拟与实机结果；虚拟网卡、系统 DNS、签名授权、托盘/磁贴与真实 NAT 直连必须实机验证。

## 视觉与交互标准

- **品牌与八卦标识**：中文「飞弦」/ 英文「Fesen」，代码/深链/环境变量统一 `fesen` / `FESEN_*`。Logo 锁定纯**艮卦 ☶**（5 段直角矩形，单色深浅，禁加外框/渐变/光晕），`viewBox="0 0 64 64"` 锁定坐标：上阳爻 `(6,8,52,8)`，中阴爻 `(6,28,21,8)` + `(37,28,21,8)`，下阴爻 `(6,48,21,8)` + `(37,48,21,8)`。
- **单色三态演进（`createTrigram`）**：`idle` 死门坤 ☷（`--yao-idle: 0.42`）→ `connecting` 变爻演卦（变爻 `1.0`、余爻 `--yao-dim: 0.38`）→ `active` 生门艮 ☶（满墨 `1.0`）。全端载体一致：Web/顶栏 `20×20`、macOS 菜单栏 `36×36px` 模板图（稳态 0 轮询）、Windows 托盘 `32×32` BGRA、Android 磁贴 `fesen_tile.xml`、启动图标 `mark.svg`（`#111315` 底 + `#f8fafc` 艮卦）。
- **色彩与尺度真源（`packages/ui/styles/tokens.css`）**：三层令牌架构（原语 → 语义 → 组件）。暖墨宣纸（浅）/ 松烟墨（深）基底，唯一品牌主导色哑光古金 `--fesen-gold: #c7a96b`（主按钮、激活开关、在线点、本机标识、焦点环）；支持 `sm|md|lg` 三档全局尺度；数值/地址统一 `var(--mono)` + `tabular-nums`。
- **布局与交互语法**：统一 `52px` 单行顶栏（左纯 `[ ☷/☶ 飞弦 ]`，macOS Overlay 交通灯右侧加竖线，Windows 无原生边框并在右侧并入最小化/最大化/关闭控件；右工具区超管额外带管理切换钮 `⌘⇧M / Ctrl+Shift+M`）；客户端与 Web `/admin/` 共用 `.panel-bar`（成员/设备/服务）；服务目录本机置顶并以古金 `osBadge` 区分，行操作统一收口至 `⋯` 原地 `.popover-menu`；保留 `:focus-visible` 焦点环、`Esc` 关闭弹窗与 `prefers-reduced-motion`。

## 构建、发布与验收

共用工具链：Rust 1.90+、Node.js 22.12+、pnpm 11。

| 目标 | 基线 | 本地构建命令 |
| --- | --- | --- |
| Windows | Windows 10/11 (x64, `x86-64-v2`) | `powershell -File scripts/build-windows.ps1`（需 MSVC / Windows SDK / WebView2，产 NSIS 安装包） |
| macOS | macOS 15+ (Apple Silicon) | 正式签名：`bash scripts/build-macos.sh`（配 `FESEN_TEAM_ID/SIGN_ID/HOST_PROFILE/TUNNEL_PROFILE`，`--install` 装至 `/Applications`）；本地测试：`bash scripts/build-macos.sh --unsigned` |
| Android | Android 13+ (`arm64-v8a`, `minSdk 33`) | 需 JDK 21、Android SDK 36/37 + Build Tools 36.1 + NDK 29；调试：`bash scripts/build-android.sh --debug`；签名配 `FESEN_ANDROID_KEYSTORE*` 跑 `bash scripts/build-android.sh` |
| 服务端 | Linux Docker (`amd64` / `arm64`) | `docker compose -f deploy/compose.yaml build`（原生跨编译 + `distroless/cc-debian12`，UID `10001:10001`） |

- **GitHub Actions**：
  - `ci.yml`：安全与语法检查、Rust 工作区测试与 Clippy、前端构建、`macos-15` Tauri 原生单测。
  - `release.yml`：推 `v*` 标签或手动触发，并行交付 **3 客户端 + 1 多架构 GHCR 镜像** 及 `SHA256SUMS.txt`；未配 `ANDROID_KEYSTORE_*` / `APPLE_*` Secrets 时自动降级输出 debug / unsigned 包。
- **自动化验收**：
  ```sh
  bash scripts/check-public.sh && bash -n scripts/*.sh
  cargo test --locked --workspace && cargo clippy --locked --workspace --all-targets -- -D warnings
  cargo test --locked --manifest-path apps/desk/src-tauri/Cargo.toml --lib
  pnpm -C apps/desk install --frozen-lockfile && pnpm -C apps/desk build
  ```
  - 真实 WebView 闭环：`.env.local` 配置 `FESEN_LAB_TARGET_*` 后跑 `bash scripts/test-webview.sh`。
  - 实机核验重点：原应用直访与重叠避让（虚拟 IP / `.fesen` / `127.0.0.1`）；活动流中关共享/撤设备/换网立即断流、失联最迟 60s 过期、阻断 UDP 报无法直连无中继；桌面关窗 GUI 退出核心常驻、Android 按需启停 VPN 且零通知权限。
