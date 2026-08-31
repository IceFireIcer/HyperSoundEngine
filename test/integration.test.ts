/**
 * HyperSoundEngineHost 单元测试 —— 引擎切换接线模块
 * 物理意义（切换正确性）：
 *  - attach：masterGain 全断 → 接入引擎节点 → 连 analyser（防新旧双链并联打架）；
 *  - dispose：恢复 masterGain→analyser 直连（恢复直连语义）；
 *  - 幂等 / 竞态（异步注册期间被 dispose → 放弃接线且直连已恢复）；
 *  - script 兜底通路：onaudioprocess 里音频真实经过 HyperSoundEngine 处理（限幅生效）。
 */
import { describe, it, expect, vi, beforeEach, afterEach, type Mock } from 'vitest'
import { HyperSoundEngineHost, type HyperSoundEngineAudioContextLike, type HyperSoundEngineHostHandle } from '../src/integration/HyperSoundEngineHost'
import { createDefaultParams } from '../src/types'
import type { AudioEngine } from '../src/interfaces'

// ---------------------------------------------------------------- stubs

class FakeNode {
  // HyperSoundEngine 侧 vitest 4：vi.fn() 需带实现签名才能赋给 HyperSoundEngineAudioNodeLike 的鸭子类型接口
  connect = vi.fn((_dest: unknown) => undefined)
  disconnect = vi.fn((_dest?: unknown) => undefined)
  port: { postMessage: Mock<(msg: unknown) => void>; onmessage: ((e: { data: unknown }) => void) | null }
  onaudioprocess?: (e: unknown) => void
  constructor() {
    this.port = { postMessage: vi.fn((_msg: unknown) => undefined), onmessage: null }
  }
}

class FakeAudioParam {
  value = 1
  setValueAtTime = vi.fn((value: number, _time: number) => { this.value = value })
  linearRampToValueAtTime = vi.fn((value: number, _time: number) => { this.value = value })
  cancelScheduledValues = vi.fn((_time: number) => undefined)
}

class FakeGainNode extends FakeNode {
  gain = new FakeAudioParam()
}

function makeHandle(opts?: { addModuleImpl?: () => Promise<void> }): {
  handle: HyperSoundEngineHostHandle
  ctx: HyperSoundEngineAudioContextLike
  masterGain: FakeNode
  analyser: FakeNode
  scriptNodes: FakeNode[]
  gainNodes: FakeGainNode[]
} {
  const masterGain = new FakeNode()
  const analyser = new FakeNode()
  const scriptNodes: FakeNode[] = []
  const gainNodes: FakeGainNode[] = []
  const ctx: HyperSoundEngineAudioContextLike = {
    sampleRate: 48000,
    currentTime: 10,
    audioWorklet: {
      addModule: vi.fn(opts?.addModuleImpl ?? (async () => {})),
    },
    createScriptProcessor: vi.fn((_bs: number, _i: number, _o: number) => {
      const n = new FakeNode()
      scriptNodes.push(n)
      return n
    }),
    createGain: vi.fn(() => {
      const gain = new FakeGainNode()
      gainNodes.push(gain)
      return gain
    }),
  }
  return { handle: { audioContext: ctx, masterGain, analyser }, ctx, masterGain, analyser, scriptNodes, gainNodes }
}

function advanceAudioTime(ctx: HyperSoundEngineAudioContextLike, seconds: number): void {
  ctx.currentTime = (ctx.currentTime ?? 0) + seconds
}

/** 注册 AudioWorkletNode 全局桩 */
function stubWorkletNode(onCreate?: (node: FakeNode, name: string, opts: unknown) => void): new (ctx: unknown, name: string, opts: unknown) => FakeNode {
  const cls = class AWNodeStub extends FakeNode {
    constructor(_ctx: unknown, name: string, opts: unknown) {
      super()
      onCreate?.(this, name, opts)
      const requestId = (opts as { processorOptions?: { requestId?: string } }).processorOptions?.requestId
      if (requestId && !onCreate) {
        queueMicrotask(() => this.port.onmessage?.({ data: { type: 'ready', requestId } }))
      }
    }
  }
  vi.stubGlobal('AudioWorkletNode', cls)
  return cls
}

function stubWasmFetchAndCompile(): void {
  vi.stubGlobal('fetch', vi.fn(async () => ({
    ok: true,
    status: 200,
    arrayBuffer: async () => new ArrayBuffer(8),
  })))
  vi.spyOn(WebAssembly, 'compile').mockResolvedValue({} as WebAssembly.Module)
}

function fullScaleSine(n: number, fs: number, freq = 440): Float32Array {
  const x = new Float32Array(n)
  for (let i = 0; i < n; i++) x[i] = Math.sin((2 * Math.PI * freq * i) / fs)
  return x
}

