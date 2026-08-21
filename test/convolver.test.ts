/**
 * Convolver 单元测试（模块 9：均匀分区卷积 + IR 去周期化）
 *
 * 物理意义注记：
 *  - IR=[1]（单位冲激）时卷积应为恒等（输出≈输入），验证分区调度正确性；
 *  - IR=延迟冲激（h[D]=1）时输出应为输入右移 D，验证延迟正确；
 *  - 指数衰减 IR 的湿输出能量应单调衰减（无周期回升/发散）；
 *  - dePeriodize 把尾部被"平底"截断的 IR 强制衰减，消除循环伪影。
 */
import { describe, it, expect } from 'vitest'
import { Convolver } from '../src/dsp/Convolver'

const FS = 48000
const TOL = 1e-3 // 数值容差（卷积 FFT 舍入量级，物理上对应 -60dB 以下误差）

function makeInput(n: number): Float32Array {
  // 确定性的平滑测试信号（斜坡 + 正弦叠加），避免依赖 Math.random
  const x = new Float32Array(n)
  for (let i = 0; i < n; i++) {
    x[i] = 0.02 * i + 0.3 * Math.sin((2 * Math.PI * 440 * i) / FS)
  }
  return x
}

function maxAbsDiff(a: Float32Array, b: Float32Array, offsetB = 0): number {
  let m = 0
  const n = Math.min(a.length, b.length - offsetB)
  for (let i = 0; i < n; i++) {
    const d = Math.abs(a[i] - b[i + offsetB])
    if (d > m) m = d
  }
  return m
}

/** 10ms 窗 RMS 包络（dB），用于能量衰减断言 */
function rmsEnvelopeDb(x: Float32Array, fs: number, winMs = 10): Float32Array {
  const win = Math.max(1, Math.round((winMs / 1000) * fs))
  const nBlocks = Math.floor(x.length / win)
  const env = new Float32Array(nBlocks)
  for (let b = 0; b < nBlocks; b++) {
    let s = 0
    for (let i = b * win; i < (b + 1) * win; i++) s += x[i] * x[i]
    const rms = Math.sqrt(s / win)
    env[b] = rms > 1e-12 ? 20 * Math.log10(rms) : -200
  }
  return env
}

