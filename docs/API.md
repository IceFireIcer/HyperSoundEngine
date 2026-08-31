# HyperSoundEngine —— 对外接口文档（API）

> 适用版本：1.5.1（独立引擎包）。核心 API 自 0.2.0 稳定；当前包含 WAV legacy/standard 双模式与 world-listener 几何公共函数。
> 分享串为 v2 紧凑格式（HSE2，兼容导入 v1 旧串，见 `src/engine/ShareCodec.ts`）。
> 核心原则：**小接口、深实现**。大多数接入方只需要 `createEngine` + `HyperSoundEngineParams`。

---

## 1. 安装与导入

```bash
npm install hypersoundengine
```

### 核心入口（纯 DSP，任何 JS 运行时）

```ts
import {
  createEngine,
  createDefaultParams,
  computeRelativeDirection,
  wrapAzimuthDeg,
  type HyperSoundEngineParams,
  type AudioEngine,
  type EngineStats,
  type EngineAnalysis,
  type Vec3,
  type WorldListenerPose,
} from 'hypersoundengine'
```

### 浏览器宿主入口

```ts
import { createHyperSoundEngineHost, HyperSoundEngineHost } from 'hypersoundengine/browser'
```

### AudioWorklet 打包入口

```ts
// 仅供 esbuild/vite 打包，不可在 Node 主线程直接 import
import { HseAudioEffectsProcessor, WORKLET_PROCESSOR_NAME } from 'hypersoundengine/worklet'
```

---

## 2. 核心引擎接口

### `createEngine(sampleRate, channelCount?)`

创建独立引擎实例。

```ts
const engine = createEngine(48000, 2) // 返回 AudioEngine
```

### `interface ProcessingStage`

高级扩展点：`HyperSoundEngine` 内部用 `ProcessingStage[]` 描述处理链。需要自定义处理链时可参考该接口：

```ts
interface ProcessingStage {
  id: string
  active(): boolean
  run(left: Float32Array, right: Float32Array, frameCount: number): void
}
```

### `interface AudioEngine`

所有宿主应当只依赖这个接口。

```ts
interface AudioEngine {
  setParams(params: HyperSoundEngineParams): void
  getParams(): HyperSoundEngineParams
  prepare(maxBlockSize: number): void
  process(inputs: Float32Array[], outputs: Float32Array[], sidechain?: Float32Array[]): void
  processMulti?(inputs: Float32Array[], outputs: Float32Array[], sidechain?: Float32Array[]): void
  getStats(): EngineStats
  getAnalysis(): EngineAnalysis
  getLatencySamples(): number
  reset(): void
}
```

#### `setParams(params)`

- 每次接收**完整参数快照**，引擎内部深拷贝；
- 调用方可安全复用/修改传入对象；
- 参数变更即时生效，块间切换，无爆音设计。

#### `getParams()`

- 返回当前参数快照的**深拷贝**；
- 外部修改返回值不会影响引擎内部状态。

#### `prepare(maxBlockSize)`

- 实时处理前调用一次，预分配内部工作缓冲；
- 之后 `process` 在不超过 `maxBlockSize` 的块上保持零分配。

#### `process(inputs, outputs, sidechain?)`

