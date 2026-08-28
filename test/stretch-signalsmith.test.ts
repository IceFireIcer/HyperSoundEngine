/**
 * test/stretch-signalsmith.test.ts —— signalsmith-stretch 可选适配路径专项测试
 *
 * 背景（官方包现实，实测 v1.3.2）：
 *  - signalsmith-stretch v1.x 的 default 导出是 AudioWorklet 工厂
 *    `(audioContext, channelOptions) => Promise<AudioNode>`，异步、需要 AudioContext，
 *    无法在纯 JS 环境同步驱动（README 明确其为 Web Audio/WASM 包装）。
 *  - HseStretch（src/dsp/HseStretch.ts）的探测 isSignalsmithAvailable() 只认可
 *    "同步纯 DSP 类接口"（模块具名导出 HseStretch 类），因此对官方 v1.x 包探测必为 false，
 *    适配器胶水 _processWithSignalsmith 成为当前依赖组合下不可达的死代码，
 *    processStereo 始终回退自研相位声码器。
 *
 * 本文件的策略（三层）：
 *  1. 探测现实：镜像 HseStretch 的探测逻辑记录环境事实，并断言 HseStretch 探测结果与之一致；
 *  2. 胶水路径：官方包无法同步驱动 ⇒ 将满足适配器契约的"确定替身"注入其唯一激活缝
 *     （私有静态 HseStretch._signalsmith，try/finally 恢复），端到端驱动真实的
 *     _processWithSignalsmith 代码（2048 分块流式、交织/解交织、targetFrames 记账、防御回退）；
 *     替身为确定性最近邻重采样（无随机），兼作适配器搬运正确性的 oracle；
 *  3. 真实 DSP：describe.skipIf 保留"包暴露同步纯 DSP 类接口"时的端到端测试
 *     （长度 ±8% / 有限非静音 / 过零率频率 / 位级确定性 / reset 语义），
 *     当前环境（官方 v1.x）下该组整体跳过，属预期绿灯。
 *
 * 顺带记录的适配层缺口（由测试固化）：
 *  - 无 flush/收尾：process 返回帧数不足时，输出长度仍恒为 round(n·rate)，尾部静音补零；
 *  - 方法以解绑形式调用：DSP 对象方法须为闭包/自有属性风格（不依赖 this），
 *    类原型方法形态会 TypeError 被吞掉并静默回退自研；
 *  - 每次 processStereo 新建内部实例：跨调用无状态延续（对流式多次调用场景不保连续性）。
 */
import { beforeAll, describe, expect, it } from 'vitest'
import { HseStretch } from '../src/dsp/HseStretch'

const FS = 48000

// ----------------------------------------------------------------------
// 确定性信号与物理量工具（与 stretch.test.ts 同约定：无随机、循环合成）
// ----------------------------------------------------------------------

function sine(fs: number, freq: number, seconds: number, amp = 0.5): Float32Array {
  const n = Math.round(fs * seconds)
  const x = new Float32Array(n)
  for (let i = 0; i < n; i++) x[i] = amp * Math.sin((2 * Math.PI * freq * i) / fs)
  return x
}

/** 过零率法估计频率（纯正弦精度 <0.1%）；统计中间 50% 区间避开边缘淡入淡出 */
function estimateFreq(x: Float32Array, fs: number): number {
  const start = Math.floor(x.length * 0.25)
  const end = Math.floor(x.length * 0.75)
  let crossings = 0
  for (let i = start + 1; i < end; i++) {
    if ((x[i - 1] < 0) !== (x[i] < 0)) crossings++
  }
  return crossings / 2 / ((end - start) / fs)
}

function identical(a: Float32Array, b: Float32Array): boolean {
  if (a.length !== b.length) return false
  for (let i = 0; i < a.length; i++) {
    if (a[i] !== b[i]) return false // NaN !== NaN 亦按不一致处理
  }
  return true
}

function allFinite(x: Float32Array): boolean {
  for (let i = 0; i < x.length; i++) {
    if (!Number.isFinite(x[i])) return false
  }
  return true
}

function peak(x: Float32Array): number {
  let p = 0
  for (let i = 0; i < x.length; i++) {
    const a = Math.abs(x[i])
    if (a > p) p = a
  }
  return p
}

