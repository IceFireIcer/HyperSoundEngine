/**
 * HyperSoundEngine v1 —— 引擎总成（HyperSoundEngine）
 *
 * 出处/许可：
 *  - 链式架构与参数模型：本项目《音频算法设计文档.md》§2 总体架构（自研）；
 *  - 链内各 DSP 模块（EqChain/MidSide/Deesser/Compressor/Limiter/BassEnhancer/
 *    Convolver/ReverbSimple/LufsMeter/LoudnessComp/HseStretch/FFT/features）
 *    的概念来源与许可见各自源文件头部注释（RBJ Cookbook / DSPFilters(MIT) /
 *    kissfft(BSD-3) / stk FreeVerb(MIT) / ITU-R BS.1770 / ISO 226 等）；
 *  - 智能均衡 IEQ（Post）为本文件内置实现，思路参考技术文档 §1.4（自研）；
 *  - 夜间模式（压缩增强 + 6kHz 高频衰减）为本项目历史功能（自研）。
 *
 * 处理链（顺序固定，见 API_SPEC 辅助模块 A）：
 *   输入 → 响度归一化增益 → 3D 环绕(轻量立体声旋转) → M/S(width + voiceBalance，可被调制矩阵驱动)
 *   → Pre-EQ(用户 EQ) → Deesser(可选 sidechain) → Compressor(可选 sidechain) → NightMode
 *   → Delay → Chorus → Flanger → Phaser → Tremolo
 *   → 混响(卷积|算法|off 三路路由) → BassEnhancer → LoudnessComp
 *   → IEQ(Post) → [FFT 取样点] → [LUFS 采样点] → 调制主增益 → Limiter
 *   → 空间音频(内联级，mode:'off' 默认逐位旁路) → 输出
 *
 * 说明：
 *  - LUFS 采样点严格位于 Limiter 之前（API_SPEC 要求），测的是压限前的节目响度；
 *  - getAnalysis 的内部 2048 点 FFT 取样于 LoudnessComp 之后（即 IEQ 输入处，
 *    等价地也位于 Limiter 之前），每累计 2048 样本更新一次；
 *  - HseStretch（变速/变调）不内联进主链，仅经 getStretch() 供 gapless/过渡场景调用；
 *  - process() 内零分配：工作缓冲按需惰性扩容，稳态无分配；分析路径复用预分配缓冲；
 *  - 确定性：同输入同参数必同输出（无随机、无 Date、无 console）。
 */

import type {
  HyperSoundEngineParams,
  EngineStats,
  EngineAnalysis,
  EqBand,
  SpectralFeatures,
  CompressorSettings,
  ReverbSettings,
  IeqTargetCurve,
} from '../types'
import type { AudioEngine, ProcessingStage } from '../interfaces'
import { SIMPLE_EQ_FREQUENCIES, createDefaultParams } from '../types'
import { EqChain } from '../dsp/EqChain'
import { MidSide } from '../dsp/MidSide'
import { Deesser } from '../dsp/Deesser'
import { Compressor } from '../dsp/Compressor'
import { Limiter } from '../dsp/Limiter'
import { BassEnhancer } from '../dsp/BassEnhancer'
import { Convolver } from '../dsp/Convolver'
import { ReverbSimple } from '../dsp/ReverbSimple'
import { FdnReverb } from '../dsp/FdnReverb'
import { DynamicEq } from '../dsp/DynamicEq'
import { LufsMeter } from '../dsp/LufsMeter'
import { LoudnessComp } from '../dsp/LoudnessComp'
import { Biquad } from '../dsp/biquad'
import { HseStretch } from '../dsp/HseStretch'
import { ModulationMatrix } from '../dsp/modulation'
import { HseAudioBus } from '../dsp/HseAudioBus'
import { DelayEffect, ChorusEffect, FlangerEffect, PhaserEffect, TremoloEffect } from '../dsp/ModEffects'
import { fft, hannWindow, frequencyBins } from '../dsp/fft'
import {
  computeRms,
  computeZcr,
  spectralCentroid,
  spectralRolloff,
  spectralFlatness,
  spectralCrest,
} from '../dsp/features'
// —— 空间音频（内联级；纯 TS TsConvolverBackend + 合成 HRTF 兜底网格，无浏览器依赖） ——
import { TsConvolverBackend } from '../spatial/TsConvolverBackend'
import { generateAnalyticHrtfGrid } from '../spatial/analyticHrtf'
import { instantSpeakers } from '../spatial/types'
import { headLockedSpeakers, multichannelSpeakers } from '../spatial/layouts'
import { stageSpeakers, stageRoom } from '../spatial/scenes'
import { computeRelativeDirection, computeTrajectoryPosition, computeWorldVelocity } from '../spatial/controller'
import { AmbienceRenderer } from '../spatial/ambienceMixer'
import type {
  VirtualSpeaker,
  VirtualSpeakerCfg,
  SpeakerRoute,
  WorldSettings,
  SpatialRenderConfig,
} from '../spatial/types'
import type { SpatialSettings } from '../types'

/** 引擎内部 FFT 分析窗长（2 的幂，N/2+1 = 1025 个 bin） */
const ANALYSIS_WINDOW = 2048
/** Pre-EQ 级联最大段数（EqChain 默认 20 段） */
const MAX_PRE_EQ_BANDS = 20
/** IEQ（Post）内部参数 EQ 段数（1 倍频程 10 段） */
const IEQ_BAND_COUNT = 10
/** IEQ 控制频率（1 倍频程，对齐专业 10 段） */
const IEQ_FREQS = [31.5, 63, 125, 250, 500, 1000, 2000, 4000, 8000, 16000]
/** DynamicEq 固定 5 带交叉频率（与 dsp/DynamicEq.ts 默认一致） */
const DYNAMIC_EQ_CROSSOVERS = [200, 800, 2500, 8000]
/** 响度归一化实时增益平滑时间常数（秒），防抽吸（技术文档 §7.2 慢速 AGC） */
const NORM_SMOOTH_SEC = 3.0
/** 响度归一化手动增益平滑时间常数（秒）：externalGainDb（外部分析换算/用户拖动）
 *  语义下需及时跟随且无 zipper；实时 AGC 分支仍用 NORM_SMOOTH_SEC 防抽吸。 */
const MANUAL_GAIN_SMOOTH_SEC = 0.08

interface HyperSoundEngineInternalOptions {
  /** 仅供冻结旧向量重放：内置 DSP 在短尾块按工作缓冲容量推进。默认关闭。 */
  legacyPaddedTail?: boolean
}

/** 深拷贝参数快照：数组逐元素复制，避免外部可变对象影响引擎；引擎本身不修改传入参数。 */
function cloneParams(p: HyperSoundEngineParams): HyperSoundEngineParams {
  return {
    ...p,
    eq: {
      ...p.eq,
      simpleBands: p.eq.simpleBands.slice(),
      proBands: p.eq.proBands.map((b) => ({ frequency: b.frequency, gain: b.gain, q: b.q })),
    },
    deesser: { ...p.deesser },
    compressor: { ...p.compressor },
    nightMode: { ...p.nightMode },
    bassEnhancer: { ...p.bassEnhancer },
    reverb: {
      ...p.reverb,
      algorithmic: { ...p.reverb.algorithmic },
      convolution: { ...p.reverb.convolution },
    },
    surround3d: { ...p.surround3d },
    loudnessCompensation: {
      ...p.loudnessCompensation,
      bands: p.loudnessCompensation.bands.map((b) => ({ frequency: b.frequency, gain: b.gain })),
    },
    loudnessNormalization: { ...p.loudnessNormalization },
    limiter: { ...p.limiter },
    ieq: { ...p.ieq },
    pitch: { ...p.pitch },
    modulation: {
      ...p.modulation,
      lfo: { ...p.modulation.lfo },
      envelope: { ...p.modulation.envelope },
      routes: p.modulation.routes.map((r) => ({ ...r })),
    },
    modEffects: {
      delay: { ...p.modEffects.delay },
      chorus: { ...p.modEffects.chorus },
      flanger: { ...p.modEffects.flanger },
      phaser: { ...p.modEffects.phaser },
      tremolo: { ...p.modEffects.tremolo },
    },
    hearing: { ...p.hearing },
    spatial: p.spatial ? cloneSpatial(p.spatial) : undefined,
  }
}

