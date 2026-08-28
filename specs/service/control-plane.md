# 规格：control-plane —— 引擎服务进程控制面（WebSocket JSON-RPC）

> **规格属性**：本文件是双支线共享规格，也是**引擎服务进程控制协议的唯一契约源**；
> 属兼容契约三层中的第三层（仓库根 `AGENTS.md`／[ADR-0003](../../docs/adr/0003-dual-track-native-rewrite.md)：
> 引擎服务进程控制协议），单方面破坏 = MAJOR。行为事实标准 = `HyperSoundEngineRust/crates/hse-service`（随本阶段落地）。
> 术语基线见仓库根 [`CONTEXT.md`](../../CONTEXT.md)；书写规范见 [`specs/README.md`](../README.md)。
> 关联决策：[ADR-0001 独立进程形态](../../docs/adr/0001-engine-as-independent-process.md)、
> [ADR-0002 双音频入口](../../docs/adr/0002-dual-audio-ingress.md)。

---

## 一、范围与定位

- **控制面（Control Plane）**（CONTEXT.md 术语）：接入方向引擎服务进程下发参数、查询状态、管理会话的通道。
  本契约覆盖其全部对外可观测行为；音频数据面（回环捕获、DSP、渲染）不在本文件范围内，
  仅以状态机相位与统计计数器的形式在结果中显影。
- **控制面与数据面分离**（规划书 §2.2）：控制面连接可以随时断开重连，不影响已建立的音频流；
  数据面异常通过事件通知与统计计数器上报，不要求控制面在线。
- **当前阶段范围声明**（Phase 2 起，Phase 3 批次一修订）：单客户端假设（§九）、应用层心跳暂缓（§九）、
  处理链为引擎子链 midSide → biquad → compressor → reverb-simple → bassEnhancer → limiter（§八）、
  音频入口为回环拦截 + 推流（Phase 3 起，见 push-stream.md）。
  推流入口的同端口复用分流规则见 [`push-stream.md`](push-stream.md)。

## 二、传输与寻址

| 项 | 契约 |
|---|---|
| 协议 | WebSocket（RFC 6455），文本帧承载 JSON-RPC 2.0 |
| 默认地址 | `ws://127.0.0.1:4780/` |
| 端口覆盖 | 启动参数 `--port <N>` **优先于**环境变量 `HSE_SERVICE_PORT`，二者均缺省时取 4780；端口值非法或被占用时进程启动失败并以非零码退出 |
| 绑定接口 | 仅绑定回环地址 127.0.0.1，不监听外部网卡 |
| 路径分量 | 不参与语义：任何请求路径等价处理 |
| 子协议 | 不使用 `Sec-WebSocket-Protocol` 协商标识 |

## 三、消息封包（JSON-RPC 2.0 三形态）

所有控制消息均为 UTF-8 编码的 JSON 文本帧，形态仅有三种。

### 形态 A：请求（带 id）

```json
{"jsonrpc":"2.0","id":1,"method":"listDevices","params":{}}
```

- `jsonrpc` 恒为字符串 `"2.0"`；`method` 为本契约方法表内的方法名；
- `id` 为整数或字符串（本协议不接受 `null` 作为 id）；同一连接内允许乱序 id；
- `params` 必须为对象（按名传参）；省略时视为空对象 `{}`。

### 形态 B：响应——成功

```json
{"jsonrpc":"2.0","id":1,"result":{"render":[],"capture":[]}}
```

### 形态 C：响应——失败

```json
{"jsonrpc":"2.0","id":7,"error":{"code":-32001,"message":"state does not allow configure"}}
```

- `error.code` 取值见 §六错误码表；`message` 为面向诊断的英文短句，其措辞不进入兼容契约，
  客户端只允许依赖 `code` 判定。

### 形态 D：事件通知（无 id 字段的请求形态）

```json
{"jsonrpc":"2.0","method":"event.phase","params":{"from":"idle","to":"starting"}}
```

