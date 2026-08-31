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
│  - worklet/HseAudioEffectsProcessor.ts                         │
│  - 负责音频图接线、消息管道、模式回退                         │
└───────────────────────────┬────────────────────────────────┘
                            │ 调用 AudioEngine
┌───────────────────────────┴────────────────────────────────┐
│ 引擎核心（Core）                                             │
│  - HyperSoundEngine：22 级处理链编排                                 │
│  - ScenePresets / ShareCodec                                │
│  - analysis / offline                                       │
└───────────────────────────┬────────────────────────────────┘
                            │ 调用 DSP 模块
┌───────────────────────────┴────────────────────────────────┐
│ DSP 内核（dsp/）                                             │
│  - fft / biquad / EqChain / MidSide / Deesser / Compressor  │
│  - Limiter / BassEnhancer / Convolver / ReverbSimple        │
│  - LufsMeter / LoudnessComp / Resampler / HseStretch / PitchYin│
│  - features                                                 │
└─────────────────────────────────────────────────────────────┘
```

## 3. 模块与接缝

### 核心接缝：`AudioEngine`

所有外部接入方只学习一个接口：

```ts
interface AudioEngine {
  setParams(params: HyperSoundEngineParams): void
  process(inputs: Float32Array[], outputs: Float32Array[]): void
  processMulti?(inputs: Float32Array[], outputs: Float32Array[]): void
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

`HyperSoundEngine` 内部使用 `ProcessingStage[]` 描述处理链（默认 22 级，含空间音频内联级）：

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
 ├─ 22) 空间音频（TS 内联；mode='off' 时旁路）
 └─ 输出
