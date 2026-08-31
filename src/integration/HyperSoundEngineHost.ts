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
 *  - 'worklet'：AudioWorklet 处理器；engineBackend='ts' 使用既有 TS 引擎，
 *    engineBackend='wasm' 使用完整 HseEngine wasm 与 ready requestId 协议；参数快照通过预建节点交叉淡变替换；
 *  - 'script'：ScriptProcessorNode 兜底（已废弃但 Electron/Chromium 可用），
 *    onaudioprocess 内直接调 HyperSoundEngine.process（同一纯 TS 内核，无需打包）；
 *  - 'auto'：优先所选 worklet；wasm 初始化失败时回退 TS worklet，再回退 script（默认）。
 *
 * 确定性/测试：AudioNode 均为鸭子类型（最小接口），Node 测试环境可 stub 验证接线语义。
 */

import { HyperSoundEngine } from '../engine/HyperSoundEngine'
import type { HyperSoundEngineParams, EngineStats, EngineAnalysis } from '../types'
import type { HrtfGrid } from '../spatial/types'
import type { AudioEngine } from '../interfaces'

export type HyperSoundEngineHostMode = 'worklet' | 'script' | 'auto'
export type HyperSoundEngineBackend = 'ts' | 'wasm'
export type HyperSoundEngineInputChannelCount = 2 | 6 | 8

/** 最小 AudioParam 接口，仅覆盖 wasm 节点替换所需的自动化能力。 */
export interface HyperSoundEngineAudioParamLike {
  value: number
  cancelScheduledValues?(time: number): void
  setValueAtTime?(value: number, time: number): unknown
  linearRampToValueAtTime?(value: number, endTime: number): unknown
}

/** 最小 AudioNode 接口（鸭子类型；Node 测试环境可用 stub 实现） */
export interface HyperSoundEngineAudioNodeLike {
  connect?(dest: unknown): unknown
  disconnect?(dest?: unknown): unknown
  port?: {
    postMessage(msg: unknown): void
    onmessage?: ((e: { data: unknown }) => void) | null
    start?(): void
  }
  onaudioprocess?: (e: {
    inputBuffer: { getChannelData(ch: number): Float32Array }
    outputBuffer: { getChannelData(ch: number): Float32Array }
  }) => void
  gain?: HyperSoundEngineAudioParamLike
}

export interface HyperSoundEngineAudioContextLike {
  sampleRate: number
  currentTime?: number
  audioWorklet?: { addModule(url: string): Promise<void> }
  createScriptProcessor?(bufferSize: number, inCh: number, outCh: number): HyperSoundEngineAudioNodeLike
  createGain?(): HyperSoundEngineAudioNodeLike
}

export interface HyperSoundEngineHostHandle {
  audioContext: HyperSoundEngineAudioContextLike
  masterGain: HyperSoundEngineAudioNodeLike
  analyser: HyperSoundEngineAudioNodeLike
}

export interface HyperSoundEngineHostOptions {
  /** 接入模式，默认 'auto'（worklet 优先，失败回退 script） */
  mode?: HyperSoundEngineHostMode
  /** worklet 打包产物 URL（TS worklet 或 auto 回退 TS worklet 时必需） */
  workletUrl?: string
  /** worklet 内核，默认 'ts'；workletBackend 是等价别名 */
  engineBackend?: HyperSoundEngineBackend
  workletBackend?: HyperSoundEngineBackend
  /** wasm 专用 worklet 打包产物 URL */
  wasmWorkletUrl?: string
  /** hse_wasm_bg.wasm URL；主线程 fetch 后编译为 WebAssembly.Module */
  wasmUrl?: string
  /** 可选 SOFA URL；主线程 fetch 一次并将 bytes 传给 wasm worklet 构造阶段 */
  hrtfUrl?: string
  /** 可选 SOFA bytes 或预解析 HRTF grid；与 hrtfUrl 互斥 */
  hrtf?: ArrayBuffer | HrtfGrid
  /** wasm ready 回执超时，默认 2000ms */
  wasmRequestTimeoutMs?: number
  /** worklet 参数替换的交叉淡变窗口，默认 20ms */
  workletCrossfadeMs?: number
  /** @deprecated 使用 workletCrossfadeMs；保留为兼容别名 */
  wasmCrossfadeMs?: number
  /** worklet 处理器注册名，默认 'hypersoundengine' */
  processorName?: string
  /** 输入总线声道数；支持立体声、5.1、7.1，输出始终为双声道。默认 2。 */
  inputChannelCount?: HyperSoundEngineInputChannelCount
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
  private readonly engineBackend: HyperSoundEngineBackend
  private readonly wasmWorkletUrl: string | undefined
  private readonly wasmUrl: string | undefined
  private readonly hrtfUrl: string | undefined
  private readonly hrtf: ArrayBuffer | HrtfGrid | undefined
  private readonly wasmRequestTimeoutMs: number
  private readonly workletCrossfadeMs: number
  private readonly processorName: string
  private readonly inputChannelCount: HyperSoundEngineInputChannelCount
  private readonly blockSize: number
  private readonly scriptInputRefs: Float32Array[]
  private readonly scriptStereoInputRefs: Float32Array[] = new Array<Float32Array>(2)
  private readonly scriptOutputRefs: Float32Array[] = new Array<Float32Array>(2)

