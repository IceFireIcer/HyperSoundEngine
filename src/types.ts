/**
 * HyperSoundEngine v1 —— 类型与参数模型
 *
 * 设计依据：
 *  - research/docs/音频算法设计文档.md §3（功能→算法→采用方式映射）
 *  - research/docs/MIT套用与自研决策表.md（可套用/自研边界）
 *
 * 约定：所有参数为不可变快照语义；HyperSoundEngine.setParams 每次接收完整 HyperSoundEngineParams。
 */

import type {
  SpatialMode,
  ConvolutionMode,
  HrtfInterpMode,
  DistanceModel,
  InstantSpatialSettings,
  HeadLockedSettings,
  WorldSettings,
  StageSettings,
  AmbienceSettings,
} from './spatial/types'
import { createDefaultSpatialParams } from './spatial/types'

/** 混响路由：卷积混响 / 算法混响(Freeverb) / FDN 算法混响 / 关闭 */
export type ReverbMode = 'convolution' | 'algorithmic' | 'fdn' | 'off'
/** 均衡模式：简约 5 段 / 专业多段 */
export type EqMode = 'simple' | 'pro'
/** 虚拟低频谐波非线性类型 */
export type HarmonicType = 'odd' | 'even' | 'atan' | 'soft'
/** 频响补偿模式：auto=ISO 226 等响度自适应 / preset=场景预设 / custom=自定义频段 */
export type CompensationMode = 'auto' | 'preset' | 'custom'
/** 算法混响类型（5 种） */
export type ReverbType = 'hall' | 'room' | 'plate' | 'spring' | 'stage'
/** 智能均衡目标曲线 */
export type IeqTargetCurve = 'flat' | 'warm' | 'bright' | 'vocal'

/** 单段均衡：频率(Hz) / 增益(dB) / Q */
export interface EqBand {
  frequency: number
  gain: number
  q: number
}

/** 均衡器设置（简约 5 段 + 专业 10/20 段 + 级联 Q 补偿） */
export interface EqSettings {
  enabled: boolean
  mode: EqMode
  /** 简约版 5 段：[低音, 中低, 中音, 中高, 高音] 增益 dB */
  simpleBands: number[]
  /** 专业版 bands（proBands；支持 10/20 段） */
  proBands: EqBand[]
  /** 专业版段数：10（octave）或 20（1/2 octave） */
  bandCount: 10 | 20
  /** 级联 Q 补偿：迭代修正相邻段叠加误差（技术文档 §1.3） */
  qCompensation: boolean
  /** EQ 锁定（防误改） */
  locked: boolean
}

/** 齿音抑制（技术文档 §4） */
export interface DeesserSettings {
  enabled: boolean
  /** 侧链中心频率 Hz，默认 6000（4–8kHz 齿音频段） */
  centerHz: number
  /** 侧链带通 Q，默认 0.7 */
  q: number
  /** 触发阈值 dB，默认 -30 */
  thresholdDb: number
  /** 压缩比率，默认 8 */
  ratio: number
  /** attack ms，默认 1 */
  attackMs: number
  /** release ms，默认 80 */
  releaseMs: number
  /** true=分带式（只压高频带，推荐）/ false=宽带式 */
  splitBand: boolean
  /** 效果混合 0..1 */
  mix: number
  /** 是否使用外部 sidechain 信号检测齿音（默认 false） */
  sidechainEnabled?: boolean
}

/** 动态压缩（knee + sidechain） */
export interface CompressorSettings {
  enabled: boolean
  thresholdDb: number
  ratio: number
  kneeDb: number
  attackMs: number
  releaseMs: number
  /** 补偿增益 dB */
  makeupDb: number
  /** 输出线性增益 0..2，默认 1 */
  outputGain: number
  /** 是否使用外部 sidechain 信号驱动包络（默认 false） */
  sidechainEnabled?: boolean
}

/** 夜间模式（动态压缩增强 + 高频衰减，深夜语义） */
export interface NightModeSettings {
  enabled: boolean
  /** 强度 0..10 */
  amount: number
}

