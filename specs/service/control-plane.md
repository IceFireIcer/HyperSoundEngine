# 规格：control-plane —— 引擎服务进程控制面（WebSocket JSON-RPC）

> **规格属性**：本文件是双支线共享规格，也是**引擎服务进程控制协议的唯一契约源**；
> 属兼容契约三层中的第三层（仓库根 `AGENTS.md`／[ADR-0003](../../docs/adr/0003-dual-track-native-rewrite.md)：
> 引擎服务进程控制协议），单方面破坏 = MAJOR。行为事实标准 = `HyperSoundEngineRust/crates/hse-service`（随本阶段落地）。
> 术语基线见仓库根 [`CONTEXT.md`](../../CONTEXT.md)；书写规范见 [`specs/README.md`](../README.md)。
> 关联决策：[ADR-0001 独立进程形态](../../docs/adr/0001-engine-as-independent-process.md)、
> [ADR-0002 双音频入口](../../docs/adr/0002-dual-audio-ingress.md)。
>
> **1.5.1 实现范围**：控制面已加性支持 `shareMode:"shared"|"exclusive"` 与双环排队帧统计；
> `hse-real-audio-check` 提供显式门控的真机验收入口。自动门禁不等于真实设备验收，shared/exclusive
> 端到端延迟、xrun 与整进程 CPU 仍须用户分别实测；固定时长测试已删除且不得恢复。

---

## 一、范围与定位

- **控制面（Control Plane）**（CONTEXT.md 术语）：接入方向引擎服务进程下发参数、查询状态、管理会话的通道。
  本契约覆盖其全部对外可观测行为；音频数据面（回环捕获、DSP、渲染）不在本文件范围内，
  仅以状态机相位与统计计数器的形式在结果中显影。
- **控制面与数据面分离**（规划书 §2.2）：控制面连接可以随时断开重连，不影响已建立的音频流；
  数据面异常通过事件通知与统计计数器上报，不要求控制面在线。
- **当前实现范围**：服务数据面运行 `hse-core::EngineChainStage` 1–22 级完整链；第 22 级四模式通过 idle-only `loadHrtf` 从本地 SOFA 预载 grid 后可启用，无 grid 时仅允许 off。`setParams` 保留兼容 wire 键并投影为完整 canonical 参数，world 相邻快照确定速度在控制线程推导。音频入口为回环拦截、捕获端点直捕与推流。
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
- `id` 为整数或字符串（本协议不接受 `null`、布尔、对象或数组作为 id）；同一连接内允许乱序 id；
- 显式携带非法类型的 `id` 时，整条消息按 Invalid Request（-32600，响应 id 为 null）拒绝，且不得分派方法或产生任何业务副作用；
- `params` 必须为对象（按名传参）；省略时视为空对象 `{}`。

### 形态 B：响应——成功

```json
{"jsonrpc":"2.0","id":1,"result":{"render":[],"capture":[]}}
```

### 形态 C：响应——失败

```json
{"jsonrpc":"2.0","id":7,"error":{"code":-32001,"message":"state does not allow configure"}}
```

- `error.code` 取值见 §六错误码表；`message` 为面向诊断的短句，语言和措辞不进入兼容契约，
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
    │  数据面停止＋设备释放完成    ┌──────────┐                   │
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
| stopping | 数据面线程停止且设备释放完成 | idle | 发 `event.phase {from:"stopping",to:"idle"}`；随后发 stop 成功响应 |

约束：

1. `starting` 与 `stopping` 是**瞬态**：只由服务内部推进，客户端无法用任何方法令服务停留在其中；
2. 事件先于对应的 RPC 响应发出；start 成功响应发出的时刻 phase 已是 `running`，
   stop 成功响应发出的时刻 phase 已是 `idle`；
3. 相位只在上述边上变化；不存在跨态跃迁（如 running 直达 idle）。

## 五、方法规格