  private handle: HyperSoundEngineHostHandle | null = null
  private node: HyperSoundEngineAudioNodeLike | null = null
  private outputGain: HyperSoundEngineAudioNodeLike | null = null
  private retiringWorkletPath: {
    handle: HyperSoundEngineHostHandle
    node: HyperSoundEngineAudioNodeLike
    gain: HyperSoundEngineAudioNodeLike
  } | null = null
  private wasmModulePromise: Promise<WebAssembly.Module> | null = null
  private hrtfPromise: Promise<ArrayBuffer | HrtfGrid | undefined> | null = null
  private readonly wasmWorkletModulePromises = new WeakMap<object, Promise<void>>()
  private workletReplacement: Promise<void> = Promise.resolve()
  private readonly crossfadeWaiters = new Set<() => void>()
  private activeMode: 'worklet' | 'script' | null = null
  private activeBackend: HyperSoundEngineBackend | null = null
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
  private requestSeq = 0
  private readonly pendingRequests = new Map<string, {
    resolve(): void
    reject(error: Error): void
    timeoutId: ReturnType<typeof setTimeout>
  }>()

  constructor(opts?: HyperSoundEngineHostOptions) {
    this.defaultMode = opts?.mode ?? 'auto'
    this.workletUrl = opts?.workletUrl
    if (opts?.engineBackend && opts?.workletBackend && opts.engineBackend !== opts.workletBackend) {
      throw new Error('host: engineBackend 与 workletBackend 不能冲突')
    }
    this.engineBackend = opts?.engineBackend ?? opts?.workletBackend ?? 'ts'
    this.wasmWorkletUrl = opts?.wasmWorkletUrl
    this.wasmUrl = opts?.wasmUrl
    this.hrtfUrl = opts?.hrtfUrl
    this.hrtf = opts?.hrtf
    if (this.hrtfUrl && this.hrtf) {
      throw new Error('host: hrtfUrl 与 hrtf 不能同时设置')
    }
    this.wasmRequestTimeoutMs = opts?.wasmRequestTimeoutMs ?? 2000
    if (!Number.isFinite(this.wasmRequestTimeoutMs) || this.wasmRequestTimeoutMs < 0) {
      throw new RangeError('host: wasmRequestTimeoutMs 必须是非负有限数')
    }
    const crossfadeMs = opts?.workletCrossfadeMs ?? opts?.wasmCrossfadeMs ?? 20
    this.workletCrossfadeMs = crossfadeMs
    if (!Number.isFinite(this.workletCrossfadeMs) || this.workletCrossfadeMs < 0) {
      throw new RangeError('host: workletCrossfadeMs 必须是非负有限数')
    }
    this.processorName = opts?.processorName ?? 'hypersoundengine'
    this.inputChannelCount = opts?.inputChannelCount ?? 2
    if (this.inputChannelCount !== 2 && this.inputChannelCount !== 6 && this.inputChannelCount !== 8) {
      throw new RangeError('host: inputChannelCount 必须是 2、6 或 8')
    }
    this.scriptInputRefs = new Array<Float32Array>(this.inputChannelCount)
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
      ? this.engineFactory(sampleRate, this.inputChannelCount)
      : new HyperSoundEngine(sampleRate, this.inputChannelCount)
    engine.prepare(this.blockSize)
    return engine
  }