function tailAllZero(x: Float32Array, from: number): boolean {
  for (let i = from; i < x.length; i++) {
    if (x[i] !== 0) return false
  }
  return true
}

// ----------------------------------------------------------------------
// 探测（镜像 HseStretch.isSignalsmithAvailable 的逻辑，顶层 await 记录环境事实）
// ----------------------------------------------------------------------

interface ProbeFacts {
  /** 动态 import 是否成功（可选依赖是否安装） */
  imported: boolean
  /** 是否满足 HseStretch 探测要求的"同步纯 DSP 类接口"（具名导出 HseStretch 类） */
  hasSyncClassInterface: boolean
  /** default 导出是否为函数（官方包形态：AudioWorklet 工厂） */
  defaultIsFunction: boolean
}

const probe: ProbeFacts = await (async (): Promise<ProbeFacts> => {
  try {
    const spec = 'signalsmith-stretch' // 变量形式：与 HseStretch 相同，避免 TS 静态解析缺失模块
    const mod: unknown = await import(spec)
    const m = mod as { HseStretch?: unknown; default?: unknown }
    return {
      imported: true,
      hasSyncClassInterface: typeof m.HseStretch === 'function',
      defaultIsFunction: typeof m.default === 'function',
    }
  } catch {
    return { imported: false, hasSyncClassInterface: false, defaultIsFunction: false }
  }
})()

// ----------------------------------------------------------------------
// 适配器激活缝：私有静态 HseStretch._signalsmith 的注入/恢复
// （isSignalsmithAvailable 是其唯一正常写入点；官方包探测结果为 null，无法同步驱动，
//   故用满足适配器契约的确定替身注入以驱动真实胶水代码；try/finally 保证恢复）
// ----------------------------------------------------------------------

function withSignalsmithStatic<T>(value: unknown, fn: () => T): T {
  const holder = HseStretch as unknown as { _signalsmith: unknown }
  const saved = holder._signalsmith
  holder._signalsmith = value
  try {
    return fn()
  } finally {
    holder._signalsmith = saved
  }
}

/** 适配器对替身实例的一次完整调用记录（构造参数 / 参数下发 / 收到的块帧数序列） */
interface FakeCallRecord {
  ctorArgs: [number, number]
  semitones: number | null
  rate: number | null
  chunkFrames: number[]
}

interface FakeModule {
  HseStretch: new (channels: number, block: number) => unknown
  calls: FakeCallRecord[]
}

/**
 * 确定替身：满足 _processWithSignalsmith 期望的接口契约
 * （ctor(channels, block) / reset? / setTransposeSemitones / setTimeFactor /
 *   process(interleavedInput, interleavedOutput, frames) => 写入的交织样本数）。
 * DSP 行为 = 确定性最近邻时间重采样（rate=1 恒等、rate=0.5 每帧取 2 ⇒ 频率 ×2、
 * rate=2 每帧重复 2 次 ⇒ 频率 ÷2），无随机、无时钟，可作 oracle。
 *
 * 实现形态注意（适配层契约缺口，实测发现）：适配器把 DSP 方法从对象上**解绑后直接调用**
 * （`(s.setTransposeSemitones)(v)`、`process(in, out, cnt)`），因此 DSP 对象必须用
 * 闭包/自有属性风格的方法（不依赖 this 绑定）；类原型方法形态会在严格模式下抛
 * TypeError 被适配器吞掉并静默回退自研。本替身即闭包风格。
 */
