# Akikazu Remote — Phase 0 P2P Test

Windows 10/11 x64 上验证 **不经过 TURN 中继** 的加密 P2P DataChannel 能否建立并稳定传输。

这是 Phase 0 连通性测试程序，不是远程桌面成品。当前使用**手动交换 SDP 令牌**，尚未实现自动信令、设备 ID、连接密码、剪贴板和屏幕控制。

> 仓库：<https://github.com/AkiCust-PC/akikazu-remote>
>
> 状态：手动信令原型可运行；自动信令 / 认证 / GUI 尚未实现。

---

## 1. 已验证与未验证

### 已验证（本机，2026-11-06 JST）

- 两个独立进程通过 ICE 建立 P2P DataChannel。
- DTLS 加密、SCTP 可靠有序通道。
- 双向 Ping/Pong，RTT、丢包统计。
- IPv4 host 路径与 IPv6 host 路径分别实测成功。
- 双栈 + 公共 STUN：生成 IPv4/IPv6 host 与 srflx candidate。
- 单 EXE、静态 CRT、无 VC++ Redistributable、无额外 DLL。
- Release EXE **3.64 MiB**。
- 单元测试 7 个、回环集成测试 1 个全部通过。

### 未验证

- **两台真实 Windows PC、不同家庭宽带之间**的 NAT 穿透成功率。
- MAP-E / DS-Lite / CGNAT / PPPoE / IPv6 IPoE 的实际组合。
- 长时间稳定性、吞吐量、断线重连。
- 东京 ↔ 千叶 / 埼玉 / 神奈川的实际线路。

同机回环成功不能替代跨网络验证；这两类结果必须分开看。

---

## 2. 快速开始

### 2.1 开发机构建

需要：

- Windows 10/11 x64
- Rust stable（`rust-toolchain.toml` 会固定到 stable）
- Visual Studio 2022 Build Tools，安装 **Desktop development with C++**（提供 MSVC linker）

```powershell
cargo build --release
```

产物：

```text
target\release\akikazu-remote.exe
```

`.cargo/config.toml` 已启用 `+crt-static`，因此运行时不需要 VC++ Redistributable。第三方依赖仍以系统 DLL 为主，见 `docs/phase0-design.md`。

> 如果项目路径过长导致 MSVC 报 `c1.dll` / D8050，可把构建目录放到短路径：
>
> ```powershell
> $env:CARGO_TARGET_DIR = "C:\akr-target"
> cargo build --release
> ```

### 2.2 两台 PC 手动连接

**PC A（offer 侧）**

```powershell
.\akikazu-remote.exe offer
```

程序输出一行 `AKR1...` 令牌。把整行发给 PC B。

**PC B（answer 侧）**

```powershell
.\akikazu-remote.exe answer
```

粘贴 PC A 的令牌，回车。PC B 再输出一行 `AKR1...` 应答令牌，把它发回 PC A。

**PC A**

粘贴应答令牌，回车。双方进入：

```text
P2P status: connected
Connection path: IPv4 / host candidate
RTT 1.2 ms (seq 1)
```

按 `Ctrl-C` 结束，双方各写一份 JSON 诊断报告。

令牌很长，终端可能自动换行；解码时会去掉所有空白字符，因此整段粘贴即可。

### 2.3 同一台 PC 自动验证

开两个 PowerShell 窗口，或让 answer 先启动：

```powershell
New-Item -ItemType Directory .\run -Force

# 窗口 1
.\akikazu-remote.exe answer `
  --offer-file .\run\offer.txt `
  --answer-file .\run\answer.txt `
  --bind 127.0.0.1:0 --no-stun `
  --duration-secs 10 `
  --report .\run\answer-report.json

# 窗口 2
.\akikazu-remote.exe offer `
  --offer-file .\run\offer.txt `
  --answer-file .\run\answer.txt `
  --bind 127.0.0.1:0 --no-stun `
  --duration-secs 10 `
  --report .\run\offer-report.json
```