- 就地处理：`outputs[i]` 会被覆盖写入；
- `outputs[i].length` 应 >= `inputs[i].length`；
- `process` 支持单声道（`channelCount=1`）与立体声（`channelCount=2`）；
- `processMulti` 是可选扩展；Host 检测到旧 `AudioEngine` 未实现它时，仅将 `ch0/ch1` 交给既有双声道 `process`，因此无需 MAJOR 升级；
- `processMulti` 是 3–8 路非交错输入到双声道输出的实时安全入口：调用方必须预分配并复用通道缓冲及 `inputs`/`outputs` 数组，并先调用 `prepare(maxBlockSize)`；
- 两个入口的拓扑有意不同：旧 `process` 固定执行第 1–21 级后再执行 stage 22 spatial，保持既有冻结语义；`processMulti` 在 spatial 开启时先调用 `SpatialBackend.processMulti` 双耳化全部输入，再让所得 L/R 执行完整第 1–21 级，因此 EQ、动态器、分析与最终 Limiter 作用于所有输入声道的空间求和结果，且不会递归再次空间化；
- 多声道顺序采用 Web Audio `discrete` 约定：`L, R, C, LFE, SL, SR, BL, BR`。`spatial.mode !== 'off'` 且 `instant.multichannelAuto=true` 时，各输入声道由 `SpatialBackend.processMulti` 渲染为双耳，LFE 槽位不做 HRTF；`spatial.mode='off'` 时兼容下混为 `ch0→L`、`ch1→R`，额外声道忽略后直接执行第 1–21 级；
- 两个实时入口都只产出物理双声道，不提供多声道物理输出；
- `sidechain`：可选外部侧链输入（与 inputs 同长度的 Float32Array[]）。只有开启 `sidechainEnabled` 的效果器（Compressor / Deesser）会使用它驱动包络/检测；
- 多通道便捷入口 `processBus(input: HseAudioBus, output: HseAudioBus, sidechain?: HseAudioBus, options?)`：
  - 默认（`mode: 'downmix'`）：>2 声道下混为立体声处理，输出不足 2 声道写第一声道、超过 2 声道复制到其余声道；
  - `mode: 'perChannelPair'`：按立体声对 (0,1)、(2,3)… 逐对独立处理（每对独立子引擎，参数与主引擎同步），支持 5.1/7.1 各通道独立 DSP；奇数剩余通道复制成立体声处理取 L 写回；sidechain 按对切片；
  - 非实时路径，会分配临时缓冲；
- **实时安全**：稳态处理内零分配，可在 AudioWorklet / 音频回调中调用。

#### `getStats()`

```ts
interface EngineStats {
  lufsIntegrated: number   // 整合响度 LUFS，未测到为 NaN
  lufsMomentary: number    // 瞬时响度 LUFS
  lra: number              // 响度范围 LU
  peakDb: number           // 样本峰值 dBFS
  truePeakDb: number       // 真峰值 dBFS
  limiterReductionDb: number // 限幅器当前衰减 dB（<=0）
  engineLatencySamples: number // 引擎当前延迟（样本数）
}
```

#### `getAnalysis()`

```ts
interface EngineAnalysis {
  spectrum: Float32Array | null // 2048 点 FFT 幅度谱（N/2+1 bins）
  features: SpectralFeatures | null
}
```

#### `getLatencySamples()`

返回当前处理链引入的延迟（限幅器 lookahead + 混响延迟等）。

#### `reset()`

复位所有滤波器、包络、响度计、分析缓冲与内部状态；同时调用自定义 `ProcessingStage` 的可选 `reset()`。

### `HseAudioBus` 多通道缓冲（`dsp/HseAudioBus.ts`）

非交错 N 通道缓冲抽象，通道级工具均为确定性、纯函数（`HseAudioBus` 自身不持有状态）：

```ts
const bus = HseAudioBus.create(6, 1024)            // 5.1：6 通道 × 1024 帧（零填充）
const bus2 = HseAudioBus.fromInterleaved(inter, 6) // 交错 → 非交错（拷贝）
const inter = bus.toInterleaved()               // 非交错 → 交错（新分配）
bus.copyTo(target)                              // 拷贝到目标 bus
bus.fill(0); bus.applyGain(0.5)                 // 填充 / 线性增益
bus.mixFrom(other, 0.3)                         // 混入 other×gain（就地累加）
const sub = bus.extract([0, 1])                 // 提取通道子集（引用原通道）
const mono = bus.downmixToMono()                // 全通道平均下混
```

### `HyperSoundEngine` 扩展方法（非 `AudioEngine` 通用接口）

```ts
engine.registerStage(stage: ProcessingStage, index?: number): void
engine.unregisterStage(id: string): boolean
engine.getStages(): ProcessingStage[]
```

- `registerStage`：插入自定义处理阶段；`index` 缺省时插到 `limiter` 之前；同 id 会原位替换。
- `unregisterStage`：按 id 移除自定义阶段。
- `getStages`：返回当前处理链副本。

```ts
const gainStage: ProcessingStage = {
  id: 'my-gain',
  active: () => true,
  run: (l, r) => { /* 就地处理 */ },
  reset: () => { /* 可选 */ },
}
engine.registerStage(gainStage)
```

