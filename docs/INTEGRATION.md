# HyperSoundEngine 接入指南

本文是其他项目和自动化编码代理接入 HyperSoundEngine 的首要入口。先按下表选择路径，再执行对应章节。不要把不同路径的 API 混用。

协议与类型的事实标准：

- TypeScript 公共 API：[`API.md`](API.md) 与构建产物 `dist/*.d.ts`
- Rust 服务控制面：[`../specs/service/control-plane.md`](../specs/service/control-plane.md)
- Rust 服务 PCM 推流：[`../specs/service/push-stream.md`](../specs/service/push-stream.md)
- 空间薄 C ABI：[`SPATIAL_ABI.md`](SPATIAL_ABI.md)，只覆盖 renderer，不是完整 HSE

## 1. 先选择接入路径

| 需求 | 应使用 | 不应使用 |
|---|---|---|
| Node、Electron 主进程或脚本离线处理 PCM | TypeScript core：`hypersoundengine` | AudioWorklet、`hse-service` |
| Web Audio 实时处理 | `hypersoundengine/browser` 的 `HyperSoundEngineHost` | 手工向 worklet 发送参数消息 |
| 浏览器内运行 Rust DSP | Host 的 `engineBackend: 'wasm'` | 空间 C ABI |
| Windows 上由独立原生进程处理并输出到声卡 | Rust `hse-service` | 尚未实现的 `hse-napi` |
| C/C++ 只调用双耳空间 renderer | 空间 C ABI | 把该 ABI 当作完整 1–22 级引擎 |
| Rust workspace 内部开发 | 直接依赖 `hse-core` / `hrtf-core` | 假定 crates.io 已发布 |

关键边界：

1. TS core 与 Rust 实现没有源码依赖，通过 `specs/` 共享行为契约。
2. `hse-service` 接收控制消息和立体声 PCM，处理结果输出到配置的 WASAPI 渲染设备，**不会通过 WebSocket 回传处理后 PCM**。
3. 服务的 `setParams` 使用服务 wire 参数模型，不接受 TS `HyperSoundEngineParams` 的原始完整对象。字段表以 control-plane §5.6 为准。
4. 浏览器 Host 的 2/6/8 表示输入声道数；所有当前产品路径最终输出双声道。
5. `hse-napi` 尚未实现。Node/Electron 若要使用 Rust 完整引擎，应启动 `hse-service`，或在浏览器渲染进程使用 wasm Host。

## 2. 从仓库构建

```bash
git clone https://github.com/IceFireIcer/HyperSoundEngine.git
cd HyperSoundEngine
npm install
npm run build

cd HyperSoundEngineRust
cargo build --workspace --release --locked
```

仓库内 TS 示例从 `dist/` 导入，因此先运行 `npm run build`。Rust crates 当前按 workspace 使用，不应假定 npm、crates.io 或系统 PATH 已存在已发布版本。

## 3. TypeScript core：离线或自有音频回调

### 3.1 最小立体声处理

```ts
import {
  createDefaultParams,
  createEngine,
  type AudioEngine,
} from 'hypersoundengine'

const sampleRate = 48_000
const maxBlockSize = 512
const engine: AudioEngine = createEngine(sampleRate, 2)

engine.prepare(maxBlockSize)
const params = createDefaultParams(sampleRate)
params.eq.enabled = true
params.eq.simpleBands = [2, 0, 0, 0, 1]
params.limiter.enabled = true
params.limiter.thresholdDb = -1
engine.setParams(params)

function processRealtimeBlock(
  inputL: Float32Array,
  inputR: Float32Array,
  outputL: Float32Array,
  outputR: Float32Array,
) {
  if (
    inputL.length !== inputR.length ||
    outputL.length < inputL.length ||
    outputR.length < inputL.length ||
    inputL.length > maxBlockSize
  ) {
    throw new RangeError('invalid audio block')
  }
  engine.process([inputL, inputR], [outputL, outputR])
}

// 实时宿主在回调外创建并复用 input/output 数组和通道缓冲。
```

生命周期固定为：

```text
createEngine -> prepare -> setParams -> process/processMulti repeatedly -> reset when needed
```

规则：

