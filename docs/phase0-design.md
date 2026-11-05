# Phase 0 — P2P 连通性测试设计

> 版本：0.1.0
> 日期：2026-11-06
> 状态：手动信令原型完成；自动信令与认证未实现
> 目标平台：Windows 10 / 11 x64

---

## 1. 目标与范围

Phase 0 要回答一个问题：

> 在东京及周边的普通家庭网络之间，不使用 TURN / 数据中继，能否建立稳定的加密 P2P DataChannel？

为回答这个问题，本阶段只实现：

- 手动交换 SDP / ICE candidate。
- WebRTC DataChannel。
- 双向 Ping/Pong、RTT、丢包统计。
- 选中路径、连接建立耗时、断线计数。
- JSON 诊断报告。
- 单 EXE、免安装、静态 CRT。

不实现：

- 屏幕采集/编码、鼠标键盘、剪贴板、文件传输。
- 设备 ID、连接密码、正式认证、限速。
- 自动信令、设备在线查询。
- TURN 中继。
- 自动重连 / ICE restart。
- GUI。

---

## 2. 技术验证结论

### 2.1 Rust

- 本机：Rust 1.99.1 stable，`x86_64-pc-windows-msvc`。
- 项目固定 `stable`，不依赖 nightly。
- Windows 发布使用 MSVC linker；未要求客户安装 Rust。

### 2.2 libdatachannel / Rust 绑定

| 方案 | 版本/状态 | 结论 |
|---|---|---|
| `datachannel`（libdatachannel Rust 绑定） | 0.16.1，2026-07 更新，MPL-2.0 | 可用，但需要 CMake + C++ + OpenSSL；静态发布链更复杂 |
| `libdatachannel` 本体 | C++，MPL-2.0 | 成熟，Windows 支持好；本阶段不作为首选 |
| `webrtc`（webrtc-rs） | 0.21.1，MIT/Apache-2.0 | 纯 Rust，ICE/DTLS/SCTP 完整；本阶段采用 |
| `str0m` | 0.25.0 | 更底层、更轻，但需要自行处理更多 I/O 与集成 |

选择 webrtc-rs 的原因：

1. 只需 Rust + MSVC，无需 CMake/OpenSSL。
2. `webrtc` crate 提供完整 PeerConnection/DataChannel 异步 API。
3. 本机验证 IPv4、IPv6、STUN、DTLS、SCTP 全部可用。
4. 静态 CRT 后 EXE 3.64 MiB，符合“数 MB”目标。
5. MIT/Apache-2.0，许可证简单。

保留的调整条件：如果真实家庭网络测试发现 webrtc-rs 在 MAP-E / DS-Lite / 对称 NAT 下的 ICE 行为不达标，再以 `libdatachannel` 或 `libjuice` 做对照实现。不要为了库的偏好提前增加构建复杂度。

### 2.3 加密与身份

- DataChannel 使用 DTLS 1.2；SDP `a=fingerprint:sha-256` 绑定证书。
- SCTP 提供可靠有序传输。
- 证书由 `rcgen` 生成，随机源来自 OS/`ring`。
- 当前没有应用层身份认证；SDP 令牌本身是唯一共享秘密。
- 正式版必须新增：设备 ID、临时密码、PAKE/OPAQUE、失败限速、会话权限、密钥与 DTLS 指纹绑定。

### 2.4 免费信令

Phase 0 不依赖信令服务，因此没有账户/免费额度风险。

下一阶段候选：

| 方案 | 优点 | 风险 |
|---|---|---|
| Cloudflare Workers + Durable Objects | WebSocket、全球边缘、免费额度 | Durable Objects 免费套餐限制需在实现前复核；账户/计费规则可能变化 |
| PHP + HTTPS 轮询（现有静态站） | 复用现有主机、无新账户 | 轮询延迟、并发与超时处理更复杂；免费主机可能限制 |
| 自建小型 VPS | 可控 | 违反“不使用付费服务”的约束，除非已有资源 |

结论：先实现一个**只交换 SDP/candidate 的最小 WebSocket 服务**，并在实现前用真实免费账户验证额度、WebSocket 与 Durable Objects 限制。信令服务不得接触 DataChannel 明文或最终会话密钥。