- 无 `id` 字段即通知，服务端对通知**永不回包**；
- 当前契约定义的事件见 §七。

### 封包总则

1. **批处理不支持**：数组形态的请求整体按一条无效请求应答（-32600，id 取 null）：

```json
{"jsonrpc":"2.0","id":null,"error":{"code":-32600,"message":"batch requests are not supported"}}
```

2. 解析失败（文本帧不是合法 JSON）→ -32700，id 取 null；
3. 同一连接上，请求按到达序串行处理，响应与事件按发生序发出（§九）；
4. JSON 数值精度：协议中的 u64 计数器（xrun 计数、framesProcessed 等）实际运行值远小于 2^53，
   可安全穿过 JS 客户端的 number 类型；实现侧必须以 64 位无符号整数计数，不得因序列化截断。

## 四、phase 状态机

服务进程维护单一引擎相位（phase），取值四态：`idle` / `starting` / `running` / `stopping`。

### 4.1 状态图

```text
            configure 成功
  ┌──────┐ ──────────────────▶ ┌────────────┐
  │ idle │                     │ idle·已配置 │
  └──────┘ ◀ ─ ─ ─ ─ ─ ─ ─ ─  └────────────┘
    ▲  ▲     start 失败(-32000)      │
    │  │                           │ start（唯一入边）
    │  │                           ▼
    │  │  后端就绪失败        ┌──────────┐   后端就绪    ┌─────────┐
    │  └──────────────────── │ starting │ ───────────▶ │ running │
    │                        └──────────┘              └─────────┘
    │                                                       │ stop
    │  渲染排空＋设备释放完成    ┌──────────┐                   │
    └──────────────────────── │ stopping │ ◀─────────────────┘
                              └──────────┘
```

### 4.2 跃迁表

| 从 | 触发 | 到 | 可观测副作用（按发生序） |
|---|---|---|---|
| idle | configure 成功 | idle | `config` 更新为 applied 快照 |
| idle | start 且已配置 | starting | 发 `event.phase {from:"idle",to:"starting"}` |
| starting | 后端流建立完成 | running | 发 `event.phase {from:"starting",to:"running"}`；随后发 start 成功响应 |
| starting | 后端初始化失败 | idle | 发 `event.phase {from:"starting",to:"idle"}`；随后发 start 失败响应（-32000） |
| running | stop | stopping | 发 `event.phase {from:"running",to:"stopping"}` |
| stopping | 渲染环排空且设备释放完成 | idle | 发 `event.phase {from:"stopping",to:"idle"}`；随后发 stop 成功响应 |

约束：

1. `starting` 与 `stopping` 是**瞬态**：只由服务内部推进，客户端无法用任何方法令服务停留在其中；
2. 事件先于对应的 RPC 响应发出；start 成功响应发出的时刻 phase 已是 `running`，
   stop 成功响应发出的时刻 phase 已是 `idle`；
3. 相位只在上述边上变化；不存在跨态跃迁（如 running 直达 idle）。

## 五、方法规格

> 方法表共六个。规划书草案阶段的独立 getStats 方法已并入 `getState.stats` 字段，以本契约为准。

### 5.1 listDevices —— 枚举音频端点

请求（params 必须为空对象）：

```json
{"jsonrpc":"2.0","id":1,"method":"listDevices","params":{}}
```

成功结果示例：

```json
{"jsonrpc":"2.0","id":1,"result":{"render":[{"id":"{0.0.0.00000000}.{11112222-3333-4444-5555-666677778888}","name":"Speakers (High Definition Audio)","isDefault":true},{"id":"{0.0.0.00000000}.{99998888-7777-6666-5555-444433332222}","name":"CABLE Input (VB-Audio Virtual Cable)","isDefault":false}],"capture":[{"id":"{0.0.1.00000000}.{aaaabbbb-cccc-dddd-eeee-ffff00001111}","name":"CABLE Output (VB-Audio Virtual Cable)","isDefault":false}]}}
```