两侧都会输出 `Result: duration reached` 和相同路径。

### 2.4 常用参数

| 参数 | 说明 |
|---|---|
| `--stun <URL>` | 覆盖默认 STUN，可重复。默认使用两个 Google 公共 STUN。 |
| `--no-stun` | 只用 host candidate，适合局域网/回环测试。 |
| `--bind <ADDR>` | 指定本地 UDP 绑定地址，可重复。默认自动选择默认路由的 IPv4/IPv6 源地址。 |
| `--gather-timeout-secs` | ICE candidate 收集上限，默认 6 秒。 |
| `--exchange-timeout-secs` | 等待对端令牌的时间，默认 120 秒。 |
| `--connect-timeout-secs` | 等待 DataChannel 打开的时间，默认 30 秒。 |
| `--ping-interval-secs` | Ping 间隔，默认 1 秒。 |
| `--ping-timeout-secs` | 超过该时间未收到 Pong 记为丢包，默认 5 秒。 |
| `--duration-secs` | 到时自动结束；省略则一直运行到 Ctrl-C。 |
| `--report <PATH>` | JSON 诊断报告路径。 |
| `-v` | 打开应用与 WebRTC 调试日志。 |

---

## 3. 输出与诊断

正常连接时输出包括：

- `Local ICE candidates`：本地 IPv4/IPv6 host 与 srflx 数量。
- `STUN`：是否收集到 srflx candidate。
- `Connection path`：从**已提名**的 ICE candidate pair 读取，不根据本机网卡猜测。
- `RTT`：应用层 Ping/Pong 的最近一次 RTT。
- 退出摘要：min/avg/max RTT、丢包率、断线次数、路径。

JSON 报告包含：

- 角色、结束原因、连接建立耗时、会话时长。
- 本地 candidate 类型统计。
- 选中路径的地址族、candidate 类型、ICE RTT、包/字节计数。
- Ping/Pong 统计。
- 已知限制说明。

报告**不写公网/私网 IP**。终端调试日志默认也不打印地址。

---

## 4. 技术选型

| 模块 | 选择 |
|---|---|
| 语言 | Rust stable（本机验证 1.99.1） |
| P2P | `webrtc` 0.21.1（webrtc-rs，纯 Rust） |
| ICE / STUN | `rtc-ice` / `rtc-stun` |
| 加密 | DTLS 1.2 + SCTP，自签名证书，指纹写入 SDP |
| 异步运行时 | Tokio |
| CLI | clap |
| 信令 | Phase 0 手动令牌；服务端尚未实现 |
| 中继 | **不使用 TURN** |

### 为什么不是先上 libdatachannel

`datachannel-rs`（0.16.1）是 libdatachannel 的 Rust 绑定，功能完整，许可证为 MPL-2.0；但它需要 CMake、C++ 工具链和 OpenSSL 构建链，静态链接与 Windows 发布复杂度更高。Phase 0 的首要目标是尽快得到可运行、可诊断的 Windows 单 EXE，因此先选纯 Rust 的 webrtc-rs：

- 只需 Rust + MSVC linker。
- 静态 CRT 后单 EXE 约 3.6 MiB。
- 支持 ICE、STUN、DTLS、SCTP DataChannel、IPv4/IPv6。
- MIT / Apache-2.0 双许可证。

如果后续在 MAP-E / DS-Lite 等网络中发现 webrtc-rs 的 ICE 行为不满足要求，再评估 libdatachannel 或 libjuice，而不是现在为 C++ 构建链增加交付风险。

---

## 5. 安全边界