---

## 3. 架构

```text
┌──────────────────────────────┐
│ CLI (clap)                   │
│  offer / answer              │
└──────────────┬───────────────┘
               │
┌──────────────▼───────────────┐
│ exchange                     │
│  AKR1.<base64(JSON SDP)>     │
│  file / stdin / stdout       │
└──────────────┬───────────────┘
               │
┌──────────────▼───────────────┐
│ network::peer                │
│  PeerConnectionBuilder       │
│  ICE/STUN, UDP bind          │
│  event channels              │
└──────────────┬───────────────┘
               │
┌──────────────▼───────────────┐
│ session                      │
│  wait OnOpen                 │
│  Hello / Ping / Pong / Bye   │
│  RTT + loss + path           │
└──────────────┬───────────────┘
               │
┌──────────────▼───────────────┐
│ diagnostics                  │
│  CandidateSummary            │
│  SelectedPath (nominated)    │
│  SessionReport JSON          │
└──────────────────────────────┘
```

### 3.1 手动信令流程

```text
Offerer                          Answerer
  create PC                        (wait for token)
  create DataChannel
  create offer
  set local description
  wait ICE gathering
  emit AKR1 token  ──────────────▶ decode
                                   set remote offer
                                   create answer
                                   set local description
                                   wait ICE gathering
  decode ◀──────────────────────  emit AKR1 token
  set remote answer
  ICE/DTLS/SCTP
  DataChannel open
  Ping/Pong both directions
```

Phase 0 使用非 trickle ICE：等待 candidate 收集完成后把全部 candidate 放进 SDP。这样只需要交换一条令牌，适合人工复制。

### 3.2 Candidate 收集策略

- 默认 STUN：
  - `stun:stun.l.google.com:19302`
  - `stun:stun1.l.google.com:19302`
- 默认绑定：对 `8.8.8.8:80` 和 `[2001:4860:4860::8888]:80` 做 UDP `connect`，取默认路由源地址，再以 `:0` 绑定。
  - 只做路由查找，不发送数据包。
  - 避免 `0.0.0.0` / `[::]` 展开到 Hyper-V、WSL、VPN 等无效网卡。
  - 多网卡场景使用 `--bind` 显式指定。
- `--no-stun`：只收集 host candidate，用于回环/局域网。
- 不配置任何 TURN URL。

### 3.3 为什么等待“安静期”

实测 webrtc-rs 0.21.1 在双栈 + 多 interface 场景下，`on_ice_gathering_state_change(Complete)` 不一定触发。实现采用：

- 收到 `Complete` 立即结束；
- 否则至少等 2 秒；
- candidate 数量连续 900 ms 不变化则结束；
- 最长 `--gather-timeout-secs`。

这比固定 sleep 更稳，也避免永远等不到 Complete。

---

## 4. 协议

DataChannel label：`akikazu-phase0`
可靠、有序（默认 SCTP）。

消息为 JSON 文本：

```json
{"type":"hello","protocol":1,"role":"offerer"}
{"type":"ping","seq":1,"sent_unix_ms":1793897832000}
{"type":"pong","seq":1,"sent_unix_ms":1793897832000}
{"type":"bye","reason":"user exit"}
```

- 双方打开通道后都发送 `hello`。
- 双方每秒发送 `ping`，对端回 `pong`。
- 发送方用本地 `Instant` 计算 RTT，不依赖双方时钟同步。
- 超过 `ping_timeout` 未收到 `pong` 的 ping 计入丢失。
- 通道关闭、连接失败、Ctrl-C 或持续时间到期时写报告。

后续扩展预留：

- `seq` 可用于吞吐量测试的块序号。
- `type` 可用于文件传输控制消息。
- `protocol` 用于版本协商。

---

## 5. 诊断与隐私

### 5.1 CandidateSummary

统计本地 candidate：

- `host_ipv4` / `host_ipv6`
- `srflx_ipv4` / `srflx_ipv6`
- `prflx` / `relay` / `other`