/** 虚拟低频增强（技术文档 §5） */
export interface BassEnhancerSettings {
  enabled: boolean
  /** 低通截止 Hz，默认 90 */
  cutoffHz: number
  /** 低通 Q，默认 0.7 */
  q: number
  /** 谐波非线性类型：odd=奇次(x³) / even=偶次(整流) / atan=ATSR / soft=tanh */
  harmonicType: HarmonicType
  /** 谐波增益 0..1 */
  harmonicGain: number
  /** 干湿混合 0..1 */
  mix: number
  /** 整体电平 dB -6..6 */
  levelDb: number
  /** 低音下潜 dB -6..12：低通提取的低频带按 (10^(lowBoostDb/20)−1) 真实混回（默认 0=关闭）。
   *  可选字段：旧参数快照缺省时按 0 处理（dsp 侧 Number.isFinite 防御）。 */
  lowBoostDb?: number
}

/** 延迟效果 */
export interface DelaySettings {
  enabled: boolean
  delayMs: number
  feedback: number
  mix: number
}

/** 合唱 */
export interface ChorusSettings {
  enabled: boolean
  rateHz: number
  depthMs: number
  mix: number
}

/** 镶边 */
export interface FlangerSettings {
  enabled: boolean
  rateHz: number
  depthMs: number
  feedback: number
  mix: number
}

/** 移相器 */
export interface PhaserSettings {
  enabled: boolean
  rateHz: number
  /** 0..1 调制深度 */
  depth: number
  feedback: number
  mix: number
  /** 全通级数（建议 2/4/6/8） */
  stages: number
}

/** 颤音 */
export interface TremoloSettings {
  enabled: boolean
  rateHz: number
  /** 0..1 调制深度 */
  depth: number
  mix: number
}

/** 调制类效果组 */
export interface ModEffectsSettings {
  delay: DelaySettings
  chorus: ChorusSettings
  flanger: FlangerSettings
  phaser: PhaserSettings
  tremolo: TremoloSettings
}

/** 混响（卷积 + 算法双路，IR 去周期化） */
export interface ReverbSettings {
  enabled: boolean
  /** 路由：convolution=分区卷积 / algorithmic=Freeverb 类 / off */
  mode: ReverbMode
  algorithmic: {
    type: ReverbType
    roomSize: number
    damping: number
    wet: number
    dry: number
    preDelayMs: number
    /** 立体声宽度 0..2 */
    width: number
  }
  convolution: {
    /** 脉冲响应（单声道 Float32Array）；null 表示未加载（自动回退 algorithmic） */
    ir: Float32Array | null
    irName: string | null
    mix: number
    preDelayMs: number
    /** IR 去周期化：尾部指数衰减窗消除循环伪影 */
    dePeriodize: boolean
  }
}

/** 3D 环绕（轻量立体声旋转实现） */
export interface Surround3dSettings {
  enabled: boolean
  distance: number
  speed: number
  angle: number
  direction: 1 | -1
}

/** 频响补偿（auto/preset/custom + 音量线性等响度） */
export interface LoudnessCompSettings {
  enabled: boolean
  mode: CompensationMode
  /** preset 模式预设 id：flat/bass/vocal/warm/bright/night */
  preset: string
  /** custom 模式目标曲线控制点 */
  bands: { frequency: number; gain: number }[]
  /** 系统音量 0..100（auto 模式输入） */
  volumePercent: number
  /** 最大提升 dB，默认 12 */
  maxBoostDb: number
  /** 增益平滑时间常数 s，默认 0.2 */
  smoothingSeconds: number
}

/** 响度归一化（目标 -14 LUFS + 引擎内实时测量） */
export interface LoudnessNormSettings {
  enabled: boolean
  /** 目标响度 LUFS，默认 -14 */
  targetLufs: number
  maxGainDb: number
  minGainDb: number
  /** true=引擎内实时 LUFS 测量驱动（替代旧外部测量服务）/ false=外部给定增益 */
  useRealtimeMeter: boolean
  /** useRealtimeMeter=false 时使用（由整曲测量换算的增益） */
  externalGainDb: number
}

/** 前瞻限幅器（lookahead + true peak，技术文档 §3.3） */
export interface LimiterSettings {
  enabled: boolean
  thresholdDb: number
  lookaheadMs: number
  attackMs: number
  releaseMs: number
  /** 真峰值检测（4× 过采样） */
  truePeak: boolean
}