### WAV 文件 I/O（`io/wav.ts`）

```ts
encodeWav(
  channels: Float32Array[],
  sampleRate: number,
  opts?: { bitDepth?: 16 | 32; format?: 'legacy' | 'standard' },
): ArrayBuffer
decodeWav(buffer: ArrayBuffer | Uint8Array): { sampleRate: number; channels: Float32Array[]; bitDepth: 16 | 32 }
```

- 16-bit PCM（formatTag=1）/ 32-bit Float（formatTag=3），支持多通道。
- 编码默认 `format: 'legacy'`，保留 1.0.0 及更早版本的大端数值头字节契约；对外交换文件应显式使用 `format: 'standard'`，生成标准小端 RIFF/WAVE。
- 解码自动识别 legacy 与 standard；standard 路径严格校验 RIFF/data 长度、采样率、byteRate 与 blockAlign。
- 多通道直接对应 `HseAudioBus` 非交错布局，解码结果可零拷贝进入 `processBus`。

### World listener 几何

```ts
computeRelativeDirection(listener: WorldListenerPose, source: Vec3): RelativeDirection
wrapAzimuthDeg(angle: number): number
```

- 右手坐标：`+X` 右、`+Y` 上、`+Z` 前；位置单位米，角度单位度。
- `yaw=0` 朝 `+Z`，正 yaw 向右；输出方位固定在 `[-180, 180)`。
- 该 API 仅处理 position/yaw 几何，不包含 pitch/roll、多普勒、HRIR 或卷积。

---

## 3. 参数模型

### `createDefaultParams(sampleRate): HyperSoundEngineParams`

生成全量默认参数快照，推荐作为任何参数修改的起点。

```ts
const params = createDefaultParams(48000)
params.eq.enabled = true
params.eq.simpleBands = [0, 0, 0, 0, 0]
params.limiter.thresholdDb = -1
engine.setParams(params)
```

### `interface HyperSoundEngineParams`

完整字段（节选）：

| 字段 | 说明 |
|---|---|
| `sampleRate` | 采样率 |
| `eq` | EQ：simple 5 段 / pro 10-20 段 + Q 补偿 |
| `deesser` | 齿音抑制 |
| `compressor` | 动态压缩 |
| `nightMode` | 夜间模式 |
| `bassEnhancer` | 虚拟低频增强 |
| `reverb` | 混响：卷积 / 算法 Freeverb / FDN 网络 / off |
| `surround3d` | 3D 环绕 |
| `loudnessCompensation` | 等响度补偿 |
| `loudnessNormalization` | 响度归一化 |
| `limiter` | 前瞻限幅器 |
| `ieq` | 智能均衡 |
| `dynamicEq` | 自适应动态均衡（频谱包络自动混音，5 带全通交叉） |
| `pitch` | 变速/变调（离线 HseStretch 参数） |
| `modulation` | 参数调制矩阵（LFO / Envelope Follower → masterGain / stereoWidth 路由） |
| `modEffects` | 调制类效果：delay / chorus / flanger / phaser / tremolo |
| `hearing` | 听力分析 |
| `stereoWidth` | M/S 立体声宽度 |
| `sceneId` / `customized` | 场景状态 |

完整类型见 `src/types.ts` 或构建产物的 `dist/index.d.ts`。

---

## 4. 场景与分享串

```ts
import {
  SCENE_PRESETS,
  getSceneById,
  encodeShareCode,
  decodeShareCode,
} from 'hypersoundengine'

// 场景
const scene = getSceneById('pop')
if (scene) engine.setParams(scene.params)

// 分享串
const code = encodeShareCode(params)
const restored = decodeShareCode(code) // 非法输入抛 Error
```

---

## 5. 浏览器宿主（`hypersoundengine/browser`）

### `createHyperSoundEngineHost(options?): HyperSoundEngineHost`