> 基础方法六个、HRTF 加载方法一个、推流方法两个。规划书草案阶段的独立 getStats 方法已并入
> `getState.stats` 字段，以本契约为准。

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
| `id` | string | Windows IMMDevice ID；render/capture 分类独立，按类别用于 `renderDeviceId`、`captureDeviceId` 或 `outputDeviceId` |
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
{"jsonrpc":"2.0","id":2,"result":{"phase":"idle","config":null,"stats":{"xrunsIn":0,"xrunsOut":0,"framesProcessed":0,"uptimeMs":15230,"inputRingDepthFrames":0,"inputRingHighWaterFrames":0,"outputRingDepthFrames":0,"outputRingHighWaterFrames":0,"blockSequence":0,"latencyFrames":{"current":0,"p50":0,"p95":0,"max":0,"samples":0}},"sessions":[],"lastParams":null,"hrtf":{"loaded":false}}}
```

| 字段 | 类型 | 说明 |
|---|---|---|
| `phase` | string | 四态之一 |
| `config` | object\|null | 最近一次**成功** configure 的 applied 快照原样回显；从未成功配置过则为 null |
| `lastParams` | object\|null | 最近一次成功 setParams 的**规范化 wire 快照**；未知键已移除，已出现模块的缺省子键可能补齐；从未设置过则为 null |
| `hrtf` | object | HRTF grid 加载状态；未加载为 `{loaded:false}`，已加载时加性包含规范绝对 `path`、`sampleRate`、`azimuthCount`、`elevationCount`、`hrirLength` |
| `sessions` | array | 活跃推流会话诊断，按 sessionId 升序；每项含 u32 `sessionId` 与 u64 `queuedFrames`/`ingestedFrames`/`consumedFrames`，仅用于消费验收与诊断 |
| `stats.xrunsIn` | u64 | 输入侧异常累计（捕获过载、推流入环丢块），与 `event.xrun.totalIn` 同源同值 |
| `stats.xrunsOut` | u64 | 输出侧欠载累计，与 `event.xrun.totalOut` 同源同值 |
| `stats.framesProcessed` | u64 | DSP 线程累计处理帧数（立体声帧，每声道计一帧），跨 start/stop 周期累计不清零 |
| `stats.uptimeMs` | u64 | 当前运行周期自成功 start 至今的毫秒数；非 running 时为 0（时钟读取仅发生在控制面线程，不违反核心确定性铁律） |
| `stats.inputRingDepthFrames` | u64 | 最近观测到的 capture→DSP 输入环当前占用帧数；仅计环内帧，不含 DSP 已取出的当前块 |
| `stats.inputRingHighWaterFrames` | u64 | 当前成功 start 周期内输入环占用高水位（帧） |
| `stats.outputRingDepthFrames` | u64 | 最近观测到的 DSP→render 输出环当前占用帧数 |
| `stats.outputRingHighWaterFrames` | u64 | 当前成功 start 周期内输出环占用高水位（帧） |
| `stats.blockSequence` | u64 | 当前成功 start 周期内已完成并写入输出环的 DSP 块序号；每块恰加 1 |
| `stats.latencyFrames.current` | u64 | 最近完成块的确定性服务排队帧估算：该块进入处理时输入环待处理帧 + 当前块帧 + 该块写入后输出环待渲染帧 |
| `stats.latencyFrames.p50` / `p95` | u64 | 当前周期全部已完成块的帧延迟分布估算；使用预分配的 2 的幂直方图桶，返回命中桶下界，满足 p50 ≤ p95 ≤ max |
| `stats.latencyFrames.max` | u64 | 当前周期块延迟估算最大值，单调不减 |
| `stats.latencyFrames.samples` | u64 | 纳入分布的块数，恒等于 `blockSequence` |

全部状态驻内存：进程重启即回到 config=null、lastParams=null、计数器归零，无持久化。
上述 `latencyFrames` 是不读取数据面时钟的**服务排队帧估算**，用于静音/fake 自动门禁和运行期拥塞诊断；它不包含设备驱动、安全缓冲、DAC/ADC 或声学传播，不能替代真实 WASAPI 端到端延迟测量。数据面更新环深度、高水位、块序号和直方图时只允许原子操作与启动期预分配存储，DSP 稳态路径不得加锁、分配或读取时钟。

周期语义：每次 `start` 仅在后端握手、格式校验与初始链装配全部成功后清零环深度/高水位、`blockSequence` 与 `latencyFrames`；启动失败保留上一成功周期摘要。`stop` 完成后两项 current depth 归零，刚结束周期的高水位、块序号和延迟分布保留到下次成功 start。`xrunsIn`、`xrunsOut` 与 `framesProcessed` 继续跨 start/stop 周期累计。

#### GWT-CP-03：状态查询字段完备且旧字段兼容
- **给定**：服务处于任意相位
- **当**：发送 `getState`
- **则**：结果含 `phase`/`config`/`stats`/`lastParams` 四个既有顶层键与加性 `hrtf`/`sessions` 键；`stats` 保留 `xrunsIn`/`xrunsOut`/`framesProcessed`/`uptimeMs` 四个既有 u64 键，并加性包含四个环深度/高水位键、`blockSequence` 与 `latencyFrames`；`phase` 值属于四态枚举；`config` 与 `lastParams` 要么为 null 要么为对象；`hrtf.loaded` 为 bool；`sessions` 为按 sessionId 升序的数组

#### GWT-CP-04：计数与周期高水位单调不减
- **给定**：服务正在同一个成功 start 周期内处理音频
- **当**：以 `blockSequence` 已推进为屏障先后两次调用 `getState`
- **则**：第二次的 `xrunsIn`/`xrunsOut`/`framesProcessed`、两项环高水位、`blockSequence`、`latencyFrames.max` 与 `latencyFrames.samples` 均 ≥ 第一次对应值；current depth 与 current latency 可随生产/消费变化

#### GWT-CP-04A：帧延迟统计有序且无时钟依赖
- **给定**：当前周期至少完成一个 DSP 块
- **当**：发送 `getState`
- **则**：`latencyFrames.samples == blockSequence`，且 `p50 <= p95 <= max`、`current <= max`；所有值以帧表示，并只由块大小和双环占用确定，不读取数据面墙钟

#### GWT-CP-04B：成功重启复位周期统计
- **给定**：服务已完成一个含统计样本的 start/stop 周期
- **当**：stop 完成后查询，再成功 start 并在数据面放行前查询
- **则**：stop 后 input/output current depth 均为 0，而高水位、块序号和延迟分布仍保留；下一次成功 start 将这些周期字段全部重置为 0；进程级 xrun 与 `framesProcessed` 累计不清零；若 start 失败则旧周期摘要不变

### 5.3 configure —— 设置入口配置（仅 idle）

请求：

```json
{"jsonrpc":"2.0","id":3,"method":"configure","params":{"mode":"loopback","renderDeviceId":null,"outputDeviceId":null,"sampleRate":48000,"blockSizeFrames":256}}
```

成功结果（applied 为生效配置的原样回显）：

```json
{"jsonrpc":"2.0","id":3,"result":{"applied":{"mode":"loopback","renderDeviceId":null,"outputDeviceId":null,"sampleRate":48000,"blockSizeFrames":256}}}
```

capture 直捕请求示例：

```json
{"jsonrpc":"2.0","id":3,"method":"configure","params":{"mode":"capture","captureDeviceId":"cable-output-id","outputDeviceId":"speakers-id","sampleRate":48000,"blockSizeFrames":256}}
```

| 参数 | 类型 | 校验层级 | 说明 |
|---|---|---|---|
| `mode` | string | 非 `"loopback"` / `"capture"` → -32602 | 捕获形态：loopback 捕获渲染端点；capture 直接打开捕获端点（如 CABLE Output） |
| `renderDeviceId` | string\|null | loopback 必填；非法引用 → -32000 | loopback 模式的被捕获渲染端点；null 表示默认渲染端点。旧四字段请求保持此语义 |
| `captureDeviceId` | string\|null | capture 必填；非法引用 → -32000 | capture 模式的直捕端点；null 表示默认捕获端点 |
| `outputDeviceId` | string\|null | 可选；非法引用 → -32000 | 最终渲染端点；省略或 null 表示默认渲染端点。省略时旧请求的 applied/config 保持四字段形态 |
| `shareMode` | string | 可选；非 `"shared"` / `"exclusive"` → -32602 | WASAPI 访问模式，省略时为 shared；只有显式携带时才进入 applied/config 回显 |
| `sampleRate` | u32 | 非 8000..384000 整数 → -32602 | 期望采样率；捕获与渲染协商结果都必须等于该值，否则 start 报 -32000 |
| `blockSizeFrames` | u32 | 非 16..8192 整数 → -32602 | 期望每块帧数（事件驱动轮询周期）；后端能力上限校验失败在 start 时报 -32000 |

- **仅 phase=idle 可调用**，否则报 -32001 且状态（含既有 config）不变；
- 字段组合：loopback 必须携带 `renderDeviceId` 且不得携带 `captureDeviceId`；capture 必须携带 `captureDeviceId` 且不得携带 `renderDeviceId`；违反组合约束报 -32602；
- `shareMode` 缺省为 `"shared"` 并保持既有 WASAPI shared 行为。`"exclusive"` 只允许普通 capture 与 render；loopback+exclusive 在 configure 阶段固定报 -32602，config 不变，绝不静默回退 shared；
- 校验分两级：结构与静态域检查在 configure 内完成；所有显式设备 id 在提交前按类别校验，未知或类别错误报 -32000；格式协商与捕获/渲染采样率一致性在 start 检查。exclusive 必须使用设备原生支持的目标采样率立体声 f32 格式并禁用自动转换，任一端不支持目标格式、独占被占用或周期/缓冲初始化失败均使 start 报 -32000 并回滚 idle；`blockSizeFrames` 换算为 exclusive 目标 period，最终按 wasapi 0.24 的设备最小周期与对齐 API 处理；
- 兼容性：既有 `{mode:"loopback",renderDeviceId,sampleRate,blockSizeFrames}` 请求继续有效，且未显式携带 `outputDeviceId` 或 `shareMode` 时 applied/getState.config 不新增对应键。

#### GWT-CP-05：idle 合法配置生效
- **给定**：phase=idle，参数通过结构校验，显式设备 id 均存在于对应类别
- **当**：发送合法 loopback 或 capture 配置
- **则**：响应 result.applied 与请求中的生效字段逐字段相等；此后 getState.config 等于该 applied 对象；旧四字段 loopback 请求保持四字段回显

#### GWT-CP-05A：捕获源与渲染出口独立选路
- **给定**：枚举中存在非默认渲染端点 R、捕获端点 C 与输出端点 O
- **当**：分别配置 `{mode:"loopback",renderDeviceId:R,outputDeviceId:O,...}` 与 `{mode:"capture",captureDeviceId:C,outputDeviceId:O,...}` 并启动
- **则**：前者从 R 建立 loopback 捕获、后者从 C 建立直捕；两者都向 O 建立渲染流，源 id 不得传给渲染 opener

#### GWT-CP-05B：模式字段组合严格且设备类别正确
- **给定**：phase=idle
- **当**：loopback 携带 captureDeviceId、capture 携带 renderDeviceId、缺少模式所需源键，或显式 id 不属于要求的设备类别
- **则**：字段组合错误报 -32602；未知或类别错误报 -32000；getState.config 保持原值

#### GWT-CP-05C：协商采样率偏离配置时启动回滚
- **给定**：合法配置的捕获或渲染端最终协商采样率不等于配置的 sampleRate，包括两端共同协商到同一替代值
- **当**：发送 start
- **则**：返回 -32000，phase 回到 idle，config 保留，所有已启动线程被停止并回收

#### GWT-CP-05D：shared 缺省兼容与 exclusive 严格打开
- **给定**：phase=idle；分别准备省略 shareMode、显式 shared、普通 capture+exclusive 与 loopback+exclusive 配置
- **当**：依次发送 configure；对成功的普通 capture+exclusive 再发送 start
- **则**：省略 shareMode 时行为为 shared 且 applied/config 不新增该键；显式 shared/exclusive 原样回显；loopback+exclusive 在 configure 报 -32602 且 config 不变；普通 capture+exclusive 的捕获与渲染均以 EventsExclusive 打开，目标立体声 f32 格式必须由设备原生支持、禁止 autoconvert，period 以 blockSizeFrames 为目标按后端 API 对齐；任一端不支持或打开失败时 start 报 -32000 并回滚 idle，绝不回退 shared

#### GWT-CP-06：非 idle 一律拒绝
- **给定**：phase ∈ {starting, running, stopping}
- **当**：发送任意 `configure`（无论内容是否合法）
- **则**：响应 error code=-32001；getState.config 保持原值不变

#### GWT-CP-07：结构非法拒绝且不留痕
- **给定**：phase=idle；分别取 mode="other"、模式与源设备键组合冲突、sampleRate<8000 或 >384000、非整数 sampleRate、blockSizeFrames<16 或 >8192、缺失任一必填键
- **当**：逐一发送 `configure`
- **则**：每次响应 error code=-32602；getState.config 不变（此前配置过的仍保持）

#### GWT-CP-08：未知设备引用报后端失败
- **给定**：phase=idle，任一显式源设备或输出设备 id 设为枚举结果中不存在或类别不匹配的字符串
- **当**：发送 `configure`
- **则**：响应 error code=-32000；getState.config 不变

### 5.3A loadHrtf —— 从本地 SOFA 预载 HRTF grid（仅 idle）

请求与成功结果：

```json
{"jsonrpc":"2.0","id":30,"method":"loadHrtf","params":{"path":"C:\\hrtf\\subject.sofa"}}
```

```json
{"jsonrpc":"2.0","id":30,"result":{"loaded":true,"path":"C:\\hrtf\\subject.sofa","sampleRate":48000,"azimuthCount":72,"elevationCount":37,"hrirLength":256}}
```

- 前置条件：phase=idle 且已有成功 `configure`；否则 -32001；
- `path` 是唯一参数，必须为非空本机绝对路径、扩展名大小写不敏感等于 `.sofa`，且指向可访问普通文件；结构或路径校验失败报 -32602；
- 服务在控制线程按当前 `config.sampleRate` 调用 `hrtf-core::load_sofa_file`，SOFA 约定、内容、采样率转换或 grid 构建失败报 -32602；DSP 线程不得读取、打开或解析文件；
- 成功后原子替换 `EngineHandle` 中的 grid 与规范绝对路径；失败时保留此前 grid。再次成功 configure 且采样率变化时清除旧 grid，采样率不变时保留；
- 已加载 grid 只为后续 `start` 或运行态 `setParams` 的控制线程构链提供输入。`loadHrtf` 本身禁止在 running 调用，因此运行期不会直接切换 grid；
- 该方法是加性协议扩展，既有客户端无需调用，旧请求/响应语义不变。

#### GWT-CP-08A：路径与解析失败不留痕
- **给定**：phase=idle 且已 configure，并已记录调用前 `getState.hrtf`
- **当**：分别以相对路径、非 `.sofa` 路径、目录、不存在文件或内容非法的临时 `.sofa` 调用 `loadHrtf`
- **则**：每次响应 -32602，调用前 HRTF 状态和 grid 保持不变，数据面线程未执行任何文件 I/O

#### GWT-CP-08B：真实 SOFA 在控制路径加载
- **给定**：`HSE_TEST_SOFA` 指向本机真实 SimpleFreeFieldHRIR 文件，phase=idle 且 config.sampleRate 为支持目标采样率
- **当**：以该绝对路径调用 `loadHrtf`
- **则**：返回 loaded:true 及有限的 grid 元数据，`getState.hrtf` 与结果一致；该资产依赖测试默认 ignored，不下载或捆绑真实 SOFA

#### GWT-CP-08C：相位与采样率绑定
- **给定**：分别处于未 configure、running，以及已加载 grid 后重新 configure 为不同采样率三种状态
- **当**：前两者调用 `loadHrtf`，后者调用 `getState`
- **则**：前两者均报 -32001；重新配置不同采样率后 `getState.hrtf == {loaded:false}`，不得把旧采样率 grid 交给新链

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

- 前置条件：phase=running。停止次序：置停机旗 → 等待数据面线程退出 → 释放设备 → 回 idle；
  当前协议不承诺排空尚未渲染的尾块；`sessions[].queuedFrames == 0` 与 `consumedFrames` 只证明会话数据已进入混合前级，不是 WASAPI 或物理播放完成 ACK。需要无截断播放的客户端必须在会话消费后按设备缓冲与效果尾音策略留出余量；
  停止期间不再产生新的 xrun 上报；释放阶段的后端异常被吞掉并强制完成回 idle（停止路径不允许卡死）。

#### GWT-CP-13：running 正常停止
- **给定**：phase=running
- **当**：发送 `stop`
- **则**：先后收到 event.phase {from:"running",to:"stopping"}、event.phase {from:"stopping",to:"idle"}，然后收到 stopped:true；此后 getState().phase == "idle"，config 与 lastParams 均保留

#### GWT-CP-14：非 running 拒绝停止
- **给定**：phase ∈ {idle, starting, stopping}
- **当**：发送 `stop`
- **则**：响应 error code=-32001；phase 不变

### 5.6 setParams —— 参数快照下发

请求（兼容 wire 参数示例）：

```json
{"jsonrpc":"2.0","id":6,"method":"setParams","params":{"params":{"biquad":{"type":"peaking","f0":120,"q":0.8,"gainDb":3.5},"reverbSimple":{"roomSize":0.5,"damping":0.3,"wet":0.25,"dry":0.75,"preDelayMs":20,"width":1,"type":"hall"},"limiter":{"enabled":true,"thresholdDb":-1,"lookaheadMs":5,"attackMs":1,"releaseMs":60,"truePeak":true}}}}
```

成功结果（warnings 语义见下文）：

```json
{"jsonrpc":"2.0","id":6,"result":{"accepted":true,"warnings":["myPluginKey"]}}
```

- **快照语义**：params.params 是**全量快照**，整体替换上一次快照（对齐两支线既有的
  "setParams 整体替换"约定）；省略的顶层键视为回落内置缺省，**不是增量合并**；
  - `biquad` 与 `eqChain` 两键的「缺省」形态为**级不装配**（逐位直通）——与 TS
    `createDefaultParams().eq` 的 enabled:true+10×0dB 形态的差异见 §八注 4；
- params.params 缺失或不是对象 → -32602；
- 可识别顶层键内部的子键域与 clamp 行为以对应模块规格为准
  （[`biquad`](../dsp/biquad.md) §三、[`reverb-simple`](../dsp/reverb-simple.md)、[`limiter`](../dsp/limiter.md)），
  协议层只做键存在性与 JSON 类型匹配的结构检查；数值越界由模块自身 clamp，不产生 warnings、不算错误；
- **热应用**：phase=running 时，控制面先解析完整候选快照、使用当前预载 HRTF grid 构建新链并完成 `prepare(maxBlockSize)`，再经无锁命令通道送 DSP 线程；在**下一块边界**整快照生效；
  正在处理的块不受影响；控制面线程不得持任何 DSP 内部锁、不得在音频回调路径分配（架构铁律）；
- **事务提交**：running 时，候选快照只有在解析、构链、prepare 和命令投递全部成功后，才同时替换 `lastParams` 与后续 start 使用的参数；任一步失败均返回错误，`lastParams`、后续启动参数和运行链保持调用前旧值；命令环满按 -32000 拒绝，不以 accepted:true 或 warning 表示丢弃；
- 非 running 时保持既有延迟校验语义：通过结构解析后立即存储规范化快照，不提前构链；下次 start 再按实际协商格式构链，届时构建失败按启动失败回滚；

#### 可识别 wire 键表（投影到 1–22 级完整 EngineChainStage）

| 顶层键 | 子键 | 类型 | 说明 |
|---|---|---|---|
| `midSide` | `width` | number | M/S 宽度（对齐全链 stereoWidth，1=恒等） |
| | `voiceBalance` | number | 人声比例（pitch 语义位；引擎子链恒等传递于 wb=0） |
| `biquad` | `type` | string | 八种枚举之一：peaking / lowshelf / highshelf / lowpass / highpass / bandpass / notch / allpass |
| | `f0` | number（Hz） | 中心/转折频率 |
| | `q` | number | Q 值 |
| | `gainDb` | number（dB） | 仅 peaking/shelf 类生效 |
| `eqChain` | `bands` | array | 元素 `{frequency:number(Hz), gain:number(dB), q:number}`；越界钳制按 [eq-chain](../dsp/eq-chain.md) 规格；短于 bandCount 尾部填充、长于截断 |
| | `bandCount` | number | 级联段数（生效值 = max(1, floor)） |
| | `qCompensation` | bool | 级联 Q 补偿开关 |
| `deesser` | `enabled` | bool | 旁路开关（false=恒等） |
| | `centerHz` / `q` | number | 侧链带通形状 |
| | `thresholdDb` / `ratio` | number | 齿音压缩曲线 |
| | `attackMs` / `releaseMs` | number（ms） | 包络时间常数 |
| | `splitBand` | bool | 分带（LR-4 交叉）处理开关 |
| | `mix` | number | 干湿混合 |
| | `sidechainEnabled` | bool | 语义位（本链无外部 sidechain 源，不改变 DSP 状态） |
| `compressor` | `enabled` | bool | 旁路开关（false=恒等） |
| | `thresholdDb` / `ratio` / `kneeDb` | number | 压缩曲线 |
| | `attackMs` / `releaseMs` | number（ms） | 包络时间常数 |
| | `makeupDb` | number（dB） | 补偿增益 |
| | `outputGain` | number | 输出线性增益 |
| | `sidechainEnabled` | bool | 按规格 §4.5 从输入派生单声道和 sidechain |
| `modEffects` | `delay` | object | `{enabled:bool, delayMs:number, feedback:number, mix:number}`（五效果按引擎接线顺序级联） |
| | `chorus` | object | `{enabled:bool, rateHz:number, depthMs:number, mix:number}` |
| | `flanger` | object | `{enabled:bool, rateHz:number, depthMs:number, feedback:number, mix:number}` |
| | `phaser` | object | `{enabled:bool, rateHz:number, depth:number, feedback:number, mix:number, stages:number}` |
| | `tremolo` | object | `{enabled:bool, rateHz:number, depth:number, mix:number}` |
| `reverbSimple` | `roomSize` | number | 房间尺寸 |
| | `damping` | number | 高频阻尼 |
| | `wet` / `dry` | number | 湿/干信号比例 |
| | `preDelayMs` | number（ms） | 预延迟 |
| | `width` | number | 立体声宽度 |
| | `type` | string | 混响算法变体（枚举见 reverb-simple 规格） |
| `reverbRoute` | （键值本身） | string | reverb 级三路路由：`"simple"`（缺省，算法混响）/ `"fdn"` / `"convolver"` / `"off"`（整级直通）；枚举外值回退 simple（无 warnings）；类型不符 → -32602 |
| `fdnReverb` | `roomSize` / `damping` | number | FDN 反馈/阻尼（以 type 表为基准 ±0.25 微调） |
| | `wet` / `dry` / `preDelayMs` / `width` | number | 混合与预延迟 |
| | `type` | string | hall / room / plate / spring / stage（枚举外回退 hall） |
| | `lines` | number | 延迟线数，仅 2/4/8/16 合法（缺省 8；其余值 → -32602） |
| `convolver` | `irRecipe` | object | 确定性 IR 配方（specs/dsp/convolver.md §4.2）：`{kind:"delta", delay:number}` 或 `{kind:"expNoise", length:number, seed:u32, decay:number, amp:number}`；kind 枚举外 → -32602（无模块内回退形态）；`reverbRoute:"convolver"` 时必填，缺失 → -32602 |
| | `mix` | number | 干湿混合（0=纯干逐位直通，1=纯湿） |
| | `preDelayMs` | number（ms） | 湿路预延迟（clamp [0,1000]） |
| `bassEnhancer` | `enabled` | bool | 旁路开关 |
| | `cutoffHz` / `q` | number | 低通提取 |
| | `harmonicType` | string | odd / even / atan / soft |
| | `harmonicGain` / `mix` / `levelDb` | number | 谐波路径 |
| | `lowBoostDb` | number（dB） | 低音下潜（-6..12，缺省 0=关闭） |
| `loudnessComp` | `mode` | string | auto / preset / custom（枚举外回退 auto） |
| | `preset` | string | preset 模式预设 id：flat / bass / vocal / warm / bright / night（未知 id 回退 flat 曲线） |
| | `bands` | array | custom 模式目标曲线控制点 `{frequency:number(Hz), gain:number(dB)}` |
| | `volumePercent` / `maxBoostDb` / `smoothingSeconds` | number | auto 曲线音量、增益上界、逐块平滑 |
| `dynamicEq` | `enabled` | bool | 旁路开关（false=硬直通） |
| | `strength` / `thresholdDb` / `ratio` | number | 动态压缩量 |
| | `attackMs` / `releaseMs` | number（ms） | 增益平滑时间常数 |
| | `bands` | array | 固定 5 带元素 `{enabled:bool, targetGainDb?:number(dB)}`；**crossover 频率不暴露协议键**，由服务侧按引擎常量 [200,800,2500,8000] 固定注入（第 5 带无下交叉）；短于 5 项缺项带保持默认 |
| `modMatrix` | `routes` | array | 元素 `{source:string("lfo"\|"envelope"), target:string("masterGain"\|"stereoWidth"), amount:number, offset?:number}`；source/target 枚举外按 TS 求值语义回退（envelope / stereoWidth）；offset 缺省 0 |
| | `lfo` | object | `{shape:string(sine\|triangle\|square\|saw), rateHz:number, depth:number}`（shape 枚举外按 sine） |
| | `envelope` | object | `{attackMs:number, releaseMs:number, amount:number}` |
| `spatial` | `mode` | string | `off`（缺省）/ `instant` / `headLocked` / `world` / `stage`；非 off 要求已通过 `loadHrtf` 预载与图采样率一致的 grid，否则 start 报 -32000、running setParams 报 -32602 |
| | 其余字段 | object/scalar | 按完整 TS `SpatialSettings` 深合并：world 消费 listener position/yaw/pitch/roll、sources/trajectories/playhead/occlusion；stage 消费 preset/seat/roomSize/reverbAmount/customSources；共享 masterGain/instant/ambience/convolution/hrtfInterp/distanceModel/refDistance/maxDistance 同步生效。服务仍固定立体声输入与双耳输出，不增加物理 multichannel 输出 |
| `limiter` | `enabled` | bool | 限幅级旁路开关 |
| | `thresholdDb` | number（dB） | 门限 |
| | `lookaheadMs` | number（ms） | 前瞻 |
| | `attackMs` / `releaseMs` | number（ms） | 包络时间常数 |
| | `truePeak` | bool | 真峰超采样检测开关 |

注：
1. 各模块的数值钳制与枚举回退以对应模块规格为准，协议层只做键存在性与 JSON 类型匹配；
2. `biquad` 与 `eqChain` 缺省形态为**级不装配**（逐位直通），见 §八注 4；
3. `loudnessComp` 无 `enabled` 键——核心模块无旁路门控（TS 引擎由 loudnessCompensation.enabled
   在引擎层门控）；本链以参数形态表达等价「关闭」：缺省 `mode:"custom"` + 空 `bands` =
   目标曲线全 0 → 平滑增益恒 0 → 构造期恒等系数不参与重算 → 逐位直通；
4. `modMatrix` 的 stereoWidth 调制产物在本链不回灌 midSide（引擎把 modStereoWidth 回读进
   mid-side 级 width 快照；本链 midSide.width 只来自 `midSide` 键，保持既有键语义不变）；
   modMatrix 缺省（routes 空）masterGain 基线 1 → 逐位恒等；
5. HseStretch（变速/变调）不入链——引擎语义为 `getStretch()` 外置调用，无 setParams 键。

#### warnings 语义

1. warnings 恒为数组（可为空），元素为字符串；
2. **不可识别的顶层键**：整体忽略并记入 warnings，元素为该键名原文（如 "myPluginKey"）；
   这是有意的向前兼容机制——客户端可安全携带为后续版本准备的扩展键而不破坏互操作；
3. 可识别顶层键内**不可识别的子键**：忽略并记入 warnings，元素形如 "<顶层键>.<子键>"（如 "biquad.order"）；
   嵌套子对象（`modEffects.*`、`convolver.irRecipe`、`eqChain.bands`、`loudnessComp.bands`、
   `dynamicEq.bands`、`modMatrix.*`）内同理，元素形如 "<顶层键>.<子对象>.<子键>"（如 "modEffects.delay.foo"）；
4. warnings 元素按字典序升序排列（确定性输出，便于机械断言）；
5. 只要结构校验通过，warnings 不影响 accepted:true 与快照存储。

#### GWT-CP-15：合法快照接收并存储
- **给定**：phase 任意，params.params 为含三个可识别键的对象
- **当**：发送 `setParams`
- **则**：响应 {"accepted":true,"warnings":[]}；getState.lastParams 是请求快照的规范化 wire 形态，保留已识别语义并可补齐已出现模块的缺省子键

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

#### GWT-CP-19：params 结构或应用失败时事务回滚
- **给定**：已有成功快照 A；候选快照 B 分别在结构解析、构链/prepare 或运行态命令投递阶段失败
- **当**：发送 B
- **则**：响应对应错误码（结构为 -32602；running 时构链失败为 -32602、内部命令投递失败为 -32000）；`lastParams`、后续 start 使用的参数与当前运行链均保持 A，不得留下 B 的部分状态；非 running 候选只做结构解析并存储，DSP 构建错误延迟到下一次 start 报告

#### GWT-CP-20：显式非法 id 无副作用
- **给定**：请求显式携带 `id:null`、布尔、对象或数组，且 method/params 本身可产生业务副作用
- **当**：发送该请求
- **则**：响应 error code=-32600 且响应 id=null；方法不被分派，phase/config/lastParams/会话表与计数器均不变；省略 id 的合法通知仍照常执行且不回包

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

## 八、服务完整链及 wire 参数适配

服务数据面实际构造并运行 `hse-core::EngineChainStage` 1–22 级；前 21 级的级序、旁路、sidechain 与分析语义以 [`engine/chain.md`](../engine/chain.md) 为准。`PilotParams` 只是控制协议兼容层：将本节可识别 wire 键投影到完整 canonical `EngineChainParams`，未暴露级使用核心默认值。第 22 级支持 `off` / `instant` / `headLocked` / `world` / `stage`，非 off 必须消费控制面预载的 HRTF grid；world 运行态热更新以相邻已提交快照的 listener position/playhead 推导确定速度，首次与非递增 playhead 固定为零。`HseStretch` 保持链外能力。

约束：

1. 各级行为以冻结向量 + 各模块规格为准，控制面协议不重复定义 DSP 行为；
2. 数据布局契约（`hse-wasapi/src/lib.rs`）：进程内统一交错立体声 f32；planar 转换只发生在 DSP 线程边界；
3. wire 键扩展只改变兼容适配层，不改变 `EngineChainStage` 的完整级序；
4. 缺省直通回归锚仍为：全键缺省 + reverbSimple 全干 + limiter 禁用 + spatial off，整链逐位直通；
5. SOFA 读取、解析、重采样、grid 与 renderer/链构建全部发生在控制线程；DSP 线程只接收已 prepare 的整链，在块边界交换，不得解析路径或文件。

## 九、并发约束

1. **单客户端假设**：本阶段唯一受支持的形态是至多一个控制面客户端。服务端不为第二个连接
   提供隔离或仲裁：实现为每连接一个处理线程，多个连接共享同一引擎状态，跨连接的请求全序、
   会话授权与竞争行为**不构成兼容契约的一部分**。多客户端支持留待后续阶段以向后兼容方式收敛；
2. **心跳暂缓**：应用层心跳/ping 方法本契约不定义；RFC 6455 传输层 Ping/Pong 可按标准处理，
   但不承载任何业务语义，也不得作为存活判定的契约依据；
3. **串行化保证**：同一连接内请求按发送序处理、响应按序返回、通知按发生序插入该顺序流；
   由 start/stop 请求同步产生的全部 `event.phase` 与对应响应通过同一连接写路径按序发送，不经可与响应竞速的异步广播队列，因此最后一条 phase 通知的 WebSocket 文本帧必须先于响应帧；
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
   必须容忍并忽略，禁止硬失败；
7. **1.3.0 加性演进（历史兼容基线）**：`configure` 新增 capture 模式、`captureDeviceId` 与可选 `outputDeviceId`；既有 loopback 四字段请求及其 applied/config 形态保持不变。
8. **WASAPI 访问模式加性演进**：`configure` 新增可选 `shareMode:"shared"|"exclusive"`；省略时保持 shared 行为及旧回显形态。exclusive 只支持普通 capture/render，loopback+exclusive 固定在 configure 报 -32602。
9. **延迟可观测性加性演进**：`getState.stats` 新增双环 current/high-water 帧数、当前周期 `blockSequence` 与 `latencyFrames` 确定性帧延迟分布；旧四统计键名称、类型与累计语义保持不变。
10. **HRTF 服务路径加性演进**：新增 `loadHrtf{path}` 与 `getState.hrtf`，既有方法和既有结果字段保持不变；文件仅在 idle 控制路径解析，非 off spatial 通过预建整链在块边界生效。
11. **推流消费诊断加性演进**：`getState.sessions` 按 sessionId 升序暴露每条活跃会话的排队、累计接收与累计消费帧数；旧顶层键与 `stats` 语义保持不变。

## 十一、关联文件

- 总纲契约：[`specs/README.md`](../README.md)
- 兄弟文档（同端口复用与推流入口设计）：[`push-stream.md`](push-stream.md)
- 术语基线：仓库根 `CONTEXT.md`；决策记录：`docs/adr/0001`、`docs/adr/0002`、`docs/adr/0003`
- 试点模块规格：[biquad](../dsp/biquad.md) ｜ [reverb-simple](../dsp/reverb-simple.md) ｜ [limiter](../dsp/limiter.md)
- 实现落点：`HyperSoundEngineRust/crates/hse-service`（bin）、`HyperSoundEngineRust/crates/hse-wasapi`（后端）