DeviceInfo 字段表：

| 字段 | 类型 | 说明 |
|---|---|---|
| `id` | string | 稳定设备标识（Windows IMMDevice ID 字符串），同类别内唯一，可直接作为 `configure.renderDeviceId` |
| `name` | string | 友好名（如扬声器/耳机名、CABLE Input 等），仅供展示 |
| `isDefault` | bool | 是否该类别（render 或 capture）的**系统默认端点**；每个非空数组内恰有一个 `true` |

该方法在任何 phase 下可用，不改变状态、不产生副作用。

#### GWT-CP-01：枚举结果结构完备
- **给定**：服务已启动，WASAPI 端点枚举成功
- **当**：发送 `listDevices`
- **则**：结果的 `render` 与 `capture` 两数组均存在；每个元素含 `id`/`name`/`isDefault` 三字段且类型正确；每个非空数组内 `isDefault:true` 的元素恰有一个；`id` 在各自类别内无重复

#### GWT-CP-02：任意相位可枚举
- **给定**：phase 为四态中任一态
- **当**：发送 `listDevices`
- **则**：正常返回结构完备的结果；phase 与各计数器不变

### 5.2 getState —— 查询相位与统计

请求（params 为空对象）：

```json
{"jsonrpc":"2.0","id":2,"method":"getState","params":{}}
```

成功结果示例：

```json
{"jsonrpc":"2.0","id":2,"result":{"phase":"idle","config":null,"stats":{"xrunsIn":0,"xrunsOut":0,"framesProcessed":0,"uptimeMs":15230},"lastParams":null}}
```

| 字段 | 类型 | 说明 |
|---|---|---|
| `phase` | string | 四态之一 |
| `config` | object\|null | 最近一次**成功** configure 的 applied 快照原样回显；从未成功配置过则为 null |
| `lastParams` | object\|null | 最近一次 setParams 的 params 快照原样回显；从未设置过则为 null |
| `stats.xrunsIn` | u64 | 输入侧异常累计（捕获过载、推流入环丢块），与 `event.xrun.totalIn` 同源同值 |
| `stats.xrunsOut` | u64 | 输出侧欠载累计，与 `event.xrun.totalOut` 同源同值 |
| `stats.framesProcessed` | u64 | DSP 线程累计处理帧数（立体声帧，每声道计一帧），跨 start/stop 周期累计不清零 |
| `stats.uptimeMs` | u64 | 进程启动至今毫秒数（时钟读取仅发生在控制面线程，不违反核心确定性铁律） |

全部状态驻内存：进程重启即回到 config=null、lastParams=null、计数器归零，无持久化。

#### GWT-CP-03：状态查询字段完备
- **给定**：服务处于任意相位
- **当**：发送 `getState`
- **则**：结果含 `phase`/`config`/`stats`/`lastParams` 四个顶层键；`stats` 含四个计数键；`phase` 值属于四态枚举；`config` 与 `lastParams` 要么为 null 要么为对象

#### GWT-CP-04：计数器单调不减
- **给定**：服务已启动并至少经历一次 start/stop 循环
- **当**：间隔任意时长先后两次调用 `getState`
- **则**：第二次的四个 stats 计数值均 ≥ 第一次对应值

### 5.3 configure —— 设置入口配置（仅 idle）

请求：

```json
{"jsonrpc":"2.0","id":3,"method":"configure","params":{"mode":"loopback","renderDeviceId":null,"sampleRate":48000,"blockSizeFrames":256}}
```

成功结果（applied 为生效配置的原样回显）：

```json
{"jsonrpc":"2.0","id":3,"result":{"applied":{"mode":"loopback","renderDeviceId":null,"sampleRate":48000,"blockSizeFrames":256}}}
```