- `setParams` 是完整快照替换，不是 patch。始终从 `createDefaultParams(sampleRate)` 派生新快照。
- 实时回调前必须调用 `prepare(maxBlockSize)`，之后不得传入更大的块。
- 输入输出是非交错 planar `Float32Array[]`，调用方负责预分配并保证输出容量足够。输出过短不会得到可靠错误。
- `process` 取输入数组中最短通道长度作为本次有效帧数，只处理前两个输入通道。
- 参数解析、对象构造、WAV 编解码和 UI 更新不得放入实时回调。
- `getLatencySamples()` 返回当前 DSP 延迟，宿主需要时自行做延迟补偿。
- `reset()` 清状态但保留当前参数；core 没有 `dispose()`。

仓库可运行示例：

```bash
npm run build
node examples/node-offline.mjs
```

### 3.2 3–8 路输入转双耳

core 的可选 `processMulti` 接受 3–8 路非交错输入并固定输出 L/R：

```ts
const engine = createEngine(48_000, 6)
engine.prepare(128)
const params = createDefaultParams(48_000)
params.spatial.mode = 'instant'
params.spatial.instant.multichannelAuto = true
engine.setParams(params)

const inputs = Array.from({ length: 6 }, () => new Float32Array(128))
const outputs = [new Float32Array(128), new Float32Array(128)]
if (!engine.processMulti) throw new Error('engine does not support processMulti')
engine.processMulti(inputs, outputs)
```

声道顺序为 `L, R, C, LFE, SL, SR, BL, BR`。LFE 不进入 HRTF；`spatial.mode='off'` 时仅使用 ch0/ch1。`processMulti` 的输入数必须等于创建引擎时的 `channelCount`。

## 4. 浏览器 Host：TypeScript AudioWorklet

Host 管理 `masterGain -> HSE -> analyser` 接线。调用方必须提供已存在的 `AudioContext`、上游 `masterGain` 和下游 `analyser`。

```ts
import { createDefaultParams } from 'hypersoundengine'
import { createHyperSoundEngineHost } from 'hypersoundengine/browser'

const params = createDefaultParams(audioContext.sampleRate)
const host = createHyperSoundEngineHost({
  mode: 'auto',
  engineBackend: 'ts',
  workletUrl: '/assets/worklet-bundle.js',
  inputChannelCount: 2,
})

await host.attach({ audioContext, masterGain, analyser }, params)

const nextParams = structuredClone(params)
nextParams.eq.simpleBands[0] = 3
await host.setParams(nextParams)

// 页面或音频图销毁时
host.dispose()
```

构建和部署 TS worklet：

```bash
npm run build
# 部署 dist/worklet-bundle.js，并让 workletUrl 指向其实际 URL
```

Host 规则：

- `mode: 'auto'`：worklet 失败时回退 ScriptProcessor；`mode: 'worklet'`：失败即 reject。
- `inputChannelCount` 只允许 `2 | 6 | 8`，输出固定 2 路。
- `attach()` 与 `setParams()` 都返回 Promise，必须 `await` 或显式处理 reject。
- worklet 参数更新会预建新节点、等待 `ready`，以零增益接入并预滚一个 128-frame render quantum，再按音频时间交叉淡变；不要向 worklet 私自发送 `params` 消息。
- 一个 Host 切换到另一套 AudioContext/节点前，先调用 `dispose()`；`dispose()` 会恢复 `masterGain -> analyser` 直连。
- `getLastStats()` / `getLastAnalysis()` 是最近一次 TS worklet 消息快照，不应作为 wasm backend 的统计接口。

## 5. 浏览器 Host：Rust/WASM 完整引擎

### 5.1 构建 wasm 与正式 worklet

```bash
rustup target add wasm32-unknown-unknown
cargo install --locked wasm-bindgen-cli --version 0.2.127
cd HyperSoundEngineRust
cargo build -p hse-wasm --target wasm32-unknown-unknown --release --locked
wasm-bindgen target/wasm32-unknown-unknown/release/hse_wasm.wasm \
  --target web \
  --out-dir ../build/wasm-engine-pkg
cd ..
npm run build:core
npm run build:wasm-worklet -- --pkg ./build/wasm-engine-pkg
```

部署：

- `dist/wasm-worklet-bundle.js`
- `build/wasm-engine-pkg/hse_wasm_bg.wasm`
- 可选的合法 SOFA 文件

### 5.2 接入代码