```ts
interface HyperSoundEngineHostOptions {
  mode?: 'worklet' | 'script' | 'auto' // 承载模式，默认 auto
  engineBackend?: 'ts' | 'wasm'        // worklet 内核，默认 ts
  workletBackend?: 'ts' | 'wasm'       // engineBackend 的等价别名；冲突时抛错
  workletUrl?: string                  // TS worklet 产物 URL
  wasmWorkletUrl?: string              // wasm 专用 worklet 产物 URL
  wasmUrl?: string                     // hse_wasm_bg.wasm URL
  hrtfUrl?: string                     // SOFA URL；与 hrtf 互斥
  hrtf?: ArrayBuffer | {              // SOFA bytes 或预解析规则网格
    sampleRate: number
    azimuths: number[]
    elevations: number[]
    hrirLength: number
    left: Float32Array
    right: Float32Array
  }
  wasmRequestTimeoutMs?: number        // ready 超时，默认 2000ms
  workletCrossfadeMs?: number          // TS/wasm 参数替换淡变窗口，默认 20ms
  wasmCrossfadeMs?: number             // workletCrossfadeMs 的兼容别名
  processorName?: string               // TS worklet 注册名，默认 'hypersoundengine'
  inputChannelCount?: 2 | 6 | 8        // 输入总线；输出始终为双声道，默认 2
  blockSize?: number                   // script 兜底块长，默认 4096
  engine?: AudioEngine                 // 注入主线程 TS 引擎实例（测试/离线复用）
  engineFactory?: (sampleRate: number, channelCount?: number) => AudioEngine
}
```

### `host.attach(handle, params?)`

```ts
interface HyperSoundEngineHostHandle {
  audioContext: { sampleRate: number; audioWorklet?: { addModule(url: string): Promise<void> }; createScriptProcessor?(): unknown }
  masterGain: { connect(n: unknown): unknown; disconnect(): unknown }
  analyser: { connect(n: unknown): unknown }
}

await host.attach({ audioContext, masterGain, analyser }, params)
```

语义：`masterGain` 全断 → 接入处理节点 → 连 `analyser`；同一 handle 重复调用幂等；异步注册期间被 dispose 会安全放弃接线。切换到另一套 AudioContext 或节点前必须先 `dispose()`，避免旧音频图保留连接。`inputChannelCount` 可设为 `2 | 6 | 8`；这是 Host 的集成层限制，core `processMulti()` 本身接受 3–8 路。TS worklet 使用单输入总线、`channelCountMode:'max'` 与 `channelInterpretation:'discrete'` 协商最大输入声道，实际较少的通道补静音，输出固定为 2。当前 wasm worklet 仅支持 2 路输入；`mode:'auto'` 的多声道配置会回退 TS worklet，显式 wasm worklet 模式则 attach 失败。

默认 `engineBackend: 'ts'`，行为与既有版本一致。启用完整 Rust `HseEngine` wasm 链时必须同时提供 `wasmWorkletUrl` 与 `wasmUrl`；可选的 `hrtfUrl` 或 `hrtf`（`ArrayBuffer | HrtfGrid`）用于启用 Rust stage 22，二者互斥：

```ts
const host = createHyperSoundEngineHost({
  mode: 'auto',
  engineBackend: 'wasm',
  wasmWorkletUrl: '/assets/wasm-worklet-bundle.js',
  wasmUrl: '/assets/hse_wasm_bg.wasm',
  hrtfUrl: '/assets/listener.sofa', // 或 hrtf: sofaArrayBuffer / preparsedGrid
  workletUrl: '/assets/worklet-bundle.js', // auto 模式的 TS worklet 回退
})
await host.attach({ audioContext, masterGain, analyser }, params)
```

主线程分别 fetch/compile wasm module 与 fetch SOFA bytes，并缓存结果供后续参数替换节点复用。`hrtf` 也可直接接受 SOFA `ArrayBuffer` 或语言中立 `HrtfGrid`（`sampleRate/azimuths/elevations/hrirLength/left/right`）；资源通过 `processorOptions` 进入 worklet，SOFA/grid 仅在 worklet 构造控制阶段解析并调用 Rust `EngineChainStage::from_params_with_hrtf_grid`。默认 `spatial.mode='off'` 无需 HRTF；非 `off` 且未提供 HRTF 时构造明确失败。TS 与 wasm worklet 都只在构造时读取完整参数快照，宿主等待带 `requestId` 的 `ready` 回执后才接线；运行期消息只保留 `reset` 等轻量安全命令，不在 render 线程解析参数、HRTF 或重建处理链。`mode: 'auto'` 的回退顺序是 wasm worklet → TS worklet → ScriptProcessor；`mode: 'worklet'` 不跨后端静默回退。