| 参数 | 类型 | 校验层级 | 说明 |
|---|---|---|---|
| `mode` | string | 非 `"loopback"` → -32602 | 入口形态；当前唯一合法值即回环拦截 |
| `renderDeviceId` | string\|null | 非法引用 → -32000 | 回环拦截目标渲染端点的 `id`（接入方播放所至的输出设备或虚拟缆 CABLE Input）；null 表示该类别系统默认渲染端点。语义锚定：本参数选择的是**被捕获的拦截源**；最终渲染出口在本阶段固定取系统默认渲染端点，未暴露配置键 |
| `sampleRate` | u32 | 非 ≥1 整数 → -32602 | 期望采样率；实际流格式协商失败属后端错误，在 start 时报 -32000 |
| `blockSizeFrames` | u32 | 非 ≥1 整数 → -32602 | 期望每块帧数（事件驱动轮询周期）；后端能力上限校验失败在 start 时报 -32000 |

- **仅 phase=idle 可调用**，否则报 -32001 且状态（含既有 config）不变；
- 校验分两级：结构与静态域检查在 configure 内完成；需要后端参与的检查（格式协商、能力上限）
  允许推迟到 start；但 renderDeviceId 若非 null 且不在当前枚举结果中，configure 即报 -32000。

#### GWT-CP-05：idle 合法配置生效
- **给定**：phase=idle，参数通过结构校验且 renderDeviceId 为 null 或存在于当前枚举结果
- **当**：发送 `configure`
- **则**：响应 result.applied 与请求 params 四字段逐字段相等；此后 getState.config 等于该 applied 对象

#### GWT-CP-06：非 idle 一律拒绝
- **给定**：phase ∈ {starting, running, stopping}
- **当**：发送任意 `configure`（无论内容是否合法）
- **则**：响应 error code=-32001；getState.config 保持原值不变

#### GWT-CP-07：结构非法拒绝且不留痕
- **给定**：phase=idle；分别取 mode="capture"、sampleRate=0、sampleRate=48.5、blockSizeFrames=0、缺失任一键
- **当**：逐一发送 `configure`
- **则**：每次响应 error code=-32602；getState.config 不变（此前配置过的仍保持）

#### GWT-CP-08：未知设备引用报后端失败
- **给定**：phase=idle，renderDeviceId 设为枚举结果中不存在的字符串
- **当**：发送 `configure`
- **则**：响应 error code=-32000；getState.config 不变

### 5.4 start —— 启动引擎链路

请求（params 为空对象）与成功结果：

```json
{"jsonrpc":"2.0","id":4,"method":"start","params":{}}
```

```json
{"jsonrpc":"2.0","id":4,"result":{"started":true}}
```

- 前置条件：phase=idle 且 `config ≠ null`（此前至少一次 configure 成功）；
- 服务依次执行：打开拦截源流 → 打开渲染流 → 建立 rtrb 双环 → 启动 DSP 线程；
  任一步失败则整体回滚到 idle。

#### GWT-CP-09：正常启动全序
- **给定**：phase=idle 且 config 已存在，后端设备可用
- **当**：发送 `start`
- **则**：在同一连接上先后收到两条通知 event.phase {from:"idle",to:"starting"}、event.phase {from:"starting",to:"running"}，然后收到 started:true；此后 getState().phase == "running"

#### GWT-CP-10：未配置不可启动
- **给定**：phase=idle 且自进程启动以来没有任何成功的 configure
- **当**：发送 `start`
- **则**：响应 error code=-32001；不发 event.phase；phase 保持 idle

#### GWT-CP-11：瞬态与非 idle 相位拒绝重复启动
- **给定**：phase ∈ {starting, running, stopping}
- **当**：发送 `start`
- **则**：响应 error code=-32001；phase 不变

#### GWT-CP-12：后端失败回滚到 idle
- **给定**：config 存在，但拦截目标设备已被移除或被独占占用
- **当**：发送 `start`
- **则**：收到 event.phase {from:"starting",to:"idle"} 通知与 error code=-32000 响应；此后 getState().phase == "idle" 且 config 保留（修正设备后可直接重新 start）