function makeFakeModule(opts: { partialReturn?: boolean } = {}): FakeModule {
  const calls: FakeCallRecord[] = []
  function makeInstance(channels: number, block: number): Record<string, unknown> {
    const rec: FakeCallRecord = { ctorArgs: [channels, block], semitones: null, rate: null, chunkFrames: [] }
    calls.push(rec)
    let rate = 1
    return {
      reset: (): void => {
        // 替身无跨块状态；保留方法以覆盖适配器的 reset 探测分支
      },
      setTransposeSemitones: (v: number): void => {
        rec.semitones = v
      },
      setTimeFactor: (v: number): void => {
        rate = v
        rec.rate = v
      },
      process: (input: Float32Array, output: Float32Array, samples: number): number => {
        rec.chunkFrames.push(samples)
        const want = Math.round(samples * rate)
        let emitted = Math.min(want, Math.floor(output.length / 2))
        if (opts.partialReturn) emitted = Math.floor(emitted / 2) // 模拟"欠产"（真实实现存在启动延迟）
        const last = Math.max(0, samples - 1)
        for (let i = 0; i < emitted; i++) {
          const src = Math.min(last, Math.floor(i / rate))
          output[i * 2] = input[src * 2]
          output[i * 2 + 1] = input[src * 2 + 1]
        }
        return emitted * 2
      },
    }
  }
  // 构造函数形态：new makeInstance(2, 2048) —— 返回对象字面量即替身实例
  return { HseStretch: makeInstance as unknown as new (channels: number, block: number) => unknown, calls }
}

// ----------------------------------------------------------------------
// 1. 探测现实（始终运行）
// ----------------------------------------------------------------------

describe('signalsmith 可选依赖探测现实', () => {
  it('isSignalsmithAvailable 返回布尔；官方 v1.x（AudioWorklet 异步包装）不满足同步类接口', async () => {
    const holder = HseStretch as unknown as { _signalsmith: unknown }
    const saved = holder._signalsmith
    try {
      const ok = await HseStretch.isSignalsmithAvailable()
      expect(typeof ok).toBe('boolean')
      if (probe.imported && !probe.hasSyncClassInterface) {
        // 已安装官方 v1.x（^1.0.0）：default 导出为 AudioWorklet 工厂（返回 Promise<AudioNode>），
        // 无具名 HseStretch 同步类导出 ⇒ 探测必须为 false，processStereo 绝不走 signalsmith 路径。
        // （若未来包内新增同步类导出，本条件不成立，下方 skipIf 组将接管真实 DSP 测试。）
        expect(ok).toBe(false)
        expect(probe.defaultIsFunction).toBe(true)
      }
    } finally {
      holder._signalsmith = saved
    }
  })
})

// ----------------------------------------------------------------------
// 2. 适配器胶水路径（注入确定替身，驱动 _processWithSignalsmith 真实代码；始终运行）
// ----------------------------------------------------------------------

