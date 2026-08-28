/**
 * HyperSoundEngineHost —— 引擎宿主接线模块（供 HyperSoundEngine 引擎切换逻辑使用）
 *
 * 只保证一件事：**接入后音频正确经过引擎处理**。
 * 接入语义（关键约束，防新旧双链并联打架）：
 *  - attach：masterGain 全断 → 接入引擎处理节点 → 连 analyser；
 *  - dispose：断开处理节点 → masterGain 全断 → 恢复 masterGain→analyser 直连；
 *  - 幂等：重复 attach 同一 handle 直接 return；
 *  - 竞态：attach 的异步注册期间被 dispose → 完成后放弃接线（防旧节点插进新图）。
 *
 * 接入模式：
 *  - 'worklet'：AudioWorklet 处理器（`worklet/HseAudioEffectsProcessor.ts`，需先打包单文件）——
 *    参数经 `port.postMessage({type:'params'})` 下发，`stats` 周期回传；
 *  - 'script'：ScriptProcessorNode 兜底（已废弃但 Electron/Chromium 可用），
 *    onaudioprocess 内直接调 HyperSoundEngine.process（同一纯 TS 内核，无需打包）；
 *  - 'auto'：优先 worklet，失败自动回退 script（默认）。
 *
 * 确定性/测试：AudioNode 均为鸭子类型（最小接口），Node 测试环境可 stub 验证接线语义。
 */

import { HyperSoundEngine } from '../engine/HyperSoundEngine'
import type { HyperSoundEngineParams, EngineStats, EngineAnalysis } from '../types'
import type { AudioEngine } from '../interfaces'

export type HyperSoundEngineHostMode = 'worklet' | 'script' | 'auto'

/** 最小 AudioNode 接口（鸭子类型；Node 测试环境可用 stub 实现） */
export interface HyperSoundEngineAudioNodeLike {
  connect?(dest: unknown): unknown
  disconnect?(): unknown
  port?: {
    postMessage(msg: unknown): void
    onmessage?: ((e: { data: unknown }) => void) | null
  }
  onaudioprocess?: (e: {
    inputBuffer: { getChannelData(ch: number): Float32Array }
    outputBuffer: { getChannelData(ch: number): Float32Array }
  }) => void
}

export interface HyperSoundEngineAudioContextLike {
  sampleRate: number
  audioWorklet?: { addModule(url: string): Promise<void> }
  createScriptProcessor?(bufferSize: number, inCh: number, outCh: number): HyperSoundEngineAudioNodeLike
}

export interface HyperSoundEngineHostHandle {
  audioContext: HyperSoundEngineAudioContextLike
  masterGain: HyperSoundEngineAudioNodeLike
  analyser: HyperSoundEngineAudioNodeLike
}

export interface HyperSoundEngineHostOptions {
  /** 接入模式，默认 'auto'（worklet 优先，失败回退 script） */
  mode?: HyperSoundEngineHostMode
  /** worklet 打包产物 URL（worklet 模式必需） */
  workletUrl?: string
  /** worklet 处理器注册名，默认 'hypersoundengine' */
  processorName?: string
  /** script 兜底模式的块长，默认 4096 */
  blockSize?: number
  /** 注入引擎实例（测试/离线复用，采样率由调用方保证与上下文一致）；缺省时宿主按上下文采样率自建 */
  engine?: AudioEngine
  /** 自定义引擎工厂：采样率变化/首次创建时由宿主调用；缺省使用内置 HyperSoundEngine */
  engineFactory?: (sampleRate: number, channelCount?: number) => AudioEngine
}

export class HyperSoundEngineHost {
  private engineRef: AudioEngine | null
  private readonly engineInjected: boolean
  private readonly engineFactory: ((sampleRate: number, channelCount?: number) => AudioEngine) | undefined
  private readonly defaultMode: HyperSoundEngineHostMode
  private readonly workletUrl: string | undefined
  private readonly processorName: string
  private readonly blockSize: number