/** 自适应动态均衡（算法创新：频谱包络自动混音，全通交叉分带） */
export interface DynamicEqSettings {
  enabled: boolean
  /** 整体强度 0..1（0=直通） */
  strength: number
  /** 触发阈值 dB，默认 -20 */
  thresholdDb: number
  /** 每带压缩比，默认 2 */
  ratio: number
  /** 增益平滑 attack ms，默认 20 */
  attackMs: number
  /** 增益平滑 release ms，默认 200 */
  releaseMs: number
  /** 5 带（固定交叉 200/800/2500/8000 Hz）：每带开关 + 静态目标增益 dB */
  bands: { enabled: boolean; targetGainDb: number }[]
}

/** 智能均衡 IEQ（技术文档 §1.4） */
export interface IeqSettings {
  enabled: boolean
  /** 修正强度 0..1 */
  strength: number
  targetCurve: IeqTargetCurve
  /** 慢速平滑时间常数 s，默认 3（防抽吸） */
  timeConstantSec: number
}

/** 
 * 机型频响补偿（DeviceProfile）已移除：该功能由 `LoudnessComp`（等响度补偿）承担——
 * 按音量大小实施通用补偿曲线（auto 模式：音量越低，低频 0-12dB / 高频 0-6dB 提升越多，
 * ISO 226 简化近似公式），无需设备实测档案。
 */

/** 变调/变速（MIT 实现路径） */
export interface PitchSettings {
  enabled: boolean
  /** 半音 -10..10 */
  semitones: number
  /** 速率 0.25..3 */
  rate: number
  /** 人声/伴奏比例 -1(仅伴奏)..0(原声)..+1(仅人声)（M/S 处理） */
  voiceBalance: number
}

/** LFO 波形 */
export type LfoShape = 'sine' | 'triangle' | 'square' | 'saw'

/** 调制源类型 */
export type ModulationSourceType = 'lfo' | 'envelope'

/** 调制目标类型（当前内置两个可调制目标） */
export type ModulationTargetType = 'masterGain' | 'stereoWidth'

/** 调制路由：源 → 目标 */
export interface ModulationRoute {
  source: ModulationSourceType
  target: ModulationTargetType
  /** 调制深度（0..1 或按目标语义） */
  amount: number
  /** 静态偏移 */
  offset?: number
}

/** 参数调制矩阵设置 */
export interface ModulationSettings {
  enabled: boolean
  lfo: {
    enabled: boolean
    shape: LfoShape
    rateHz: number
    /** LFO 输出深度，归一化 0..1 */
    depth: number
  }
  envelope: {
    enabled: boolean
    attackMs: number
    releaseMs: number
    /** 包络跟随输出深度，归一化 0..1 */
    amount: number
  }
  routes: ModulationRoute[]
}

/** 听力分析（技术文档 §12） */
export interface HearingSettings {
  enabled: boolean
}

/** 引擎统计输出 */
export interface EngineStats {
  lufsIntegrated: number
  lufsMomentary: number
  lra: number
  peakDb: number
  truePeakDb: number
  /** 限幅器当前衰减 dB（<=0） */
  limiterReductionDb: number
  /** 引擎引入的延迟（样本数） */
  engineLatencySamples: number
}

/** 频谱特征（meyda 式自研，技术文档 §12） */
export interface SpectralFeatures {
  rms: number
  zcr: number
  centroidHz: number
  rolloffHz: number
  flatness: number
  crest: number
}

/** 引擎分析输出 */
export interface EngineAnalysis {
  /** 最近一帧幅度谱（长度由内部 FFT 决定，N/2+1） */
  spectrum: Float32Array | null
  features: SpectralFeatures | null
}

/** 场景预设快照（全参数快照） */
export interface ScenePreset {
  id: string
  name: string
  description?: string
  builtin: boolean
  /** 完整引擎参数快照（不含 IR 数据，卷积 IR 用 irName 引用） */
  params: HyperSoundEngineParams
}