describe('signalsmith 适配器胶水路径（确定替身驱动 _processWithSignalsmith）', () => {
  it('rate=1：2048 块流式分块、L/R 交织往返位级无损、参数正确下发', () => {
    const l = sine(FS, 440, 1)
    const r = sine(FS, 660, 1) // 左右不同频率，检测串音
    const fake = makeFakeModule()
    const s = new HseStretch(FS, 2)
    s.setParams({ semitones: 0, rate: 1 })
    const out = withSignalsmithStatic(fake, () => s.processStereo(l, r))

    // 输出长度 = targetFrames = round(n·rate)，精确
    expect(out.l.length).toBe(l.length)
    expect(out.r.length).toBe(l.length)
    // 位级直通：交织/解交织顺序、块序、L/R 对位全链路正确（若 L/R 串音立即失败）
    expect(identical(out.l, l)).toBe(true)
    expect(identical(out.r, r)).toBe(true)
    // 适配器按 2048 样本块流式喂数：48000 = 23×2048 + 896（尾块不足一块）
    expect(fake.calls.length).toBe(1) // 每次 processStereo 新建一个内部实例
    const call = fake.calls[0]
    expect(call.ctorArgs).toEqual([2, 2048])
    expect(call.semitones).toBe(0)
    expect(call.rate).toBe(1)
    expect(call.chunkFrames.length).toBe(24)
    for (let i = 0; i < call.chunkFrames.length - 1; i++) expect(call.chunkFrames[i]).toBe(2048)
    expect(call.chunkFrames[call.chunkFrames.length - 1]).toBe(896)
  })

  it('rate=0.5：输出帧数 ±8% 内、有限、非静音、最近邻取点频率 440→880Hz（±15%）', () => {
    const x = sine(FS, 440, 1)
    const fake = makeFakeModule()
    const s = new HseStretch(FS, 2)
    s.setParams({ semitones: 0, rate: 0.5 })
    const out = withSignalsmithStatic(fake, () => s.processStereo(x, x))

    const expected = Math.round(x.length * 0.5)
    expect(Math.abs(out.l.length - expected) / expected).toBeLessThanOrEqual(0.08)
    expect(out.r.length).toBe(out.l.length)
    expect(allFinite(out.l)).toBe(true)
    expect(allFinite(out.r)).toBe(true)
    expect(peak(out.l)).toBeGreaterThan(0.01)
    // 最近邻每 2 帧取 1 ⇒ 频率 ×2（无 FFT 的物理校验）
    const f = estimateFreq(out.l, FS)
    expect(f).toBeGreaterThan(880 * 0.85)
    expect(f).toBeLessThan(880 * 1.15)
  })

  it('rate=2：帧数精确 2n，输出=输入逐帧重复（跨块记账位级校验）', () => {
    const l = sine(FS, 440, 0.5)
    const r = sine(FS, 660, 0.5)
    const fake = makeFakeModule()
    const s = new HseStretch(FS, 2)
    s.setParams({ semitones: 0, rate: 2 })
    const out = withSignalsmithStatic(fake, () => s.processStereo(l, r))

    expect(out.l.length).toBe(2 * l.length)
    expect(out.r.length).toBe(2 * r.length)
    // 全量校验：每输入帧精确重复 2 次（验证跨块帧位记账无重/无漏）
    for (let i = 0; i < out.l.length; i++) {
      expect(out.l[i]).toBe(l[Math.floor(i / 2)])
      expect(out.r[i]).toBe(r[Math.floor(i / 2)])
    }
    expect(peak(out.l)).toBeGreaterThan(0.01)
  })

  it('无 flush 缺口：process 欠产时长度仍恒为 round(n·rate)，尾部静音补零', () => {
    const x = sine(FS, 440, 1)
    const fake = makeFakeModule({ partialReturn: true })
    const s = new HseStretch(FS, 2)
    s.setParams({ semitones: 0, rate: 1 })
    const out = withSignalsmithStatic(fake, () => s.processStereo(x, x))

    expect(out.l.length).toBe(x.length) // 长度不反映欠产，是当前适配层的已知缺口
    // 复算适配器块循环的期望写入帧数：Σ floor(cnt/2)
    const block = 2048
    let written = 0
    for (let off = 0; off < x.length; off += block) {
      written += Math.floor(Math.min(block, x.length - off) / 2)
    }
    expect(peak(out.l.subarray(0, written))).toBeGreaterThan(0.01) // 头部确有音频
    expect(tailAllZero(out.l, written)).toBe(true) // written 起全部静音（无 flush/收尾机制）
    expect(tailAllZero(out.r, written)).toBe(true)
    // 块内局部取样点：块 1 首帧重新从该块输入帧 0 取数（替身映射 out[j] ← x[2·chunk 基址]）
    expect(out.l[0]).toBe(x[0])
    expect(out.l[1024]).toBe(x[2048])
  })

  it('防御回退：ctor 抛错 / 缺 pitch API / 缺 process 均回退自研而非抛出', () => {
    const x = sine(FS, 440, 1)
    const broken: Array<{ name: string; mod: unknown }> = [
      {
        name: 'ctor 抛错',
        mod: {
          HseStretch: class {
            constructor() {
              throw new Error('boom')
            }
          },
        },
      },
      {
        name: '缺 pitch API',
        mod: {
          HseStretch: class {
            setTimeFactor(): void {}
            process(): number {
              return 0
            }
          },
        },
      },
      {
        name: '缺 process',
        mod: {
          HseStretch: class {
            setTransposeSemitones(): void {}
          },
        },
      },
    ]
    for (const { name, mod } of broken) {
      const s = new HseStretch(FS, 2)
      s.setParams({ semitones: 0, rate: 1 })
      const out = withSignalsmithStatic(mod, () => s.processStereo(x, x))
      const rel = Math.abs(out.l.length - x.length) / x.length
      expect(rel, `${name}: 应回退自研相位声码器（长度 ±3%）`).toBeLessThan(0.03)
      expect(allFinite(out.l), `${name}: 输出应有限`).toBe(true)
      expect(peak(out.l), `${name}: 输出应非静音`).toBeGreaterThan(0.01)
    }
  })

  it('确定性与 reset 语义：同输入两次处理位级一致；每次调用新建内部实例', () => {
    const x = sine(FS, 220, 0.5)
    const s = new HseStretch(FS, 2)
    s.setParams({ semitones: 3, rate: 1.5 })
    const fake = makeFakeModule()

    const a = withSignalsmithStatic(fake, () => s.processStereo(x, x))
    s.reset() // 仅清自研路径状态；signalsmith 路径每次调用新建实例，无跨调用残留
    const b = withSignalsmithStatic(fake, () => s.processStereo(x, x))

    expect(a.l.length).toBe(b.l.length)
    expect(identical(a.l, b.l)).toBe(true)
    expect(identical(a.r, b.r)).toBe(true)
    expect(fake.calls.length).toBe(2) // 两次调用各建一个内部实例（跨调用无状态延续）
    expect(fake.calls[1].semitones).toBe(3)
    expect(fake.calls[1].rate).toBe(1.5)
  })
})