  private handle: HyperSoundEngineHostHandle | null = null
  private node: HyperSoundEngineAudioNodeLike | null = null
  private activeMode: 'worklet' | 'script' | null = null
  private lastParams: HyperSoundEngineParams | null = null
  private lastStats: EngineStats | null = null
  private lastAnalysis: EngineAnalysis | null = null
  private hostFs = 0
  private attachSeq = 0
  private disposed = false
  /** setParams 去重指纹（上次下发参数）；null=尚未下发 */
  private lastParamsKey: string | null = null
  /** IR 引用指纹表：Float32Array 按引用编号参与指纹，不做 O(n) 逐样本序列化 */
  private readonly irIds = new WeakMap<Float32Array, number>()
  private irIdSeq = 0

  constructor(opts?: HyperSoundEngineHostOptions) {
    this.defaultMode = opts?.mode ?? 'auto'
    this.workletUrl = opts?.workletUrl
    this.processorName = opts?.processorName ?? 'hypersoundengine'
    this.blockSize = opts?.blockSize ?? 4096
    this.engineInjected = opts?.engine != null
    this.engineFactory = opts?.engineFactory
    this.engineRef = opts?.engine ?? null
    if (opts?.engine) this.hostFs = NaN // 注入引擎：采样率未知，attach 时不做重建
  }

  /** 引擎实例（惰性创建：attach 时按上下文采样率自建，或返回注入实例） */
  get engine(): AudioEngine {
    if (!this.engineRef) this.engineRef = this.createEngineInstance(this.hostFs > 0 ? this.hostFs : 48000)
    return this.engineRef
  }

  /** 按采样率创建引擎实例（优先自定义工厂，缺省 HyperSoundEngine） */
  private createEngineInstance(sampleRate: number): AudioEngine {
    const engine = this.engineFactory
      ? this.engineFactory(sampleRate, 2)
      : new HyperSoundEngine(sampleRate, 2)
    engine.prepare(this.blockSize)
    return engine
  }

  /**
   * 把引擎接入音频图（幂等：同一 handle 重复调用直接 return）。
   * 语义：masterGain 全断 → 接引擎处理节点 → 连 analyser；防新旧双链并联。
   */
async attach(handle: HyperSoundEngineHostHandle, params?: HyperSoundEngineParams): Promise<void> {
    if (this.handle === handle) {
      if (params) this.setParams(params)
      return
    }
    const seq = ++this.attachSeq
    this.disposed = false
    const ctx = handle.audioContext

    // 采样率校准（仅自建引擎；注入引擎由调用方保证一致）
    if (!this.engineInjected) {
      if (this.engineRef === null || Math.abs(this.hostFs - ctx.sampleRate) > 1) {
        this.engineRef = this.createEngineInstance(ctx.sampleRate)
        this.hostFs = ctx.sampleRate
        if (this.lastParams) this.engineRef.setParams(this.lastParams)
      }
    }

    // ★ 尽早记录 handle：attach 的异步注册期间若被 dispose，
    //   dispose 也能据此恢复 masterGain→analyser 直连（否则音频会死）
    this.handle = handle

    // 先全断 masterGain（避免与旧引擎并联打架）
    try {
      handle.masterGain.disconnect?.()
    } catch {
      /* noop */
    }

    let node: HyperSoundEngineAudioNodeLike | null = null
    let mode: 'worklet' | 'script' | null = null

    // worklet 路径
    if (this.defaultMode === 'auto' || this.defaultMode === 'worklet') {
      const AWNode = (globalThis as { AudioWorkletNode?: new (ctx: unknown, name: string, opts: unknown) => HyperSoundEngineAudioNodeLike })
        .AudioWorkletNode
      if (ctx.audioWorklet?.addModule && AWNode && this.workletUrl) {
        try {
          await ctx.audioWorklet.addModule(this.workletUrl)
          // 竞态防护：注册期间被 dispose/重 attach → 放弃接线
          if (this.disposed || seq !== this.attachSeq) return
          node = new AWNode(ctx, this.processorName, { outputChannelCount: [2] })
          const port = node.port
          if (port) {
            port.onmessage = (e: { data: unknown }) => {
              const d = e.data as { type?: string; stats?: EngineStats; analysis?: EngineAnalysis }
              if (d?.type === 'stats') {
                if (d.stats) this.lastStats = d.stats
                if (d.analysis) this.lastAnalysis = d.analysis
              }
            }
            const p = params ?? this.lastParams
            if (p) port.postMessage({ type: 'params', params: p })
          }
          mode = 'worklet'
        } catch {
          node = null
        }
      }
    }

    // script 兜底路径（同一纯 TS 内核）
    if (!node && (this.defaultMode === 'auto' || this.defaultMode === 'script') && ctx.createScriptProcessor) {
      const sp = ctx.createScriptProcessor(this.blockSize, 2, 2)
      sp.onaudioprocess = (e) => {
        const inL = e.inputBuffer.getChannelData(0)
        const inR = e.inputBuffer.getChannelData(1)
        const outL = e.outputBuffer.getChannelData(0)
        const outR = e.outputBuffer.getChannelData(1)
        this.engine.process([inL, inR], [outL, outR])
      }
      node = sp
      mode = 'script'
    }

    if (!node) {
      // 无可用音频通路：恢复 masterGain 直连后抛错（handle 已提前记录）
      try {
        handle.masterGain.disconnect?.()
      } catch {
        /* noop */
      }
      try {
        handle.masterGain.connect?.(handle.analyser)
      } catch {
        /* noop */
      }
      this.handle = null
      throw new Error('host: no audio path available（worklet 未打包或 script 不可用）')
    }

    handle.masterGain.connect?.(node)
    node.connect?.(handle.analyser)
    this.node = node
    this.activeMode = mode
    if (params) this.lastParams = params
  }