/** 深拷贝空间音频设置（数组/嵌套对象逐层复制；无 Float32Array，plain data） */
function cloneSpatial(s: SpatialSettings): SpatialSettings {
  return {
    ...s,
    instant: { ...s.instant },
    headLocked: {
      ...s.headLocked,
      speakers: s.headLocked.speakers.map((sp) => ({ ...sp })),
      routes: s.headLocked.routes.slice(),
    },
    world: {
      ...s.world,
      listener: { ...s.world.listener, position: { ...s.world.listener.position } },
      sources: s.world.sources.map((src) => ({ ...src, position: { ...src.position } })),
      trajectories: s.world.trajectories.map((t) => ({
        sourceId: t.sourceId,
        keyframes: t.keyframes.map((k) => ({ t: k.t, position: { ...k.position } })),
      })),
    },
    stage: {
      ...s.stage,
      customSources: s.stage.customSources.map((src) => ({ ...src, position: { ...src.position } })),
    },
    ambience: { ...s.ambience },
  }
}

// ==================== 空间音频配置推导（内联级） ====================
// 复用 spatial/ 纯模块（layouts/scenes/controller），把 SpatialSettings 投影为后端渲染配置。
// 差异（立体声内核）：① 无 output 分支；② hrtfInterp 直接由 settings 给出；
// ③ instant.multichannelAuto 退化为 instantSpeakers（输入恒 2 声道）；
// ④ ambience 由内联级的 AmbienceRenderer 叠加，使用调用方持有缓冲与有效帧数。

/** 扬声器方位角 → 输入声道索引（az≤0→左源 0、az>0→右源 1） */
function headLockedChannel(azimuthDeg: number): number {
  return azimuthDeg <= 0 ? 0 : 1
}

/** 模式 B 单只扬声器按路由展开（'l'→0、'r'→1、'both'→两只半增益、undefined→就近） */
function routeSpeaker(cfg: VirtualSpeakerCfg, route: SpeakerRoute | undefined): VirtualSpeaker[] {
  const base = {
    azimuthDeg: cfg.azimuthDeg,
    elevationDeg: cfg.elevationDeg,
    distance: cfg.distance,
    gain: cfg.gain,
    size: cfg.size,
  }
  if (route === 'both') {
    return [
      { ...base, channel: 0, gain: cfg.gain * 0.5 },
      { ...base, channel: 1, gain: cfg.gain * 0.5 },
    ]
  }
  const channel = route === 'r' ? 1 : route === 'l' ? 0 : headLockedChannel(cfg.azimuthDeg)
  return [{ ...base, channel }]
}

/** 模式 C 声源轨迹查询（轨迹优先，无匹配→null 回退静态位置） */
function trajectoryPosition(world: WorldSettings, sourceId: string): { x: number; y: number; z: number } | null {
  const traj = world.trajectories.find((t) => t.sourceId === sourceId)
  if (!traj) return null
  return computeTrajectoryPosition(traj.keyframes, world.playhead)
}

/** SpatialSettings → 虚拟扬声器列表 */
function speakersFromSettings(s: SpatialSettings): VirtualSpeaker[] {
  if (s.mode === 'instant') {
    // 立体声内核（2 声道）：多声道自动映射无意义 → 常规立体声对
    return instantSpeakers(s.instant)
  }
  if (s.mode === 'headLocked') {
    const routes = s.headLocked.routes
    return headLockedSpeakers(s.headLocked).flatMap((cfg, i) =>
      routeSpeaker(cfg.muted ? { ...cfg, gain: 0 } : cfg, i < routes.length ? routes[i] : undefined),
    )
  }
  if (s.mode === 'stage') {
    const custom = s.stage.customSources.map((src) => {
      const rel = computeRelativeDirection(
        { position: { x: 0, y: 1.6, z: 0 }, yaw: 0 },
        src.position,
      )
      return {
        channel: headLockedChannel(rel.azimuthDeg),
        azimuthDeg: rel.azimuthDeg,
        elevationDeg: rel.elevationDeg,
        distance: rel.distance,
        gain: src.gain,
        size: src.size,
      }
    })
    return [
      ...stageSpeakers(s.stage).map((cfg) => ({
        channel: headLockedChannel(cfg.azimuthDeg),
        azimuthDeg: cfg.azimuthDeg,
        elevationDeg: cfg.elevationDeg,
        distance: cfg.distance,
        gain: cfg.gain,
        size: cfg.size,
      })),
      ...custom,
    ]
  }
  if (s.mode === 'world') {
    return s.world.sources.map((src) => {
      const pos = trajectoryPosition(s.world, src.id) ?? src.position
      const rel = computeRelativeDirection(s.world.listener, pos)
      return {
        channel: headLockedChannel(rel.azimuthDeg),
        azimuthDeg: rel.azimuthDeg,
        elevationDeg: rel.elevationDeg,
        distance: rel.distance,
        gain: src.gain,
        size: src.size,
      }
    })
  }
  return []
}

/**
 * SpatialSettings → 后端渲染配置。
 * stage 模式 room/roomAmount 取场景预设（与 instant 全局房间解耦）；wet/dry amount
 * 恒取 instant.amount（空间化强度全局由 instant.amount 控制）。
 * world 模式透传遮挡量，并由相邻 listener/playhead 参数快照推导确定性多普勒速度。
 */
function spatialConfigFromSettings(
  s: SpatialSettings,
  dopplerVelocity?: { x: number; y: number; z: number },
  inputChannelCount = 2,
): SpatialRenderConfig {
  const stageActive = s.mode === 'stage'
  const speakers = s.mode === 'instant' && s.instant.multichannelAuto && inputChannelCount > 2
    ? multichannelSpeakers(inputChannelCount)
    : speakersFromSettings(s)
  return {
    speakers,
    room: stageActive ? stageRoom(s.stage) : s.instant.room,
    roomAmount: stageActive ? s.stage.reverbAmount : s.instant.roomAmount,
    roomSizeScale: stageActive ? s.stage.roomSize : 1,
    amount: s.instant.amount,
    distanceModel: s.distanceModel ?? 'inverse',
    refDistance: s.refDistance,
    maxDistance: s.maxDistance,
    hrtfInterp: s.hrtfInterp,
    convolution: s.convolution,
    masterGain: s.masterGain,
    occlusionAmount: s.mode === 'world' ? s.world.occlusion : undefined,
    dopplerVelocity: s.mode === 'world' ? dopplerVelocity : undefined,
    ambienceAmount: s.ambience.enabled ? s.ambience.amount : 0,
  }
}

export class HyperSoundEngine implements AudioEngine {
  private readonly _fs: number
  private readonly _channels: number
  private readonly _legacyPaddedTail: boolean
  private _params: HyperSoundEngineParams

  // —— 链上 DSP 模块（构造时固定采样率，setParams 只重算系数） ——
  private readonly _eqChain: EqChain
  private readonly _midSide: MidSide
  private readonly _deesser: Deesser
  private readonly _compressor: Compressor
  private readonly _limiter: Limiter
  private readonly _bass: BassEnhancer
  private _convolver: Convolver // 非 readonly：dePeriodize 选项变化时重建（死参数修复）
  private _convolverDePeriodize = true
  private readonly _reverbSimple: ReverbSimple
  private readonly _fdnReverb: FdnReverb
  private _useFdn = false
  private readonly _lufs: LufsMeter
  private readonly _loudnessComp: LoudnessComp
  private readonly _stretch: HseStretch
  private readonly _modMatrix: ModulationMatrix
  private _modMasterGain = 1
  private _modStereoWidth = 1
  private readonly _modulationResult = { masterGain: 1, stereoWidth: 1 }
  private readonly _delay: DelayEffect
  private readonly _chorus: ChorusEffect
  private readonly _flanger: FlangerEffect
  private readonly _phaser: PhaserEffect
  private readonly _tremolo: TremoloEffect

  // —— 多通道逐对处理子引擎池（processBus perChannelPair；懒创建，setParams/reset 同步） ——
  private readonly _pairEngines: HyperSoundEngine[] = []