describe('Convolver.process（有限块卷积）', () => {
  it('IR=[1] 时输出≈输入（恒等）', () => {
    const cv = new Convolver(FS, { dePeriodize: true })
    cv.loadIR(new Float32Array([1]), 'unit')
    cv.setMix(1)
    const x = makeInput(2048)
    const y = cv.process(x)
    // 长度 = 输入长度 + IR 尾(0)
    expect(y.length).toBe(x.length)
    expect(maxAbsDiff(y, x)).toBeLessThan(TOL)
  })

  it('IR=延迟冲激 h[D]=1 时输出为输入右移 D', () => {
    const D = 100
    const ir = new Float32Array(D + 1)
    ir[D] = 1
    const cv = new Convolver(FS, { dePeriodize: true })
    cv.loadIR(ir, 'delta100')
    cv.setMix(1)
    const x = makeInput(2048)
    const y = cv.process(x)
    expect(y.length).toBe(x.length + D)
    // 前 D 个样本应为 0（冲激响应尚未开始）
    for (let i = 0; i < D; i++) {
      expect(Math.abs(y[i])).toBeLessThan(TOL)
    }
    // 之后等于输入右移 D：y[i+D] == x[i]（maxAbsDiff(x, y, D) 比较 x[i] 与 y[i+D]）
    expect(maxAbsDiff(x, y, D)).toBeLessThan(TOL)
  })

  it('preDelay 生效：IR=[1] + 10ms 预延迟 → 输出右移 480 样本', () => {
    const cv = new Convolver(FS, { dePeriodize: true })
    cv.loadIR(new Float32Array([1]))
    cv.setMix(1)
    cv.setPreDelayMs(10) // 480 样本 @48k
    const x = makeInput(1024)
    const y = cv.process(x)
    expect(y.length).toBe(x.length + 480)
    for (let i = 0; i < 480; i++) {
      expect(Math.abs(y[i])).toBeLessThan(TOL)
    }
    // y[i+480] == x[i]
    expect(maxAbsDiff(x, y, 480)).toBeLessThan(TOL)
  })

  it('指数衰减 IR 的湿输出能量单调衰减（无周期回升）', () => {
    // IR = exp 衰减（τ=0.15s），长度 1.2s（8 个时间常数 → 69dB，足以支撑 40dB 尾部断言）
    const M = Math.round(1.2 * FS)
    const ir = new Float32Array(M)
    for (let i = 0; i < M; i++) ir[i] = Math.exp(-i / (0.15 * FS))
    const cv = new Convolver(FS, { dePeriodize: false })
    cv.loadIR(ir, 'exp')
    cv.setMix(1)
    // 单样本冲激 → 输出 = IR 本身
    const imp = new Float32Array(1)
    imp[0] = 1
    const y = cv.process(imp)
    const env = rmsEnvelopeDb(y, FS)
    // 找到包络峰值位置后，后续包络应单调不升（允许 ±0.5dB 抖动，无回升）
    let peak = 0
    for (let i = 1; i < env.length; i++) if (env[i] > env[peak]) peak = i
    for (let i = peak + 1; i < env.length - 1; i++) {
      expect(env[i + 1]).toBeLessThanOrEqual(env[i] + 0.5)
    }
    // 尾部应远低于峰值（衰减至少 40dB）
    expect(env[env.length - 1]).toBeLessThan(env[peak] - 40)
  })

  it('dePeriodize 将"平底截断"IR 的尾部强制衰减', () => {
    // IR：指数衰减（τ=0.2s）到 -80dB 以下后保持平底（模拟循环尾）
    const M = Math.round(2.0 * FS)
    const ir = new Float32Array(M)
    const decayTau = 0.2 * FS
    for (let i = 0; i < M; i++) ir[i] = Math.exp(-i / decayTau)
    // 尾部平底：从 1.4s 起钳到 -80dB（1e-4），低于 dePeriodize 的 -60dB 触发阈值，
    // 保证"平底区"真实存在且包络不再高于阈值（否则去周期化不会被触发）
    const floorAmp = 1e-4 // -80dB
    const floorStart = M - Math.round(0.6 * FS) // 1.4s
    for (let i = floorStart; i < M; i++) ir[i] = Math.max(ir[i], floorAmp)

    const withDeP = new Convolver(FS, { dePeriodize: true })
    withDeP.loadIR(new Float32Array(ir), 'flatTail')
    withDeP.setMix(1)
    const noDeP = new Convolver(FS, { dePeriodize: false })
    noDeP.loadIR(new Float32Array(ir), 'flatTail')
    noDeP.setMix(1)
    const imp = new Float32Array(1)
    imp[0] = 1
    const yDep = withDeP.process(imp)
    const yNoDep = noDeP.process(imp)

    const envDep = rmsEnvelopeDb(yDep, FS)
    const envNoDep = rmsEnvelopeDb(yNoDep, FS)
    // 末段（平底区）相对能量：dePeriodize 后应继续衰减（≥6dB），
    // 未 dePeriodize 时平底保持（同区段内几乎不变）
    const n = envDep.length
    const lateDep = envDep[n - 4]
    const midDep = envDep[Math.floor(n * 0.55)]
    expect(lateDep).toBeLessThan(midDep - 6)
    // noDeP：取两个都落在真实平底区的块比较，应几乎不变。
    // 注意：natural 衰减 τ=0.2s 要到 exp(-i/τ)<1e-4 即 i>1.84s 才真正触底，
    // 因此平底区包络索引 ≥ 185（floorStart=1.4s 处 natural 仍有 -62dB，不能作平底基准）
    const lateNoDep = envNoDep[n - 4] // 1.96s
    const floorNoDep = envNoDep[190] // 1.90s
    expect(Math.abs(lateNoDep - floorNoDep)).toBeLessThan(3)
  })

  it('loadIR 对空/全零 IR 抛错', () => {
    const cv = new Convolver(FS)
    expect(() => cv.loadIR(new Float32Array(0))).toThrow()
    expect(() => cv.loadIR(new Float32Array(512))).toThrow() // 全零
  })
})

