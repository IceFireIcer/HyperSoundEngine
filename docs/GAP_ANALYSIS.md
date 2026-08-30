# HyperSoundEngine 差距分析与优化计划

> 依据根目录四份 DSP 引擎文档交叉对比：
> - Kimi-k2.6：技术、功能与架构详解
> - GLM5.2：合格 DSP 音频处理引擎技术指南
> - Minimax-M2.7：技术架构文档
> - Mimo2.5：技术架构与功能全景指南
>
> 范围：**除音频驱动/I/O 层以外**的功能与架构差距。
>
> **状态（2026-08-29）**：本文是早期时点分析（基于旧谱系参考文档交叉对比），其结论已大多
> 落地或被超越（空间音频、双支线、引擎服务进程、推流协议均已实现）；现行差距与路线以
> `原生化双支线与Windows音频接入规划书.md` §五阶段计划与 `CHANGELOG.md` 为准，本文仅存档。

---

## 1. 当前 HyperSoundEngine 已具备（简版）

- 纯 TS 实时安全内核：`process()` 稳态零分配、确定性、`prepare()` 预分配。
- 21 级固定处理链：响度归一化、3D环绕、M/S宽度、Pre-EQ、Deesser、Compressor、NightMode、Delay、Chorus、Flanger、Phaser、Tremolo、Reverb、BassEnhancer、LoudnessComp、IEQ、FFT取样、DynamicEq、LUFS取样、调制主增益、Limiter 等。
- DSP 基础库：Biquad/EQ、FFT、压缩/限制、卷积/算法混响、LUFS、重采样、变速变调、YIN、特征提取。
- 场景预设、分享串、频谱/听力分析、声源分离任务队列（占位）。
- 浏览器宿主 `HyperSoundEngineHost`（worklet/script 双模式）。
- 测试体系 32 文件 / 331 用例；CI、benchmark、文档。

---

## 2. 功能差距（除驱动外）

| 领域 | 文档要求 | HSE 现状 | 优先级 |
|---|---|---|---|
| 处理图 / 路由 | 动态节点图、DAG、并行 lanes、虚拟总线、Sidechain | 固定链，无动态图、无并行、无 sidechain | P0 |
| 插件/扩展 | 模块化效果器注册、第三方处理器 | 有 `ProcessingStage`/`StereoProcessor` 接口，但引擎未暴露注册入口 | P0 |
| 参数调制 | LFO、Envelope Follower、ADSR、Macro、Sample-accurate automation | 仅静态参数快照 | P0/P1 |
| 多通道 | 5.1/7.1、Ambisonics、任意通道路由 | 仅单声道/立体声 | P1 |
| 调制类效果 | Chorus/Flanger/Phaser/Tremolo/Vibrato/Delay | 无 | P1 |
| 语音处理 | AEC、ANS、VAD、啸叫抑制、AGC | 无（仅听力测试） | P1 |
| 文件 I/O | WAV/AIFF/FLAC/MP3/Opus 编解码、流式解码 | 无（仅适配层有简单 WAV 编码） | P1 |
| 合成器 | 振荡器、波表、FM、采样播放 | 无 | P2 |
| 空间音频 | HRTF、Ambisonics、对象音频 | 仅轻量 3D 环绕 | P2 |
| AI | 降噪、声源分离、神经网络推理 | 分离队列占位，无实际模型 | P2 |
| 用户功能 | A/B、参数随机化、撤销/重做 | 无 | P2 |

---

## 3. 架构差距

| 架构点 | 文档要求 | HSE 现状 | 优先级 |
|---|---|---|---|
| 动态图（copy-on-write + 原子切换） | 运行时增删节点/连线 | 固定 `ProcessingStage[]`，不支持热改拓扑 | P0 |
| 节点/处理器注册系统 | 新增效果器无需改核心 | 接口存在，缺公开注册 API | P0 |
| 参数自动化/调制矩阵 | LFO→参数、事件队列、sample-accurate | 无 | P1 |
| 多通道 HseAudioBus | 任意通道、总线/通道协商 | 无 | P1 |
| Worker 并行调度 | 独立分支并行渲染 | 无 | P2 |
| WASM/SIMD 加速层 | FFT/卷积/热路径 | 纯 JS | P2 |
| 背压/降级 | 高负载切简化算法 | 无 | P2 |
| Denormal 处理 | FTZ/DAZ 或注入微扰 | 未显式处理 | P1 |

---

## 4. 优化计划（按优先级）

