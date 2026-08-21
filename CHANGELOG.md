# Changelog

## [Unreleased]

### Changed
- **命名规范审计与整改**（规则见 `docs/VERSIONING.md` 公开标识符命名分层）：泛词类加 `Hse` 前缀并同步文件名——`AudioBus`→`HseAudioBus`、`Stretch`/`StretchParams`→`HseStretch`/`HseStretchParams`、`HearingTest`→`HseHearingTest`、`SeparationQueue`→`HseSeparationQueue`、`AudioEffectsProcessor`→`HseAudioEffectsProcessor`；DSP 行业域名类（Biquad/Compressor/Convolver 等）与工具函数（createEngine/encodeWav 等）按规则保留原名。
- **版本谱系重置**：废止旧 WaveForge v1/v2/v3 引擎谱系描述，现引擎定名 **HyperSoundEngine v1**（版本策略见 `docs/VERSIONING.md`）。
- 适配层 `attachV3Engine.ts` 重命名为 `attachEngine.ts`；导出 `attachV3Engine/detachV3Engine/getV3Bridge/isV3Attached/setV3SystemVolume/exportV3Wav/V3GraphHandle` 相应改为 `attachEngine/detachEngine/getBridge/isAttached/setSystemVolume/exportWav/EngineGraphHandle`。
- 调音室 UI 移除引擎版本切换入口（`engineVersion`/`onSwitchEngine` props 与 v1/v2/v3 切换器）。
- 命名去版本化：`v3HearingPlay`→`hseHearingPlay`、存储键 `hypersound:v3-*`/`waveforge:v3-params`→`hse-*`、worklet URL `v3-worklet*`→`hse-worklet*`、CSS 动画 `v3-*`→`hse-*`、WAV 导出文件名前缀 `waveforge-v3-`→`waveforge-hse-`。

### Added
- 版本策略文档 `docs/VERSIONING.md`（生成代号 ↔ semver 映射、bump 规则、向量纪律、命名规范）。
- `npm run benchmark:scenes`：接入场景化基准脚本（卷积/FDN 混响、DynamicEq；原 `scripts/benchmark-optimized.mjs` 无人引用，本次纳入 npm scripts）。

### Removed
- 死文件审计（入口可达性分析 54/55 全可达）：删除 `.hse-bench/` 实验脚手架（含优化前 `old/` 算法副本，结论已记录于本 CHANGELOG）；修正 integration 测试引用不存在的 `test/setup.ts` 的过时注释。

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
- 多通道 HseAudioBus：`dsp/HseAudioBus.ts` 非交错 N 通道缓冲抽象 + `processBus()` 便利入口（当前内核立体声，上下混兼容）。
- 调制类效果：`dsp/ModEffects.ts` —— Delay / Chorus / Flanger / Phaser / Tremolo 五个新处理阶段。
- 调制类效果 + Sidechain UI：效果页新增 延迟/合唱/镶边/移相/颤音 五卡片与参数调制矩阵卡片（`ui/modalsModulation.tsx`）；Compressor/Deesser 弹窗新增外部 Sidechain 开关。
- HseAudioBus 多通道工具：`create/fromInterleaved/toInterleaved/copyTo/fill/applyGain/mixFrom/extract/downmixToMono`。
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
- 引擎目录重构为 `HyperSoundEngine`。
- 核心类名统一为 `HyperSoundEngine` / `HyperSoundEngineHost`。
- Worklet 处理器名：`hypersoundengine`。
- DSP 内部去重：Deesser/BassEnhancer 共用 `dsp/biquad.ts`，HseStretch 共用 `dsp/fft.ts`。
- `EqChain.processStereo` 改为块处理，减少每样本方法调用开销。
- 宿主/Worklet 在创建引擎时预分配工作缓冲。

### Fixed
- 保持 331 测试全绿。

## [0.1.0] - 2026-08