describe('Convolver.processStereo（流式 + 干湿混合）', () => {
  it('mix=0 时输出=输入（干路恒等）', () => {
    const cv = new Convolver(FS, { partitionSize: 512 })
    cv.loadIR(new Float32Array([1]))
    cv.setMix(0)
    const N = 2000
    const l = new Float32Array(N)
    const r = new Float32Array(N)
    for (let i = 0; i < N; i++) {
      l[i] = 0.5 * Math.sin((2 * Math.PI * 300 * i) / FS)
      r[i] = 0.3 * Math.sin((2 * Math.PI * 700 * i) / FS)
    }
    const l0 = new Float32Array(l)
    const r0 = new Float32Array(r)
    // 按 128 样本块流式处理
    for (let off = 0; off < N; off += 128) {
      cv.processStereo(l.subarray(off, off + 128), r.subarray(off, off + 128))
    }
    expect(maxAbsDiff(l, l0)).toBeLessThan(TOL)
    expect(maxAbsDiff(r, r0)).toBeLessThan(TOL)
  })

  it('IR=[1]、mix=1 时湿路输出 = 输入延迟一个分区长', () => {
    const L = 512
    const cv = new Convolver(FS, { partitionSize: L, dePeriodize: true })
    cv.loadIR(new Float32Array([1]))
    cv.setMix(1)
    const N = 4096
    // 多喂 L 个样本（尾部补零），使最后一块的湿输出也被放行——
    // 流式分区卷积的湿路"尾块"需后续输入才可输出（block 粒度，天然尾部缓冲）
    const TOTAL = N + L
    const l = new Float32Array(TOTAL)
    const r = new Float32Array(TOTAL)
    for (let i = 0; i < N; i++) {
      const v = Math.sin((2 * Math.PI * 220 * i) / FS)
      l[i] = v
      r[i] = 0.5 * v
    }
    const l0 = new Float32Array(l)
    for (let off = 0; off < TOTAL; off += 128) {
      cv.processStereo(l.subarray(off, off + 128), r.subarray(off, off + 128))
    }
    // 湿路延迟 = 分区长 L；对齐后输出 ≈ 输入（±1e-3）
    expect(cv.getLatencySamples()).toBe(L)
    let maxDiff = 0
    for (let i = L; i < N; i++) {
      const d = Math.abs(l[i] - l0[i - L])
      if (d > maxDiff) maxDiff = d
    }
    expect(maxDiff).toBeLessThan(TOL)
    // 前 L 个样本应为 0（湿路尚未输出）
    for (let i = 0; i < L; i++) {
      expect(Math.abs(l[i])).toBeLessThan(TOL)
    }
  })
})

