# Changelog

## [0.2.0] - 2026-08

### Added
- 独立引擎包 `hypersoundengine`（HyperSoundEngine v1 架构）。
- 核心/浏览器/Worklet 三个子路径导出。
- `AudioEngine` 接口新增 `getParams()` 与 `prepare(maxBlockSize)`。
- `ProcessingStage` 处理链抽象。
- 独立接入示例（Node 离线 / 浏览器 Host）。
- 接口文档（API / ARCHITECTURE / INTEGRATION）。
- WaveForge 适配层独立到 `adapters/waveforge/`。
- 性能基准脚本 `npm run benchmark` 与性能冒烟测试。
- GitHub Actions CI。
- 自定义处理阶段注册：`registerStage()` / `unregisterStage()` / `getStages()`。
- 差距分析文档 `docs/GAP_ANALYSIS.md`。
- Sidechain 输入：`process(inputs, outputs, sidechain?)` 第三参数，Compressor/Deesser 支持外部信号驱动包络（`sidechainEnabled`）。
- 参数调制矩阵：`dsp/modulation.ts`（LFO 四种波形 + Envelope Follower），路由到 masterGain / stereoWidth。
- 多通道 AudioBus：`dsp/AudioBus.ts` 非交错 N 通道缓冲抽象 + `processBus()` 便利入口（当前内核立体声，上下混兼容）。
- 调制类效果：`dsp/ModEffects.ts` —— Delay / Chorus / Flanger / Phaser / Tremolo 五个新处理阶段。
- 调制类效果 + Sidechain UI：效果页新增 延迟/合唱/镶边/移相/颤音 五卡片与参数调制矩阵卡片（`ui/modalsModulation.tsx`）；Compressor/Deesser 弹窗新增外部 Sidechain 开关。
- AudioBus 多通道工具：`create/fromInterleaved/toInterleaved/copyTo/fill/applyGain/mixFrom/extract/downmixToMono`。
- `processBus()` 新增 `perChannelPair` 模式：按立体声对逐对独立处理（子引擎池），支持 5.1/7.1 各通道独立 DSP。
- MIDI 事件接口 / MIDI Learn：`sendMidi(events)` 预分配环形队列 + `process()` 块头消费；`midiLearn(cc, target, opts?)` / `midiUnlearn(cc)` / `getMidiBindings()` / `getMidiDroppedCount()`；`AutomationTarget`（builtin masterGain/stereoWidth 或任意参数路径白名单）+ CC/Note → 范围映射 + 一阶平滑（防 zipper）。
- WAV 文件 I/O：`src/io/wav.ts` —— `encodeWav(channels, sampleRate, opts?)` / `decodeWav(buffer)`，支持 16-bit PCM 与 32-bit Float、多通道、严格 RIFF 校验（防注入）。
- UI MIDI Learn 面板：调音室新增 MIDI 页签（`ui/midiPanel.tsx`），参数路径下拉 + CC/Note 绑定 + 绑定表 + 测试发送；bridge 可选 `midi` 对象（HyperSoundEngine 后端探测填充）。
- **Convolver 非均匀分区卷积**：两级分区（短分区=partitionSize 默认 512 / 长分区默认 4096），长 IR 每块耗时降约 77%，延迟语义不变。
- **FFT 基-4 蝶形**：N=1024/2048 提速 32-34%（±j 免乘、stage 数减半，数值容差内一致）。
- **ReverbSimple 热循环内联**：14 次/样本方法调用消除，提速约 17%（逐位一致）。
- **Limiter 真峰值插值优化**：相位对称合并 + 全展开，提速约 18%（逐位一致）。
- **FDN 混响（算法创新）**：`dsp/FdnReverb.ts` —— 反馈延迟网络（Jot 1991），素数互质延迟线 + Householder 正交反馈矩阵（O(N) 快速应用，无条件稳定）；引擎 `reverb.mode='fdn'` 接线。
- **自适应动态均衡（算法创新）**：`dsp/DynamicEq.ts` —— 全通交叉分带（5 带，单位增益精确重建）+ 块级 RMS 分析 + 软拐点压缩 + attack/release 平滑；引擎 `dynamicEq` 参数组接线。

### Changed
- LICENSE 改为 CC BY-NC-ND 4.0。
- 引擎目录由 `waveforge-engine-v3` 重构为 `HyperSoundEngine`。
- 核心类名：`EngineV3` → `HyperSoundEngine`，`EngineV3Host` → `HyperSoundEngineHost`。
- Worklet 处理器名：`hypersoundengine`。
- DSP 内部去重：Deesser/BassEnhancer 共用 `dsp/biquad.ts`，Stretch 共用 `dsp/fft.ts`。
- `EqChain.processStereo` 改为块处理，减少每样本方法调用开销。
- 宿主/Worklet 在创建引擎时预分配工作缓冲。

### Fixed
- 保持 331 测试全绿。

## [0.1.0] - 2026-08

- 初始 WaveForge v3 引擎（历史版本，已重构为 HyperSoundEngine）。