  /** 参数指纹：IR（Float32Array）按引用编号参与，避免对大 IR 做逐样本 JSON 序列化 */
  private paramsKey(p: HyperSoundEngineParams): string {
    return JSON.stringify(p, (_k, v) => {
      if (v instanceof Float32Array) {
        let id = this.irIds.get(v)
        if (id === undefined) {
          id = ++this.irIdSeq
          this.irIds.set(v, id)
        }
        return { __irRef: id, irLen: v.length }
      }
      return v
    })
  }

  /** 下发参数：主线程引擎与 worklet 处理器同步更新；与上次逐字段一致时跳过（去重） */
  setParams(p: HyperSoundEngineParams): void {
    this.lastParams = p
    const key = this.paramsKey(p)
    if (key === this.lastParamsKey) return // 未变化：跳过整链重配与 postMessage
    this.lastParamsKey = key
    this.engine.setParams(p)
    if (this.node?.port) this.node.port.postMessage({ type: 'params', params: p })
  }

  /** 拆除引擎链路并恢复 masterGain→analyser 直连（恢复直连语义） */
  dispose(): void {
    this.disposed = true
    this.attachSeq++
    this.lastParamsKey = null // 拆除后重新下发参数时不再误判"未变化"
    const h = this.handle
    const n = this.node
    this.node = null
    this.handle = null
    this.activeMode = null
    this.lastAnalysis = null
    if (n) {
      try {
        n.disconnect?.()
      } catch {
        /* noop */
      }
    }
    if (h) {
      try {
        h.masterGain.disconnect?.()
      } catch {
        /* noop */
      }
      try {
        h.masterGain.connect?.(h.analyser)
      } catch {
        /* noop */
      }
    }
  }

  /** 当前接入模式（未接入返回 null） */
  getMode(): 'worklet' | 'script' | null {
    return this.activeMode
  }

  /** 最近一次 worklet 回传的统计（script 模式为 null） */
  getLastStats(): EngineStats | null {
    return this.lastStats
  }

  /** 最近一次 worklet 回传的频谱/特征（script 模式为 null；主线程引擎自身可分析） */
  getLastAnalysis(): EngineAnalysis | null {
    return this.lastAnalysis
  }

  /** 当前引擎处理节点（未接入返回 null）。供融合层在 masterGain 与处理节点之间
   *  插入前置节点（如 SoundTouch 变速变调），接线方负责断开重连语义。 */
  getAudioNode(): HyperSoundEngineAudioNodeLike | null {
    return this.node
  }
}

/**
 * 浏览器宿主工厂：创建 HyperSoundEngineHost 实例。
 * 这是接入 Web Audio 图的最简入口：
 *   const host = createHyperSoundEngineHost({ mode: 'auto', workletUrl: '/hse-worklet.js' })
 */
export function createHyperSoundEngineHost(opts?: HyperSoundEngineHostOptions): HyperSoundEngineHost {
  return new HyperSoundEngineHost(opts)
}