### 5.5 stop —— 停止引擎链路

请求（params 为空对象）与成功结果：

```json
{"jsonrpc":"2.0","id":5,"method":"stop","params":{}}
```

```json
{"jsonrpc":"2.0","id":5,"result":{"stopped":true}}
```

- 前置条件：phase=running。停止次序：停流取新 → 渲染环排空 → 释放设备 → 回 idle；
  排空期间不再产生新的 xrun 上报；释放阶段的后端异常被吞掉并强制完成回 idle（停止路径不允许卡死）。

#### GWT-CP-13：running 正常停止
- **给定**：phase=running
- **当**：发送 `stop`
- **则**：先后收到 event.phase {from:"running",to:"stopping"}、event.phase {from:"stopping",to:"idle"}，然后收到 stopped:true；此后 getState().phase == "idle"，config 与 lastParams 均保留

#### GWT-CP-14：非 running 拒绝停止
- **给定**：phase ∈ {idle, starting, stopping}
- **当**：发送 `stop`
- **则**：响应 error code=-32001；phase 不变

### 5.6 setParams —— 参数快照下发

请求（试点子链三键齐全的示例）：

```json
{"jsonrpc":"2.0","id":6,"method":"setParams","params":{"params":{"biquad":{"type":"peaking","f0":120,"q":0.8,"gainDb":3.5},"reverbSimple":{"roomSize":0.5,"damping":0.3,"wet":0.25,"dry":0.75,"preDelayMs":20,"width":1,"type":"hall"},"limiter":{"enabled":true,"thresholdDb":-1,"lookaheadMs":5,"attackMs":1,"releaseMs":60,"truePeak":true}}}}
```

成功结果（warnings 语义见下文）：

```json
{"jsonrpc":"2.0","id":6,"result":{"accepted":true,"warnings":["myPluginKey"]}}
```

- **快照语义**：params.params 是**全量快照**，整体替换上一次快照（对齐两支线既有的
  "setParams 整体替换"约定）；省略的顶层键视为回落内置缺省，**不是增量合并**；
- params.params 缺失或不是对象 → -32602；
- 可识别顶层键内部的子键域与 clamp 行为以对应模块规格为准
  （[`biquad`](../dsp/biquad.md) §三、[`reverb-simple`](../dsp/reverb-simple.md)、[`limiter`](../dsp/limiter.md)），
  协议层只做键存在性与 JSON 类型匹配的结构检查；数值越界由模块自身 clamp，不产生 warnings、不算错误；
- **热应用**：phase=running 时快照经无锁命令通道送 DSP 线程，在**下一块边界**整快照生效；
  正在处理的块不受影响；控制面线程不得持任何 DSP 内部锁、不得在音频回调路径分配（架构铁律）；
- 非 running 时快照仅存入状态（lastParams），待下次 start 生效。

#### 可识别键表（引擎子链六模块；随模块落地扩展，属向后兼容变更）