```ts
import { createDefaultParams } from 'hypersoundengine'
import { createHyperSoundEngineHost } from 'hypersoundengine/browser'

const params = createDefaultParams(audioContext.sampleRate)
params.spatial.mode = 'instant'

const host = createHyperSoundEngineHost({
  mode: 'worklet',
  engineBackend: 'wasm',
  wasmWorkletUrl: '/assets/wasm-worklet-bundle.js',
  wasmUrl: '/assets/hse_wasm_bg.wasm',
  hrtfUrl: '/assets/listener.sofa',
  inputChannelCount: 2,
  wasmRequestTimeoutMs: 5_000,
  workletCrossfadeMs: 20,
})

await host.attach({ audioContext, masterGain, analyser }, params)
await host.setParams(nextCompleteParamsSnapshot)
```

HRTF 可用二选一入口：

- `hrtfUrl: string`：Host fetch SOFA bytes。
- `hrtf: ArrayBuffer | HrtfGridShape`：直接传 SOFA bytes 或预解析网格。

`HrtfGridShape` 是结构约定，不是当前包入口导出的命名类型：

```ts
type HrtfGridShape = {
  sampleRate: number
  azimuths: number[]
  elevations: number[]
  hrirLength: number
  left: Float32Array
  right: Float32Array
}
```

`left/right.length` 必须等于 `azimuths.length * elevations.length * hrirLength`。`hrtfUrl` 与 `hrtf` 互斥。`spatial.mode='off'` 不需要 HRTF；其他模式没有 HRTF 时节点构造失败。wasm Host 当前只支持 2 路输入。

`mode:'auto'` + wasm 的回退顺序是 wasm worklet、TS worklet、ScriptProcessor。若产品要求必须使用 Rust，选择 `mode:'worklet'` 并处理 `attach()` reject，避免静默回退。

## 6. Rust `hse-service`：其他项目的推荐原生接入

这是非浏览器、跨语言项目接入 Rust 完整 HSE 的稳定路径。外部程序无需链接 Rust crate，只需实现 WebSocket 文本 JSON-RPC 和二进制 PCM 帧。

### 6.1 服务启动

在仓库的 `HyperSoundEngineRust/` 中：

```powershell
cargo run -p hse-service
cargo run -p hse-service -- --port 5000
```

默认地址：`ws://127.0.0.1:4780/`。服务只绑定本机 IPv4 loopback，不提供鉴权或远程网络暴露。生产程序应负责启动、监督和停止该子进程，并从 stdout/退出码判断启动失败。

当前实现对非法 `--port` 会回退环境变量或默认端口；接入方应只传 `1..65535` 的已校验端口。

### 6.2 两种音频入口

| 入口 | 数据来源 | 接入方工作 |
|---|---|---|
| loopback/capture | 服务从 WASAPI 捕获系统输出或捕获端点 | 只控制配置和参数 |
| push-stream | 接入方解码后发送立体声 f32 PCM | 实现会话、PCM 封包和实时节流 |

两种入口会同时存在并先求和，再共同经过一次 1–22 级 DSP。当前服务没有“纯推流且完全不开捕获设备”的配置：`start` 总会打开一个 capture/loopback 源和一个 render 输出。

### 6.3 推荐生命周期

```text
连接一个 WebSocket
-> listDevices / getState
-> configure
-> 可选 loadHrtf（只允许 idle）
-> setParams（完整 wire 快照）
-> 可选 openSession
-> start，并按 id 等待响应，同时处理 event.phase
-> 按实时速率发送 PCM，监控 event.xrun 与 getState
-> 停止生产，等待 queuedFrames=0 且 consumedFrames 达到已发送帧数（仅证明混合前级已消费）
-> 按宿主的设备缓冲/尾音策略等待必要余量
-> closeSession
-> stop
-> 关闭 WebSocket
```

强制规则：

- 一个集成实例只使用一个控制连接。多连接共享全局引擎状态，会话 ID 不是授权边界。
- 响应和事件共用文本流，必须按 `id` 关联响应；没有 `id` 的 `event.phase` / `event.xrun` 单独处理。
- 改采样率前先关闭全部会话，再 `stop -> configure -> openSession -> start`。旧会话不会自动重新协商。
- `stop` 会停止并 join 数据面线程，但当前不承诺排空尚未渲染的尾块。`queuedFrames === 0` 且 `consumedFrames` 达到发送量只证明 PCM 已离开会话队列并进入混合前级，不能证明已穿过双环、WASAPI 缓冲或物理播放完成。需要无截断播放的宿主必须在此前提后按设备缓冲和效果尾音策略留出余量；当前协议没有“已物理播放”ACK。
- 客户端只能依赖 JSON-RPC `error.code`，不能依赖错误消息文本。

