# HyperSoundEngine —— 对外接口文档（API）

> 适用版本：0.2.0（独立引擎包）
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
  type HyperSoundEngineParams,
  type AudioEngine,
  type EngineStats,
  type EngineAnalysis,
} from 'hypersoundengine'
```

### 浏览器宿主入口

```ts
import { createHyperSoundEngineHost, HyperSoundEngineHost } from 'hypersoundengine/browser'
```

### AudioWorklet 打包入口

```ts
// 仅供 esbuild/vite 打包，不可在 Node 主线程直接 import
import { AudioEffectsProcessor, WORKLET_PROCESSOR_NAME } from 'hypersoundengine/worklet'
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
- 当前支持单声道（`channelCount=1`）与立体声（`channelCount=2`）；
- `sidechain`：可选外部侧链输入（与 inputs 同长度的 Float32Array[]）。只有开启 `sidechainEnabled` 的效果器（Compressor / Deesser）会使用它驱动包络/检测；
- 多通道便捷入口 `processBus(input: AudioBus, output: AudioBus, sidechain?: AudioBus, options?)`：
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

### `AudioBus` 多通道缓冲（`dsp/AudioBus.ts`）

非交错 N 通道缓冲抽象，通道级工具均为确定性、纯函数（`AudioBus` 自身不持有状态）：

```ts
const bus = AudioBus.create(6, 1024)            // 5.1：6 通道 × 1024 帧（零填充）
const bus2 = AudioBus.fromInterleaved(inter, 6) // 交错 → 非交错（拷贝）
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

### MIDI 事件接口 / MIDI Learn（`HyperSoundEngine` 专属）

```ts
type MidiEvent =
  | { type: 'cc'; channel: number; cc: number; value: number }
  | { type: 'noteOn'; channel: number; note: number; velocity: number }
  | { type: 'noteOff'; channel: number; note: number }

type AutomationTarget =
  | { kind: 'builtin'; param: 'masterGain' | 'stereoWidth' }
  | { kind: 'path'; path: string }   // 任意参数路径白名单（见 AUTOMATABLE_PARAMS）

engine.sendMidi(events: MidiEvent[]): void
engine.midiLearn(cc: number, target: AutomationTarget, opts?: {
  eventType?: 'cc' | 'note'   // 默认 'cc'
  min?: number; max?: number  // 覆盖白名单范围
  smoothMs?: number           // 一阶平滑，默认 20
  invert?: boolean            // 反向映射
}): void
engine.midiUnlearn(cc: number, opts?: { eventType?: 'cc' | 'note' }): boolean
engine.getMidiBindings(): MidiBinding[]
engine.getMidiDroppedCount(): number
```

- `sendMidi` 写入预分配环形队列（容量 4096，溢出丢最旧并累计 dropped），`process()` 块头消费（块速率，非 sample-accurate）。
- CC 0–127 线性映射到 [min, max]；note on→max / note off→min（布尔参数 on→true / off→false）。
- 路径白名单 `AUTOMATABLE_PARAMS`（compressor/deesser/bassEnhancer/reverb/modEffects/ieq/limiter/pitch 等），非法路径在 `midiLearn` 时抛错。
- 平滑防 zipper；绑定属配置（`reset` 保留，仅清空运行时队列与平滑状态）。

### WAV 文件 I/O（`io/wav.ts`）

```ts
encodeWav(channels: Float32Array[], sampleRate: number, opts?: { bitDepth?: 16 | 32 }): ArrayBuffer
decodeWav(buffer: ArrayBuffer | Uint8Array): { sampleRate: number; channels: Float32Array[]; bitDepth: 16 | 32 }
```

- 16-bit PCM（format=1）/ 32-bit Float（format=3），标准 RIFF/WAVE 头。
- 多通道直接对应 `AudioBus` 非交错布局，解码结果可零拷贝进入 `processBus`。
- 畸形输入（坏魔数 / 缺 chunk / 块不对齐 / 0 声道 / 不支持位深）一律抛错（防注入）。

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
| `pitch` | 变速/变调（离线 Stretch 参数） |
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
  mode?: 'worklet' | 'script' | 'auto' // 默认 auto
  workletUrl?: string                  // worklet 打包产物 URL
  processorName?: string               // 默认 'hypersoundengine'
  blockSize?: number                   // script 兜底块长，默认 4096
  engine?: AudioEngine                 // 注入引擎实例（测试/离线复用）
  engineFactory?: (sampleRate: number, channelCount?: number) => AudioEngine // 自定义引擎工厂
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

语义：`masterGain` 全断 → 接入处理节点 → 连 `analyser`；幂等；异步注册期间被 dispose 会安全放弃接线。

### `host.setParams(params)`

同步更新主线程引擎与 worklet 处理器。

### `host.dispose()`

断开处理节点并恢复 `masterGain → analyser` 直连。

### 其他

- `host.getMode()`：当前实际模式 `'worklet' | 'script' | null`
- `host.getLastStats()` / `host.getLastAnalysis()`：worklet 回传的最近数据
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

---

## 7. 性能与实时安全约定

- `prepare(maxBlockSize)` 预分配后，`process()` 稳态零分配；
- 不要在音频线程调用 `setParams()` 以外的重操作（`setParams` 内部会重算滤波器系数，建议在 UI/控制线程调用）；
- 参数快照语义避免撕裂；
- `getStats()` / `getAnalysis()` 为同步读取，可在 UI 线程轮询；
- 可用 `npm run benchmark` 运行本地性能基准（默认参数全链、48kHz/128 帧）。