  // —— 夜间模式（压缩增强 + 6kHz 高频 shelf） ——
  private readonly _nightCompressor: Compressor
  private readonly _nightShelfL: Biquad
  private readonly _nightShelfR: Biquad
  private _nightActive = false

  // —— IEQ（Post）：内部实现，参考技术文档 §1.4 ——
  private readonly _ieqChain: EqChain
  private readonly _dynamicEq: DynamicEq
  private _ieqActive = false
  private _ieqStrength = 0.5
  private _ieqSmooth = 0.01
  private readonly _ieqGains = new Float32Array(IEQ_BAND_COUNT)
  private readonly _ieqLevels = new Float32Array(IEQ_BAND_COUNT)
  private readonly _ieqBands: EqBand[] = []
  private readonly _ieqZeroBands: EqBand[] = []
  private _ieqTargets: number[] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
  private readonly _ieqBinRanges: Array<[number, number]> = []

  // —— 分析路径（2048 点 FFT，每累计一窗更新一次） ——
  private readonly _ring: Float32Array
  private _ringPos = 0
  private _analysisPos = 0
  private _analysisReady = false
  private readonly _timeBuf: Float32Array
  private readonly _real: Float32Array
  private readonly _imag: Float32Array
  private readonly _magBuf: Float32Array
  private readonly _hann: Float32Array
  private readonly _binFreqs: Float32Array
  private readonly _featCache: SpectralFeatures

  // —— 工作缓冲（惰性扩容，稳态零分配） ——
  private _workL = new Float32Array(0)
  private _workR = new Float32Array(0)
  private _sideL = new Float32Array(0)
  private _sideR = new Float32Array(0)
  private _sidechainActive = false

  // —— 处理链（20 级，顺序即数组顺序） ——
  private readonly _stages: ProcessingStage[] = []

  // —— 运行时状态 ——
  private _preEqActive = false
  private _useConvolver = false
  private _loadedIr: Float32Array | null = null
  private _normGain = 1
  private _surroundPhase = 0

  // —— 空间音频（内联级；TsConvolverBackend + 合成 HRTF 兜底网格） ——
  private readonly _spatialBackend: TsConvolverBackend
  private readonly _ambienceRenderer: AmbienceRenderer
  private _spatialActive = false
  private _ambienceAmount = 0
  private _worldHistoryValid = false
  private _worldPrevPlayhead = 0
  private _worldPrevX = 0
  private _worldPrevY = 0
  private _worldPrevZ = 0
  /** spatial 配置变更签名（JSON）——仅当 settings 实际变化时才 setConfig，避免非空间参数
   *  变更触发后端 resample/decorr 状态清零（fill(0)）造成咔哒声 */
  private _spatialCfgKey = ''
  private _spatialOutL = new Float32Array(0)
  private _spatialOutR = new Float32Array(0)
  /** 多声道空间渲染使用的预分配输入引用视图；每块只更新元素。 */
  private readonly _spatialInputs: Float32Array[]
  private _preparedCapacity = 0

  constructor(sampleRate: number, channelCount = 2, options?: HyperSoundEngineInternalOptions) {
    if (!Number.isFinite(sampleRate) || sampleRate <= 0) {
      throw new Error('invalid sample rate')
    }
    this._fs = sampleRate
    this._channels = channelCount > 0 ? channelCount : 2
    this._spatialInputs = new Array<Float32Array>(this._channels)
    this._legacyPaddedTail = options?.legacyPaddedTail === true

    this._eqChain = new EqChain(sampleRate, MAX_PRE_EQ_BANDS)
    this._midSide = new MidSide()
    this._deesser = new Deesser(sampleRate)
    this._compressor = new Compressor(sampleRate)
    this._limiter = new Limiter(sampleRate)
    this._bass = new BassEnhancer(sampleRate)
    this._convolver = new Convolver(sampleRate)
    this._reverbSimple = new ReverbSimple(sampleRate)
    this._fdnReverb = new FdnReverb(sampleRate)
    this._lufs = new LufsMeter(sampleRate)
    this._loudnessComp = new LoudnessComp(sampleRate)
    this._stretch = new HseStretch(sampleRate, 2)
    this._modMatrix = new ModulationMatrix(sampleRate)
    this._delay = new DelayEffect(sampleRate)
    this._chorus = new ChorusEffect(sampleRate)
    this._flanger = new FlangerEffect(sampleRate)
    this._phaser = new PhaserEffect(sampleRate)
    this._tremolo = new TremoloEffect(sampleRate)

    this._nightCompressor = new Compressor(sampleRate)
    this._nightShelfL = new Biquad('highshelf', 6000, 0.707, 0, sampleRate)
    this._nightShelfR = new Biquad('highshelf', 6000, 0.707, 0, sampleRate)

    this._ieqChain = new EqChain(sampleRate, IEQ_BAND_COUNT)
    this._dynamicEq = new DynamicEq(sampleRate)
    for (let i = 0; i < IEQ_BAND_COUNT; i++) {
      this._ieqBands.push({ frequency: IEQ_FREQS[i], gain: 0, q: 1.1 })
      this._ieqZeroBands.push({ frequency: IEQ_FREQS[i], gain: 0, q: 1.1 })
    }
    // 预计算各频段的 bin 范围（相邻中心频率几何中点作为边界）
    const binHz = sampleRate / ANALYSIS_WINDOW
    for (let i = 0; i < IEQ_BAND_COUNT; i++) {
      const loEdge = i === 0 ? 20 : Math.sqrt(IEQ_FREQS[i - 1] * IEQ_FREQS[i])
      const hiEdge =
        i === IEQ_BAND_COUNT - 1 ? sampleRate / 2 : Math.sqrt(IEQ_FREQS[i] * IEQ_FREQS[i + 1])
      const lo = Math.max(0, Math.floor(loEdge / binHz))
      const hi = Math.min(ANALYSIS_WINDOW / 2, Math.ceil(hiEdge / binHz))
      this._ieqBinRanges.push([lo, hi])
    }

    this._ring = new Float32Array(ANALYSIS_WINDOW)
    this._timeBuf = new Float32Array(ANALYSIS_WINDOW)
    this._real = new Float32Array(ANALYSIS_WINDOW)
    this._imag = new Float32Array(ANALYSIS_WINDOW)
    this._magBuf = new Float32Array(ANALYSIS_WINDOW / 2 + 1)
    this._hann = hannWindow(ANALYSIS_WINDOW)
    this._binFreqs = frequencyBins(ANALYSIS_WINDOW, sampleRate)
    this._featCache = { rms: 0, zcr: 0, centroidHz: 0, rolloffHz: 0, flatness: 0, crest: 0 }

    // 空间音频后端（纯 TS TsConvolverBackend）+ 合成 HRTF 兜底网格
    // （KEMAR 实测网格后续可运行时经 loadHrtf 重载 + _spatialCfgKey='' 强制重下；首启用合成网格保证可用）
    this._spatialBackend = new TsConvolverBackend()
    this._spatialBackend.loadHrtf(generateAnalyticHrtfGrid(sampleRate))
    this._ambienceRenderer = new AmbienceRenderer(sampleRate)

    // 初始快照：默认参数
    this._params = createDefaultParams(sampleRate)
    this.buildStages()
    this.setParams(this._params)
  }