  private createRequestId(kind: 'ready'): string {
    return `${kind}-${++this.requestSeq}`
  }

  private rejectPendingRequests(error: Error): void {
    for (const pending of this.pendingRequests.values()) {
      clearTimeout(pending.timeoutId)
      pending.reject(error)
    }
    this.pendingRequests.clear()
  }

  private handleWorkletMessage(data: unknown): void {
    const d = data as {
      type?: string
      requestId?: string
      phase?: string
      message?: string
      stats?: EngineStats
      analysis?: EngineAnalysis
    }
    if (d?.type === 'stats') {
      if (d.stats) this.lastStats = d.stats
      if (d.analysis) this.lastAnalysis = d.analysis
      return
    }
    if (d?.type === 'ready' && d.requestId) {
      const pending = this.pendingRequests.get(d.requestId)
      if (!pending) return
      clearTimeout(pending.timeoutId)
      this.pendingRequests.delete(d.requestId)
      pending.resolve()
      return
    }
    if (d?.type !== 'error') return
    const error = new Error(d.message ?? 'wasm worklet reported an error')
    error.name = 'HyperSoundEngineWasmError'
    const pending = d.requestId ? this.pendingRequests.get(d.requestId) : undefined
    if (pending) {
      clearTimeout(pending.timeoutId)
      this.pendingRequests.delete(d.requestId!)
      pending.reject(error)
    } else if (d.phase === 'construct' || d.phase === 'process') {
      this.rejectPendingRequests(error)
    }
  }

  private waitForRequest(requestId: string): Promise<void> {
    return new Promise((resolve, reject) => {
      const timeoutId = setTimeout(() => {
        this.pendingRequests.delete(requestId)
        reject(new Error(`host: worklet ${requestId} timed out after ${this.wasmRequestTimeoutMs}ms`))
      }, this.wasmRequestTimeoutMs)
      this.pendingRequests.set(requestId, { resolve, reject, timeoutId })
    })
  }

  private bindPort(node: HyperSoundEngineAudioNodeLike): void {
    if (!node.port) return
    node.port.onmessage = (event) => this.handleWorkletMessage(event.data)
    node.port.start?.()
  }

  private async createTsWorkletNode(
    ctx: HyperSoundEngineAudioContextLike,
    AWNode: new (ctx: unknown, name: string, opts: unknown) => HyperSoundEngineAudioNodeLike,
    params?: HyperSoundEngineParams,
  ): Promise<HyperSoundEngineAudioNodeLike> {
    if (!ctx.audioWorklet?.addModule || !this.workletUrl) {
      throw new Error('host: TS worklet 资源不可用')
    }
    if (!ctx.createGain) {
      throw new Error('host: TS worklet 参数替换要求 AudioContext.createGain')
    }
    await ctx.audioWorklet.addModule(this.workletUrl)
    const requestId = this.createRequestId('ready')
    const node = new AWNode(ctx, this.processorName, {
      numberOfInputs: 1,
      numberOfOutputs: 1,
      channelCount: this.inputChannelCount,
      channelCountMode: 'max',
      channelInterpretation: 'discrete',
      outputChannelCount: [2],
      processorOptions: {
        inputChannelCount: this.inputChannelCount,
        initialParams: params,
        requestId,
      },
    })
    this.bindPort(node)
    try {
      await this.waitForRequest(requestId)
      return node
    } catch (error) {
      try { node.disconnect?.() } catch { /* noop */ }
      throw error
    }
  }

  private loadHrtf(): Promise<ArrayBuffer | HrtfGrid | undefined> {
    this.hrtfPromise ??= this.hrtfUrl
      ? (async () => {
          const response = await fetch(this.hrtfUrl!)
          if (!response.ok) throw new Error(`host: HRTF fetch failed (${response.status})`)
          return response.arrayBuffer()
        })()
      : Promise.resolve(this.hrtf)
    return this.hrtfPromise
  }