### `host.setParams(params)`

script 路径在主线程原位应用完整参数快照。TS/wasm worklet 路径不修改当前渲染实例：宿主先以新快照构造并等待新节点 `ready`，再以零增益接入并预滚一个 128-frame render quantum，随后通过两个输出 GainNode 在 `workletCrossfadeMs`（默认 20ms，旧 `wasmCrossfadeMs` 仍作为别名）内线性交叉淡变；Promise 仅在 `audioContext.currentTime` 到达淡变终点且旧路径断开后完成，context 暂停时不会因墙钟超时提前清链。该语义不迁移旧引擎的滤波器、动态或混响状态；旧节点尾音仅在淡变窗口内参与输出。新节点构造失败时 Promise reject，当前可听链保持不变；并发调用按调用顺序串行替换，dispose 会立即清理活动与退役路径并结束淡变等待。

### `host.reset()`

复位主线程引擎，并向当前 worklet 下发 `{ type: 'reset' }`。

### `host.dispose()`

断开处理节点并恢复 `masterGain → analyser` 直连。

### 其他

- `host.getMode()`：当前实际承载模式 `'worklet' | 'script' | null`
- `host.getEngineBackend()`：当前实际内核 `'ts' | 'wasm' | null`；script 恒为 `ts`
- `host.getLastStats()` / `host.getLastAnalysis()`：TS worklet 回传的最近数据
- `host.getAudioNode()`：当前处理节点，可在前面插入自定义节点

---

## 6. AudioWorklet 打包（`hypersoundengine/worklet`）

AudioWorklet 全局作用域不支持 ESM import，必须打包为 IIFE：

```bash
npx esbuild src/worklet.ts --bundle --format=iife --outfile=dist/worklet-bundle.js
```

或直接使用本仓库脚本：

```bash
npm run build:worklet
```

产物 `dist/worklet-bundle.js` 可通过 `audioWorklet.addModule(url)` 加载。

完整 wasm worklet 是独立可选产物，不覆盖上述 TS 默认实现。先使用 `wasm-bindgen --target web` 生成包含 `HseEngine` 的 glue 与 wasm，再把生成目录显式传给构建脚本：

```bash
npm run build:wasm-worklet -- --pkg /path/to/wasm-bindgen-output
```

脚本会检查 wasm magic 与 `HseEngine` 的 HRTF 构造入口、缓冲、处理、reset 绑定，然后生成 `dist/wasm-worklet-bundle.js`。部署时还需把同目录的 `hse_wasm_bg.wasm` 作为 `wasmUrl` 静态资源发布；构建不读取 `target/` 或 wasm pilot 的忽略目录作为隐式输入。

真实浏览器门禁在构建 core、wasm-bindgen glue 与 wasm worklet 后运行：

```bash
npm run test:wasm-worklet:e2e -- --wasm /path/to/wasm-bindgen-output/hse_wasm_bg.wasm
```

该命令通过 `playwright-core` 复用本机 Chromium（也可设置 `HSE_CHROMIUM_EXECUTABLE`），从 `127.0.0.1` 加载正式产物，并把音频图终止到 `MediaStreamAudioDestinationNode`，不申请麦克风权限、不连接系统播放目的节点。CI 覆盖 Chromium；Firefox 因当前门禁环境没有与 Playwright 匹配的可用浏览器，尚未纳入自动测试。

---

## 7. 性能与实时安全约定

- `prepare(maxBlockSize)` 预分配后，`process()` / `processMulti()` 在容量内稳态零分配；多声道调用方还必须复用通道引用数组；
- 不要在音频线程执行参数解析或构链；TS/wasm worklet 参数更新均由 Host 预建节点并切换；
- 参数快照语义避免撕裂；
- `getStats()` / `getAnalysis()` 为同步读取，可在 UI 线程轮询；
- 可用 `npm run benchmark` 运行本地性能基准（默认参数全链、48kHz/128 帧）。