  /** 参数更新：重算所有模块系数（即时生效）。不修改传入的 p。 */
  setParams(p: HyperSoundEngineParams): void {
    // 旁通→重新启用检测（旧快照 vs 新参数）：重新启用的级在下方清空流状态，
    // 避免旁通窗口积压在延迟线/卷积缓冲/包络里的旧音频被回放（爆音/串音）
    const prev = {
      eq: this._preEqActive,
      deesser: this._params.deesser.enabled,
      compressor: this._params.compressor.enabled,
      night: this._nightActive,
      delay: this._params.modEffects.delay.enabled,
      chorus: this._params.modEffects.chorus.enabled,
      flanger: this._params.modEffects.flanger.enabled,
      phaser: this._params.modEffects.phaser.enabled,
      tremolo: this._params.modEffects.tremolo.enabled,
      reverb: this._params.reverb.enabled && this._params.reverb.mode !== 'off',
      bass: this._params.bassEnhancer.enabled,
      loudnessComp: this._params.loudnessCompensation.enabled,
      ieq: this._ieqActive,
      dynamicEq: this._params.dynamicEq.enabled,
      limiter: this._params.limiter.enabled,
    }
    this._params = cloneParams(p)
    const p2 = this._params

    // —— Pre-EQ：用户 EQ（simple/pro）——
    const bands = this.buildPreEqBands(p2)
    this._eqChain.setBands(bands)
    this._eqChain.setQCompensation(p2.eq.qCompensation)
    // Pre-EQ 仅由用户 EQ 开关控制（设备档案已并入 LoudnessComp 音量补偿）
    this._preEqActive = p2.eq.enabled

    // —— Deesser / Compressor ——
    this._deesser.setParams(p2.deesser)
    this._compressor.setParams(p2.compressor)

    // —— NightMode：压缩增强(ratio×1.5, threshold−6dB) + 6kHz 高频 shelf 衰减(amount×1.5dB) ——
    const nm = p2.nightMode
    this._nightActive = nm.enabled && nm.amount > 0
    if (this._nightActive) {
      const k = nm.amount / 10 // 强度 0..1
      const base = p2.compressor
      const night: CompressorSettings = {
        enabled: true,
        thresholdDb: base.thresholdDb - 6 * k,
        ratio: Math.max(1, base.ratio * (1 + 0.5 * k)), // 满强度时 ratio×1.5
        kneeDb: base.kneeDb,
        attackMs: base.attackMs,
        releaseMs: base.releaseMs,
        makeupDb: base.makeupDb,
        outputGain: 1,
        sidechainEnabled: base.sidechainEnabled,
      }
      this._nightCompressor.setParams(night)
      const shelfGainDb = -1.5 * nm.amount // 衰减 amount×1.5 dB
      this._nightShelfL.setParams('highshelf', 6000, 0.707, shelfGainDb)
      this._nightShelfR.setParams('highshelf', 6000, 0.707, shelfGainDb)
    }

    // —— 混响三路路由：convolution | algorithmic | off ——
    this.configureReverb(p2.reverb)

    // —— BassEnhancer / LoudnessComp / Limiter ——
    this._bass.setParams(p2.bassEnhancer)
    this._loudnessComp.setParams(p2.loudnessCompensation)
    this._limiter.setParams(p2.limiter)

    // —— IEQ（Post）配置 ——
    this._ieqActive = p2.ieq.enabled
    this._ieqStrength = p2.ieq.strength
    this._ieqTargets = this.ieqTargetCurve(p2.ieq.targetCurve)
    this._dynamicEq.setParams({
      enabled: p2.dynamicEq.enabled,
      strength: p2.dynamicEq.strength,
      thresholdDb: p2.dynamicEq.thresholdDb,
      ratio: p2.dynamicEq.ratio,
      attackMs: p2.dynamicEq.attackMs,
      releaseMs: p2.dynamicEq.releaseMs,
      bands: p2.dynamicEq.bands.map((b, i) => ({
        enabled: b.enabled,
        frequency: DYNAMIC_EQ_CROSSOVERS[i] ?? 0,
        targetGainDb: b.targetGainDb,
      })),
    })
    // 增益平滑系数：α = 1 − exp(−分析间隔/时间常数)，时间常数默认 3s 防抽吸
    const intervalSec = ANALYSIS_WINDOW / this._fs
    this._ieqSmooth = 1 - Math.exp(-intervalSec / Math.max(0.1, p2.ieq.timeConstantSec))
    if (!this._ieqActive) {
      this._ieqGains.fill(0)
      this._ieqChain.setBands(this._ieqZeroBands)
    }

    // —— 调制矩阵 ——
    const mod = p2.modulation
    this._modMatrix.setRoutes(mod.routes)
    this._modMatrix.setLfoParams(mod.lfo.shape, mod.lfo.rateHz, mod.lfo.depth)
    this._modMatrix.setEnvelopeParams(mod.envelope.attackMs, mod.envelope.releaseMs, mod.envelope.amount)

    // —— 调制类效果 ——
    const me = p2.modEffects
    this._delay.setParams(me.delay)
    this._chorus.setParams(me.chorus)
    this._flanger.setParams(me.flanger)
    this._phaser.setParams(me.phaser)
    this._tremolo.setParams(me.tremolo)

    // —— 响度归一化 ——
    if (!p2.loudnessNormalization.enabled) {
      this._normGain = 1
    }

    // —— 多通道子引擎参数同步（perChannelPair 复用同一套参数） ——
    for (const e of this._pairEngines) e.setParams(p2)

    // —— 重新启用级的流状态清空（检测见本方法顶部 prev 快照） ——
    if (!prev.eq && this._preEqActive) this._eqChain.reset()
    if (!prev.deesser && p2.deesser.enabled) this._deesser.reset()
    if (!prev.compressor && p2.compressor.enabled) this._compressor.reset()
    if (!prev.night && this._nightActive) {
      this._nightCompressor.reset()
      this._nightShelfL.reset()
      this._nightShelfR.reset()
    }
    if (!prev.delay && me.delay.enabled) this._delay.reset()
    if (!prev.chorus && me.chorus.enabled) this._chorus.reset()
    if (!prev.flanger && me.flanger.enabled) this._flanger.reset()
    if (!prev.phaser && me.phaser.enabled) this._phaser.reset()
    if (!prev.tremolo && me.tremolo.enabled) this._tremolo.reset()
    if (!prev.reverb && p2.reverb.enabled && p2.reverb.mode !== 'off') {
      if (this._useConvolver) this._convolver.reset()
      else if (this._useFdn) this._fdnReverb.reset()
      else this._reverbSimple.reset()
    }
    if (!prev.bass && p2.bassEnhancer.enabled) this._bass.reset()
    if (!prev.loudnessComp && p2.loudnessCompensation.enabled) this._loudnessComp.reset()
    if (!prev.ieq && this._ieqActive) this._ieqChain.reset()
    if (!prev.dynamicEq && p2.dynamicEq.enabled) this._dynamicEq.reset()
    if (!prev.limiter && p2.limiter.enabled) this._limiter.reset()

    // —— 空间音频配置同步（内联级；仅当 SpatialSettings 实际变化时 setConfig，
    //    避免非空间参数变更触发后端状态清零）——mode='off' 或缺省 spatial 时旁路。
    //    合成 HRTF 网格随采样率固定（ctor 装载），setConfig 复用；若需换 KEMAR 网格，
    //    另设 _spatialBackend.loadHrtf + _spatialCfgKey='' 强制重下。
    //    刚从 off→on（_spatialActive 之前为 false）时先 reset() 后端流式状态：
    //    空间音频关闭期间后端不被调用，dryLine/卷积历史残留上次会话内容，若不清空，
    //    重新启用瞬间会从 dryLine 吐出 ~512 样本旧音频（爆音/串音）。reset 后前 11ms
    //    （512 样本）静音是 dryLine/湿路对齐的固有起播空白，可接受（非突降）。
    const sp = p2.spatial
    const spatialActive = !!sp && sp.mode !== 'off'
    const wasSpatialActive = this._spatialActive
    this._spatialActive = spatialActive
    let dopplerVelocity: { x: number; y: number; z: number } | undefined
    if (sp?.mode === 'world') {
      const listener = sp.world.listener.position
      const playhead = sp.world.playhead
      dopplerVelocity = computeWorldVelocity(
        this._worldHistoryValid
          ? {
              position: { x: this._worldPrevX, y: this._worldPrevY, z: this._worldPrevZ },
              playhead: this._worldPrevPlayhead,
            }
          : null,
        { position: listener, playhead },
      )
      this._worldHistoryValid = true
      this._worldPrevPlayhead = playhead
      this._worldPrevX = listener.x
      this._worldPrevY = listener.y
      this._worldPrevZ = listener.z
    } else {
      this._worldHistoryValid = false
    }
    const wasAmbienceActive = this._ambienceAmount > 0
    this._ambienceAmount = spatialActive && sp?.ambience.enabled
      ? Math.min(1, Math.max(0, sp.ambience.amount))
      : 0
    if (!wasAmbienceActive && this._ambienceAmount > 0) this._ambienceRenderer.reset()
    if (spatialActive && sp) {
      const key = JSON.stringify(sp)
      if (key !== this._spatialCfgKey) {
        if (!wasSpatialActive) {
          this._spatialBackend.reset()
          this._ambienceRenderer.reset()
        }
        this._spatialBackend.setConfig(spatialConfigFromSettings(sp, dopplerVelocity, this._channels))
        this._spatialCfgKey = key
      }
    } else {
      // off/缺省：失效签名（下次再启用时强制重下配置）
      this._spatialCfgKey = ''
      this._ambienceAmount = 0
    }
  }

