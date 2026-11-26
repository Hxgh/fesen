# 飞弦（Fesen）

一台服务器就是一个组。客户端支持 Windows 10/11 (x64)、macOS 14+ (Apple Silicon)、Android 13+ (ARM64)，设备间以双向认证 QUIC 直连共享 IPv4/TCP 服务；服务端推荐在 Linux 上以 Docker 运行。

## 目录职责（禁止跨层堆砌）

- `apps/server`：控制面（HTTPS/WSS）、SQLite、组目录、直连信令与 Web `/admin/`；不监听或转发业务流量。
- `apps/core`：独立 Rust 核心（`node/` 状态机与隧道、`direct/` QUIC、`probe/` 探测、`routing.rs` 分流、`platform/` 三端适配）。
- `apps/desk`：Tauri 2 客户端（`src/{app,model,ui,dialogs,views,styles}` 单向分层，`src-tauri/` 仅负责 IPC 与预览壳）；关窗后 GUI 退出，传输不依赖 WebView。
- `packages/acl`、`packages/proto`：全端共用的地址/端口规则与协议定义。
- `packages/ui`：全端唯一设计系统与样式真源（`styles/tokens.css` 令牌体系 + 零依赖 ESM 组件），客户端与 Web `/admin/` 直接共用。
- `deploy/`、`docs/`、`scripts/`：部署清单、产品/设计/构建规范、构建与门禁脚本。

## 核心铁律

1. **产品第一性原理**：仅交付 IPv4/TCP 端到端直连；不开注册、不建业务中继、不扫网段、不改默认路由。保护本地非默认路由，冲突交由使用方虚拟 IPv4 或 `127.0.0.1` 端口转换解决。
2. **品牌与样式主导权**：严格遵守 [设计规范](docs/design.md)。Logo 锁定纯艮卦 ☶ 5 矩形单色深浅；全端颜色、阴影、圆角与尺度必须消费 `packages/ui/styles/tokens.css` 语义令牌，**严禁在各端样式中硬编码色值或复制组件变体**，只展示当前平台真实生效的控件。
3. **工程精简与卫生**：以最小范围完成改动，不添加未要求的抽象、依赖、文档或兼容壳；删除功能时同步清掉对应 CSS、JS 与文案，不留死代码，新增逻辑按既定目录归位，不重新堆积巨型单文件。
4. **配置与隐私隔离**：私有配置仅入 `.env.local` 或 GitHub Secrets，本地笔记仅入 `.local/debug.md`（均不入 Git）；代码、测试与文档只用 RFC 5737 网段（`192.0.2.0/24`、`198.51.100.0/24`、`203.0.113.0/24`）及 `example.com`。提交前必须通过 `bash scripts/check-public.sh`。
5. **诚实验收**：按 [计划](docs/plan.md) 与 [构建与验收](docs/build.md) 核验，严格区分编译、模拟与实机结果。