/**
 * 空间音频引擎侧设置（内联级渲染用子集）。
 * 子结构（instant/headLocked/world/stage/ambience）复用 spatial/types 的同名接口，
 * 保证与纯函数布局/场景/控制器助手（headLockedSpeakers/stageSpeakers/computeRelativeDirection 等）
 * 形状兼容。角度=度，距离=米。相对全量 SpatialParams（Rust 服务/WASM 后端契约用）
 * 裁掉 output/perfMode/sinkId/keymap/multichannelChannels，perfMode 由直接的 hrtfInterp 表达。
 */
export interface SpatialSettings {
  /** 空间模式：off=关闭（旁路）/ instant=一键空间化 / headLocked=头锁定环绕 / world=世界漫游 / stage=舞台影院 */
  mode: SpatialMode
  /** 双耳输出主增益 0.5..1（防削波预留） */
  masterGain: number
  /** 模式 A：一键空间化（立体声 → ±spreadDeg/2 两只虚拟扬声器） */
  instant: InstantSpatialSettings
  /** 模式 B：头锁定环绕（布局预设 + 自定义编辑器；声场固定于头部朝向） */
  headLocked: HeadLockedSettings
  /** 模式 C：世界漫游（听者 + 声源对象；移动/旋转由 controller 纯函数驱动） */
  world: WorldSettings
  /** 模式 D：舞台/影院（场景预设 + 座位/房间调节） */
  stage: StageSettings
  /** 环境声 Ambisonics 上混（enabled/amount 由内联空间级直接渲染） */
  ambience: AmbienceSettings
  /** 卷积模式：partitioned=FFT 分区（默认）/ time=时域直接卷积 */
  convolution: ConvolutionMode
  /** HRTF 插值：nearest=最近邻网格查表（默认）/ spherical=球谐插值（方位过渡更平滑） */
  hrtfInterp: HrtfInterpMode
  /** 距离衰减模型（全部模式的全局渲染参数） */
  distanceModel: DistanceModel
  /** 距离衰减参考距离（米，默认 1；ref 内不衰减） */
  refDistance: number
  /** 距离衰减最大距离（米，默认 50；linear 模型在此衰减到 0） */
  maxDistance: number
}

/**
 * 生成默认空间音频设置（mode='off'——引擎旁路，逐位回归）。
 * 复用 spatial/types 的 createDefaultSpatialParams 单事实源，投影出 SpatialSettings
 * （perfMode → hrtfInterp：quality→spherical，balanced/lowLatency→nearest）。
 */
export function createDefaultSpatialSettings(): SpatialSettings {
  const p = createDefaultSpatialParams()
  return {
    mode: p.mode,
    masterGain: p.masterGain,
    instant: p.instant,
    headLocked: p.headLocked,
    world: p.world,
    stage: p.stage,
    ambience: p.ambience,
    convolution: p.convolution,
    hrtfInterp: p.perfMode === 'quality' ? 'spherical' : 'nearest',
    distanceModel: 'inverse',
    refDistance: 1,
    maxDistance: 50,
  }
}

/** 引擎总参数（一次性快照） */
export interface HyperSoundEngineParams {
  sampleRate: number
  eq: EqSettings
  deesser: DeesserSettings
  compressor: CompressorSettings
  nightMode: NightModeSettings
  bassEnhancer: BassEnhancerSettings
  reverb: ReverbSettings
  surround3d: Surround3dSettings
  loudnessCompensation: LoudnessCompSettings
  loudnessNormalization: LoudnessNormSettings
  limiter: LimiterSettings
  ieq: IeqSettings
  dynamicEq: DynamicEqSettings
  pitch: PitchSettings
  modulation: ModulationSettings
  modEffects: ModEffectsSettings
  hearing: HearingSettings
  /** 空间音频（内联级，Limiter 之后；缺省/ mode:'off' 时逐位旁路） */
  spatial?: SpatialSettings
  /** M/S 立体声宽度 0..2（1=原始） */
  stereoWidth: number
  /** 当前场景 id；null=自定义 */
  sceneId: string | null
  /** 用户手动改过参数（脱离场景快照） */
  customized: boolean
}