  /** 返回当前参数快照（深拷贝，外部修改不影响引擎内部状态）。 */
  getParams(): HyperSoundEngineParams {
    return cloneParams(this._params)
  }

  /** 预分配内部工作缓冲；实时处理前调用一次，之后 process 内零分配。 */
  prepare(maxBlockSize: number): void {
    const size = Number.isFinite(maxBlockSize) ? Math.max(0, Math.floor(maxBlockSize)) : 0
    if (size > 0) {
      this.ensureCapacity(size)
      this._preparedCapacity = Math.max(this._preparedCapacity, size)
      this.ensureSideCapacity(size)
      this._convolver.prepare(size)
      this._spatialBackend.prepare(size)
      this._ambienceRenderer.prepare(size)
    }
  }

  /** 就地处理：outputs[i] 写入处理结果（长度 = inputs[i] 长度）。process 内零分配。 */
  process(inputs: Float32Array[], outputs: Float32Array[], sidechain?: Float32Array[]): void {
    this.processInternal(inputs, outputs, sidechain, false)
  }

  /**
   * 3–8 路实时输入到双耳/立体声输出。数组和通道缓冲均由调用方预分配并复用。
   * spatial 开启时先由 SpatialBackend.processMulti 双耳化，再让所得 L/R 经过第 1–21 级；
   * spatial 关闭时保持 ch0→L、ch1→R，ch2+ 忽略的兼容语义并直接执行第 1–21 级。
   */
  processMulti(inputs: Float32Array[], outputs: Float32Array[], sidechain?: Float32Array[]): void {
    if (inputs.length < 3 || inputs.length > 8) {
      throw new RangeError('processMulti requires 3 to 8 input channels')
    }
    if (inputs.length !== this._channels) {
      throw new RangeError(`processMulti requires configured input channel count ${this._channels}, got ${inputs.length}`)
    }
    if (outputs.length < 2) {
      throw new RangeError('processMulti requires two output channels')
    }
    this.processInternal(inputs, outputs, sidechain, true)
  }

  private processInternal(
    inputs: Float32Array[],
    outputs: Float32Array[],
    sidechain: Float32Array[] | undefined,
    multi: boolean,
  ): void {
    let n = Infinity
    for (const ch of inputs) {
      if (ch) n = Math.min(n, ch.length)
    }
    if (n === Infinity || n <= 0) return
    this.ensureCapacity(n)
    const L = this._workL
    const R = this._workR
    const inL = inputs[0]
    // 单声道引擎（channelCount=1）忽略第二输入声道
    const inR = this._channels > 1 && inputs.length > 1 ? inputs[1] : undefined
    if (multi && this._spatialActive) {
      for (let channel = 0; channel < inputs.length; channel++) this._spatialInputs[channel] = inputs[channel]
      this._spatialBackend.processMulti(this._spatialInputs, L, R, n)
      if (this._ambienceAmount > 0) {
        this._ambienceRenderer.processAdd(inputs[0], inputs[1], L, R, n, this._ambienceAmount)
      }
    } else {
      for (let i = 0; i < n; i++) L[i] = inL ? inL[i] : 0
      for (let i = 0; i < n; i++) R[i] = inR ? inR[i] : 0
    }

    // 可选 sidechain：复制到内部缓冲，供 Compressor/Deesser 等 stage 使用
    if (sidechain && sidechain.length > 0 && (sidechain[0]?.length ?? 0) > 0) {
      this.ensureSideCapacity(n)
      const sL = sidechain[0]
      const sR = sidechain.length > 1 ? sidechain[1] : sL
      for (let i = 0; i < n; i++) {
        this._sideL[i] = sL && i < sL.length ? sL[i] : 0
        this._sideR[i] = sR && i < sR.length ? sR[i] : 0
      }
      this._sidechainActive = true
    } else {
      this._sidechainActive = false
    }

    // 参数调制矩阵（块速率更新 masterGain / stereoWidth）
    if (this._params.modulation.enabled) {
      this._modMatrix.processBlockInto(L, R, n, this._modulationResult)
      this._modMasterGain = this._modulationResult.masterGain
      this._modStereoWidth = this._modulationResult.stereoWidth
    } else {
      this._modMasterGain = 1
      this._modStereoWidth = 1
    }

    // 普通 process 保持 1–21 → spatial 的冻结顺序；processMulti 专用顺序为
    // spatial multi → 1–21，避免双耳求和绕过 EQ、Limiter 等共享参数。
    if (multi) this.processCore21(L, R, n)
    else this.processAllStages(L, R, n)

    // 写出
    const outL = outputs[0]
    if (outL) for (let i = 0; i < n; i++) outL[i] = L[i]
    if (outputs.length > 1 && outputs[1]) {
      const outR = outputs[1]
      for (let i = 0; i < n; i++) outR[i] = R[i]
    }
  }

  /**
   * 多通道 HseAudioBus 处理入口。
   *
   * 引擎 DSP 核心为立体声，支持两种多通道路由（`options.mode`）：
   * - `'downmix'`（默认）：输入 >2 声道下混为立体声处理；输出写入时不足 2 声道写第一声道、
   *   超过 2 声道把处理后的立体声复制到其余声道。适合环绕声监听（各声道听感一致）。
   * - `'perChannelPair'`：真正的 N 通道处理——按立体声对 (0,1)、(2,3)… 分组，
   *   每对由独立引擎实例（子引擎池，参数/复位与主引擎同步）分别处理，互不串扰；
   *   奇数剩余通道复制成立体声处理并取 L 写回。适合 5.1/7.1 各通道独立处理。
   *   sidechain 同样按对切片；不足 2 声道时取第 0 声道广播到各对。
   *
   * 注意：本方法为便利入口，会分配临时缓冲；实时立体声使用 `process()`，
   * 实时 3–8 路输入使用 `processMulti()`。
   */
  processBus(input: HseAudioBus, output: HseAudioBus, sidechain?: HseAudioBus, options?: { mode?: 'downmix' | 'perChannelPair' }): void {
    if (options?.mode === 'perChannelPair' && input.channelCount > 2) {
      this.processBusPerChannelPair(input, output, sidechain)
      return
    }
    const n = Math.min(input.frameCount, output.frameCount)
    const { l, r } = input.downmixToStereo()
    const outL = new Float32Array(n)
    const outR = new Float32Array(n)
    let sideL: Float32Array | undefined
    let sideR: Float32Array | undefined
    if (sidechain) {
      const s = sidechain.downmixToStereo()
      sideL = s.l
      sideR = s.r
    }
    this.process([l, r], [outL, outR], sideL && sideR ? [sideL, sideR] : undefined)
    output.writeStereo(outL, outR)
  }

