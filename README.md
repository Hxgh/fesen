# 飞弦（Fesen）

一个自托管服务器就是一个组。成员用接入链接和账号登录，即可在组内发布和访问 IPv4/TCP 服务——无需两两配对，无需为应用配置代理。

支持 **Windows 10/11 (x64)**、**macOS 15+ (Apple Silicon)**、**Android 13+ (ARM64)**；服务端推荐在 **Linux** 上通过 Docker 运行。

## 工作方式

- **控制面（`apps/server`）**：Rust + SQLite，走 HTTPS/WSS。只负责身份认证、成员与设备管理、在线服务目录、候选地址交换（打洞信令）与账号配置同步，**不监听、不转发任何业务流量**。
- **数据面（`apps/core`）**：两端直接建立双向 TLS 1.3 认证的 QUIC（UDP）直连。无地址重叠时直访原 `IP:端口`；本地网段重叠或同组多端同名碰撞时，原地址仍走本地，自动分配虚拟 IPv4（`198.18.x.y`）或 `.fesen` 别名，也可手动映射为本机 `127.0.0.1:<端口>`。无法直连时直接报错，不降级中转。

## 服务端部署

Linux 推荐使用 Docker Compose（也支持 GitHub Releases 二进制或 `cargo build --release -p fesen-server`）：

```sh
export FESEN_DOMAIN=group.example.com
export FESEN_ADMIN_PASSWORD='请替换为强密码'   # 仅首次空库生效，初始化后可移除
docker compose -f deploy/compose.yaml pull
docker compose -f deploy/compose.yaml up -d
```

每组仅允许 **1 个超级管理员**，也可用 `docker compose -f deploy/compose.yaml run --rm server admin <账号>` 初始化、用 `passwd <账号>` 重置密码。打开 `https://你的域名/admin/` 创建成员并分发接入链接（无注册入口）；保留 `state` 卷持久化数据。已有证书时可直接设置 `FESEN_TLS_CERT`、`FESEN_TLS_KEY`、`FESEN_ORIGIN`、`FESEN_LISTEN` 启动（支持证书热轮换，证书无效不退回明文），完整变量见 [`.env.example`](.env.example)。

跨 IPv4 NAT 辅助探测默认关闭（macOS / Android 自动优先尝试公网 IPv6 直连），按需二选一：
- **外部 STUN**：设 `FESEN_STUN=域名:端口`（中心仍仅开 HTTPS）。
- **内置 UDP 探测**：Compose 加 `-f deploy/compose.udp.yaml` 并放行 `4455/UDP`（或单容器设 `FESEN_UDP` / `FESEN_PUBLIC_UDP_PORT`）。

## 客户端使用

从 GitHub Releases 下载安装包，或按 [构建与验收](docs/build.md) 源码构建。

- **统一目录与发布**：打开接入链接登录即可访问与发布（支持单 IP、范围、CIDR、端口清单，及单机只读端口发现）。管理员登录客户端可在顶栏直接切换至组管理台，无需二次登录。
- **连通与预览**：点击服务状态徽章执行端到端协议探测（HTTP 校验 `GET /` 响应头与前 `8 KiB` 正文，SSH 读 Banner，RDP 走 X.224，HTTPS 验 TLS ServerHello，TCP/SMB 验建连）；行菜单 `⋯` 支持应用内网页预览、浏览器打开、复制命令/地址与 `127.0.0.1` 端口转换。
- **生命周期与安全**：支持启动自动连接、同网安全恢复上次共享（换网自动拦截）与按设备 `machine_id` 云备份；桌面关窗后 GUI 退出而核心常驻（macOS 菜单栏 / Windows 托盘）；Android 按需启停系统 VPN 且不申请通知权限。日志仅存本机（上限 10 MiB / 7 天，不记录密码、令牌与业务内容）。

## 文档

- [产品边界与验收](docs/plan.md)
- [视觉与交互规范](docs/design.md)
- [构建、发布与验收](docs/build.md)
- [MIT License](LICENSE)