beforeEach(() => {
  vi.restoreAllMocks()
  vi.unstubAllGlobals()
})
afterEach(() => {
  vi.unstubAllGlobals()
})

describe('HyperSoundEngineHost —— worklet 模式', () => {
  it('attach：参数通过 processorOptions 初始化，ready 后接入带输出增益的 worklet 路径', async () => {
    let created: { node: FakeNode; opts: unknown } | undefined
    stubWorkletNode((node, _name, opts) => {
      created = { node, opts }
      queueMicrotask(() => {
        const requestId = (opts as { processorOptions: { requestId: string } }).processorOptions.requestId
        node.port.onmessage?.({ data: { type: 'ready', requestId } })
      })
    })
    const { handle, masterGain, analyser, gainNodes } = makeHandle()
    const host = new HyperSoundEngineHost({ mode: 'worklet', workletUrl: '/hse-worklet.js' })
    const params = createDefaultParams(48000)
    await host.attach(handle, params)

    expect(masterGain.disconnect).toHaveBeenCalledTimes(1)
    expect(analyser.connect).not.toHaveBeenCalled() // analyser 是目标，不向外连
    expect(created?.opts).toMatchObject({
      processorOptions: { inputChannelCount: 2, initialParams: params },
    })
    expect(created?.node.port.postMessage).not.toHaveBeenCalledWith(expect.objectContaining({ type: 'params' }))
    expect(masterGain.connect).toHaveBeenCalledWith(created?.node)
    expect(created?.node.connect).toHaveBeenCalledWith(gainNodes[0])
    expect(gainNodes[0].connect).toHaveBeenCalledWith(analyser)
    expect(host.getMode()).toBe('worklet')
    host.dispose()
  })

  it('6/8 路 TS worklet 显式协商输入声道且输出固定为 2', async () => {
    for (const inputChannelCount of [6, 8] as const) {
      let options: unknown
      stubWorkletNode((node, _name, opts) => {
        options = opts
        queueMicrotask(() => {
          const requestId = (opts as { processorOptions: { requestId: string } }).processorOptions.requestId
          node.port.onmessage?.({ data: { type: 'ready', requestId } })
        })
      })
      const { handle } = makeHandle()
      const host = new HyperSoundEngineHost({
        mode: 'worklet',
        workletUrl: '/hse-worklet.js',
        inputChannelCount,
      })
      await host.attach(handle)
      expect(options).toEqual({
        numberOfInputs: 1,
        numberOfOutputs: 1,
        channelCount: inputChannelCount,
        channelCountMode: 'max',
        channelInterpretation: 'discrete',
        outputChannelCount: [2],
        processorOptions: expect.objectContaining({ inputChannelCount }),
      })
      host.dispose()
      vi.unstubAllGlobals()
    }
  })

  it('幂等：同一 handle 重复 attach 不重复接线', async () => {
    stubWorkletNode()
    const { handle, masterGain } = makeHandle()
    const host = new HyperSoundEngineHost({ mode: 'worklet', workletUrl: '/hse-worklet.js' })
    await host.attach(handle)
    await host.attach(handle)
    expect(masterGain.connect).toHaveBeenCalledTimes(1)
    host.dispose()
  })

  it('dispose：断开节点 + masterGain 全断 + 恢复 masterGain→analyser 直连', async () => {
    stubWorkletNode()
    const { handle, masterGain, analyser } = makeHandle()
    const host = new HyperSoundEngineHost({ mode: 'worklet', workletUrl: '/hse-worklet.js' })
    await host.attach(handle)
    const node = (masterGain.connect as ReturnType<typeof vi.fn>).mock.calls[0][0] as FakeNode

    host.dispose()
    expect(node.disconnect).toHaveBeenCalledTimes(1)
    expect(masterGain.disconnect).toHaveBeenCalled()
    // 恢复直连：masterGain.connect(analyser)
    const connects = (masterGain.connect as ReturnType<typeof vi.fn>).mock.calls
    expect(connects[connects.length - 1][0]).toBe(analyser)
    expect(host.getMode()).toBe(null)
  })

  it('竞态：addModule 挂起期间被 dispose → 放弃接线且 masterGain 直连已恢复', async () => {
    stubWorkletNode()
    let resolveAdd!: () => void
    const gate = new Promise<void>((res) => {
      resolveAdd = res
    })
    const { handle, masterGain, analyser } = makeHandle({ addModuleImpl: () => gate })
    const host = new HyperSoundEngineHost({ mode: 'worklet', workletUrl: '/hse-worklet.js' })
    const attaching = host.attach(handle) // 挂起在 addModule
    host.dispose()
    resolveAdd()
    await attaching
    // 接线被放弃，但 dispose 已恢复直连（masterGain 只连了 analyser）
    const connects = (masterGain.connect as ReturnType<typeof vi.fn>).mock.calls
    expect(connects.length).toBe(1)
    expect(connects[0][0]).toBe(analyser)
    expect(host.getMode()).toBe(null)
  })

  it('setParams：TS worklet 预建新节点并等待音频时间走完淡变才断开旧链', async () => {
    vi.useFakeTimers()
    const workletNodes: FakeNode[] = []
    const options: unknown[] = []
    stubWorkletNode((node, _name, opts) => {
      workletNodes.push(node)
      options.push(opts)
      queueMicrotask(() => {
        const requestId = (opts as { processorOptions: { requestId: string } }).processorOptions.requestId
        node.port.onmessage?.({ data: { type: 'ready', requestId } })
      })
    })
    const { handle, ctx, gainNodes } = makeHandle()
    const host = new HyperSoundEngineHost({ mode: 'worklet', workletUrl: '/hse-worklet.js', workletCrossfadeMs: 20 })
    await host.attach(handle)
    const p = createDefaultParams(48000)
    const replacing = host.setParams(p)
    await vi.advanceTimersByTimeAsync(0)

    expect(workletNodes).toHaveLength(2)
    expect(options[1]).toMatchObject({ processorOptions: { initialParams: p } })
    expect(workletNodes[0].port.postMessage).not.toHaveBeenCalledWith(expect.objectContaining({ type: 'params' }))
    const quantumSeconds = 128 / 48000
    expect(gainNodes[0].gain.setValueAtTime).toHaveBeenCalledWith(1, 10 + quantumSeconds)
    expect(gainNodes[1].gain.setValueAtTime).toHaveBeenCalledWith(0, 10 + quantumSeconds)
    expect(gainNodes[0].gain.linearRampToValueAtTime).toHaveBeenCalledWith(0, 10.02 + quantumSeconds)
    expect(gainNodes[1].gain.linearRampToValueAtTime).toHaveBeenCalledWith(1, 10.02 + quantumSeconds)

    await vi.advanceTimersByTimeAsync(100)
    expect(workletNodes[0].disconnect).not.toHaveBeenCalled()
    advanceAudioTime(ctx, 0.02 + quantumSeconds)
    await vi.advanceTimersByTimeAsync(5)
    await replacing
    expect(workletNodes[0].disconnect).toHaveBeenCalledTimes(1)
    host.dispose()
    vi.useRealTimers()
  })

  it('并发 setParams 串行替换，后一次以最新活动节点为旧链', async () => {
    vi.useFakeTimers()
    const workletNodes: FakeNode[] = []
    stubWorkletNode((node, _name, opts) => {
      workletNodes.push(node)
      queueMicrotask(() => {
        const requestId = (opts as { processorOptions: { requestId: string } }).processorOptions.requestId
        node.port.onmessage?.({ data: { type: 'ready', requestId } })
      })
    })
    const { handle, ctx } = makeHandle()
    const host = new HyperSoundEngineHost({ mode: 'worklet', workletUrl: '/hse-worklet.js' })
    await host.attach(handle)
    const firstParams = createDefaultParams(48000)
    firstParams.stereoWidth = 0.8
    const secondParams = createDefaultParams(48000)
    secondParams.stereoWidth = 0.6

    const first = host.setParams(firstParams)
    const second = host.setParams(secondParams)
    await vi.advanceTimersByTimeAsync(0)
    expect(workletNodes).toHaveLength(2)
    advanceAudioTime(ctx, 0.02 + 128 / 48000)
    await vi.advanceTimersByTimeAsync(5)
    await first
    await vi.advanceTimersByTimeAsync(0)
    expect(workletNodes).toHaveLength(3)
    expect(workletNodes[1].disconnect).not.toHaveBeenCalled()
    advanceAudioTime(ctx, 0.02 + 128 / 48000)
    await vi.advanceTimersByTimeAsync(5)
    await second
    expect(workletNodes[1].disconnect).toHaveBeenCalledTimes(1)
    expect(host.getAudioNode()).toBe(workletNodes[2])
    host.dispose()
    vi.useRealTimers()
  })

  it('TS 替换节点构造失败时保留当前可听链', async () => {
    const workletNodes: FakeNode[] = []
    stubWorkletNode((node, _name, opts) => {
      workletNodes.push(node)
      queueMicrotask(() => {
        const requestId = (opts as { processorOptions: { requestId: string } }).processorOptions.requestId
        node.port.onmessage?.({
          data: workletNodes.length === 1
            ? { type: 'ready', requestId }
            : { type: 'error', phase: 'construct', requestId, message: 'invalid params' },
        })
      })
    })
    const { handle, masterGain } = makeHandle()
    const host = new HyperSoundEngineHost({ mode: 'worklet', workletUrl: '/hse-worklet.js' })
    await host.attach(handle)

    await expect(host.setParams(createDefaultParams(48000))).rejects.toThrow('invalid params')
    expect(host.getAudioNode()).toBe(workletNodes[0])
    expect(workletNodes[0].disconnect).not.toHaveBeenCalled()
    expect(masterGain.disconnect).not.toHaveBeenCalledWith(workletNodes[0])
    host.dispose()
  })

  it('TS 淡变等待期间 dispose 立即清理新旧路径并结束更新', async () => {
    vi.useFakeTimers()
    const workletNodes: FakeNode[] = []
    stubWorkletNode((node, _name, opts) => {
      workletNodes.push(node)
      queueMicrotask(() => {
        const requestId = (opts as { processorOptions: { requestId: string } }).processorOptions.requestId
        node.port.onmessage?.({ data: { type: 'ready', requestId } })
      })
    })
    const { handle, gainNodes } = makeHandle()
    const host = new HyperSoundEngineHost({ mode: 'worklet', workletUrl: '/hse-worklet.js' })
    await host.attach(handle)
    const replacing = host.setParams(createDefaultParams(48000))
    await vi.advanceTimersByTimeAsync(0)

    host.dispose()
    await replacing
    expect(workletNodes[0].disconnect).toHaveBeenCalled()
    expect(workletNodes[1].disconnect).toHaveBeenCalled()
    expect(gainNodes[0].disconnect).toHaveBeenCalled()
    expect(gainNodes[1].disconnect).toHaveBeenCalled()
    vi.useRealTimers()
  })
})