`STUN: reachable` 只表示收集到 srflx candidate；`unverified` 不等于 STUN 失败，也可能是 `--no-stun` 或 IPv6 直连不需要 STUN。

### 5.2 SelectedPath

从 `get_stats(StatsSelector::None)`：

1. 选择 `nominated = true` 的 candidate pair；没有则取包数最多的一对。
2. 找到 local candidate 的地址族与类型。
3. 读取 pair 的 ICE RTT、包数、字节数。

报告只写 `ipv4` / `ipv6`、`host` / `srflx`，不写 IP 地址。

`remote_candidate_type` 可能为 `unknown`：webrtc-rs 0.21.1 的 stats 没有返回 remote candidate 条目。实现不猜测，报告中保留 `unknown`。

### 5.3 报告字段

`schema_version`、`role`、`result`、`started_unix_s`、`duration_ms`、`connection_setup_ms`、`peer_state`、`local_candidates`、`selected_path`、`rtt`、`disconnect_count`、`data_channel`、`notes`。

### 5.4 日志

- 默认 `akikazu_remote=info,webrtc=error,rtc=off`，避免 ICE 多网卡噪声淹没用户输出。
- `-v` 打开 debug。
- 不记录 SDP 令牌、密码或地址。
- 手动复制令牌时，终端/剪贴板仍可能泄露；这是 Phase 0 的已知信任边界。

---

## 6. 安全设计

| 项目 | Phase 0 | 正式版要求 |
|---|---|---|
| 传输加密 | DTLS 1.2 + SCTP | 保持，并做密码套件审计 |
| 对端身份 | SDP 指纹 + 手动令牌 | 设备密钥 + 密码认证 + 指纹绑定 |
| 设备 ID | 无 | 安全随机、可持久化、非硬件信息 |
| 连接密码 | 无 | OS 安全随机、默认临时、可固定 |
| 认证限速 | 无 | 失败计数、指数退避、临时封禁 |
| 信令明文 | 令牌含 ICE 凭据/指纹 | 信令服务只转发，不见会话密钥 |
| 日志脱敏 | 不写 IP/令牌 | 保持并加自动化测试 |
| 会话清理 | close DataChannel/PC | 清理临时凭据、票据、候选缓存 |

关键设计约束（下一阶段）：

1. 密码不能通过信令服务器明文传输。
2. 认证必须绑定到实际 DTLS 指纹/通道，防止设备 ID 冒用和中继 MITM。
3. 优先使用成熟 PAKE（如 OPAQUE）或经验证的 SAS 方案，不自行设计密码协议。
4. 连接密码只用于认证，不作为数据加密密钥。

---

## 7. 验收对照

| 验收项 | 状态 | 说明 |
|---|---|---|
| 两台 Win10/11 可运行 | 部分 | 单机双进程已验证；两台物理机未测 |
| 不需安装 Rust | 已验证 | 静态 CRT 单 EXE |
| 不需额外运行环境 | 已验证 | 仅系统 DLL |
| 不要求端口映射 | 设计满足 | ICE/STUN，无入站映射；防火墙仍可能影响 |
| 设备 ID 发起连接 | 未实现 | Phase 0 手动令牌 |
| 随机密码认证 | 未实现 | 下一阶段 |
| 加密 P2P 通道 | 已验证 | DTLS/SCTP |
| 收发测试数据 | 已验证 | Ping/Pong |
| IPv4 | 已验证 | 回环/本机网卡 |
| IPv6 | 已验证 | 强制 IPv6 绑定 |
| UDP NAT 穿透 | 部分 | ICE + STUN 已工作；跨 NAT 未测 |
| 不使用数据中继 | 已验证 | 无 TURN |
| 显示实际路径 | 已验证 | nominated pair |
| 检测连接中断 | 部分 | 状态事件 + 15 秒宽限；无自动重连 |
| 重新连接 | 未实现 | 手动信令无法重协商 |
| 延迟测试 | 已验证 | Ping/Pong RTT |
| 吞吐量测试 | 未实现 | 下一阶段 |
| 导出诊断日志 | 已验证 | JSON |
| 区分信令/ICE 失败 | 部分 | 令牌解码错误 vs 连接超时；自动信令后需细分 |
| 区分认证/网络失败 | N/A | 认证尚未实现 |
| 显示 IPv4/IPv6 | 已验证 | 选中路径 |
| 不把推测显示为确定 | 已验证 | STUN 用 reachable/unverified，remote 类型 unknown |
| 数据加密 | 已验证 | DTLS |
| 日志无明文密码 | N/A | 尚无密码 |
| 防止未认证设备 | 未实现 | 当前令牌即信任 |
| 认证限速 | 未实现 | 下一阶段 |
| 断开后清理 | 部分 | close + 进程退出；无显式凭据清理测试 |
| 多真实家庭网络 | 未验证 | 需要 Phase 0.5 实地测试 |

