# HyperSoundEngine 接线与 API 指南

> 适用版本：1.5.1。本文保留 TS/UI 专项细节。新项目和自动化编码代理应先阅读 [`INTEGRATION.md`](INTEGRATION.md)，由其中的路径选择表决定使用 TS core、浏览器 TS/Rust Host、Rust `hse-service` 或空间 C ABI。服务 wire 契约不在本文重复定义。

## 目录

1. [三种接入形态](#1-三种接入形态)
2. [安装与构建](#2-安装与构建)
3. [核心 API:AudioEngine 接口](#3-核心-apiaudioengine-接口)
4. [场景一:Node.js 离线处理](#4-场景一nodejs-离线处理)
5. [场景二:浏览器实时播放(AudioWorklet)](#5-场景二浏览器实时播放audioworklet)
6. [场景三:多通道处理(processBus)](#6-场景三多通道处理processbus)
7. [WAV 文件 I/O](#7-wav-文件-io)
8. [Sidechain](#8-sidechain)
9. [参数调制矩阵](#9-参数调制矩阵)
10. [自定义处理阶段](#10-自定义处理阶段)
11. [场景预设与分享串](#11-场景预设与分享串)
12. [UI 接入方式(React 调音室)](#12-ui-接入方式react-调音室)
13. [类型速查](#13-类型速查)

---

## 1. 三种接入形态

| 形态 | 入口 | 适用 | 特点 |
|------|------|------|------|
| **核心内核** | `hypersoundengine` | Node / 任意 JS 运行时 | 纯 TS,零 DOM 依赖,离线批处理 |
| **浏览器宿主** | `hypersoundengine/browser` | Web 应用 | AudioWorklet/ScriptProcessor 接入 Web Audio 图 |
| **Worklet 打包** | `hypersoundengine/worklet` | 打包器 | AudioWorklet 处理器单文件入口 |

三者共用同一套纯 TS DSP 内核,`process()` 行为完全一致。

---

## 2. 安装与构建

```bash
# 从源码构建(产出 dist/)
cd HyperSoundEngine
npm install
npm run build        # = build:types + build:core + build:worklet
```

构建产物:

| 产物 | 说明 |
|------|------|
| `dist/index.js` | 核心内核(Node + 浏览器通用) |
| `dist/browser.js` | 浏览器宿主(HyperSoundEngineHost) |
| `dist/worklet.js` | AudioWorklet 打包入口 |
| `dist/worklet-bundle.js` | AudioWorklet 单文件处理器(供 `audioWorklet.addModule`) |

UI(`ui/`)有独立 `tsconfig.ui.json`,不随核心构建,需由宿主应用自行打包。

---

## 3. 核心 API:AudioEngine 接口

所有接入形态最终都通过这个 8 方法接口驱动引擎:

```ts
import type { AudioEngine } from 'hypersoundengine'

interface AudioEngine {
  setParams(params: HyperSoundEngineParams): void   // 全量参数快照(引擎深拷贝)
  getParams(): HyperSoundEngineParams                // 当前快照深拷贝
  prepare(maxBlockSize: number): void                // 预分配工作缓冲(实时前调一次)
  process(inputs, outputs, sidechain?): void         // 就地处理,稳态零分配
  getStats(): EngineStats                            // LUFS/峰值/限幅衰减/延迟
  getAnalysis(): EngineAnalysis                      // 频谱 + 特征
  getLatencySamples(): number                        // 引擎延迟(样本)
  reset(): void                                      // 复位内部状态
}
```

**关键约定**:
- `setParams` 每次接收**完整快照**,引擎内部深拷贝,调用方可安全复用对象;
- `process` 就地写入 `outputs`,长度须 ≥ `inputs`;**稳态零分配**(可在实时线程调用);
- `process` **确定性**:同输入同参数 → 同输出(无随机/Date)。

### 创建引擎

```ts
import { createEngine, createDefaultParams } from 'hypersoundengine'

// 通用工厂(返回 AudioEngine 接口)
const engine = createEngine(48000, 2)

// 需要访问 HyperSoundEngine 专有 API(processBus/registerStage)时:
import { createHyperSoundEngine } from 'hypersoundengine'
const engine = createHyperSoundEngine(48000, 2)  // 返回具体类
```

---

## 4. 场景一:Node.js 离线处理

适合批处理、响度对齐、转格式、自动化测试。

```ts
import { createEngine, createDefaultParams } from 'hypersoundengine'
import { encodeWav, decodeWav } from 'hypersoundengine'

const fs = 48000
const engine = createEngine(fs, 2)
engine.prepare(4096)

// 配置参数
const params = createDefaultParams(fs)
params.compressor.enabled = true
params.compressor.thresholdDb = -20
params.reverb.enabled = true
engine.setParams(params)

// 读入 WAV → 处理 → 写出 WAV
const fileBytes = await readFile('input.wav')           // Node fs
const { channels, sampleRate } = decodeWav(fileBytes)   // 非交错 Float32Array[]
const out: Float32Array[] = [new Float32Array(channels[0].length), new Float32Array(channels[0].length)]

// 分块处理(避免一次处理过长)
const block = 4096
for (let off = 0; off < channels[0].length; off += block) {
  const n = Math.min(block, channels[0].length - off)
  const inL = channels[0].subarray(off, off + n)
  const inR = (channels[1] ?? channels[0]).subarray(off, off + n)
  const outL = out[0].subarray(off, off + n)
  const outR = out[1].subarray(off, off + n)
  engine.process([inL, inR], [outL, outR])
}

const wavBytes = encodeWav(out, sampleRate, { bitDepth: 16, format: 'standard' })
await writeFile('output.wav', Buffer.from(wavBytes))

console.log(engine.getStats())  // { lufsIntegrated, peakDb, ... }
```

完整示例见 `examples/node-offline.mjs`。

---

## 5. 场景二:浏览器实时播放(AudioWorklet)

把引擎接入 Web Audio 图,`masterGain → 引擎处理节点 → analyser`。

```ts
import { createHyperSoundEngineHost } from 'hypersoundengine/browser'
import { createDefaultParams } from 'hypersoundengine'

const audioContext = new AudioContext()
const masterGain = audioContext.createGain()
const analyser = audioContext.createAnalyser()

// 创建宿主(worklet 优先,失败回退 ScriptProcessor)
const host = createHyperSoundEngineHost({
  mode: 'auto',                       // 'worklet' | 'script' | 'auto'
  workletUrl: '/worklet-bundle.js',   // AudioWorklet 单文件 URL(未打包会自动回退 script)
  blockSize: 4096,                    // script 模式块长
})

// 接入音频图(幂等:同 handle 重复调用安全)
const params = createDefaultParams(audioContext.sampleRate)
await host.attach({ audioContext, masterGain, analyser }, params)

// 实时更新参数：从当前完整快照派生，再整体提交
async function updateCompressorThreshold(thresholdDb: number) {
  const merged = host.engine.getParams()
  merged.compressor.thresholdDb = thresholdDb
  await host.setParams(merged)
}

// 拆除(恢复 masterGain → analyser 直连)
host.dispose()

// host.engine —— AudioEngine 实例(可读 stats/analysis)
console.log(host.engine.getStats())
```

### 接入语义

- `attach`:`masterGain.disconnect()` → 接处理节点 → `node.connect(analyser)`(防新旧双链并联)
- `dispose`:断处理节点 → 恢复 `masterGain.connect(analyser)`
- **幂等**:重复 `attach` 同一 handle 直接 return
- **竞态防护**:worklet 异步注册期间被 dispose → 放弃接线

### worklet 模式 vs script 模式

| | worklet | script |
|---|---|---|
| 线程 | 渲染线程(独立) | 主线程 |
| 延迟 | 低(128 帧量子) | 高(blockSize) |
| stats | 经 `port` 回传(约 80ms 一次) | 主线程 `engine.getStats()` 直读 |
| 打包 | 需 `worklet-bundle.js` | 无需打包 |

worklet 模式下完整参数快照经 `processorOptions.initialParams` 在节点构造期应用；运行中由 Host 预建新节点、等待 `ready` 后以零增益接入并预滚一个 128-frame render quantum，再按 `AudioContext.currentTime` 交叉淡变替换，不向渲染线程发送参数重建消息。stats/analysis 仍经 `port.postMessage({type:'stats', ...})` 回传。

完整示例见 `examples/browser-host.mjs`。

---

## 6. 场景三:多通道处理(processBus)

`processBus` 是 `process` 的多通道便利入口(非实时,会分配临时缓冲):

```ts
import { HseAudioBus, createHyperSoundEngine } from 'hypersoundengine'

const engine = createHyperSoundEngine(48000, 2)

// 5.1 = 6 通道输入
const input = HseAudioBus.create(6, 4096)
const output = HseAudioBus.create(6, 4096)

// 模式一:downmix(默认)—— 下混立体声处理,结果复制到各通道
engine.processBus(input, output)

// 模式二:perChannelPair —— 按立体声对逐对独立处理(每对独立子引擎)
engine.processBus(input, output, undefined, { mode: 'perChannelPair' })
//   对 (0,1)、(2,3)、(4,5) 各用独立引擎实例处理,互不串扰
//   奇数剩余通道复制成立体声处理取 L 写回
//   sidechain 同样按对切片
```

### HseAudioBus 通道工具

```ts
HseAudioBus.create(channelCount, frameCount)         // 零填充创建
HseAudioBus.fromInterleaved(interleaved, channelCount) // 交错 → 非交错
bus.toInterleaved()                                 // 非交错 → 交错
bus.copyTo(target) / fill(v) / applyGain(g) / mixFrom(other, g)
bus.extract([0, 1])                                 // 提取通道子集
bus.downmixToMono() / downmixToStereo() / writeStereo(l, r)
```

---

## 7. WAV 文件 I/O

```ts
import { encodeWav, decodeWav } from 'hypersoundengine'

// 编码:非交错 Float32Array[] → 标准 RIFF/WAVE ArrayBuffer
const buf = encodeWav([left, right], 48000, { bitDepth: 16, format: 'standard' })  // 位深也可用 32(float)

// 解码:自动识别 standard 与 1.0.0 历史 legacy 格式
const { sampleRate, channels, bitDepth } = decodeWav(buf)

// 解码结果可直接构造 HseAudioBus
import { HseAudioBus } from 'hypersoundengine'
const bus = new HseAudioBus(channels)
```

- 支持 **16-bit PCM**(formatTag=1)与 **32-bit Float**(formatTag=3)
- 编码缺省 `legacy` 仅用于旧字节契约兼容；文件交换与播放器导出应显式选择 `standard`
- 解码自动识别两种模式；standard 文件执行严格 RIFF/fmt/data 一致性校验
- 多通道直接对应 HseAudioBus 非交错布局

---

## 8. Sidechain

```ts
// 主信号 + 外部 sidechain 信号
const mainL = new Float32Array(4096), mainR = new Float32Array(4096)
const sideL = new Float32Array(4096), sideR = new Float32Array(4096)

// 开启压缩器的 sidechain
params.compressor.enabled = true
params.compressor.sidechainEnabled = true
engine.setParams(params)

// 第三参数 = sidechain;只有 sidechainEnabled 的效果器(Compressor/Deesser)消费它
engine.process([mainL, mainR], [outL, outR], [sideL, sideR])
```

---

## 9. 参数调制矩阵

```ts
const params = createDefaultParams(48000)
params.modulation.enabled = true
params.modulation.lfo.enabled = true
params.modulation.lfo.shape = 'sine'
params.modulation.lfo.rateHz = 2
params.modulation.lfo.depth = 0.5

// 路由:LFO → masterGain,深度 0.3,偏移 0
params.modulation.routes = [
  { source: 'lfo', target: 'masterGain', amount: 0.3, offset: 0 },
  { source: 'envelope', target: 'stereoWidth', amount: 0.5 },
]
engine.setParams(params)
```

- LFO:正弦/三角/方波/锯齿
- Envelope Follower:起控/释放/强度
- 目标:`masterGain` / `stereoWidth`

---

## 10. 自定义处理阶段

```ts
import type { ProcessingStage } from 'hypersoundengine'

const myGain: ProcessingStage = {
  id: 'my-gain',
  active: () => true,
  run: (l, r, n) => { for (let i = 0; i < n; i++) { l[i] *= 1.5; r[i] *= 1.5 } },
  reset: () => {},
}

engine.registerStage(myGain)              // 插到 limiter 之前
engine.registerStage(myGain, 5)           // 指定位置
engine.unregisterStage('my-gain')         // 移除
engine.getStages()                        // 当前链副本
```

---

## 11. 场景预设与分享串

```ts
import { SCENE_PRESETS, applyScene, encodeShareCode, decodeShareCode } from 'hypersoundengine'

// 内置 12 场景
const popScene = SCENE_PRESETS.find(s => s.id === 'pop')!
engine.setParams(popScene.params)

// 当前编码为 HSE2 + Crockford Base32 差异载荷；decode 兼容历史 v1
const code = encodeShareCode(engine.getParams())   // "HSE2-..."
const restored = decodeShareCode(code)             // 非法输入抛 Error
engine.setParams(restored)
```

---

## 12. UI 接入方式(React 调音室)

UI 是**可选**的 React 组件库,经 `HyperSoundEngineUiBridge` 桥接,不直接 import 引擎。适合已用 React 的宿主嵌入完整调音界面。

### 12.1 架构

```
你的 App
  │
  ├─ engine = createHyperSoundEngine(fs, 2)        # 引擎实例
  ├─ bridge = createHyperSoundEngineUiBridge(engine, fs)   # 桥(封装引擎 API)
  │
  └─ <HyperSoundEngineMixingStudio bridge={bridge} ... />  # UI 组件
```

```
UI 组件 ──读/写──> HyperSoundEngineUiBridge ──调用──> AudioEngine(HyperSoundEngine)
```

UI 只依赖 bridge 接口,不直接依赖引擎具体类——换引擎实现只需换 bridge。

### 12.2 最小接入

```tsx
import { createHyperSoundEngine } from 'hypersoundengine'
import {
  HyperSoundEngineMixingStudio,
  createHyperSoundEngineUiBridge,
} from 'hypersoundengine/ui'   // 宿主自行打包 ui/

// 1. 创建引擎与桥
const engine = createHyperSoundEngine(48000, 2)
const bridge = createHyperSoundEngineUiBridge(engine, 48000)

// 2. 渲染调音室
function App() {
  const [open, setOpen] = useState(false)
  return (
    <>
      <button onClick={() => setOpen(true)}>打开调音室</button>
      {open && (
        <HyperSoundEngineMixingStudio
          bridge={bridge}
          playerTheme="dark"            // 'dark' | 'light'
          onClose={() => setOpen(false)}
          anchorRect={null}             // 弹窗锚点(可选)
          exportWav={null}              // 离线导出回调(可选)
          exporting={false}
        />
      )}
    </>
  )
}
```

### 12.3 主面板 Props

```ts
interface HyperSoundEngineMixingStudioProps {
  bridge: HyperSoundEngineUiBridge       // 必需:引擎桥
  onClose: () => void                    // 必需:关闭回调
  playerTheme: 'dark' | 'light'          // 必需:主题
  anchorRect?: { x, y, width, height } | null  // 弹窗锚点(CSS 动画)
  exportWav?: (() => Promise<void>) | null  // 离线导出(可选)
  exporting?: boolean
}
```

### 12.4 四个页签

| 页签 | 功能 |
|------|------|
| 音效场景 | 12 内置场景 + 效果卡片(压缩/混响/低音/调制类效果/调制矩阵)+ 启用开关 |
| 均衡器 | 5/10/20 段 EQ 曲线编辑器 |
| 调音器 | 分享串编码/解码 + 离线 WAV 导出 |
| 分析 | 实时频谱 + 频谱特征 + LUFS |

### 12.5 Bridge 接口

```ts
interface HyperSoundEngineUiBridge {
  // 参数
  getParams(): HyperSoundEngineParams
  setParams(p: HyperSoundEngineParams): void
  // 统计/分析
  getStats(): EngineStats
  getAnalysis(): EngineAnalysis
  getLatencySamples(): number
  getSampleRate(): number
  // 场景
  getScenes(): ScenePreset[]
  applyScene(id: string): void
  saveMyScene(name: string): boolean       // localStorage 持久化
  deleteMyScene(id: string): void
  // 分享串
  encodeShare(p): string
  decodeShare(code: string): HyperSoundEngineParams
  // 听力测试
  beginHearing(): void
  hearingStep(): HyperSoundEngineHearingSession
  answerHearing(heard: boolean): HyperSoundEngineHearingSession
  resetHearing(): void
}
```

### 12.6 参数更新模式(UI 侧)

UI 通过 `useHyperSoundEngineParams` hook 操作参数:

```tsx
import { useHyperSoundEngineParams } from 'hypersoundengine/ui'

function MyPanel({ bridge }) {
  const controller = useHyperSoundEngineParams(bridge)
  // controller.params    —— 当前快照(深拷贝)
  // controller.patch(p)  —— 深合并局部修改后提交(常用)
  // controller.replace(next) —— 整包替换(场景/分享串)

  controller.patch({ compressor: { thresholdDb: -30 } })  // 局部改
}
```

### 12.7 UI 打包注意

- UI 在独立 `tsconfig.ui.json`,依赖 `react` / `react-dom` / `lucide-react`(peerDependencies,宿主自备)
- 核心包构建(`npm run build`)**不包含** UI;宿主应用用 Vite/Webpack 自行打包 `ui/`
- UI 通过相对路径 `from '../src/types'` 引用核心类型,需保证核心已构建或 TS path 配置正确

---

## 13. 类型速查

```ts
// 引擎
createEngine(sampleRate, channelCount?): AudioEngine
createHyperSoundEngine(sampleRate, channelCount?): HyperSoundEngine
createDefaultParams(sampleRate): HyperSoundEngineParams

// 浏览器宿主
createHyperSoundEngineHost(opts?): HyperSoundEngineHost
host.attach(handle, params?): Promise<void>
host.setParams(p): Promise<void> // 必须 await 或处理 reject
host.dispose(): void
host.engine: AudioEngine

// WAV
encodeWav(channels, sampleRate, { bitDepth?, format?: 'legacy' | 'standard' }): ArrayBuffer
decodeWav(buffer): { sampleRate, channels, bitDepth }

// 多通道
HseAudioBus.create(ch, frames) / HseAudioBus.from(arr)
engine.processBus(input, output, sidechain?, options?): void

// UI
createHyperSoundEngineUiBridge(engine, sampleRate): HyperSoundEngineUiBridge
<HyperSoundEngineMixingStudio bridge={bridge} ... />
useHyperSoundEngineParams(bridge): { params, patch, replace }
```
