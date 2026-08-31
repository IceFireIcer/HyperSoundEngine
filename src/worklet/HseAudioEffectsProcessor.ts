/**
 * HyperSoundEngine v1 —— AudioWorklet 处理器（实时渲染线程）
 *
 * 出处/许可：架构借鉴 Tone.js(MIT) 的 AudioWorklet 消息管道思路（设计文档 §15 /
 *   映射表 #20 🟡 借鉴架构），处理器本体为自研封装，不含第三方代码。
 *
 * 融合打包注意：AudioWorklet 全局作用域**不支持裸 import/export**——引擎与全部 DSP
 *   依赖必须内联进单个处理器文件（esbuild / vite worklet 插件打包后替换本文件内容），
 *   并保留文件末尾的 registerProcessor 守卫；本文件以源码形态给出，便于阅读与单测。
 *
 * 线程模型：
 *   - 构造：以全局 sampleRate 创建 HyperSoundEngine（2 声道）；
 *   - 构造：从 processorOptions.initialParams 应用完整参数快照，完成后回传 ready；
 *   - port.onmessage：仅接收 {type:'reset'} 轻量命令，运行期不解析参数或重建处理链；
 *   - 每 STATS_INTERVAL_CALLBACKS 次 process 回调（约 30×128 帧 ≈ 80ms @48kHz）
 *     向主线程回传一次 {type:'stats', stats: EngineStats}。
 */

import { HyperSoundEngine } from '../engine/HyperSoundEngine'
import type { HyperSoundEngineParams } from '../types'

export const WORKLET_PROCESSOR_NAME = 'hypersoundengine'

/** AudioWorklet 全局作用域环境声明（lib.dom 未内置这些全局符号，故本地声明） */
declare class AudioWorkletProcessor {
  readonly port: MessagePort
  readonly currentTime: number
  readonly currentFrame: number
  readonly sampleRate: number
  constructor(options?: AudioWorkletProcessorOptions)
  process(inputs: Float32Array[][], outputs: Float32Array[][], parameters: Record<string, Float32Array>): boolean
}

interface AudioWorkletProcessorOptions {
  numberOfInputs: number
  numberOfOutputs: number
  outputChannelCount: number[]
  parameterData: Record<string, number>
  processorOptions: unknown
}

declare const sampleRate: number
declare function registerProcessor(name: string, ctor: new (options?: AudioWorkletProcessorOptions) => AudioWorkletProcessor): void

/** stats 回传周期（process 回调次数） */
const STATS_INTERVAL_CALLBACKS = 30

export class HseAudioEffectsProcessor extends AudioWorkletProcessor {
  private readonly engine: HyperSoundEngine
  private readonly inputChannelCount: number
  private readonly inputRefs: Float32Array[]
  private readonly outputRefs: Float32Array[]
  private callbackCount = 0
  private scratch: Float32Array
  private silence: Float32Array

  constructor(options?: AudioWorkletProcessorOptions) {
    super(options)
    const processorOptions = options?.processorOptions as {
      inputChannelCount?: number
      initialParams?: HyperSoundEngineParams
      requestId?: string
    } | undefined
    const requested = processorOptions?.inputChannelCount
    this.inputChannelCount = requested === 6 || requested === 8 ? requested : 2
    // 输入/输出引用数组在构造期固定，process 回调只替换元素，不创建数组。
    this.inputRefs = new Array<Float32Array>(this.inputChannelCount)
    this.outputRefs = new Array<Float32Array>(2)
    this.silence = new Float32Array(128)
    this.scratch = new Float32Array(128)
    // 全局 sampleRate 在 AudioWorklet 全局作用域恒存在（48kHz/44.1kHz 等）
    this.engine = new HyperSoundEngine(sampleRate, this.inputChannelCount)
    this.engine.prepare(128)
    try {
      if (processorOptions?.initialParams) this.engine.setParams(processorOptions.initialParams)
    } catch (error) {
      this.port.postMessage({
        type: 'error',
        phase: 'construct',
        requestId: processorOptions?.requestId,
        message: error instanceof Error ? error.message : String(error),
      })
      return
    }
    this.port.onmessage = (event: MessageEvent) => {
      const msg = event.data as { type?: string }
      if (msg !== null && typeof msg === 'object' && msg.type === 'reset') {
        this.engine.reset()
      }
    }
    if (processorOptions?.requestId) {
      this.port.postMessage({ type: 'ready', requestId: processorOptions.requestId })
    }
  }

  process(inputs: Float32Array[][], outputs: Float32Array[][], _parameters: Record<string, Float32Array>): boolean {
    const outChannels = outputs.length > 0 ? outputs[0] : []
    if (outChannels.length === 0) return true // 无输出通道，保持处理器存活
    const frameCount = outChannels[0].length
    // Web Audio 渲染量子当前固定为 128；超出预分配容量时静音而不在实时线程扩容。
    if (frameCount > this.silence.length) {
      for (let channel = 0; channel < outChannels.length; channel++) outChannels[channel].fill(0)
      return true
    }

    const inChannels = inputs.length > 0 ? inputs[0] : []
    this.outputRefs[0] = outChannels[0]
    this.outputRefs[1] = outChannels.length >= 2 ? outChannels[1] : this.scratch
    if (this.inputChannelCount > 2) {
      for (let channel = 0; channel < this.inputChannelCount; channel++) {
        this.inputRefs[channel] = inChannels[channel] ?? this.silence
      }
      // 多声道专用拓扑由引擎保证：先双耳化全部输入，再执行共享 1–21 级主链。
      this.engine.processMulti(this.inputRefs, this.outputRefs)
    } else {
      const left = inChannels[0] ?? this.silence
      this.inputRefs[0] = left
      this.inputRefs[1] = inChannels[1] ?? left
      this.engine.process(this.inputRefs, this.outputRefs)
    }

    if (outChannels.length < 2) {
      // 防御性单声道宿主：节点正常协商时 outputChannelCount 固定为 2。
      for (let i = 0; i < frameCount; i++) {
        outChannels[0][i] = (outChannels[0][i] + this.scratch[i]) * 0.5
      }
    }

    this.callbackCount++
    if (this.callbackCount >= STATS_INTERVAL_CALLBACKS) {
      this.callbackCount = 0
      // stats + analysis 一并回传：worklet 模式下主线程引擎不接触音频流，
      // 频谱/特征只能由渲染线程回传（否则 UI 分析页频谱静止不动）
      this.port.postMessage({ type: 'stats', stats: this.engine.getStats(), analysis: this.engine.getAnalysis() })
    }
    return true // 保持处理器存活
  }
}

// AudioWorklet 全局作用域下才存在 registerProcessor；Node/测试环境跳过注册。
typeof registerProcessor !== 'undefined' &&
  registerProcessor(WORKLET_PROCESSOR_NAME, HseAudioEffectsProcessor)