| 顶层键 | 子键 | 类型 | 说明 |
|---|---|---|---|
| `midSide` | `width` | number | M/S 宽度（对齐全链 stereoWidth，1=恒等） |
| | `voiceBalance` | number | 人声比例（pitch 语义位；引擎子链恒等传递于 wb=0） |
| `biquad` | `type` | string | 八种枚举之一：peaking / lowshelf / highshelf / lowpass / highpass / bandpass / notch / allpass |
| | `f0` | number（Hz） | 中心/转折频率 |
| | `q` | number | Q 值 |
| | `gainDb` | number（dB） | 仅 peaking/shelf 类生效 |
| `compressor` | `enabled` | bool | 旁路开关（false=恒等） |
| | `thresholdDb` / `ratio` / `kneeDb` | number | 压缩曲线 |
| | `attackMs` / `releaseMs` | number（ms） | 包络时间常数 |
| | `makeupDb` | number（dB） | 补偿增益 |
| | `outputGain` | number | 输出线性增益 |
| | `sidechainEnabled` | bool | 按规格 §4.5 从输入派生单声道和 sidechain |
| `reverbSimple` | `roomSize` | number | 房间尺寸 |
| | `damping` | number | 高频阻尼 |
| | `wet` / `dry` | number | 湿/干信号比例 |
| | `preDelayMs` | number（ms） | 预延迟 |
| | `width` | number | 立体声宽度 |
| | `type` | string | 混响算法变体（枚举见 reverb-simple 规格） |
| `bassEnhancer` | `enabled` | bool | 旁路开关 |
| | `cutoffHz` / `q` | number | 低通提取 |
| | `harmonicType` | string | odd / even / atan / soft |
| | `harmonicGain` / `mix` / `levelDb` | number | 谐波路径 |
| | `lowBoostDb` | number（dB） | 低音下潜（-6..12，缺省 0=关闭） |
| `limiter` | `enabled` | bool | 限幅级旁路开关 |
| | `thresholdDb` | number（dB） | 门限 |
| | `lookaheadMs` | number（ms） | 前瞻 |
| | `attackMs` / `releaseMs` | number（ms） | 包络时间常数 |
| | `truePeak` | bool | 真峰超采样检测开关 |

#### warnings 语义

1. warnings 恒为数组（可为空），元素为字符串；
2. **不可识别的顶层键**：整体忽略并记入 warnings，元素为该键名原文（如 "myPluginKey"）；
   这是有意的向前兼容机制——客户端可安全携带为后续版本准备的扩展键而不破坏互操作；
3. 可识别顶层键内**不可识别的子键**：忽略并记入 warnings，元素形如 "<顶层键>.<子键>"（如 "biquad.order"）；
4. warnings 元素按字典序升序排列（确定性输出，便于机械断言）；
5. 只要结构校验通过，warnings 不影响 accepted:true 与快照存储。

#### GWT-CP-15：合法快照接收并存储
- **给定**：phase 任意，params.params 为含三个可识别键的对象
- **当**：发送 `setParams`
- **则**：响应 {"accepted":true,"warnings":[]}；getState.lastParams 与请求快照逐键相等

#### GWT-CP-16：未知键进 warnings 且被忽略
- **给定**：快照同时含可识别键与未知顶层键 myPluginKey、biquad 内未知子键 order
- **当**：发送 `setParams`
- **则**：响应 accepted:true；warnings 恰为 ["biquad.order","myPluginKey"]（字典序）；
  lastParams 中不含 myPluginKey、biquad 中不含 order

#### GWT-CP-17：快照整体替换
- **给定**：先发送含三键的快照 A 并成功
- **当**：再发送仅含 limiter 键的快照 B
- **则**：lastParams 只剩 B 的内容（不含来自 A 的 biquad/reverbSimple 残留）

#### GWT-CP-18：热应用在块边界生效
- **给定**：phase=running，DSP 线程正在逐块处理
- **当**：发送合法 `setParams`
- **则**：响应返回后，从某一完整块起输出完全按新快照计算，且不存在任何一块混合新旧两种快照
  （机械判定：探针在相邻两块的边界处观察到参数版本号恰好切换一次）

#### GWT-CP-19：params 结构非法拒绝
- **给定**：params 缺失、params.params 为数组或字符串
- **当**：逐一发送 `setParams`
- **则**：每次响应 error code=-32602；lastParams 不变

## 六、错误码表

两份服务层文档共用本表（[`push-stream.md`](push-stream.md) 引用同一套码，语义以此为准）：