  /** 按立体声对逐对处理（perChannelPair）。每对独立子引擎，输出就地写入 output 对应通道。 */
  private processBusPerChannelPair(input: HseAudioBus, output: HseAudioBus, sidechain?: HseAudioBus): void {
    const n = Math.min(input.frameCount, output.frameCount)
    const cc = input.channelCount
    const pairCount = Math.floor(cc / 2)

    for (let p = 0; p < pairCount; p++) {
      const e = this.ensurePairEngine(p)
      const inL = input.getChannel(p * 2).subarray(0, n)
      const inR = input.getChannel(p * 2 + 1).subarray(0, n)
      const outL = output.getChannel(p * 2).subarray(0, n)
      const outR = output.getChannel(p * 2 + 1).subarray(0, n)
      let side: Float32Array[] | undefined
      if (sidechain) {
        const sc = sidechain.channelCount
        if (sc >= 2) {
          side = [sidechain.getChannel(Math.min(p * 2, sc - 1)).subarray(0, n), sidechain.getChannel(Math.min(p * 2 + 1, sc - 1)).subarray(0, n)]
        } else {
          const mono = sidechain.getChannel(0).subarray(0, n)
          side = [mono, mono]
        }
      }
      e.process([inL, inR], [outL, outR], side)
    }

    // 奇数剩余通道：复制成立体声处理，取 L 写回
    if (cc % 2 === 1) {
      const p = pairCount
      const e = this.ensurePairEngine(p)
      const mono = input.getChannel(cc - 1).subarray(0, n)
      const out = output.getChannel(cc - 1).subarray(0, n)
      const tmpL = new Float32Array(n)
      const tmpR = new Float32Array(n)
      for (let i = 0; i < n; i++) {
        tmpL[i] = mono[i]
        tmpR[i] = mono[i]
      }
      let side: Float32Array[] | undefined
      if (sidechain) {
        const mono = sidechain.getChannel(0).subarray(0, n)
        side = [mono, mono]
      }
      e.process([tmpL, tmpR], [tmpL, tmpR], side)
      for (let i = 0; i < n; i++) out[i] = tmpL[i]
    }
  }

  /** 获取（或懒创建）第 index 个立体声子引擎；参数与主引擎当前快照一致。 */
  private ensurePairEngine(index: number): HyperSoundEngine {
    let e = this._pairEngines[index]
    if (!e) {
      e = new HyperSoundEngine(this._fs, 2)
      e.setParams(this._params)
      this._pairEngines[index] = e
    }
    return e
  }

  getStats(): EngineStats {
    return {
      lufsIntegrated: this._lufs.getIntegratedLufs(),
      lufsMomentary: this._lufs.getMomentaryLufs(),
      lra: this._lufs.getLra(),
      peakDb: this._lufs.getPeakDb(),
      truePeakDb: this._lufs.getTruePeakDb(),
      limiterReductionDb: this._limiter.getReductionDb(),
      engineLatencySamples: this.getLatencySamples(),
    }
  }

  /** 最近一帧频谱 + 特征（内部 2048 点 FFT + Hann 窗）。未测到返回 null。 */
  getAnalysis(): EngineAnalysis {
    if (!this._analysisReady) return { spectrum: null, features: null }
    const spectrum = new Float32Array(this._magBuf)
    const features: SpectralFeatures = { ...this._featCache }
    return { spectrum, features }
  }

  /** 引擎引入的延迟（样本数）= 限幅器前瞻 + 混响延迟 + 空间音频分区延迟。 */
  getLatencySamples(): number {
    let lat = 0
    const p = this._params
    if (p.limiter.enabled) lat += this._limiter.getLatencySamples()
    if (p.reverb.enabled) {
      if (this._useConvolver) lat += this._convolver.getLatencySamples()
      else if (p.reverb.mode === 'algorithmic') {
        lat += Math.round((p.reverb.algorithmic.preDelayMs / 1000) * this._fs)
      }
    }
    // 空间音频（mode!=='off' 时，TsConvolverBackend 有扬声器 → 1 分区长 512）
    if (this._spatialActive) lat += this._spatialBackend.getLatencySamples()
    return lat
  }

  /** 变速/变调处理器（不内联进主链，供 gapless/过渡场景调用）。 */
  getStretch(): HseStretch {
    return this._stretch
  }

  /**
   * 注册自定义处理阶段（模块化效果器扩展点）。
   * - `index` 缺省时插到 `limiter` 之前（即参与主链但位于最终保护之前）；
   * - 若 `id` 已存在则原位替换；
   * - 自定义阶段可提供可选 `reset()`，引擎 `reset()` 时会调用。
   */
  registerStage(stage: ProcessingStage, index?: number): void {
    if (!stage || typeof stage.id !== 'string' || stage.id.length === 0) {
      throw new Error('registerStage: stage.id must be a non-empty string')
    }
    const existing = this._stages.findIndex((s) => s.id === stage.id)
    if (existing >= 0) {
      this._stages[existing] = stage
      return
    }
    let insertAt: number
    if (index === undefined) {
      const limiterIdx = this._stages.findIndex((s) => s.id === 'limiter')
      insertAt = limiterIdx >= 0 ? limiterIdx : this._stages.length
    } else {
      insertAt = Math.max(0, Math.min(this._stages.length, Math.floor(index)))
    }
    this._stages.splice(insertAt, 0, stage)
  }

  /** 按 id 移除自定义处理阶段；返回是否移除成功。 */
  unregisterStage(id: string): boolean {
    const idx = this._stages.findIndex((s) => s.id === id)
    if (idx < 0) return false
    this._stages.splice(idx, 1)
    return true
  }

  /** 当前处理阶段列表（返回副本，外部修改不影响引擎内部链）。 */
  getStages(): ProcessingStage[] {
    return this._stages.slice()
  }

  /** 复位所有模块与内部状态。 */
  reset(): void {
    this._eqChain.reset()
    this._midSide.reset()
    this._deesser.reset()
    this._compressor.reset()
    this._limiter.reset()
    this._bass.reset()
    this._convolver.reset()
    this._reverbSimple.reset()
    this._fdnReverb.reset()
    this._lufs.reset()
    this._loudnessComp.reset()
    this._nightCompressor.reset()
    this._nightShelfL.reset()
    this._nightShelfR.reset()
    this._ieqChain.reset()
    this._dynamicEq.reset()
    this._stretch.reset()
    this._delay.reset()
    this._chorus.reset()
    this._flanger.reset()
    this._phaser.reset()
    this._tremolo.reset()
    // 空间音频后端与环境渲染流式状态清零（配置与 IR 保留）
    this._spatialBackend.reset()
    this._ambienceRenderer.reset()
    this._worldHistoryValid = false
    this._normGain = 1
    this._surroundPhase = 0
    this._sidechainActive = false
    this._modMatrix.reset()
    this._modMasterGain = 1
    this._modStereoWidth = 1
    this._ringPos = 0
    this._analysisPos = 0
    this._analysisReady = false
    this._ring.fill(0)
    this._magBuf.fill(0)
    this._ieqGains.fill(0)
    const f = this._featCache
    f.rms = 0
    f.zcr = 0
    f.centroidHz = 0
    f.rolloffHz = 0
    f.flatness = 0
    f.crest = 0
    // 自定义阶段复位（内置阶段未提供 reset 时自动跳过）
    for (const stage of this._stages) stage.reset?.()
    // 多通道子引擎复位
    for (const e of this._pairEngines) e.reset()
  }

  // ==================== 内部实现 ====================

  private processCore21(L: Float32Array, R: Float32Array, frameCount: number): void {
    for (const stage of this._stages) {
      if (stage.id === 'spatial') continue
      if (stage.active()) stage.run(L, R, frameCount)
    }
  }

  private processAllStages(L: Float32Array, R: Float32Array, frameCount: number): void {
    for (const stage of this._stages) {
      if (stage.active()) stage.run(L, R, frameCount)
    }
  }