```

> 普通单/双声道 `process()` 保持上图冻结顺序（1–21 → stage 22 spatial）。多声道实时输入 `processMulti()` 使用专用顺序：3–8 路 caller-owned 非交错缓冲先经 `SpatialBackend.processMulti` 双耳化，再让所得 L/R 执行完整第 1–21 级；这样所有声道共享 EQ、动态器、分析与最终 Limiter，且不会递归再次空间化。spatial off 时固定取 ch0/ch1、忽略 ch2+，随后执行第 1–21 级。`processBus()` 仍是会分配的非实时便利入口。
>
> Host 的 `inputChannelCount` 支持 2/6/8；AudioWorklet 以 max/discrete 协商最大输入声道，物理输出始终为 2，不实现多声道设备输出。`AudioEngine.processMulti` 是兼容性可选扩展；注入的旧实现缺少该方法时，Host 仅把 ch0/ch1 交给既有双声道 `process`。
>
> 说明：调制矩阵在块头计算 `masterGain / stereoWidth` 并应用于 M/S 宽度（3）与输出前主增益（19）；
> Sidechain 输入经 `process()` 第三参数传入，仅 `sidechainEnabled` 的效果器消费；
> 自定义阶段经 `registerStage()` 插入（缺省位于 Limiter 之前）；
> 多通道：`processBus()` 默认把 N 通道下混为立体声处理（环绕监听语义），
> `mode:'perChannelPair'` 时按立体声对逐对独立处理（每对独立子引擎），适合 5.1/7.1 各通道独立 DSP；

## 5. Rust 支线边界（1.5.1）

Rust workspace 与 TS 支线零代码依赖，由 `specs/` 共享行为契约：

- `hse-core::engine_chain::EngineChainStage` 镜像 TS 主链第 1–21 级，包含链内组合级与分析/计量状态；它实现 `Stage`，以 `prepare` 预分配并通过 `process` 原位处理。72/72 音频冻结向量仍固定 `spatial.mode='off'`。
- Rust 第 22 级支持 `instant`、`headLocked`、`world` 与 `stage`：宿主先在控制路径注入 HRTF grid；world 消费完整 listener 姿态、轨迹、playhead、遮挡与相邻快照确定速度，source id 映射到稳定 slot；stage 对齐 preset/seat/roomSize/reverbAmount/customSources，ambience 在同一内联级叠加。物理输出仍固定双耳立体声。
- `hrtf-core` 已实现 world-listener 完整欧拉姿态、规则 grid、SOFA 解析、44.1/48/96 kHz 确定性重采样、nearest/spherical 插值、time/partitioned 卷积、距离/空气吸收、Doppler、遮挡、声源大小、房间与稳定 slot。共享空间门禁为 world-listener 14/14 + renderer/ABI 14/14 = 28/28；真实 SOFA 资产尚未进入自动门禁。
- `hse-wasm` 是依赖 `hse-core` 的边界 crate，`HseEngine` 导出默认构造以及 `withSofaBytes` / `withHrtfGrid` 控制路径入口：前者解析 SOFA，后者验证预解析规则 grid，随后统一调用 `EngineChainStage::from_params_with_hrtf_grid` 建立 1–22 级链。`spatial.mode='off'` 的默认构造保持兼容；非 off 且无 HRTF 明确失败。四块预分配 planar 缓冲与 `process(frames)` 原位交换主输入/sidechain，render 稳态不解析 HRTF。
- TS 与 wasm worklet backend 的参数更新都由 `HyperSoundEngineHost` 在控制路径预建完整 AudioWorkletNode，参数快照只经 `processorOptions` 在构造期应用；wasm Host 还会在主线程 fetch/缓存 SOFA bytes 或复用调用方提供的 `ArrayBuffer`/grid，并在节点替换时复用同一 HRTF 资源与已编译 module。等待 `ready` 后先以零增益接入并预滚一个 128-frame render quantum，再经双 GainNode 短交叉淡变替换，render callback 不解析参数/HRTF、不构链。旧链仅在音频时间窗口内保留尾音，`AudioContext.currentTime` 暂停时不会由墙钟提前断开；dispose 可立即取消等待并清理新旧路径。这不是 DSP 状态迁移。CI 在 headless Chromium 中加载正式 bundle/wasm，以无设备的 `MediaStreamAudioDestinationNode` 驱动真实 AudioWorklet，门禁 `ready`、spatial off 的 1–21 级非静音处理、构造失败静音和参数节点替换淡变；Firefox 尚未纳入自动门禁。
- `hse-wasm::spatial_abi` 另行导出规划中的 8 个空间 C ABI 函数及生命周期/错误辅助符号；正式 Host 现通过 `HseEngine` 构造入口接入 stage 22，但不直接调用该薄 ABI。
- `hse-wasapi`/`hse-service` 支持 shared/exclusive、事件等待、排队帧统计与真机验收工具；服务控制面可在 idle 从本地绝对 SOFA 路径预载 HRTF grid，`start` 与运行态 `setParams` 在控制线程构建带 grid 的 1–22 级链并于块边界交换，DSP 线程不解析文件。真实设备 shared/exclusive 端到端延迟与 CPU 必须由用户验收，排队帧统计不得冒充端到端测量。
- 固定时长测试已删除且不得恢复；异步和实时测试以事件、帧数、块序号或显式超时上限收敛。

## 6. 独立包与适配层

- `src/`：独立引擎包（构建为 `dist/`）；
- `adapters/waveforge/`：WaveForge 专属接线，不属于核心包；
- `ui/`：可选 React 调音室，不参与核心构建；
- 其他软件接入时只依赖 `hypersoundengine` 与 `hypersoundengine/browser`。

## 7. 关键设计决策

1. **纯 TS 内核而非 Web Audio 节点图**：双路径一致、可测试、可进 AudioWorklet；
2. **参数快照语义**：`setParams` 整体替换、`getParams` 返回深拷贝，避免状态分叉；
3. **确定性**：无随机、无 Date、无 console，同输入同参数同输出；
4. **零分配稳态**：`prepare(maxBlockSize)` 预分配后，`process()` 稳态零分配；
5. **零 LGPL 依赖**：可选路径 soundtouchjs 与适配层已于 2026-08-22 移除；宿主侧实时变速变调由 WaveForge 自有的 @soundtouchjs/audio-worklet 承担；
6. **工程化**：提供 `npm run benchmark`、性能冒烟测试与 GitHub Actions CI。