| 码 | 名称 | 触发条件 |
|---|---|---|
| -32700 | Parse error（解析错误） | 文本帧不是合法 JSON |
| -32600 | Invalid Request（无效请求） | jsonrpc ≠ "2.0"、缺 method、批处理数组等封包级违规 |
| -32601 | Method not found（方法不存在） | method 不在方法表内 |
| -32602 | Invalid params（参数无效） | 参数缺失、类型错误、静态域非法（如 sampleRate=0、channels≠2）、closeSession 引用未知会话 id |
| -32000 | Backend failure（后端失败） | 需要后端参与的操作失败：设备未找到、格式协商失败、能力上限、流错误、会话 id 空间耗尽 |
| -32001 | Invalid state（状态不允许） | 方法与当前引擎状态不匹配——相位不符或前置状态缺失（非 idle 调 configure、未 configure 即 start、图未配置采样率即 openSession） |

保留规则：-32768..-32000 为 JSON-RPC 2.0 保留段；本协议自定义占用 -32000/-32001 两码；
新增自定义码须取更小的负值（如 -32002 起）并登记进本表，属向后兼容变更。

## 七、事件通知语义

| method | params 形态 | 发出时机 |
|---|---|---|
| `event.phase` | {"from":str,"to":str} | 相位每发生一次真实跃迁发一条；from/to ∈ 四态枚举且组合必须是 §4.2 跃迁表中存在的边 |
| `event.xrun` | {"dir":"in"\|"out","count":u64,"totalIn":u64,"totalOut":u64} | 输入侧过载（dir="in"，含推流入环丢块）或输出侧欠载（dir="out"）发生后上报 |

```json
{"jsonrpc":"2.0","method":"event.phase","params":{"from":"starting","to":"running"}}
```

```json
{"jsonrpc":"2.0","method":"event.xrun","params":{"dir":"out","count":1,"totalIn":0,"totalOut":42}}
```

补充规则：

1. count 为本次通知覆盖的**增量**；totalIn/totalOut 为进程启动以来累计值，全局单调不减；
2. 服务可将极短时间窗内的多次 xrun 合并为一条通知（count 为窗口内次数）以避免洪泛；
   合并窗口是实现细节，建议 ≤ 100 ms，不进入兼容契约；合并与否不改变累计值语义；
3. totalIn/totalOut 分别与 getState.stats.xrunsIn/xrunsOut 同源同值——任意时刻查询 getState
   所得计数 ≥ 最后一条对应通知中的累计值；
4. 通知只经控制面连接推送；控制面断线期间的 xrun 不补发（重连后靠 getState 对账）；
5. 同一连接上通知与响应严格按发生序交错发送。

## 八、引擎子链及其与全链的对应关系

Phase 3 批次一起，管线内实际运行的链为**六模块引擎子链**，顺序固定：

```text
交错 f32 进（L,R,L,R,…） → [拆包 planar] → midSide → biquad → compressor
  → reverb-simple → bass-enhancer → limiter → [打包交错] → 交错 f32 出
```

与 TS 支线全链（文档口径 22 级，`src/engine/HyperSoundEngine.ts` 的 buildStages 固定数组）
的对应关系：六者选取的是全链中 M/S → Pre-EQ → Compressor → Reverb → BassEnhancer → Limiter 的
**相对出现顺序**（全链第 3、4、6、13、14、21 级），其余各级在本阶段不参与管线（未移植，而非激活旁路）：

| 子链级 | 对应模块规格 | 全链位置 | 全链实现形态 |
|---|---|---|---|
| midSide | [`specs/dsp/mid-side.md`](../dsp/mid-side.md) | 第 3 级 M/S | MidSide 本体（width + voiceBalance） |
| biquad | [`specs/dsp/biquad.md`](../dsp/biquad.md) | 第 4 级 pre-eq | EqChain（biquad 级联）——以单节 biquad 代表该级 |
| compressor | [`specs/dsp/compressor.md`](../dsp/compressor.md) | 第 6 级 compressor | Compressor 本体（sidechain 派生见其 §4.5） |
| reverb-simple | [`specs/dsp/reverb-simple.md`](../dsp/reverb-simple.md) | 第 13 级 reverb | 三路路由（卷积/算法/off），子链取其中**算法路** ReverbSimple |
| bass-enhancer | [`specs/dsp/bass-enhancer.md`](../dsp/bass-enhancer.md) | 第 14 级 bass | BassEnhancer 本体（含 lowBoostDb） |
| limiter | [`specs/dsp/limiter.md`](../dsp/limiter.md) | 第 21 级（末级）limiter | Limiter 本体 |