  /**
   * 构建处理链（22 级，含调制类效果、调制主增益与空间音频）。
   * 顺序固定，与 API_SPEC 辅助模块 A 一致；数组顺序即处理顺序。
   */
  private buildStages(): void {
    this._stages.length = 0
    this._stages.push(
      {
        id: 'loudness-normalization',
        active: () => this._params.loudnessNormalization.enabled,
        run: (L, R, n) => {
          // 响度归一化增益（目标 LUFS + 引擎内实时测量驱动）
          const ln = this._params.loudnessNormalization
          if (ln.useRealtimeMeter) {
            const integrated = this._lufs.getIntegratedLufs()
            const measured = Number.isFinite(integrated) ? integrated : this._lufs.getMomentaryLufs()
            // 无测量期不放大（ref=-70 会导致启动瞬间 +9dB 膨胀——审计修复）
            const gainDb = Number.isFinite(measured)
              ? Math.min(ln.maxGainDb, Math.max(ln.minGainDb, ln.targetLufs - measured))
              : 0
            const targetLin = Math.pow(10, gainDb / 20)
            const alpha = 1 - Math.exp(-(n / this._fs) / NORM_SMOOTH_SEC)
            this._normGain += alpha * (targetLin - this._normGain)
          } else {
            // 外部给定增益：平滑过渡（整曲测量换算语义；审计修复：不再瞬时阶跃）。
            // 手动分支用短时间常数——拖动音量曲线需及时到位（zipper 由平滑消除）
            const targetLin = Math.pow(10, Math.min(ln.maxGainDb, Math.max(ln.minGainDb, ln.externalGainDb)) / 20)
            const alpha = 1 - Math.exp(-(n / this._fs) / MANUAL_GAIN_SMOOTH_SEC)
            this._normGain += alpha * (targetLin - this._normGain)
          }
          const g = this._normGain
          for (let i = 0; i < n; i++) {
            L[i] *= g
            R[i] *= g
          }
        },
      },
      {
        id: 'surround3d',
        active: () => this._params.surround3d.enabled,
        run: (L, R, n) => {
          // 3D 环绕：轻量立体声旋转（angle 静态旋转 + speed 随时间缓慢旋转）
          const s3 = this._params.surround3d
          const dt = n / this._fs
          this._surroundPhase += 2 * Math.PI * s3.speed * dt * 0.125 // speed=1 → 0.125 圈/秒
          const theta = (s3.angle * Math.PI) / 180 + s3.direction * this._surroundPhase
          const c = Math.cos(theta)
          const s = Math.sin(theta)
          const scale = 0.5 + 0.5 * s3.distance // 距离 0..1 映射为电平 0.5..1
          for (let i = 0; i < n; i++) {
            const l = L[i]
            const r = R[i]
            L[i] = (l * c - r * s) * scale
            R[i] = (l * s + r * c) * scale
          }
        },
      },
      {
        id: 'mid-side',
        active: () => true,
        run: (L, R, n) => {
          // M/S：立体声宽度 + 人声比例（voiceBalance 仅在 pitch.enabled 时生效）
          const vb = this._params.pitch.enabled ? this._params.pitch.voiceBalance : 0
          const width = this._params.modulation.enabled ? this._modStereoWidth : this._params.stereoWidth
          this._midSide.setParams(width, vb)
          this._midSide.processStereo(L, R, this.dspFrameCount(n))
        },
      },
      {
        id: 'pre-eq',
        active: () => this._preEqActive,
        run: (L, R, n) => this._eqChain.processStereo(L, R, this.dspFrameCount(n)),
      },
      {
        id: 'deesser',
        active: () => this._params.deesser.enabled,
        run: (L, R, n) => {
          if (this._sidechainActive && this._params.deesser.sidechainEnabled) {
            this._deesser.processStereo(L, R, this._sideL, this._sideR, this.dspFrameCount(n))
          } else {
            this._deesser.processStereo(L, R, undefined, undefined, this.dspFrameCount(n))
          }
        },
      },
      {
        id: 'compressor',
        active: () => this._params.compressor.enabled,
        run: (L, R, n) => {
          if (this._sidechainActive && this._params.compressor.sidechainEnabled) {
            this._compressor.processStereo(L, R, this._sideL, this._sideR, this.dspFrameCount(n))
          } else {
            this._compressor.processStereo(L, R, undefined, undefined, this.dspFrameCount(n))
          }
        },
      },
      {
        id: 'night-mode',
        active: () => this._nightActive,
        run: (L, R, n) => {
          // NightMode：压缩增强 + 6kHz 高频衰减
          const dspN = this.dspFrameCount(n)
          this._nightCompressor.processStereo(L, R, undefined, undefined, dspN)
          this._nightShelfL.processBlock(L, L, dspN)
          this._nightShelfR.processBlock(R, R, dspN)
        },
      },
      {
        id: 'delay',
        active: () => this._params.modEffects.delay.enabled,
        run: (L, R, n) => this._delay.processStereo(L, R, this.dspFrameCount(n)),
      },
      {
        id: 'chorus',
        active: () => this._params.modEffects.chorus.enabled,
        run: (L, R, n) => this._chorus.processStereo(L, R, this.dspFrameCount(n)),
      },
      {
        id: 'flanger',
        active: () => this._params.modEffects.flanger.enabled,
        run: (L, R, n) => this._flanger.processStereo(L, R, this.dspFrameCount(n)),
      },
      {
        id: 'phaser',
        active: () => this._params.modEffects.phaser.enabled,
        run: (L, R, n) => this._phaser.processStereo(L, R, this.dspFrameCount(n)),
      },
      {
        id: 'tremolo',
        active: () => this._params.modEffects.tremolo.enabled,
        run: (L, R, n) => this._tremolo.processStereo(L, R, this.dspFrameCount(n)),
      },
      {
        id: 'reverb',
        active: () => this._params.reverb.enabled && this._params.reverb.mode !== 'off',
        run: (L, R, n) => {
          // 混响（三路路由：卷积 / 算法 / off；mode='off' 时完全直通——审计修复）
          const dspN = this.dspFrameCount(n)
          if (this._useConvolver) this._convolver.processStereo(L, R, dspN)
          else if (this._useFdn) this._fdnReverb.processStereo(L, R, dspN)
          else this._reverbSimple.processStereo(L, R, dspN)
        },
      },
      {
        id: 'bass-enhancer',
        active: () => this._params.bassEnhancer.enabled,
        run: (L, R, n) => this._bass.processStereo(L, R, this.dspFrameCount(n)),
      },
      {
        id: 'loudness-compensation',
        active: () => this._params.loudnessCompensation.enabled,
        run: (L, R, n) => this._loudnessComp.processStereo(L, R, this.dspFrameCount(n)),
      },
      {
        id: 'ieq-post',
        active: () => this._ieqActive,
        run: (L, R, n) => this._ieqChain.processStereo(L, R, this.dspFrameCount(n)),
      },
      {
        id: 'analysis',
        active: () => true,
        run: (L, R, n) => {
          // 分析取样（IEQ 处理后——闭环修正）：取样点在 IEQ 之后，
          // IEQ 抬高/压低频段后分析能看到修正结果，增益随修正收敛到目标曲线
          this.feedAnalysis(L, R, n)
        },
      },
      {
        id: 'dynamic-eq',
        active: () => this._params.dynamicEq.enabled,
        run: (L, R, n) => this._dynamicEq.processStereo(L, R, this.dspFrameCount(n)),
      },
      {
        id: 'lufs',
        active: () => true,
        run: (L, R, n) => {
          // LUFS 采样点（Limiter 之前，API_SPEC 要求）
          this._lufs.processStereo(L, R, this.dspFrameCount(n))
        },
      },
      {
        id: 'mod-master-gain',
        active: () => this._params.modulation.enabled,
        run: (L, R, n) => {
          const g = this._modMasterGain
          for (let i = 0; i < n; i++) {
            L[i] *= g
            R[i] *= g
          }
        },
      },
      {
        id: 'limiter',
        active: () => this._params.limiter.enabled,
        run: (L, R, n) => this._limiter.processStereo(L, R, this.dspFrameCount(n)),
      },
      {
        id: 'spatial',
        active: () => this._spatialActive,
        run: (L, R, n) => {
          // 空间音频（内联级；mode='off' 时旁路逐位回归——不触碰 L/R）。
          // 后端内部包含房间模拟；环境声在同一内联级叠加到双耳输出。
          // 所有块级缓冲均由实例持有并在 prepare/ensureCapacity 控制路径扩容。
          if (this._spatialOutL.length < n) {
            this._spatialOutL = new Float32Array(n)
            this._spatialOutR = new Float32Array(n)
          }
          const oL = this._spatialOutL
          const oR = this._spatialOutR
          this._spatialBackend.processStereo(L, R, oL, oR, n)
          if (this._ambienceAmount > 0) {
            this._ambienceRenderer.processAdd(L, R, oL, oR, n, this._ambienceAmount)
          }
          for (let i = 0; i < n; i++) {
            L[i] = oL[i]
            R[i] = oR[i]
          }
        },
      },
    )
  }

  private dspFrameCount(frameCount: number): number {
    return this._legacyPaddedTail ? this._workL.length : frameCount
  }