### 6.4 配置和启动

先枚举设备：

```json
{"jsonrpc":"2.0","id":1,"method":"listDevices","params":{}}
```

捕获默认输入并输出到默认渲染设备：

```json
{"jsonrpc":"2.0","id":2,"method":"configure","params":{"mode":"capture","captureDeviceId":null,"outputDeviceId":null,"shareMode":"shared","sampleRate":48000,"blockSizeFrames":256}}
```

捕获指定渲染端点的 loopback：

```json
{"jsonrpc":"2.0","id":2,"method":"configure","params":{"mode":"loopback","renderDeviceId":null,"outputDeviceId":null,"shareMode":"shared","sampleRate":48000,"blockSizeFrames":256}}
```

限制：

- `sampleRate`: 整数 `8000..384000`
- `blockSizeFrames`: 整数 `16..8192`
- `shareMode`: `shared | exclusive`
- loopback 只支持 shared；exclusive 仅支持普通 capture + render，失败不会回退 shared
- `null` 表示对应类别的系统默认设备

空间模式需要在 `configure` 后、`start` 前加载本机绝对 SOFA 路径：

```json
{"jsonrpc":"2.0","id":3,"method":"loadHrtf","params":{"path":"C:\\hrtf\\subject.sofa"}}
```

然后下发完整服务 wire 参数快照：

```json
{"jsonrpc":"2.0","id":4,"method":"setParams","params":{"params":{"reverbRoute":"off","spatial":{"mode":"off"},"limiter":{"enabled":true,"thresholdDb":-1,"lookaheadMs":5,"attackMs":1,"releaseMs":60,"truePeak":true}}}}
```

省略的 wire 顶层键回到服务默认值，不会保留上次值。完整字段和类型见 control-plane §5.6。

### 6.5 推流会话和 PCM 帧

打开会话：

```json
{"jsonrpc":"2.0","id":5,"method":"openSession","params":{"sampleRate":48000,"channels":2,"format":"f32le"}}
```

返回：

```json
{"jsonrpc":"2.0","id":5,"result":{"sessionId":7,"granted":{"sampleRate":48000,"channels":2,"format":"f32le"}}}
```

每条 WebSocket binary message 恰好包含一块：

```text
offset 0   sessionId  u32 little-endian
offset 4   seq        u64 little-endian
offset 12  PCM        L0,R0,L1,R1,... f32 little-endian
总长度      12 + frameCount * 2 * 4 bytes
```

JavaScript 封包：

```js
function packPcmFrame(sessionId, sequence, interleavedStereo) {
  if (interleavedStereo.length === 0 || interleavedStereo.length % 2 !== 0) {
    throw new RangeError('PCM must contain interleaved stereo frames')
  }
  const buffer = new ArrayBuffer(12 + interleavedStereo.length * 4)
  const view = new DataView(buffer)
  view.setUint32(0, sessionId, true)
  view.setBigUint64(4, BigInt(sequence), true)
  for (let i = 0; i < interleavedStereo.length; i++) {
    view.setFloat32(12 + i * 4, interleavedStereo[i], true)
  }
  return buffer
}
```

约束：

- 当前仅支持双声道、交错、`f32le`，采样率必须等于 `configure`。
- payload 为 8 的倍数，至少 8 字节，最多 1 MiB。
- `seq` 应从 0 严格递增；当前服务只解析、不拒绝跳号。
- 非法帧和未知 sessionId 会静默丢弃，没有 ACK。
- 服务按完整块 drop-oldest 背压并增加 `xrunsIn`。客户端必须按实时速率发送，并监控 `WebSocket.bufferedAmount`。
- WebSocket 断开会自动关闭该连接创建的会话。

仓库参考实现：[`../examples/hse-service-client.mjs`](../examples/hse-service-client.mjs)。它只导出客户端和封包函数，不自动连接或打开音频设备。