  private async createWasmWorkletNode(
    ctx: HyperSoundEngineAudioContextLike,
    AWNode: new (ctx: unknown, name: string, opts: unknown) => HyperSoundEngineAudioNodeLike,
    params?: HyperSoundEngineParams,
  ): Promise<HyperSoundEngineAudioNodeLike> {
    if (this.inputChannelCount !== 2) {
      throw new Error('host: wasm backend 当前仅支持 2 声道输入')
    }
    if (!ctx.audioWorklet?.addModule || !this.wasmWorkletUrl || !this.wasmUrl) {
      throw new Error('host: wasm 模式要求 wasmWorkletUrl 与 wasmUrl')
    }
    if (!ctx.createGain) {
      throw new Error('host: wasm 参数替换要求 AudioContext.createGain')
    }
    this.wasmModulePromise ??= (async () => {
      const response = await fetch(this.wasmUrl!)
      if (!response.ok) throw new Error(`host: wasm fetch failed (${response.status})`)
      return WebAssembly.compile(await response.arrayBuffer())
    })()
    let workletModulePromise = this.wasmWorkletModulePromises.get(ctx as object)
    if (!workletModulePromise) {
      workletModulePromise = ctx.audioWorklet.addModule(this.wasmWorkletUrl)
      this.wasmWorkletModulePromises.set(ctx as object, workletModulePromise)
    }
    const [wasmModule, , hrtf] = await Promise.all([
      this.wasmModulePromise,
      workletModulePromise,
      this.loadHrtf(),
    ])
    const requestId = this.createRequestId('ready')
    const node = new AWNode(ctx, 'hypersoundengine-wasm', {
      numberOfInputs: 2,
      numberOfOutputs: 1,
      outputChannelCount: [2],
      processorOptions: {
        wasmModule,
        maxFrames: 128,
        params: params ?? this.lastParams ?? {},
        hrtf,
        requestId,
      },
    })
    this.bindPort(node)
    try {
      await this.waitForRequest(requestId)
      return node
    } catch (error) {
      try {
        node.disconnect?.()
      } catch {
        /* noop */
      }
      throw error
    }
  }

  private createOutputGain(
    ctx: HyperSoundEngineAudioContextLike,
    initialValue: number,
  ): HyperSoundEngineAudioNodeLike {
    const gainNode = ctx.createGain?.()
    if (!gainNode?.gain) throw new Error('host: worklet 参数替换要求 AudioContext.createGain')
    gainNode.gain.value = initialValue
    return gainNode
  }

  private connectWorkletPath(
    handle: HyperSoundEngineHostHandle,
    node: HyperSoundEngineAudioNodeLike,
    gainNode: HyperSoundEngineAudioNodeLike,
  ): void {
    handle.masterGain.connect?.(node)
    node.connect?.(gainNode)
    gainNode.connect?.(handle.analyser)
  }

  private disconnectWorkletPath(
    handle: HyperSoundEngineHostHandle,
    node: HyperSoundEngineAudioNodeLike,
    gainNode: HyperSoundEngineAudioNodeLike,
  ): void {
    try { handle.masterGain.disconnect?.(node) } catch { /* noop */ }
    try { node.disconnect?.() } catch { /* noop */ }
    try { gainNode.disconnect?.() } catch { /* noop */ }
  }

  private fadeGain(gainNode: HyperSoundEngineAudioNodeLike, from: number, to: number, startTime: number, endTime: number): void {
    const gain = gainNode.gain
    if (!gain) throw new Error('host: worklet output gain is unavailable')
    gain.cancelScheduledValues?.(startTime)
    if (gain.setValueAtTime && gain.linearRampToValueAtTime) {
      gain.setValueAtTime(from, startTime)
      gain.linearRampToValueAtTime(to, endTime)
    } else {
      gain.value = to
    }
  }

  private waitForAudioTime(ctx: HyperSoundEngineAudioContextLike, endTime: number): Promise<void> {
    if ((ctx.currentTime ?? endTime) >= endTime || this.workletCrossfadeMs === 0) return Promise.resolve()
    return new Promise((resolve) => {
      let timer: ReturnType<typeof setTimeout> | null = null
      let settled = false
      const finish = () => {
        if (settled) return
        settled = true
        if (timer !== null) clearTimeout(timer)
        this.crossfadeWaiters.delete(finish)
        resolve()
      }
      const check = () => {
        if (this.disposed || (ctx.currentTime ?? endTime) >= endTime) {
          finish()
          return
        }
        timer = setTimeout(check, Math.min(5, this.workletCrossfadeMs))
      }
      this.crossfadeWaiters.add(finish)
      check()
    })
  }