/** 生成默认参数快照 */
export function createDefaultParams(sampleRate: number): HyperSoundEngineParams {
  return {
    sampleRate,
    eq: {
      enabled: true,
      mode: 'pro',
      simpleBands: [0, 0, 0, 0, 0],
      proBands: PRO_EQ_DEFAULT_BANDS.map((f) => ({ frequency: f, gain: 0, q: 1.1 })),
      bandCount: 10,
      qCompensation: true,
      locked: false,
    },
    deesser: { enabled: false, centerHz: 6000, q: 0.7, thresholdDb: -30, ratio: 8, attackMs: 1, releaseMs: 80, splitBand: true, mix: 1, sidechainEnabled: false },
    compressor: { enabled: false, thresholdDb: -20, ratio: 4, kneeDb: 6, attackMs: 10, releaseMs: 150, makeupDb: 0, outputGain: 1, sidechainEnabled: false },
    nightMode: { enabled: false, amount: 0 },
    bassEnhancer: { enabled: false, cutoffHz: 90, q: 0.7, harmonicType: 'odd', harmonicGain: 0.6, mix: 0.5, levelDb: 0, lowBoostDb: 0 },
    reverb: {
      enabled: false,
      mode: 'algorithmic',
      algorithmic: { type: 'hall', roomSize: 0.5, damping: 0.5, wet: 0.3, dry: 0.7, preDelayMs: 0, width: 1 },
      convolution: { ir: null, irName: null, mix: 0.3, preDelayMs: 0, dePeriodize: true },
    },
    surround3d: { enabled: false, distance: 0.5, speed: 1, angle: 0, direction: 1 },
    loudnessCompensation: { enabled: false, mode: 'auto', preset: 'flat', bands: [], volumePercent: 80, maxBoostDb: 12, smoothingSeconds: 0.2 },
    loudnessNormalization: { enabled: false, targetLufs: -14, maxGainDb: 9, minGainDb: -9, useRealtimeMeter: true, externalGainDb: 0 },
    limiter: { enabled: true, thresholdDb: -1, lookaheadMs: 5, attackMs: 0.5, releaseMs: 150, truePeak: true },
    ieq: { enabled: false, strength: 0.5, targetCurve: 'flat', timeConstantSec: 3 },
    dynamicEq: {
      enabled: false,
      strength: 0.5,
      thresholdDb: -20,
      ratio: 2,
      attackMs: 20,
      releaseMs: 200,
      bands: [0, 1, 2, 3, 4].map(() => ({ enabled: true, targetGainDb: 0 })),
    },
    pitch: { enabled: false, semitones: 0, rate: 1, voiceBalance: 0 },
    modulation: {
      enabled: false,
      lfo: { enabled: false, shape: 'sine', rateHz: 1, depth: 0.5 },
      envelope: { enabled: false, attackMs: 10, releaseMs: 200, amount: 0.5 },
      routes: [],
    },
    modEffects: {
      delay: { enabled: false, delayMs: 250, feedback: 0.3, mix: 0.3 },
      chorus: { enabled: false, rateHz: 1, depthMs: 3, mix: 0.4 },
      flanger: { enabled: false, rateHz: 0.5, depthMs: 2, feedback: 0.4, mix: 0.5 },
      phaser: { enabled: false, rateHz: 0.5, depth: 0.5, feedback: 0.4, mix: 0.5, stages: 4 },
      tremolo: { enabled: false, rateHz: 5, depth: 0.5, mix: 1 },
    },
    hearing: { enabled: false },
    spatial: createDefaultSpatialSettings(),
    stereoWidth: 1,
    sceneId: null,
    customized: false,
  }
}

/** 简约 5 段中心频率 */
export const SIMPLE_EQ_FREQUENCIES = [80, 250, 1000, 4000, 12000]
/** 专业 10 段（octave） */
export const PRO_EQ_DEFAULT_BANDS = [31.5, 63, 125, 250, 500, 1000, 2000, 4000, 8000, 16000]
/** 20 段（1/2 octave，20Hz–20kHz） */
export const PRO_EQ_20_BANDS = [20, 31.5, 50, 63, 100, 125, 200, 250, 400, 500, 800, 1000, 1600, 2000, 3200, 4000, 6300, 8000, 12500, 16000, 20000].slice(0, 20)