  private ensureCapacity(n: number): void {
    if (this._workL.length < n) {
      if (this._preparedCapacity > 0) {
        throw new RangeError(`HyperSoundEngine block ${n} exceeds prepared capacity ${this._preparedCapacity}`)
      }
      this._workL = new Float32Array(n)
      this._workR = new Float32Array(n)
      this._spatialBackend.prepare(n)
    }
    if (this._spatialOutL.length < n) {
      this._spatialOutL = new Float32Array(n)
      this._spatialOutR = new Float32Array(n)
    }
  }

  private ensureSideCapacity(n: number): void {
    if (this._sideL.length < n) {
      this._sideL = new Float32Array(n)
      this._sideR = new Float32Array(n)
    }
  }

  /** 收集用户 EQ（simple/pro）bands，上限 20 段。 */
  private buildPreEqBands(p: HyperSoundEngineParams): EqBand[] {
    const out: EqBand[] = []
    // 用户 EQ 仅在 eq.enabled 时并入（eq 关闭时不得泄漏——审计修复）；
    // 机型补偿已移除（由 LoudnessComp 音量曲线承担）
    if (!p.eq.enabled) return out
    if (p.eq.mode === 'simple') {
      for (let i = 0; i < SIMPLE_EQ_FREQUENCIES.length; i++) {
        out.push({ frequency: SIMPLE_EQ_FREQUENCIES[i], gain: p.eq.simpleBands[i] ?? 0, q: 1.1 })
      }
    } else {
      const count = Math.min(p.eq.bandCount, p.eq.proBands.length)
      for (let i = 0; i < count; i++) {
        const b = p.eq.proBands[i]
        out.push({ frequency: b.frequency, gain: b.gain, q: b.q })
      }
    }
    return out.slice(0, MAX_PRE_EQ_BANDS)
  }

  /**
   * 混响路由配置：convolution 且 IR 有效 → 卷积；fdn → FDN 网络混响；
   * 否则算法混响（Freeverb，含卷积自动回退）。
   */
  private configureReverb(rv: ReverbSettings): void {
    this._reverbSimple.setParams({ ...rv.algorithmic })
    this._fdnReverb.setParams({ ...rv.algorithmic, type: rv.algorithmic.type })
    this._useConvolver = false
    this._useFdn = false
    // dePeriodize 参数化（死参数修复）：选项变化时重建 Convolver 并强制重载 IR
    const wantDeP = rv.convolution.dePeriodize
    if (wantDeP !== this._convolverDePeriodize) {
      this._convolver = new Convolver(this._fs, { dePeriodize: wantDeP })
      if (this._preparedCapacity > 0) this._convolver.prepare(this._preparedCapacity)
      this._convolverDePeriodize = wantDeP
      this._loadedIr = null // 新实例无 IR，强制重载
    }
    if (rv.enabled && rv.mode === 'fdn') {
      this._useFdn = true
      return
    }
    if (rv.enabled && rv.mode === 'convolution') {
      const ir = rv.convolution.ir
      if (ir && ir.length > 0) {
        try {
          if (ir !== this._loadedIr) {
            // 复制 IR 再载入：避免模块就地改写调用方数组
            this._convolver.loadIR(new Float32Array(ir), rv.convolution.irName ?? undefined)
            if (this._preparedCapacity > 0) this._convolver.prepare(this._preparedCapacity)
            this._loadedIr = ir
          }
          this._convolver.setMix(rv.convolution.mix)
          this._convolver.setPreDelayMs(rv.convolution.preDelayMs)
          this._useConvolver = true
        } catch {
          // 空/非法 IR（模块抛错）：自动回退算法混响
          this._useConvolver = false
        }
      }
    }
  }

  /** 把单声道下混写入环形分析缓冲；累计满一窗后执行 FFT + 特征 + IEQ 更新。 */
  private feedAnalysis(l: Float32Array, r: Float32Array, n: number): void {
    const W = ANALYSIS_WINDOW
    for (let i = 0; i < n; i++) {
      this._ring[this._ringPos] = 0.5 * (l[i] + r[i])
      this._ringPos = (this._ringPos + 1) % W
    }
    this._analysisPos += n
    // 循环触发：一次 process 可能喂入任意长度（离线导出/大块测试会一次数十万样本），
    // 若只取模触发一次会丢掉所有中间窗（IEQ 增益/频谱/特征只更新一小步——审计实测
    // 4s 大块只跑 1 次分析、增益收敛到目标的 1/7）。逐窗递减保证每个完整窗都分析。
    while (this._analysisPos >= W) {
      this._analysisPos -= W
      this.runAnalysis()
    }
  }

  /** 对最近一窗做 2048 点 FFT（Hann 窗），计算幅度谱与特征，并更新 IEQ。 */
  private runAnalysis(): void {
    const W = ANALYSIS_WINDOW
    for (let i = 0; i < W; i++) {
      const src = this._ring[(this._ringPos + i) % W]
      this._timeBuf[i] = src
      this._real[i] = src * this._hann[i]
      this._imag[i] = 0
    }
    fft(this._real, this._imag, false)
    // 手动幅度谱（复用预分配缓冲，避免 magnitudeSpectrum 的分配）
    const half = W / 2
    const mag = this._magBuf
    for (let k = 0; k <= half; k++) {
      const re = this._real[k]
      const im = this._imag[k]
      mag[k] = Math.sqrt(re * re + im * im)
    }
    const f = this._featCache
    f.rms = computeRms(this._timeBuf)
    f.zcr = computeZcr(this._timeBuf)
    f.centroidHz = spectralCentroid(mag, this._binFreqs)
    f.rolloffHz = spectralRolloff(mag, this._binFreqs)
    f.flatness = spectralFlatness(mag)
    f.crest = spectralCrest(mag)
    this._analysisReady = true
    if (this._ieqActive) this.updateIeq(mag)
  }

  /** IEQ：长时频谱与目标曲线之差 → 平滑增益 → 写入内部参数 EQ。 */
  private updateIeq(mag: Float32Array): void {
    const levels = this._ieqLevels
    let overall = 0
    for (let i = 0; i < IEQ_BAND_COUNT; i++) {
      const [lo, hi] = this._ieqBinRanges[i]
      // 频段电平用 RMS（能量平均）而非线性幅度平均：稀疏频谱（纯音/稀疏乐器）下
      // 线性平均会把少数尖峰稀释到接近噪声底（实测 4kHz 单 bin 频段被摊到 -80dB），
      // 驱动 IEQ 增益在两个 ±12dB 极端 clamp 间振荡、输出过度整形；RMS 对尖峰
      // 敏感（平方项），稀释效应小约一个量级。噪声底 clamp 防 -126dB 极端值。
      let sumSq = 0
      for (let k = lo; k <= hi; k++) sumSq += mag[k] * mag[k]
      const rms = Math.sqrt(sumSq / (hi - lo + 1))
      levels[i] = 20 * Math.log10(Math.max(rms, 1e-4)) // -80dB 噪声底
      overall += levels[i]
    }
    overall /= IEQ_BAND_COUNT
    const alpha = this._ieqSmooth
    const strength = this._ieqStrength
    for (let i = 0; i < IEQ_BAND_COUNT; i++) {
      const relative = levels[i] - overall // 相对频谱形状（去掉整体电平偏移）
      const desired = strength * (this._ieqTargets[i] - relative)
      let g = this._ieqGains[i] + alpha * (desired - this._ieqGains[i])
      if (g > 12) g = 12
      else if (g < -12) g = -12
      this._ieqGains[i] = g
      this._ieqBands[i].gain = g
    }
    this._ieqChain.setBands(this._ieqBands)
  }

  /** IEQ 目标曲线（dB，按 1 倍频程 10 段）。 */
  private ieqTargetCurve(curve: IeqTargetCurve): number[] {
    switch (curve) {
      case 'flat':
        return [0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
      case 'warm':
        return [4, 3.5, 2.5, 1.5, 0.5, 0, -0.5, -1.5, -2.5, -3.5]
      case 'bright':
        return [-3.5, -2.5, -1.5, -0.5, 0, 0.5, 1.5, 2.5, 3.5, 4]
      case 'vocal':
        return [-1.5, -1, 0, 1, 2, 2.5, 2, 1, 0, -0.5]
    }
  }
}