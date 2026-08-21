/**
 * HyperSoundEngine v3 独立音频引擎 —— 对外统一接口（Seam）
 *
 * 设计意图（deep module）：
 *  - 外部软件/宿主只需要依赖 `AudioEngine` 这一小接口，就能接入完整引擎；
 *  - `HyperSoundEngine` 是它的具体实现，内部再复杂也不泄漏到调用方；
 *  - `StereoProcessor` 是给希望扩展/替换 DSP 模块的接入方使用的内部接缝，
 *    不是使用引擎的必需入口。
 */

import type { HyperSoundEngineParams, EngineStats, EngineAnalysis } from './types'

/**
 * 音频引擎统一接口。
 *
 * 约定：
 * - `setParams` 每次接收完整参数快照，引擎内部会深拷贝，调用方可安全复用对象；
 * - `process` 就地写入 `outputs`，长度必须不小于对应 `inputs`；
 * - `process` 内零分配（稳态），可在实时音频线程调用；
 * - `getStats` / `getAnalysis` 可在任意线程读取最近一次处理结果。
 */
export interface AudioEngine {
  /** 全量参数快照更新；即时生效，不修改传入对象 */
  setParams(params: HyperSoundEngineParams): void
  /** 返回当前参数快照（深拷贝，外部修改不影响引擎内部状态） */
  getParams(): HyperSoundEngineParams
  /** 预分配内部工作缓冲；实时处理前调用一次，之后 process 内零分配 */
  prepare(maxBlockSize: number): void
  /**
   * 就地处理多声道块（当前实现为单声道/立体声，但接口按通道数组设计）。
   * `sidechain` 为可选外部侧链输入；只有开启 sidechain 的效果器会使用它。
   */
  process(inputs: Float32Array[], outputs: Float32Array[], sidechain?: Float32Array[]): void
  /** 引擎统计：LUFS、LRA、峰值、限幅衰减、延迟 */
  getStats(): EngineStats
  /** 最近一帧频谱与特征；未分析到时为 null */
  getAnalysis(): EngineAnalysis
  /** 引擎当前引入的延迟（样本数） */
  getLatencySamples(): number
  /** 复位所有内部状态（滤波器、包络、响度计、分析缓冲等） */
  reset(): void
}

/** 音频引擎工厂：宿主可用它按采样率创建引擎实例 */
export interface AudioEngineFactory {
  (sampleRate: number, channelCount?: number): AudioEngine
}

/**
 * 立体声 DSP 处理器通用契约（可选扩展点）。
 * 核心 DSP 模块多数已符合此形态；新增自定义处理器时可实现该接口再接入引擎链。
 */
export interface StereoProcessor<TParams = unknown> {
  /** 更新处理器参数（实现方可定义更具体的参数类型） */
  setParams(params: TParams): void
  /** 就地处理左右声道 */
  processStereo(left: Float32Array, right: Float32Array): void
  /** 复位内部状态 */
  reset(): void
}

/**
 * 处理链中的一个阶段（Stage）。
 *
 * `HyperSoundEngine` 内部用 `ProcessingStage[]` 描述 14 级处理链：
 *  - `active()` 决定本块是否执行（旁路语义）；
 *  - `run()` 对左右声道就地处理；
 *  - 顺序即数组顺序，新增/调整处理阶段只需增删数组元素。
 */
export interface ProcessingStage {
  readonly id: string
  /** 当前参数下是否激活；false 时引擎跳过该阶段 */
  active(): boolean
  /** 处理一个音频块（就地改写 left/right） */
  run(left: Float32Array, right: Float32Array, frameCount: number): void
  /** 可选：引擎 reset() 时同步复位该阶段内部状态 */
  reset?(): void
}