---

## 8. 实测记录

### 8.1 环境

- Windows 11 x64 build 26300
- Rust 1.99.1
- VS 2022 Build Tools 14.44
- 网络：WLAN，IPv4 私网 + 全局 IPv6，双栈
- STUN：Google 公共 STUN

### 8.2 回环 IPv4

命令：`--bind 127.0.0.1:0 --no-stun --duration-secs 8`

结果：

- 两个独立进程连接成功。
- `Connection path: IPv4 / host candidate`
- RTT min/avg/max：0.3 / 0.6 / 1.3 ms
- 丢包：0%
- 报告正常写出。

### 8.3 强制 IPv6

命令：`--bind [<global IPv6>]:0 --no-stun --duration-secs 5`

结果：

- `Connection path: IPv6 / host candidate`
- RTT min/avg/max：0.8 / 1.3 / 2.6 ms
- 丢包：0%
- 说明 webrtc-rs 的 IPv6 ICE 路径可用。

### 8.4 双栈 + STUN

默认绑定 + 默认 STUN：

- 4 个 candidate：IPv4 host、IPv6 host、IPv4 srflx、IPv6 srflx。
- `STUN: reachable`
- 选中 `IPv4 / host candidate`（本机同网段，host 优先级更高）。
- 连接建立约 0.8–1.0 秒。

### 8.5 发布物体积

- `cargo build --release`，`+crt-static`。
- 3,819,008 bytes / 3.64 MiB。
- `dumpbin /dependents`：仅 `kernel32`、`ws2_32`、`bcrypt`、`IPHLPAPI`、`ADVAPI32`、`ntdll` 等系统 DLL；无 `VCRUNTIME140.dll`。
- 3 秒采样：Working Set 8.4–8.9 MiB，Private 2.2 MiB。

---

## 9. 已知问题与后续处理

1. **无自动信令**：下一阶段实现 WebSocket 信令；先验证 Cloudflare 免费额度。
2. **无认证**：实现设备 ID + PAKE + 认证绑定 DTLS 指纹。
3. **无重连**：需要信令通道支持 ICE restart 的 SDP 交换；Phase 0 只检测并报告断开。
4. **无吞吐量/长稳**：增加分块随机数据、速率/流量上限、丢包统计。
5. **remote candidate 类型 unknown**：stats 缺失；可后续从 SDP 解析候选并在选中后按 IP/端口匹配，或等待上游修复。
6. **Windows 防火墙**：首次运行提示；需在文档中指导允许专用网络。正式产品应设计签名与防火墙规则说明。
7. **webrtc 0.x**：升级前做完整回归。
8. **默认单路由绑定**：多网卡/多路径需 `--bind`；后续可用 Windows `GetAdaptersAddresses` 做更精细的接口选择。

---

## 10. 下一阶段建议顺序

1. 用两台真实 PC 在东京都内做最小跨网络测试，记录接入方式、IPv4/IPv6、candidate 类型、是否成功、路径、RTT。
2. 实现 Cloudflare Workers + Durable Objects（或 PHP 轮询）最小信令，只交换 SDP/candidate。
3. 设备 ID + 临时密码 + PAKE，拒绝未认证会话。
4. 吞吐量/长稳/断线重连。
5. Win32 GUI。
6. 覆盖 MAP-E / DS-Lite / PPPoE / IPv6 IPoE / CGNAT 的线路矩阵。