约束：

1. 试点级的行为以冻结向量 + 各模块规格为准（双绿门禁），控制面协议不重复定义 DSP 行为；
2. 数据布局契约（`hse-wasapi/src/lib.rs`）：进程内统一交错立体声 f32；planar 转换只发生在
   DSP 线程边界——交错进、planar 过链、交错出；
3. Phase 3 起新模块按规格逐级插入链中，插入动作只改变服务进程内部拓扑；
   本契约的方法表与状态机不受影响（setParams 可识别键随模块落地同步扩展，属向后兼容变更）。

## 九、并发约束

1. **单客户端假设**：本阶段唯一受支持的形态是至多一个控制面客户端。服务端不为第二个连接
   提供隔离或仲裁：多个连接的请求进入同一串行控制线程按到达序处理，跨连接的可见性与竞争
   行为**不构成兼容契约的一部分**。多客户端支持留待后续阶段以向后兼容方式收敛；
2. **心跳暂缓**：应用层心跳/ping 方法本契约不定义；RFC 6455 传输层 Ping/Pong 可按标准处理，
   但不承载任何业务语义，也不得作为存活判定的契约依据；
3. **串行化保证**：同一连接内请求按发送序处理、响应按序返回、通知按发生序插入该顺序流；
   控制面线程因此允许自由分配、加锁、执行系统调用（实时纪律只约束音频回调路径，
   见 `hse-core/src/lib.rs` 头注释全库铁律）；
4. 控制面慢消费（客户端长时间不读 socket）不得阻塞音频线程；服务端可对积压连接执行传输层
   断开，这属于自保行为而非错误。

## 十、版本演进规则

1. **兼容契约地位**：本协议是三层兼容契约的第三层；破坏本契约 = MAJOR（docs/VERSIONING.md 分级）；
2. **向后兼容变更（MINOR 及以下）**：
   - 新增方法（如 Phase 3 将新增 openSession/closeSession，见 push-stream.md）；
   - 既有方法的 params/result 新增**可选**键；setParams 新增可识别键或子键；
   - 新增事件 method；warnings 新增文案模式；错误码表新增条目；
   - 新增状态机边须保持既有边的触发条件与副作用不变；
3. **破坏性变更（MAJOR）**：删除或重命名方法/键；改变既有键的类型、必填性或语义；
   改变错误码含义；改变既有状态机边的触发条件或响应-事件次序；
4. **协议自述版本**：未来引入握手类方法时可携带数字型 schemaVersion 字段做协商；
   标识符（方法名、键名、事件名、参数名）一律不带版本字样，需要消歧时用 hse- 前缀
   （specs/README.md §七、AGENTS.md 命名铁律）；
5. 每次对本文件的实质修订按 docs/VERSIONING.md 记录进仓库根 CHANGELOG.md；
6. 客户端兼容义务：对未知 method 返回的 -32601、未知键产生的 warnings、未知事件通知
   必须容忍并忽略，禁止硬失败。

## 十一、关联文件

- 总纲契约：[`specs/README.md`](../README.md)
- 兄弟文档（同端口复用与推流入口设计）：[`push-stream.md`](push-stream.md)
- 术语基线：仓库根 `CONTEXT.md`；决策记录：`docs/adr/0001`、`docs/adr/0002`、`docs/adr/0003`
- 试点模块规格：[biquad](../dsp/biquad.md) ｜ [reverb-simple](../dsp/reverb-simple.md) ｜ [limiter](../dsp/limiter.md)
- 实现落点：`HyperSoundEngineRust/crates/hse-service`（bin）、`HyperSoundEngineRust/crates/hse-wasapi`（后端）