```js
import { HseServiceClient } from './examples/hse-service-client.mjs'

const client = await HseServiceClient.connect('ws://127.0.0.1:4780/')
try {
  await client.rpc('configure', {
    mode: 'capture',
    captureDeviceId: null,
    outputDeviceId: null,
    shareMode: 'shared',
    sampleRate: 48_000,
    blockSizeFrames: 256,
  })
  await client.rpc('setParams', {
    params: { reverbRoute: 'off', spatial: { mode: 'off' } },
  })
  const { sessionId } = await client.rpc('openSession', {
    sampleRate: 48_000,
    channels: 2,
    format: 'f32le',
  })
  await client.rpc('start')

  const interleavedStereoBlock = new Float32Array(256 * 2)
  client.sendPcm(sessionId, 0n, interleavedStereoBlock)
  await client.waitForSessionConsumed(sessionId, interleavedStereoBlock.length / 2)
  // consumed 只表示已进入混合前级；生产宿主还应按设备缓冲/效果尾音等待。

  await client.rpc('closeSession', { sessionId })
  await client.rpc('stop')
} finally {
  client.close()
}
```

### 6.6 状态、事件和错误

必须监控：

- `getState.phase`
- `getState.stats.xrunsIn/xrunsOut`
- `getState.stats.framesProcessed`
- `getState.stats.inputRingDepthFrames/outputRingDepthFrames`
- `getState.stats.latencyFrames`：仅为服务排队帧估算，不是声卡端到端延迟
- `getState.sessions[].queuedFrames/ingestedFrames/consumedFrames`
- `event.phase`
- `event.xrun`

错误码：

| code | 含义 | 客户端处理 |
|---|---|---|
| `-32700` | 文本不是合法 JSON | 修正序列化 |
| `-32600` | JSON-RPC 封包或 id 非法 | 修正请求结构 |
| `-32601` | 方法不存在 | 检查服务版本与拼写 |
| `-32602` | 参数、会话或运行态构链非法 | 修正参数；不要盲目重试 |
| `-32000` | 设备、格式、命令环或后端失败 | 查询状态、重新枚举设备后重试 |
| `-32001` | phase 或前置状态不允许 | 按生命周期恢复到正确 phase |

## 7. Rust 进程内接入边界

- `hse-core` 和 `hrtf-core` 可供本 workspace 内 Rust 代码直接依赖，但当前没有承诺稳定的 crates.io SDK 包装层。
- `hse-napi` 仍是占位，Node/Electron 不应导入它。
- 空间 C ABI 只提供 HRTF renderer 的 8 个主函数及辅助生命周期/错误函数，不包括 EQ、压缩、混响主链、WASAPI 或服务控制面。
- 需要跨语言调用完整 Rust HSE 时，使用 `hse-service`。

## 8. 集成验收清单

### TS core

- 使用真实采样率创建引擎。
- 实时路径调用 `prepare(maxBlockSize)`。
- 每次 `setParams` 发送完整快照。
- 复用实时缓冲；测试短尾块和 `reset()`。
- 使用 `getLatencySamples()` 处理宿主延迟补偿。

### 浏览器 Host

- worklet 和 wasm URL 可由页面同源访问。
- `attach()` / `setParams()` 被 await 并处理失败。
- 只在一个 handle 上 attach；切换前 dispose。
- wasm 非 off 空间模式提供 HRTF。
- 验证实际 `getMode()` 与 `getEngineBackend()`，特别是 `auto` 回退场景。

### Rust service

- 使用单控制连接并按 id 分派响应。
- 先 configure，再 loadHrtf/setParams/openSession/start。
- PCM 严格为匹配采样率的交错立体声 f32le。
- 发送按实时速率节流，并监控客户端 `bufferedAmount`。
- 验收期间要求 `xrunsIn === 0 && xrunsOut === 0`。
- 停止生产后可等待会话 `queuedFrames === 0` 且 `consumedFrames` 达到发送量，以证明混合前级已消费；这不是渲染或物理播放完成 ACK。
- 按设备缓冲和效果尾音策略留出停止余量；不把 `stop` 当成 drain。
- 不把服务排队帧统计当作物理设备端到端延迟。

## 9. 验证命令

```bash
npm run typecheck
npm test
npm run build

cd HyperSoundEngineRust
cargo test --workspace --locked
cargo run -q -p hse-parity
```

浏览器 Rust/WASM 的完整构建和 Chromium E2E 命令见 [`API.md`](API.md#6-audioworklet-打包hypersoundengineworklet)。真实设备验收必须遵循 [`audit/phase4-real-audio-acceptance.md`](audit/phase4-real-audio-acceptance.md)，同时显式设置 `HSE_ALLOW_REAL_AUDIO=1` 并传 `--run`；默认命令不得打开或播放真实音频。