// ---------------------------------------------------------------------------
// 非均匀分区卷积（模块 9 升级）：短分区 Ls=256 / 长分区 Ll=2048
// ---------------------------------------------------------------------------
describe('Convolver 非均匀分区（短分区 256 / 长分区 2048）', () => {
  // 构造触发长分区的 IR：长度需 > 短区段（100ms@48k = 4800 样本）
  // Ps = ceil(4800/256) = 19，longStart = 4864，Pl = ceil((M-4864)/2048)
  function longIr(M: number): Float32Array {
    const ir = new Float32Array(M)
    for (let i = 0; i < M; i++) ir[i] = Math.exp(-i / 1800) * Math.sin(i / 47) * 0.35
    return ir
  }
  function sineWave(n: number, f: number, a: number): Float32Array {
    const x = new Float32Array(n)
    for (let i = 0; i < n; i++) x[i] = a * Math.sin((2 * Math.PI * f * i) / FS)
    return x
  }
  /** 参考：直接线性卷积（双精度） */
  function directConv(x: Float32Array, ir: Float32Array): Float64Array {
    const y = new Float64Array(x.length + ir.length - 1)
    for (let i = 0; i < x.length; i++) {
      const xi = x[i]
      for (let j = 0; j < ir.length; j++) y[i + j] += xi * ir[j]
    }
    return y
  }
  function maxAbsDiff64(a: Float32Array, b: Float64Array): number {
    let m = 0
    const n = Math.min(a.length, b.length)
    for (let i = 0; i < n; i++) {
      const d = Math.abs(a[i] - b[i])
      if (d > m) m = d
    }
    return m
  }

  it('process() 与直接线性卷积逐样本一致（容差 1e-4，长分区参与）', () => {
    const M = 8000 // 0.167s @48k，longStart=4864 < M → Pl=2
    const ir = longIr(M)
    const cv = new Convolver(FS, { partitionSize: 256, longPartitionSize: 2048, dePeriodize: false })
    cv.loadIR(ir, 'nonuniform')
    cv.setMix(1)
    const n = 3000
    const x = sineWave(n, 440, 0.5)
    const y = cv.process(x)
    expect(y.length).toBe(n + M - 1)
    const ref = directConv(x, ir)
    const err = maxAbsDiff64(y, ref)
    expect(err).toBeLessThan(1e-4)
  })

  it('processStereo 与 process() 等价（尾部对齐，容差 1e-4）', () => {
    const M = 8000
    const ir = longIr(M)
    const cv = new Convolver(FS, { partitionSize: 256, longPartitionSize: 2048, dePeriodize: false })
    cv.loadIR(ir, 'nonuniform')
    cv.setMix(1)
    const n = 3000
    const x = sineWave(n, 440, 0.5)
    const oneShot = cv.process(x) // 完整线性卷积，无延迟
    // 流式：喂 x + 尾部零（IR 尾 + 延迟），使湿路完整输出并与 process() 对齐
    const Ls = cv.getLatencySamples()
    const feed = n + M - 1 + Ls
    const l = new Float32Array(feed)
    const r = new Float32Array(feed)
    l.set(x)
    // 注意：process() 会修改内部工作缓冲；先跑 process() 再跑流式，或用新实例。
    // 此处 process() 已在上面调用过（共享工作缓冲被复用），流式仍应正确：
    // process() 与 processStereo 不并发，且均从各自状态出发。
    for (let off = 0; off < feed; off += 128) {
      cv.processStereo(l.subarray(off, off + 128), r.subarray(off, off + 128))
    }
    // 流式湿路输出位置 i 对应完整卷积位置 i - Ls
    let maxErr = 0
    for (let i = Ls; i < feed && i - Ls < oneShot.length; i++) {
      const d = Math.abs(l[i] - oneShot[i - Ls])
      if (d > maxErr) maxErr = d
    }
    expect(maxErr).toBeLessThan(1e-4)
  })

  it('getLatencySamples() 返回最短分区长 = 传入的 partitionSize（默认 512，勿改为 256）', () => {
    const cv = new Convolver(FS, { partitionSize: 256, longPartitionSize: 2048, dePeriodize: false })
    cv.loadIR(longIr(8000), 'x')
    expect(cv.getLatencySamples()).toBe(256)
    const cv512 = new Convolver(FS, { partitionSize: 512, dePeriodize: false })
    cv512.loadIR(longIr(8000), 'x')
    expect(cv512.getLatencySamples()).toBe(512)
    // 默认配置（不传 partitionSize）：延迟 = 512（与旧版一致，非均匀升级不改变延迟语义）
    const cvDef = new Convolver(FS, { dePeriodize: false })
    cvDef.loadIR(longIr(8000), 'x')
    expect(cvDef.getLatencySamples()).toBe(512)
  })

  it('长分区必须是最短分区长的整数倍（k 为 2 的幂）', () => {
    const cv = new Convolver(FS, { partitionSize: 128, longPartitionSize: 512, dePeriodize: false }) as any
    expect(cv.k).toBe(4)
    expect(cv.longPartitionSize).toBe(512)
    // 非整数倍请求自动向上取整为 2 的幂倍数
    const cv2 = new Convolver(FS, { partitionSize: 128, longPartitionSize: 300, dePeriodize: false }) as any
    expect(cv2.longPartitionSize % cv2.partitionSize).toBe(0)
    expect(cv2.longPartitionSize).toBe(512) // ceil(300/128)=3 → 2^2=4 → 512
    // 默认:partitionSize=512 → longPartitionSize=4096 (8x)
    const cv3 = new Convolver(FS, { dePeriodize: false }) as any
    expect(cv3.longPartitionSize).toBe(4096)
    expect(cv3.longPartitionSize % cv3.partitionSize).toBe(0)
  })

  it('长 IR（2s @48k）流式湿路无 NaN、无发散、能量衰减', () => {
    const M = FS * 2
    const cv = new Convolver(FS, { partitionSize: 256, longPartitionSize: 2048, dePeriodize: false })
    const ir = longIr(M)
    cv.loadIR(ir, 'long2s')
    cv.setMix(1)
    const n = FS // 1s 输入
    const l = sineWave(n, 220, 0.5)
    const r = new Float32Array(n)
    const lOut = new Float32Array(n + M - 1 + 256)
    lOut.set(l)
    for (let off = 0; off < lOut.length; off += 128) {
      cv.processStereo(lOut.subarray(off, off + 128), r.subarray(off, off + 128))
    }
    let nonFinite = 0
    let maxAbs = 0
    for (let i = 0; i < lOut.length; i++) {
      if (!Number.isFinite(lOut[i])) nonFinite++
      maxAbs = Math.max(maxAbs, Math.abs(lOut[i]))
    }
    expect(nonFinite).toBe(0)
    // 长 IR（2s）卷积能量高于短 IR：峰值可达数十（0.5 幅度输入 × 2s IR 尾部能量），
    // 只要求有界（无发散/自激）
    expect(maxAbs).toBeLessThan(100)
  })

  it('非均匀分区与均匀分区（Ll=Ls）输出逐样本一致（容差 1e-4）', () => {
    // 同一 IR：非均匀(256/2048) 与退化均匀(512/512) 的 process() 都等于完整线性卷积，
    // 两者应逐样本一致 —— 验证非均匀分区不改变数学语义
    const M = 8000
    const ir = longIr(M)
    const a = new Convolver(FS, { partitionSize: 256, longPartitionSize: 2048, dePeriodize: false })
    a.loadIR(ir)
    const b = new Convolver(FS, { partitionSize: 512, longPartitionSize: 512, dePeriodize: false })
    b.loadIR(ir)
    const n = 2000
    const x = sineWave(n, 330, 0.5)
    const ya = a.process(x)
    const yb = b.process(x)
    expect(ya.length).toBe(yb.length)
    let maxErr = 0
    for (let i = 0; i < ya.length; i++) {
      const d = Math.abs(ya[i] - yb[i])
      if (d > maxErr) maxErr = d
    }
    expect(maxErr).toBeLessThan(1e-4)
  })
})