// ----------------------------------------------------------------------
// 3. signalsmith-stretch 真实 DSP（仅当包暴露同步纯 DSP 类接口时运行；
//    当前官方 v1.x 为 AudioWorklet 异步包装，此组整体跳过——属预期绿灯）
// ----------------------------------------------------------------------

describe.skipIf(!probe.hasSyncClassInterface)('signalsmith-stretch 真实 DSP 路径', () => {
  beforeAll(async () => {
    // HseStretch 的设计用法：先探测（副作用：缓存模块到内部静态），此后 processStereo 走真实适配器
    const ok = await HseStretch.isSignalsmithAvailable()
    expect(ok).toBe(true)
  })

  it('(a) 440Hz 正弦 2s、rate=2 分块流式处理：输出帧数 ±8% 内', () => {
    const x = sine(FS, 440, 2)
    const s = new HseStretch(FS, 2)
    s.setParams({ semitones: 0, rate: 2 })
    const out = s.processStereo(x, x)
    const expected = Math.round(x.length * 2)
    // 注：适配器无 flush，若真实实现欠产，长度仍恒为 targetFrames（尾部静音），
    // 由 (b) 的非静音断言守护该缺口
    expect(Math.abs(out.l.length - expected) / expected).toBeLessThanOrEqual(0.08)
    expect(out.r.length).toBe(out.l.length)
  })

  it('(b) 输出有限、非静音（peak > 0.01）、时间伸缩保频：过零率 440Hz ±15%', () => {
    const x = sine(FS, 440, 2)
    const s = new HseStretch(FS, 2)
    s.setParams({ semitones: 0, rate: 2 })
    const out = s.processStereo(x, x)
    expect(allFinite(out.l)).toBe(true)
    expect(allFinite(out.r)).toBe(true)
    expect(peak(out.l)).toBeGreaterThan(0.01)
    const f = estimateFreq(out.l, FS)
    expect(f).toBeGreaterThan(440 * 0.85)
    expect(f).toBeLessThan(440 * 1.15)
  })

  it('(c) 位级确定性：两套全新实例同输入同参数逐样本一致', () => {
    const x = sine(FS, 220, 1)
    const s1 = new HseStretch(FS, 2)
    s1.setParams({ semitones: 3, rate: 1.5 })
    const a = s1.processStereo(x, x)
    const s2 = new HseStretch(FS, 2)
    s2.setParams({ semitones: 3, rate: 1.5 })
    const b = s2.processStereo(x, x)
    expect(a.l.length).toBe(b.l.length)
    expect(identical(a.l, b.l)).toBe(true)
    expect(identical(a.r, b.r)).toBe(true)
  })

  it('(d) reset() 语义：reset 后重跑与首次、与全新实例均位级一致', () => {
    const x = sine(FS, 440, 1)
    const s = new HseStretch(FS, 2)
    s.setParams({ semitones: 0, rate: 1.5 })
    const a = s.processStereo(x, x)
    s.reset()
    const b = s.processStereo(x, x)
    const s2 = new HseStretch(FS, 2)
    s2.setParams({ semitones: 0, rate: 1.5 })
    const c = s2.processStereo(x, x)
    expect(identical(a.l, b.l)).toBe(true)
    expect(identical(a.l, c.l)).toBe(true)
    expect(identical(a.r, c.r)).toBe(true)
  })
})
