# 飞弦（Fesen）

一台服务器就是一个组。客户端面向现代主流平台（Windows 10/11 x64、macOS 14+ Apple Silicon、Android 13+ ARM64），每个设备都能发布和访问 IPv4/TCP 服务；服务端推荐在 Linux 上以 Docker 部署。

- `apps/server`：Rust、SQLite、Web 管理台、组目录和直连信令。不开注册，不监听或转发业务流量。
- `apps/core`：独立 Rust 核心、QUIC 直连、地址分流和平台网卡适配。发布端对每个新连接验身份、租约和地址 × 端口规则。
- `apps/desk`：Tauri 2、TypeScript、Tailwind。只走本机 IPC；关闭桌面窗口后 GUI 进程退出，传输不依赖 WebView。
- `packages/acl`、`packages/proto`、`packages/ui`：共用规则、协议与全端设计系统。

## 核心约束

- **路由与分流**：保护本地非默认路由，只捕获共享范围；同 IP 不同端口可共存，本地重叠时使用虚拟地址。
- **品牌与界面统一**：严格遵守 [视觉与交互规范](docs/design.md)。Logo 仅用纯艮卦 ☶ 锁定 5 矩形单色深浅，禁止添加卦外元素或色彩变化；所有端共用 `packages/ui` 基础样式与组件，只展示当前平台真实生效的控件。
- **配置与隐私隔离**：
  - 机器配置只写入 `.env.local`（不入 Git）或 GitHub Secrets；代码、测试与文档仅使用 RFC 5737 保留网段（`192.0.2.0/24`、`198.51.100.0/24`、`203.0.113.0/24`）及 `example.com`。
  - 本地调试文档规范为 `.local/debug.md`（不入 Git，**允许不存在**）；若本机存在，联调与排查时将其作为私有上下文参考，严禁把 `.env.local` 或 `.local/` 中的任何地址、端口、账号或记录写入受版本控制的文件。
  - 提交前运行 `bash scripts/check-public.sh`。
- **验收口径**：验收按 [计划](docs/plan.md) 和 [构建、发布与验收](docs/build.md)，严格区分编译、模拟和实机结果。
