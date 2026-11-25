# 飞弦（Fesen）

一个自托管服务器就是一个组。成员用管理员分发的接入链接和账号登录，在「我的共享」发布 IPv4/TCP 服务，在「组内服务」访问其他成员的服务。不用两两配对，不用为应用配置代理。

支持现代主流平台：**Windows 10/11 (x64)**、**macOS 14+ (Apple Silicon)**、**Android 13+ (ARM64)**，服务端推荐在 **Linux** 上通过 Docker 运行。

无地址重叠时使用原 `IP:端口`；与本地网络重叠时，原地址继续走本地，用客户端显示的虚拟 IPv4 访问远端。可为指定 IPv4 设置 `名称.fesen`，端口保持不变。备注只改显示名。

控制面是 Rust、SQLite 和 HTTPS/WSS；业务数据在两个设备间走双向认证的 QUIC，不经过服务端。共享局域网中的网站时，访问端直连发布端，发布端校验权限后，再向网站的真实 IP 和端口建立 TCP。NAT 或防火墙不允许直连时，客户端会报错，不改走中继。

## 服务端部署

Linux 推荐使用 Docker Compose；也可从 GitHub Releases 下载 Linux x64/arm64 独立二进制，或在其他平台执行 `cargo build --release -p fesen-server` 原生编译。

1. 把域名的 A 记录指向服务器，开放 TCP 80、443。默认不监听 UDP。
2. 在仓库根目录运行（按需替换域名与镜像地址；省略 `FESEN_IMAGE` 时改用 `docker compose -f deploy/compose.yaml build` 本地源码构建）：

   ```sh
   export FESEN_DOMAIN=group.example.com
   export FESEN_IMAGE=ghcr.io/<owner>/<repo>-server:latest
   docker compose -f deploy/compose.yaml pull
   docker compose -f deploy/compose.yaml run --rm server admin admin
   docker compose -f deploy/compose.yaml up -d
   ```

3. 打开 `https://你的域名/admin/`，创建成员账号，分发页面中的接入链接。没有注册接口。保留 `state` 卷以持久化账号、设备和会话。

已有证书时也可用单容器或独立二进制：设置 `FESEN_TLS_CERT`、`FESEN_TLS_KEY`，将 `FESEN_ORIGIN` 设为对外 HTTPS 地址，`FESEN_LISTEN` 设为 HTTPS 监听地址。程序会检查证书文件轮换；证书无效时不会退回明文。完整变量示例见 [`.env.example`](.env.example)。

默认只交换设备自身的地址。macOS 和 Android 可经公网 IPv6 建立 QUIC 直连，共享目标仍是 IPv4/TCP。需要协助跨 IPv4 NAT 建连时，可二选一：

- 设置 `FESEN_STUN=域名:端口`，让客户端向指定 STUN 服务发现公网 IPv4/UDP 地址。中心仍只开 HTTPS；STUN 不接收账号、令牌或业务流量。
- 自托管探测：Compose 命令加 `-f deploy/compose.udp.yaml`，并放行 `4455/UDP`。单容器可显式设置 `FESEN_UDP`；外部与监听端口不同时再设 `FESEN_PUBLIC_UDP_PORT`。

## 客户端

从 GitHub Releases 下载对应平台的安装包，或按 [构建、发布与验收](docs/build.md) 从源码构建。

打开接入链接（也可粘贴到客户端）后登录。共享地址支持单个 IPv4、逗号清单、连字符范围及 CIDR；端口支持清单和范围。实际发布的是地址集合与端口集合的组合，只有地址和端口同时重叠才冲突。预设默认关闭，登录不会自动发布；控制连接失效或切换物理网络后需重新确认共享。

「组内服务」中，点状态进行轻量探测：网站请求 `GET /`（正文最多读 64 KiB），普通 TCP 只检查建连。HTTP 302 只表示跳转，不等于页面已显示。点「网页测试」在应用内看真实页面；「我的共享」中的本机测试不代替另一台设备的验证。

服务行的「⋯」可复制地址；HTTP 网站也可用系统浏览器打开。无重叠时用原地址，有重叠时用当前虚拟地址，范围共享先选择一对 IP 和端口。

Android（13+）点「接入」建立系统 VPN，点「停止接入」释放 VPN；只打开界面不会接入。不申请通知权限，可从应用或系统快捷磁贴断开，且不接管默认流量。

桌面关窗后图形界面与 WebView 进程退出，传输和发布由独立核心继续工作；macOS 使用原生菜单栏，Windows 使用系统托盘。macOS 未签名本地构建仅可发布，不安装本机访问路由。客户端诊断日志仅存本机，上限 10 MiB、最多保留 7 天，不记录密码、令牌或业务内容。

仅支持 IPv4/TCP 业务，不改默认路由；同一原 IP 上的非共享端口仍走原网络，不转发 UDP/ICMP。Android 严格私人 DNS 或应用自带 DNS 可能绕过系统别名，此时可用虚拟 IPv4。单组并集上限 65,536 个 IPv4；本机虚拟池 `198.18.0.0/15` 不参与发布。

## 文档

- [交付目标与验收](docs/plan.md)
- [视觉与交互规范](docs/design.md)
- [构建、发布与验收](docs/build.md)
- [MIT License](LICENSE)