describe('HyperSoundEngineHost —— wasm worklet', () => {
  it('编译 wasm module，通过 processorOptions 传入并等待 ready 后接线', async () => {
    stubWasmFetchAndCompile()
    let created: { node: FakeNode; name: string; opts: unknown } | undefined
    stubWorkletNode((node, name, opts) => {
      created = { node, name, opts }
      queueMicrotask(() => {
        const requestId = (opts as { processorOptions: { requestId: string } }).processorOptions.requestId
        node.port.onmessage?.({ data: { type: 'ready', requestId } })
      })
    })
    const { handle, ctx, masterGain } = makeHandle()
    const params = createDefaultParams(48000)
    const host = new HyperSoundEngineHost({
      mode: 'worklet',
      engineBackend: 'wasm',
      wasmWorkletUrl: '/hse-wasm-worklet.js',
      wasmUrl: '/hse.wasm',
    })

    await host.attach(handle, params)

    expect(fetch).toHaveBeenCalledWith('/hse.wasm')
    expect(WebAssembly.compile).toHaveBeenCalled()
    expect(ctx.audioWorklet?.addModule).toHaveBeenCalledWith('/hse-wasm-worklet.js')
    expect(created?.name).toBe('hypersoundengine-wasm')
    expect(created?.opts).toMatchObject({
      numberOfInputs: 2,
      numberOfOutputs: 1,
      outputChannelCount: [2],
      processorOptions: { maxFrames: 128, params },
    })
    expect(masterGain.connect).toHaveBeenCalledTimes(1)
    expect(host.getMode()).toBe('worklet')
    expect(host.getEngineBackend()).toBe('wasm')
    host.dispose()
  })

  it('拒绝同时配置 hrtfUrl 与直接 HRTF 数据', () => {
    expect(() => new HyperSoundEngineHost({
      hrtfUrl: '/room.sofa',
      hrtf: new ArrayBuffer(4),
    })).toThrow('hrtfUrl 与 hrtf 不能同时设置')
  })

  it('HRTF URL 只 fetch 一次，节点替换复用同一 SOFA bytes 与 wasm module', async () => {
    const wasmBytes = new ArrayBuffer(8)
    const sofaBytes = new ArrayBuffer(16)
    vi.stubGlobal('fetch', vi.fn(async (url: string) => ({
      ok: true,
      status: 200,
      arrayBuffer: async () => url === '/room.sofa' ? sofaBytes : wasmBytes,
    })))
    const wasmModule = {} as WebAssembly.Module
    vi.spyOn(WebAssembly, 'compile').mockResolvedValue(wasmModule)
    const options: Array<{ processorOptions: { requestId: string; wasmModule: WebAssembly.Module; hrtf?: ArrayBuffer } }> = []
    stubWorkletNode((node, _name, opts) => {
      const typed = opts as typeof options[number]
      options.push(typed)
      queueMicrotask(() => {
        node.port.onmessage?.({ data: { type: 'ready', requestId: typed.processorOptions.requestId } })
      })
    })
    const { handle } = makeHandle()
    const host = new HyperSoundEngineHost({
      mode: 'worklet',
      engineBackend: 'wasm',
      wasmWorkletUrl: '/hse-wasm-worklet.js',
      wasmUrl: '/hse.wasm',
      hrtfUrl: '/room.sofa',
      workletCrossfadeMs: 0,
    })

    await host.attach(handle)
    const params = createDefaultParams(48000)
    params.stereoWidth = 0.5
    await host.setParams(params)

    expect(fetch).toHaveBeenCalledTimes(2)
    expect(fetch).toHaveBeenCalledWith('/hse.wasm')
    expect(fetch).toHaveBeenCalledWith('/room.sofa')
    expect(WebAssembly.compile).toHaveBeenCalledTimes(1)
    expect(options).toHaveLength(2)
    expect(options[0].processorOptions.wasmModule).toBe(wasmModule)
    expect(options[1].processorOptions.wasmModule).toBe(wasmModule)
    expect(options[0].processorOptions.hrtf).toBe(sofaBytes)
    expect(options[1].processorOptions.hrtf).toBe(sofaBytes)
    host.dispose()
  })

  it('预解析 HRTF grid 直接进入 processorOptions，不额外 fetch', async () => {
    stubWasmFetchAndCompile()
    const grid = {
      sampleRate: 48000,
      azimuths: [-30, 30],
      elevations: [0],
      hrirLength: 1,
      left: new Float32Array([1, 0]),
      right: new Float32Array([0, 1]),
    }
    let processorOptions: { hrtf?: unknown; requestId: string } | undefined
    stubWorkletNode((node, _name, opts) => {
      processorOptions = (opts as { processorOptions: { hrtf?: unknown; requestId: string } }).processorOptions
      queueMicrotask(() => {
        node.port.onmessage?.({ data: { type: 'ready', requestId: processorOptions?.requestId } })
      })
    })
    const { handle } = makeHandle()
    const host = new HyperSoundEngineHost({
      mode: 'worklet',
      engineBackend: 'wasm',
      wasmWorkletUrl: '/hse-wasm-worklet.js',
      wasmUrl: '/hse.wasm',
      hrtf: grid,
    })

    await host.attach(handle)

    expect(fetch).toHaveBeenCalledTimes(1)
    expect(processorOptions?.hrtf).toBe(grid)
    host.dispose()
  })

  it('setParams 预建新节点并在固定窗口交叉淡变，旧尾音在窗口结束后截断', async () => {
    vi.useFakeTimers()
    stubWasmFetchAndCompile()
    const workletNodes: FakeNode[] = []
    stubWorkletNode((node, _name, opts) => {
      workletNodes.push(node)
      queueMicrotask(() => {
        const requestId = (opts as { processorOptions: { requestId: string } }).processorOptions.requestId
        node.port.onmessage?.({ data: { type: 'ready', requestId } })
      })
    })
    const { handle, ctx, masterGain, analyser, gainNodes } = makeHandle()
    const host = new HyperSoundEngineHost({
      mode: 'worklet',
      workletBackend: 'wasm',
      wasmWorkletUrl: '/hse-wasm-worklet.js',
      wasmUrl: '/hse.wasm',
      wasmCrossfadeMs: 20,
    })
    await host.attach(handle)
    expect(workletNodes).toHaveLength(1)
    expect(gainNodes).toHaveLength(1)

    const params = createDefaultParams(48000)
    const replacing = host.setParams(params)
    await vi.advanceTimersByTimeAsync(0)
    expect(workletNodes).toHaveLength(2)
    expect(gainNodes).toHaveLength(2)
    expect(workletNodes[1].port.postMessage).not.toHaveBeenCalledWith(expect.objectContaining({ type: 'configure' }))
    expect(masterGain.connect).toHaveBeenCalledWith(workletNodes[1])
    expect(workletNodes[1].connect).toHaveBeenCalledWith(gainNodes[1])
    expect(gainNodes[1].connect).toHaveBeenCalledWith(analyser)
    const quantumSeconds = 128 / 48000
    expect(gainNodes[0].gain.setValueAtTime).toHaveBeenCalledWith(1, 10 + quantumSeconds)
    expect(gainNodes[0].gain.linearRampToValueAtTime).toHaveBeenCalledWith(0, 10.02 + quantumSeconds)
    expect(gainNodes[1].gain.setValueAtTime).toHaveBeenCalledWith(0, 10 + quantumSeconds)
    expect(gainNodes[1].gain.linearRampToValueAtTime).toHaveBeenCalledWith(1, 10.02 + quantumSeconds)

    expect(workletNodes[0].disconnect).not.toHaveBeenCalled()
    await vi.advanceTimersByTimeAsync(19)
    expect(workletNodes[0].disconnect).not.toHaveBeenCalled()
    advanceAudioTime(ctx, 0.02 + quantumSeconds)
    await vi.advanceTimersByTimeAsync(1)
    await replacing
    expect(masterGain.disconnect).toHaveBeenCalledWith(workletNodes[0])
    expect(workletNodes[0].disconnect).toHaveBeenCalledTimes(1)
    expect(gainNodes[0].disconnect).toHaveBeenCalledTimes(1)

    host.reset()
    expect(workletNodes[1].port.postMessage).toHaveBeenCalledWith({ type: 'reset' })
    host.dispose()
    vi.useRealTimers()
  })

  it('淡变期间 dispose 会立即断开新旧两条 wasm 路径', async () => {
    vi.useFakeTimers()
    stubWasmFetchAndCompile()
    const workletNodes: FakeNode[] = []
    stubWorkletNode((node, _name, opts) => {
      workletNodes.push(node)
      queueMicrotask(() => {
        const requestId = (opts as { processorOptions: { requestId: string } }).processorOptions.requestId
        node.port.onmessage?.({ data: { type: 'ready', requestId } })
      })
    })
    const { handle, gainNodes } = makeHandle()
    const host = new HyperSoundEngineHost({
      mode: 'worklet',
      engineBackend: 'wasm',
      wasmWorkletUrl: '/hse-wasm-worklet.js',
      wasmUrl: '/hse.wasm',
      wasmCrossfadeMs: 20,
    })
    await host.attach(handle)

    const replacing = host.setParams(createDefaultParams(48000))
    await vi.advanceTimersByTimeAsync(0)
    host.dispose()

    expect(workletNodes[0].disconnect).toHaveBeenCalled()
    expect(workletNodes[1].disconnect).toHaveBeenCalled()
    expect(gainNodes[0].disconnect).toHaveBeenCalled()
    expect(gainNodes[1].disconnect).toHaveBeenCalled()
    await vi.advanceTimersByTimeAsync(20)
    await replacing
    vi.useRealTimers()
  })

  it('替换节点构造失败时保留当前可听链', async () => {
    stubWasmFetchAndCompile()
    const workletNodes: FakeNode[] = []
    stubWorkletNode((node, _name, opts) => {
      workletNodes.push(node)
      queueMicrotask(() => {
        const requestId = (opts as { processorOptions: { requestId: string } }).processorOptions.requestId
        node.port.onmessage?.({
          data: workletNodes.length === 1
            ? { type: 'ready', requestId }
            : { type: 'error', phase: 'construct', requestId, message: 'invalid params' },
        })
      })
    })
    const { handle, masterGain } = makeHandle()
    const host = new HyperSoundEngineHost({
      mode: 'worklet',
      engineBackend: 'wasm',
      wasmWorkletUrl: '/hse-wasm-worklet.js',
      wasmUrl: '/hse.wasm',
    })
    await host.attach(handle)

    await expect(host.setParams(createDefaultParams(48000))).rejects.toThrow('invalid params')
    expect(host.getAudioNode()).toBe(workletNodes[0])
    expect(workletNodes[0].disconnect).not.toHaveBeenCalled()
    expect(masterGain.disconnect).not.toHaveBeenCalledWith(workletNodes[0])
    host.dispose()
  })

  it('auto 下 6 路配置跳过仅立体声 wasm 并回退 TS worklet', async () => {
    const names: string[] = []
    stubWorkletNode((node, name, opts) => {
      names.push(name)
      queueMicrotask(() => {
        const requestId = (opts as { processorOptions: { requestId: string } }).processorOptions.requestId
        node.port.onmessage?.({ data: { type: 'ready', requestId } })
      })
    })
    const { handle } = makeHandle()
    const host = new HyperSoundEngineHost({
      mode: 'auto',
      engineBackend: 'wasm',
      inputChannelCount: 6,
      wasmWorkletUrl: '/hse-wasm-worklet.js',
      wasmUrl: '/hse.wasm',
      workletUrl: '/hse-worklet.js',
    })

    await host.attach(handle)

    expect(names).toEqual(['hypersoundengine'])
    expect(host.getEngineBackend()).toBe('ts')
    host.dispose()
  })

  it('worklet 模式将 wasm 构造错误明确返回给调用方', async () => {
    stubWasmFetchAndCompile()
    stubWorkletNode((node, _name, opts) => {
      queueMicrotask(() => {
        const requestId = (opts as { processorOptions: { requestId: string } }).processorOptions.requestId
        node.port.onmessage?.({
          data: {
            type: 'error',
            phase: 'construct',
            code: 'engine-build-failed',
            requestId,
            message: 'spatial.mode="instant" 需要 HRTF grid',
          },
        })
      })
    })
    const { handle } = makeHandle()
    const host = new HyperSoundEngineHost({
      mode: 'worklet',
      engineBackend: 'wasm',
      wasmWorkletUrl: '/hse-wasm-worklet.js',
      wasmUrl: '/hse.wasm',
    })
    const params = createDefaultParams(48000)
    params.spatial!.mode = 'instant'

    await expect(host.attach(handle, params)).rejects.toThrow('需要 HRTF grid')
  })

  it('worklet 模式缺少 wasm 资源时失败且不静默改用 TS', async () => {
    stubWorkletNode()
    const { handle } = makeHandle()
    const host = new HyperSoundEngineHost({ mode: 'worklet', engineBackend: 'wasm' })
    await expect(host.attach(handle)).rejects.toThrow('no audio path')
  })

  it('auto 下 wasm 构造错误回退 TS worklet', async () => {
    stubWasmFetchAndCompile()
    const names: string[] = []
    stubWorkletNode((node, name, opts) => {
      names.push(name)
      if (name === 'hypersoundengine-wasm') {
        queueMicrotask(() => {
          const requestId = (opts as { processorOptions: { requestId: string } }).processorOptions.requestId
          node.port.onmessage?.({ data: { type: 'error', phase: 'construct', requestId, message: 'bad wasm' } })
        })
      } else {
        queueMicrotask(() => {
          const requestId = (opts as { processorOptions: { requestId: string } }).processorOptions.requestId
          node.port.onmessage?.({ data: { type: 'ready', requestId } })
        })
      }
    })
    const { handle } = makeHandle()
    const host = new HyperSoundEngineHost({
      mode: 'auto',
      engineBackend: 'wasm',
      wasmWorkletUrl: '/hse-wasm-worklet.js',
      wasmUrl: '/hse.wasm',
      workletUrl: '/hse-worklet.js',
    })

    await host.attach(handle)

    expect(names).toEqual(['hypersoundengine-wasm', 'hypersoundengine'])
    expect(host.getMode()).toBe('worklet')
    expect(host.getEngineBackend()).toBe('ts')
    host.dispose()
  })

  it('auto 下 wasm 与 TS worklet 均失败后回退 script', async () => {
    stubWasmFetchAndCompile()
    stubWorkletNode()
    const { handle } = makeHandle({ addModuleImpl: async () => {
      throw new Error('module failed')
    } })
    const host = new HyperSoundEngineHost({
      mode: 'auto',
      engineBackend: 'wasm',
      wasmWorkletUrl: '/hse-wasm-worklet.js',
      wasmUrl: '/hse.wasm',
      workletUrl: '/hse-worklet.js',
    })

    await host.attach(handle)

    expect(host.getMode()).toBe('script')
    expect(host.getEngineBackend()).toBe('ts')
    host.dispose()
  })
})