- DataChannel 本身由 **DTLS 加密**；SDP 中的 certificate fingerprint 绑定对端证书。
- 当前信任模型是**手动令牌**：令牌包含 ICE 凭据、DTLS 指纹和 candidate。必须通过可信渠道传递，不能公开发布。
- 令牌不是身份认证。任何人拿到令牌都可能尝试应答。
- **尚未实现**设备 ID、连接密码、PAKE、认证失败限速、设备密钥绑定。
- 日志不记录令牌、密码或 IP；但终端里手动粘贴的令牌仍可能被截图/剪贴板工具捕获。
- 首次运行可能触发 Windows Defender Firewall 提示。Phase 0 不要求端口映射；若防火墙阻止入站 UDP，P2P 可能失败。

Phase 0 只适合实验室和自有设备测试，不能直接给客户使用。

---

## 6. 实测记录（同机）

环境：Windows 11 x64（build 26300）、Rust 1.99.1、Release 构建。

| 项目 | 结果 |
|---|---|
| EXE 体积 | 3,818,496 bytes / 3.64 MiB |
| 依赖 | 仅 Windows 系统 DLL；无 VC++ Redistributable、无第三方 DLL |
| 内存（3 秒采样） | 每进程 Working Set 约 8.4–8.9 MiB，Private 约 2.2 MiB |
| CPU | 采样时 0 |
| IPv4 回环 RTT | min 0.3 / avg 0.6 / max 1.3 ms，0% 丢包（8 秒） |
| IPv6 强制路径 | 连接成功，路径显示 `IPv6 / host candidate` |
| 双栈 + STUN | 生成 4 个 candidate（v4/v6 host + srflx），路径 `IPv4 / host candidate` |
| 测试 | 7 unit + 1 loopback integration，全部通过 |

这些数字只代表同一台机器上的回环/本机网卡路径，不代表东京家庭宽带之间的 NAT 穿透结果。

---

## 7. 已知限制

- 没有自动信令服务、设备 ID、连接密码、认证与限速。
- 没有 TURN；对称 NAT 或严格防火墙下可能完全无法连接。
- 没有重连 / ICE restart。网络切换后只能检测到断开并退出。
- 没有吞吐量、长稳、丢包注入测试；Phase 0 当前只完成延迟与连接路径验证。
- 没有 Win32 GUI，当前为 CLI。
- `webrtc` 仍是 0.x API，升级 minor 版本可能破坏编译。
- 选中路径的 `remote_candidate_type` 目前可能显示 `unknown`：webrtc-rs 0.21 的 stats 没有返回 remote candidate 条目。报告不会猜测。
- 默认只绑定默认路由对应的 IPv4/IPv6 地址，避免 Hyper-V / WSL / VPN 虚拟网卡产生噪声；多网卡场景可重复传 `--bind`。

---

## 8. 下一阶段

1. **自动信令**：Cloudflare Workers + Durable Objects 免费套餐，或 PHP + HTTPS 轮询；只交换 SDP/candidate，不传输数据。
2. **设备 ID + 临时密码**：密码不经过信令服务器；使用 PAKE/OPAQUE 或等价成熟方案，并把认证绑定到 DTLS 指纹。
3. **吞吐量 / 长稳 / 重连**：分块随机数据、速率上限、流量上限、ICE restart。
4. **Win32 GUI**：在 CLI 验证通过后接入。
5. **实际线路矩阵**：东京 ↔ 东京/千叶/埼玉/神奈川，覆盖 MAP-E、DS-Lite、PPPoE、IPv6 IPoE、CGNAT。

详细设计与验收对照见 [docs/phase0-design.md](docs/phase0-design.md)。

---

## 9. 许可证

项目许可证尚未确定；在明确之前，本仓库不授予使用、复制、修改或再分发许可。

第三方依赖按其各自许可证提供，主要包括：

- `webrtc` / `rtc-*`：MIT OR Apache-2.0
- `tokio`：MIT
- `clap`：MIT OR Apache-2.0
- `ring`：ISC 等（见 crate 内 LICENSE）
- `rcgen`：MIT OR Apache-2.0

引入依赖前应重新核对 `Cargo.lock` 对应版本的许可证。
