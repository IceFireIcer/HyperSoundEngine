/**
 * HyperSoundEngine v1 独立音频引擎 —— 公共出口（Core）
 *
 * 本入口只包含纯 TypeScript 核心，零 DOM / AudioContext / React 依赖：
 *   - 类型与默认参数（types.ts）
 *   - 全部 DSP 模块（dsp/）
 *   - 引擎总成 HyperSoundEngine 与工厂（engine/）
 *   - 场景预设、分享串（engine/）
 *   - 频谱分析、听力分析（analysis/）
 *   - 声源分离任务队列（offline/）
 *
 * 浏览器宿主（HyperSoundEngineHost / AudioWorklet 接线）请从子路径导入：
 *   - `hypersoundengine/browser`  → 浏览器音频图宿主
 *   - `hypersoundengine/worklet`  → AudioWorklet 处理器打包入口
 *
 * WaveForge 专属适配代码位于仓库 `adapters/waveforge/`，不属于本核心包。
 */

/** AudioWorklet 处理器注册名（与 worklet/HseAudioEffectsProcessor.ts 中常量一致） */
export const WORKLET_PROCESSOR_NAME = 'hypersoundengine'

export * from './types'
export { HyperSoundEngine } from './engine/HyperSoundEngine'
export { createEngine, createHyperSoundEngine } from './engine/factory'
export type { AudioEngine, AudioEngineFactory, StereoProcessor, ProcessingStage } from './interfaces'
export { SCENE_PRESETS, getSceneById, SCENE_IDS } from './engine/ScenePresets'
export { encodeShareCode, decodeShareCode, SHARE_CODEC_VERSION } from './engine/ShareCodec'
export { SpectrumAnalyzer } from './analysis/Spectrum'
export { HseHearingTest } from './analysis/HseHearingTest'
export type { AudiogramPoint } from './analysis/HseHearingTest'
export { HseSeparationQueue, OnnxStemSeparator, DEFAULT_STEMS } from './offline/Separator'
export type { SeparationStem, StemSeparatorAdapter, SeparationTask } from './offline/Separator'

// —— WAV 文件 I/O（核心包内置；WaveForge 适配层复用） ——
export { encodeWav, decodeWav } from './io/wav'
export type { WavContainerFormat, WavDecodeResult, WavEncodeOptions } from './io/wav'

export { computeRelativeDirection, wrapAzimuthDeg } from './spatial/controller'
export type { RelativeDirection } from './spatial/controller'
export type { Vec3, WorldListenerPose } from './spatial/types'

// —— dsp 全部模块 ——
export * from './dsp/fft'
export * from './dsp/biquad'
export * from './dsp/EqChain'
export * from './dsp/MidSide'
export * from './dsp/Deesser'
export * from './dsp/Compressor'
export * from './dsp/Limiter'
export * from './dsp/BassEnhancer'
export * from './dsp/Convolver'
export * from './dsp/ReverbSimple'
export * from './dsp/LufsMeter'
export * from './dsp/LoudnessComp'
export * from './dsp/Resampler'
export * from './dsp/HseStretch'
export * from './dsp/PitchYin'
export * from './dsp/modulation'
export * from './dsp/HseAudioBus'
export * from './dsp/ModEffects'
export * from './dsp/FdnReverb'
export * from './dsp/DynamicEq'
export {
  computeFeatures,
  computeRms,
  computeZcr,
  spectralCentroid,
  spectralRolloff,
  spectralFlatness,
  spectralCrest,
} from './dsp/features'
export type { FeatureInput } from './dsp/features'