### P0（先做，架构地基）
1. **暴露处理器注册 API**：`HyperSoundEngine.registerStage/unregisterStage`，让外部可插入自定义 `ProcessingStage`/`StereoProcessor`，不改核心。
2. **动态链排序**：支持按 index 插入、按 id 移除、查询当前 stages。
3. 补充注册/移除/重置的测试与文档。

### P1（功能补全）
4. **Sidechain 输入**：为 Compressor/Deesser 增加可选 sidechain 输入。
5. **参数调制矩阵**：LFO/Envelope Follower + 可寻址参数映射。
7. **多通道支持**：引入 `HseAudioBus` 抽象，逐步把立体声 DSP 扩展到 N 通道。
8. **调制效果器**：Delay、Chorus、Flanger、Phaser、Tremolo。
9. **文件 I/O**：WAV 编解码入库，FLAC/MP3 作为可选适配。

### P2（增强）
10. 语音处理（AEC/ANS/VAD）、AI 推理接入、空间音频、合成器。
11. WASM/SIMD 加速、Worker 并行、背压降级、denormal 处理。
12. 用户功能：A/B、随机化、撤销/重做。

---

## 5. 本轮动手项

已落地 **P0-1/P0-2** ✅：
- `ProcessingStage` 增加可选 `reset()`；
- `HyperSoundEngine` 增加 `registerStage()` / `unregisterStage()` / `getStages()`；
- 新增测试验证：插入自定义 stage 后参与处理、可移除、reset 可调用；
- 文档已同步（API / ARCHITECTURE / GAP_ANALYSIS）。

已落地 **P1 四项** ✅：
- **Sidechain 输入**：`process()` 增加第三参数 `sidechain?`；Compressor/Deesser 支持 `sidechainEnabled` 用外部信号驱动包络/齿音检测（test/sidechain.test.ts）。
- **参数调制矩阵**：`dsp/modulation.ts` 提供 LFO（sine/triangle/square/saw）+ Envelope Follower，经 `ModulationMatrix` 路由到 masterGain / stereoWidth（test/modulation.test.ts）。
- **多通道 HseAudioBus**：`dsp/HseAudioBus.ts` 非交错 N 通道容器 + `downmixToStereo/writeStereo` + 引擎 `processBus()` 入口（test/audiobus.test.ts）。
- **调制类效果**：`dsp/ModEffects.ts` —— Delay / Chorus / Flanger / Phaser / Tremolo 五个效果器接入处理链（delay→chorus→flanger→phaser→tremolo，位于 NightMode 与 Reverb 之间）。
- 分享串（ShareCodec）已同步 `modEffects` 字段编解码。

已落地 **多通道深化 + 调制/Sidechain UI** ✅：
- **HseAudioBus 多通道工具**：`create/fromInterleaved/toInterleaved/copyTo/fill/applyGain/mixFrom/extract/downmixToMono`（test/audiobus.test.ts）。
- **processBus perChannelPair 模式**：按立体声对逐对独立处理（子引擎池，参数/复位与主引擎同步），支持 5.1/7.1 各通道独立 DSP；奇数通道复制 L 写回；sidechain 按对切片。
- **调制类效果 UI**：效果页新增 Delay/Chorus/Flanger/Phaser/Tremolo 五卡片 + 参数调制矩阵卡片与弹窗（`ui/modalsModulation.tsx`）。
- **Sidechain UI 开关**：Compressor/Deesser 弹窗新增外部 Sidechain 开关。

已落地 **WAV 文件 I/O** ✅：
- **WAV**：`src/io/wav.ts` `encodeWav/decodeWav`（16-bit PCM / 32-bit Float，多通道，legacy/standard 双模式；standard 严格 RIFF 校验）。

已落地 **算法创新与优化** ✅：
- **Convolver 非均匀分区卷积**：两级分区（512/4096），长 IR 每块耗时降约 77%，延迟语义不变（test/convolver.test.ts 新增非均匀用例）。
- **FFT 基-4**：N=1024/2048 提速 32-34%（基-2→基-4 蝶形，±j 免乘）。
- **ReverbSimple 内联 / Limiter 插值优化**：各提速约 17%/18%（逐位一致）。
- **FDN 混响**（`dsp/FdnReverb.ts`）：反馈延迟网络 + Householder 正交矩阵，引擎 `reverb.mode='fdn'`。
- **自适应动态均衡**（`dsp/DynamicEq.ts`）：全通交叉分带 + 频谱包络自动混音，引擎 `dynamicEq` 参数组。

下一步建议：**sample-accurate 参数自动化**、FLAC/Opus 解码。
