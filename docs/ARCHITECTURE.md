# HyperSoundEngine —— 架构说明

## 1. 设计目标

- **独立**：引擎核心不依赖 WaveForge、不依赖 React、不依赖具体音频宿主；
- **双路径一致**：实时播放与离线导出使用同一 `HyperSoundEngine.process`；
- **实时安全**：音频回调内零分配、零锁、零系统调用；
- **可扩展**：对外只暴露小接口（`AudioEngine`），内部可替换 DSP 模块；
- **可测试**：纯 TS 内核可在 Node 中完整单测。

## 2. 分层

```
┌────────────────────────────────────────────────────────────┐
│ 接入方（其他软件 / HyperSoundEngine / Web App / 游戏引擎）          │
└───────────────────────────┬────────────────────────────────┘
                            │ 依赖 AudioEngine 接口 / HyperSoundEngineParams
┌───────────────────────────┴────────────────────────────────┐
│ 宿主层（Host）                                              │
│  - browser.ts / integration/HyperSoundEngineHost.ts                 │
│  - worklet/AudioEffectsProcessor.ts                         │
│  - 负责音频图接线、消息管道、模式回退                         │
└───────────────────────────┬────────────────────────────────┘
                            │ 调用 AudioEngine
┌───────────────────────────┴────────────────────────────────┐
│ 引擎核心（Core）                                             │
│  - HyperSoundEngine：21 级处理链编排                                 │
│  - ScenePresets / ShareCodec                                │
│  - analysis / offline                                       │
└───────────────────────────┬────────────────────────────────┘
                            │ 调用 DSP 模块
┌───────────────────────────┴────────────────────────────────┐
│ DSP 内核（dsp/）                                             │
│  - fft / biquad / EqChain / MidSide / Deesser / Compressor  │
│  - Limiter / BassEnhancer / Convolver / ReverbSimple        │
│  - LufsMeter / LoudnessComp / Resampler / Stretch / PitchYin│
│  - features / StretchLgplAdapter                            │
└─────────────────────────────────────────────────────────────┘
```

## 3. 模块与接缝

### 核心接缝：`AudioEngine`

所有外部接入方只学习一个接口：

```ts
interface AudioEngine {
  setParams(params: HyperSoundEngineParams): void
  process(inputs: Float32Array[], outputs: Float32Array[]): void
  getStats(): EngineStats
  getAnalysis(): EngineAnalysis
  getLatencySamples(): number
  reset(): void
}
```

`HyperSoundEngine` 是该接口的实现；`createEngine` 是工厂。

### 内部扩展点：`StereoProcessor`

核心 DSP 模块大多符合“`setParams` / `processStereo` / `reset`”形态。新增自定义处理器时可实现该接口，再接入引擎链。

### 处理链：`ProcessingStage`

`HyperSoundEngine` 内部使用 `ProcessingStage[]` 描述处理链（默认 21 级）：

```ts
interface ProcessingStage {
  id: string
  active(): boolean
  run(left: Float32Array, right: Float32Array, frameCount: number): void
}
```

- 顺序即数组顺序；
- `active()` 实现旁路语义；
- 内置阶段在 `buildStages()` 中构建；
- 外部可通过 `engine.registerStage(stage, index?)` / `engine.unregisterStage(id)` 动态扩展处理链。

### 宿主接缝：`HyperSoundEngineHost`

`HyperSoundEngineHost` 接受鸭子类型的 `AudioContext` / `AudioNode`，不绑定具体浏览器实现；Node 测试可用 stub 验证接线语义。

## 4. 处理链

```
输入
 ├─ 1) 响度归一化
 ├─ 2) 3D 环绕
 ├─ 3) M/S（宽度 + 人声比例 + 调制宽度）
 ├─ 4) Pre-EQ
 ├─ 5) Deesser（可选 sidechain 驱动）
 ├─ 6) Compressor（可选 sidechain 驱动）
 ├─ 7) NightMode
 ├─ 8) Delay
 ├─ 9) Chorus
 ├─ 10) Flanger
 ├─ 11) Phaser
 ├─ 12) Tremolo
 ├─ 13) 混响（卷积 / 算法 Freeverb / FDN 网络 / off）
 ├─ 14) BassEnhancer
 ├─ 15) LoudnessComp
 ├─ 16) IEQ（Post）
 ├─ 17) [FFT 取样点]
 ├─ 18) 动态均衡 DynamicEq（频谱包络自动混音）
 ├─ 19) [LUFS 取样点]
 ├─ 20) 调制主增益（LFO/Envelope → masterGain）
 ├─ 21) Limiter
 └─ 输出
```

> 说明：调制矩阵在块头计算 `masterGain / stereoWidth` 并应用于 M/S 宽度（3）与输出前主增益（19）；
> Sidechain 输入经 `process()` 第三参数传入，仅 `sidechainEnabled` 的效果器消费；
> 自定义阶段经 `registerStage()` 插入（缺省位于 Limiter 之前）；
> 多通道：`processBus()` 默认把 N 通道下混为立体声处理（环绕监听语义），
> `mode:'perChannelPair'` 时按立体声对逐对独立处理（每对独立子引擎），适合 5.1/7.1 各通道独立 DSP；
> MIDI：`sendMidi()` 事件入预分配环形队列，`process()` 块头消费，按 MIDI Learn 绑定（`AutomationTarget` 白名单路径）映射到参数并经一阶平滑应用（防 zipper）。

## 5. 独立包与适配层

- `src/`：独立引擎包（构建为 `dist/`）；
- `adapters/waveforge/`：WaveForge 专属接线，不属于核心包；
- `ui/`：可选 React 调音室，不参与核心构建；
- 其他软件接入时只依赖 `hypersoundengine` 与 `hypersoundengine/browser`。

## 6. 关键设计决策

1. **纯 TS 内核而非 Web Audio 节点图**：双路径一致、可测试、可进 AudioWorklet；
2. **参数快照语义**：`setParams` 整体替换、`getParams` 返回深拷贝，避免状态分叉；
3. **确定性**：无随机、无 Date、无 console，同输入同参数同输出；
4. **零分配稳态**：`prepare(maxBlockSize)` 预分配后，`process()` 稳态零分配；
5. **LGPL 合规**：soundtouchjs 以“不修改源码 + 链接调用”方式使用；
6. **工程化**：提供 `npm run benchmark`、性能冒烟测试与 GitHub Actions CI。