  private releaseCrossfadeWaiters(): void {
    for (const finish of [...this.crossfadeWaiters]) finish()
  }

  /**
   * 把引擎接入音频图（幂等：同一 handle 重复调用直接 return）。
   * 语义：masterGain 全断 → 接引擎处理节点 → 连 analyser；防新旧双链并联。
   */
  async attach(handle: HyperSoundEngineHostHandle, params?: HyperSoundEngineParams): Promise<void> {
    if (this.handle === handle) {
      if (params) await this.setParams(params)
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
    if (params) {
      this.lastParams = params
      this.lastParamsKey = this.paramsKey(params)
      this.engine.setParams(params)
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
    let backend: HyperSoundEngineBackend | null = null
    let workletError: unknown = null

    // worklet 路径：wasm auto 失败时先回退 TS worklet，再回退 script。
    if (this.defaultMode === 'auto' || this.defaultMode === 'worklet') {
      const AWNode = (globalThis as { AudioWorkletNode?: new (ctx: unknown, name: string, opts: unknown) => HyperSoundEngineAudioNodeLike })
        .AudioWorkletNode
      if (ctx.audioWorklet?.addModule && AWNode) {
        const p = params ?? this.lastParams ?? undefined
        if (this.engineBackend === 'wasm') {
          try {
            node = await this.createWasmWorkletNode(ctx, AWNode, p)
            backend = 'wasm'
          } catch (error) {
            workletError = error
            node = null
          }
        }
        if (!node && (this.engineBackend === 'ts' || this.defaultMode === 'auto')) {
          try {
            node = await this.createTsWorkletNode(ctx, AWNode, p)
            backend = 'ts'
          } catch {
            node = null
          }
        }
        // 竞态防护：注册/握手期间被 dispose/重 attach → 放弃接线。
        if (this.disposed || seq !== this.attachSeq) {
          try {
            node?.disconnect?.()
          } catch {
            /* noop */
          }
          return
        }
        if (node) mode = 'worklet'
      }
    }

    // script 兜底路径（同一纯 TS 内核）
    if (!node && (this.defaultMode === 'auto' || this.defaultMode === 'script') && ctx.createScriptProcessor) {
      const sp = ctx.createScriptProcessor(this.blockSize, this.inputChannelCount, 2)
      sp.onaudioprocess = (e) => {
        for (let channel = 0; channel < this.inputChannelCount; channel++) {
          this.scriptInputRefs[channel] = e.inputBuffer.getChannelData(channel)
        }
        this.scriptOutputRefs[0] = e.outputBuffer.getChannelData(0)
        this.scriptOutputRefs[1] = e.outputBuffer.getChannelData(1)
        if (this.inputChannelCount > 2 && this.engine.processMulti) {
          this.engine.processMulti(this.scriptInputRefs, this.scriptOutputRefs)
        } else {
          this.scriptStereoInputRefs[0] = this.scriptInputRefs[0]
          this.scriptStereoInputRefs[1] = this.scriptInputRefs[1]
          this.engine.process(this.scriptStereoInputRefs, this.scriptOutputRefs)
        }
      }
      node = sp
      mode = 'script'
      backend = 'ts'
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
      if (
        this.defaultMode === 'worklet'
        && workletError instanceof Error
        && workletError.name === 'HyperSoundEngineWasmError'
      ) throw workletError
      throw new Error('host: no audio path available（worklet 未打包或 script 不可用）')
    }

    if (mode === 'worklet') {
      const gainNode = this.createOutputGain(ctx, 1)
      this.connectWorkletPath(handle, node, gainNode)
      this.outputGain = gainNode
    } else {
      handle.masterGain.connect?.(node)
      node.connect?.(handle.analyser)
      this.outputGain = null
    }
    this.node = node
    this.activeMode = mode
    this.activeBackend = backend
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

  /**
   * 整体替换参数快照。worklet 后端在宿主侧预建完整新节点并交叉淡变；
   * 不迁移 DSP 状态，旧节点仅在淡变窗口内继续贡献尾音。script 后端仍在控制线程原位更新。
   */
  setParams(p: HyperSoundEngineParams): Promise<void> {
    const key = this.paramsKey(p)
    if (key === this.lastParamsKey) return Promise.resolve()
    if (this.activeMode !== 'worklet' || !this.node?.port) {
      this.lastParams = p
      this.lastParamsKey = key
      this.engine.setParams(p)
      return Promise.resolve()
    }

    const replacement = this.workletReplacement.then(async () => {
      const handle = this.handle
      const oldNode = this.node
      const oldGain = this.outputGain
      const backend = this.activeBackend
      const AWNode = (globalThis as {
        AudioWorkletNode?: new (ctx: unknown, name: string, opts: unknown) => HyperSoundEngineAudioNodeLike
      }).AudioWorkletNode
      if (!handle || !oldNode || !oldGain || !backend || !AWNode || this.disposed) {
        throw new Error('host: worklet audio path is unavailable')
      }

      const seq = this.attachSeq
      const nextNode = backend === 'wasm'
        ? await this.createWasmWorkletNode(handle.audioContext, AWNode, p)
        : await this.createTsWorkletNode(handle.audioContext, AWNode, p)
      if (this.disposed || seq !== this.attachSeq || this.node !== oldNode) {
        try { nextNode.disconnect?.() } catch { /* noop */ }
        throw new Error('host: worklet replacement was superseded')
      }

      try {
        this.engine.setParams(p)
      } catch (error) {
        try { nextNode.disconnect?.() } catch { /* noop */ }
        throw error
      }

      let nextGain: HyperSoundEngineAudioNodeLike
      try {
        nextGain = this.createOutputGain(handle.audioContext, 0)
        this.connectWorkletPath(handle, nextNode, nextGain)
      } catch (error) {
        try { nextNode.disconnect?.() } catch { /* noop */ }
        throw error
      }

      const now = handle.audioContext.currentTime ?? 0
      const renderQuantumSeconds = this.workletCrossfadeMs === 0
        ? 0
        : 128 / handle.audioContext.sampleRate
      const startTime = now + renderQuantumSeconds
      const endTime = startTime + this.workletCrossfadeMs / 1000
      this.fadeGain(oldGain, 1, 0, startTime, endTime)
      this.fadeGain(nextGain, 0, 1, startTime, endTime)
      const retiringPath = { handle, node: oldNode, gain: oldGain }
      this.retiringWorkletPath = retiringPath
      this.node = nextNode
      this.outputGain = nextGain
      this.lastParams = p
      this.lastParamsKey = key

      await this.waitForAudioTime(handle.audioContext, endTime)
      this.disconnectWorkletPath(handle, oldNode, oldGain)
      if (this.retiringWorkletPath === retiringPath) this.retiringWorkletPath = null
      if (this.disposed || seq !== this.attachSeq) {
        this.disconnectWorkletPath(handle, nextNode, nextGain)
      }
    })
    this.workletReplacement = replacement.catch(() => {})
    return replacement
  }

  /** 复位主线程引擎及当前 worklet 内核状态 */
  reset(): void {
    this.engine.reset()
    this.node?.port?.postMessage({ type: 'reset' })
  }

  /** 拆除引擎链路并恢复 masterGain→analyser 直连（恢复直连语义） */
  dispose(): void {
    this.disposed = true
    this.attachSeq++
    this.rejectPendingRequests(new Error('host: disposed'))
    this.releaseCrossfadeWaiters()
    this.lastParamsKey = null // 拆除后重新下发参数时不再误判"未变化"
    const h = this.handle
    const n = this.node
    const gain = this.outputGain
    const retiring = this.retiringWorkletPath
    this.retiringWorkletPath = null
    this.node = null
    this.outputGain = null
    this.handle = null
    this.activeMode = null
    this.activeBackend = null
    this.lastAnalysis = null
    if (n) {
      try {
        n.disconnect?.()
      } catch {
        /* noop */
      }
    }
    if (retiring) {
      this.disconnectWorkletPath(retiring.handle, retiring.node, retiring.gain)
    }
    if (gain) {
      try {
        gain.disconnect?.()
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

  /** 当前实际引擎后端（未接入返回 null；script 恒为 ts） */
  getEngineBackend(): HyperSoundEngineBackend | null {
    return this.activeBackend
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