describe('HyperSoundEngineHost —— script 兜底模式（切换后音频真实经过引擎处理）', () => {
  it('无 AudioWorkletNode 时自动回退 script；onaudioprocess 通路限幅生效', async () => {
    // Node 环境本身无 AudioWorkletNode；显式置 undefined（afterEach unstub 恢复）
    // 确保"宿主无 worklet"语义不依赖运行环境，模拟回退 ScriptProcessor 的条件
    vi.stubGlobal('AudioWorkletNode', undefined)
    const { handle, masterGain, scriptNodes } = makeHandle()
    const host = new HyperSoundEngineHost({ mode: 'auto', workletUrl: '/hse-worklet.js' })
    await host.attach(handle)
    expect(host.getMode()).toBe('script')
    expect(scriptNodes.length).toBe(1)
    expect(masterGain.connect).toHaveBeenCalledTimes(1)

    // 构造处理事件：满幅 440Hz 正弦 → 引擎限幅（默认 -1dBFS）应压到 ≤0.9
    const sp = scriptNodes[0]
    const handler = sp.onaudioprocess!
    const fs = 48000
    const B = 4096
    const outMax = (_blockIdx: number) => {
      const inL = fullScaleSine(B, fs)
      const inR = new Float32Array(B)
      const outL = new Float32Array(B)
      const outR = new Float32Array(B)
      handler({
        inputBuffer: { getChannelData: (ch: number) => (ch === 0 ? inL : inR) },
        outputBuffer: { getChannelData: (ch: number) => (ch === 0 ? outL : outR) },
      })
      let m = 0
      for (let i = 0; i < B; i++) m = Math.max(m, Math.abs(outL[i]))
      return m
    }
    for (let b = 0; b < 4; b++) outMax(b) // 预热（限幅 lookahead + attack）
    const peak = outMax(4)
    expect(peak).toBeLessThanOrEqual(0.9) // -1dBFS ≈ 0.891，含平滑余量
    expect(peak).toBeGreaterThan(0.5) // 确实有信号通过（非静音）
    host.dispose()
  })

  it('6 路 script 模式以 6 入 2 出创建并调用多声道引擎入口', async () => {
    vi.stubGlobal('AudioWorkletNode', undefined)
    const { handle, ctx, scriptNodes } = makeHandle()
    const host = new HyperSoundEngineHost({ mode: 'script', inputChannelCount: 6 })
    await host.attach(handle)
    expect(ctx.createScriptProcessor).toHaveBeenCalledWith(4096, 6, 2)

    const inputs = Array.from({ length: 6 }, () => new Float32Array(4096))
    inputs[4][0] = 0.5
    const outputs = [new Float32Array(4096), new Float32Array(4096)]
    scriptNodes[0].onaudioprocess!({
      inputBuffer: { getChannelData: (channel: number) => inputs[channel] },
      outputBuffer: { getChannelData: (channel: number) => outputs[channel] },
    })
    expect(outputs[0].every(Number.isFinite)).toBe(true)
    expect(outputs[1].every(Number.isFinite)).toBe(true)
    host.dispose()
  })

  it('注入的旧 AudioEngine 无 processMulti 时回退 ch0/ch1 双声道 process', async () => {
    vi.stubGlobal('AudioWorkletNode', undefined)
    const process = vi.fn((inputs: Float32Array[], outputs: Float32Array[]) => {
      expect(inputs).toHaveLength(2)
      outputs[0].set(inputs[0])
      outputs[1].set(inputs[1])
    })
    const engine: AudioEngine = {
      setParams: vi.fn(),
      getParams: () => createDefaultParams(48000),
      prepare: vi.fn(),
      process,
      getStats: vi.fn(() => ({
        lufsIntegrated: -Infinity,
        lufsMomentary: -Infinity,
        lra: 0,
        peakDb: -Infinity,
        truePeakDb: -Infinity,
        limiterReductionDb: 0,
        engineLatencySamples: 0,
      })),
      getAnalysis: vi.fn(() => ({ spectrum: null, features: null })),
      getLatencySamples: vi.fn(() => 0),
      reset: vi.fn(),
    }
    const { handle, scriptNodes } = makeHandle()
    const host = new HyperSoundEngineHost({ mode: 'script', inputChannelCount: 6, engine })
    await host.attach(handle)

    const inputs = Array.from({ length: 6 }, (_, channel) => new Float32Array(16).fill(channel + 1))
    const outputs = [new Float32Array(16), new Float32Array(16)]
    scriptNodes[0].onaudioprocess!({
      inputBuffer: { getChannelData: (channel: number) => inputs[channel] },
      outputBuffer: { getChannelData: (channel: number) => outputs[channel] },
    })

    expect(process).toHaveBeenCalledTimes(1)
    expect(outputs[0]).toEqual(inputs[0])
    expect(outputs[1]).toEqual(inputs[1])
    host.dispose()
  })

  it('worklet 注册失败也回退 script（auto）', async () => {
    stubWorkletNode()
    const { handle } = makeHandle({
      addModuleImpl: async () => {
        throw new Error('worklet module failed')
      },
    })
    const host = new HyperSoundEngineHost({ mode: 'auto', workletUrl: '/hse-worklet.js' })
    await host.attach(handle)
    expect(host.getMode()).toBe('script')
    host.dispose()
  })

  it('worklet 可用时 auto 优先 worklet', async () => {
    stubWorkletNode()
    const { handle } = makeHandle()
    const host = new HyperSoundEngineHost({ mode: 'auto', workletUrl: '/hse-worklet.js' })
    await host.attach(handle)
    expect(host.getMode()).toBe('worklet')
    host.dispose()
  })

  it('两种模式都不可用 → attach 抛错且 masterGain 直连已恢复', async () => {
    const { handle, masterGain, analyser } = makeHandle()
    // 去掉 audioWorklet 与 script
    ;(handle.audioContext as { audioWorklet?: unknown }).audioWorklet = undefined
    ;(handle.audioContext as { createScriptProcessor?: unknown }).createScriptProcessor = undefined
    const host = new HyperSoundEngineHost({ mode: 'auto', workletUrl: '/hse-worklet.js' })
    await expect(host.attach(handle)).rejects.toThrow('no audio path')
    const connects = (masterGain.connect as ReturnType<typeof vi.fn>).mock.calls
    expect(connects.length).toBe(1)
    expect(connects[0][0]).toBe(analyser)
  })
})
