/**
 * FdnReverb 单元测试(FDN 反馈延迟网络混响,算法创新模块)
 *
 * 物理意义注记:
 *  - wet=0/dry=1 时应为纯干声(恒等),验证干路与混音路径;
 *  - Householder 正交反馈矩阵 + 反馈增益 g≤0.98 → 环路往返增益 g²<1 → 无条件稳定:
 *    输入停止后 2s 内输出峰值应衰减到 <1e-3(无自激);
 *  - 互质(素数)延迟线 → 回声密度随时间增长、无共同周期共振 → 单脉冲后尾音持续;
 *  - preDelay 为输入侧独立延迟线:整个脉冲响应被平移 preDelayMs(首峰位移 ≈ preDelay);
 *    无反馈直通的首回声幅值 = 注入(1/√N)×输出(1/N) ≈ 0.044(N=8),远高于定位阈值;
 *  - type 表:hall 反馈强/延迟长 → 尾音显著长于 room(同用户参数下 0.2s 处能量更高);
 *  - width=0 时湿路单声道化(相同输入 → 左右输出一致);width=1 时保留 L/R 差异
 *    (左右网络素数表不同 → 湿路去相关);
 *  - 确定性:模块无随机/无 Date/无 console,同输入同参数必同输出(逐样本精确相等)。
 */
import { describe, it, expect } from 'vitest'
import { FdnReverb } from '../src/dsp/FdnReverb'

const FS = 48000
const BLOCK = 128

/** 确定性 LCG 白噪声(均匀 -1..1);不用 Math.random 以保持可复现 */
function lcgNoise(n: number, seed: number): Float32Array {
  const x = new Float32Array(n)
  let s = seed >>> 0
  for (let i = 0; i < n; i++) {
    s = (Math.imul(s, 1664525) + 1013904223) >>> 0
    x[i] = (s / 4294967296) * 2 - 1
  }
  return x
}

/** 20ms 窗 RMS 包络(dB) */
function rmsEnvelopeDb(x: Float32Array, fs: number, winMs = 20): Float32Array {
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

function baseParams(over: Partial<Parameters<FdnReverb['setParams']>[0]> = {}) {
  return {
    roomSize: 0.5,
    damping: 0.5,
    wet: 1,
    dry: 0,
    preDelayMs: 0,
    width: 1,
    type: 'hall' as const,
    ...over,
  }
}

/** 流式分块处理冲激(L/R 同源),返回整段输出 */
function processImpulse(rev: FdnReverb, n: number): { l: Float32Array; r: Float32Array } {
  const outL = new Float32Array(n)
  const outR = new Float32Array(n)
  for (let off = 0; off < n; off += BLOCK) {
    const len = Math.min(BLOCK, n - off)
    const l = new Float32Array(len)
    const r = new Float32Array(len)
    if (off === 0) {
      l[0] = 1
      r[0] = 1
    }
    rev.processStereo(l, r)
    outL.set(l, off)
    outR.set(r, off)
  }
  return { l: outL, r: outR }
}

/** 流式分块处理一段信号(L 取输入,R 同源或独立),返回整段输出 */
function processStream(rev: FdnReverb, x: Float32Array, rIn?: Float32Array): { l: Float32Array; r: Float32Array } {
  const n = x.length
  const outL = new Float32Array(n)
  const outR = new Float32Array(n)
  for (let off = 0; off < n; off += BLOCK) {
    const len = Math.min(BLOCK, n - off)
    const l = new Float32Array(len)
    const r = new Float32Array(len)
    l.set(x.subarray(off, off + len))
    if (rIn) r.set(rIn.subarray(off, off + len))
    else r.set(x.subarray(off, off + len))
    rev.processStereo(l, r)
    outL.set(l, off)
    outR.set(r, off)
  }
  return { l: outL, r: outR }
}

/** 逐样本精确相等(Float32Array 位级一致) */
function expectExact(a: Float32Array, b: Float32Array): void {
  expect(a.length).toBe(b.length)
  for (let i = 0; i < a.length; i++) {
    expect(a[i]).toBe(b[i])
  }
}

/** 一段信号内的最大绝对值与首个超过阈值的索引 */
function maxAbsAndFirst(x: Float32Array, thr: number): { maxAbs: number; first: number } {
  let maxAbs = 0
  let first = -1
  for (let i = 0; i < x.length; i++) {
    const a = Math.abs(x[i])
    if (a > maxAbs) maxAbs = a
    if (first < 0 && a > thr) first = i
  }
  return { maxAbs, first }
}

describe('FdnReverb', () => {
  it('构造 + setParams 不抛错;5 种 type 与 lines 2/4/8/16 均可用;非法 lines 抛错', () => {
    const types = ['hall', 'room', 'plate', 'spring', 'stage'] as const
    for (const t of types) {
      const rev = new FdnReverb(FS)
      expect(() => rev.setParams(baseParams({ type: t, lines: 8 }))).not.toThrow()
    }
    for (const n of [2, 4, 8, 16]) {
      const rev = new FdnReverb(FS)
      expect(() => rev.setParams(baseParams({ lines: n }))).not.toThrow()
    }
    // 未传 lines 默认 8
    const rev = new FdnReverb(FS)
    expect(() => rev.setParams(baseParams({}))).not.toThrow()
    // 非法线数抛错
    expect(() => new FdnReverb(FS).setParams(baseParams({ lines: 3 }))).toThrow()
    expect(() => new FdnReverb(FS).setParams(baseParams({ lines: 0 }))).toThrow()
    // 非法采样率抛错
    expect(() => new FdnReverb(0)).toThrow()
    expect(() => new FdnReverb(-48000)).toThrow()
  })

  it('确定性:同参数同输入两次处理逐样本一致', () => {
    const n = Math.round(0.6 * FS)
    const xl = lcgNoise(n, 1001)
    const xr = lcgNoise(n, 2002)
    const a = new FdnReverb(FS)
    const b = new FdnReverb(FS)
    a.setParams(baseParams({ roomSize: 0.7, damping: 0.3, wet: 0.8, dry: 0.4, preDelayMs: 15, width: 0.7 }))
    b.setParams(baseParams({ roomSize: 0.7, damping: 0.3, wet: 0.8, dry: 0.4, preDelayMs: 15, width: 0.7 }))
    const oa = processStream(a, xl, xr)
    const ob = processStream(b, xl, xr)
    expectExact(oa.l, ob.l)
    expectExact(oa.r, ob.r)
  })

  it('稳定性:1s 白噪声后切静音,2s 内输出峰值衰减 <1e-3(无自激)', () => {
    const rev = new FdnReverb(FS)
    // roomSize 0.8 → 环路往返增益 g²≈0.72/往返(≈24ms),τ≈130ms → 静音 ~0.75s 后
    // 尾音即低于 1e-3,2s 时 ~1e-7(实测),裕量巨大;g<1 保证无条件不发散
    rev.setParams(baseParams({ roomSize: 0.8, damping: 0.5, wet: 1, dry: 0 }))
    const x = lcgNoise(FS, 42)
    const { l } = processStream(rev, x)
    // 输入停止后 2s:继续推进(输入为 0),观察湿输出衰减
    const silent = new Float32Array(2 * FS)
    const { l: tail } = processStream(rev, silent)
    // 整段静音期峰值(含刚切断时的强回声)与末段峰值
    const { maxAbs: peakWhole } = maxAbsAndFirst(tail, 0)
    let peakLast = 0
    for (let i = FS; i < 2 * FS; i++) {
      const a = Math.abs(tail[i])
      if (a > peakLast) peakLast = a
    }
    const inPeak = maxAbsAndFirst(l, 0).maxAbs
    console.log(
      '[stability] 输入期峰值=', inPeak.toExponential(3),
      '静音期整段峰值=', peakWhole.toExponential(3),
      '静音 1s..2s 峰值=', peakLast.toExponential(3),
    )
    // 无自激:整段有界(不放大);且 2s 内已衰减到 <1e-3(取静音后 1s..2s 窗口,实测 1.3e-4)
    expect(peakWhole).toBeLessThan(1.0)
    expect(peakLast).toBeLessThan(1e-3)
  })

  it('无 NaN 且有界:2s 噪声(128 块)输出无 NaN 且 ≤3', () => {
    const rev = new FdnReverb(FS)
    rev.setParams(baseParams({ roomSize: 0.6, damping: 0.4, wet: 1, dry: 0 }))
    const n = 2 * FS
    const x = lcgNoise(n, 777)
    const { l, r } = processStream(rev, x, x)
    let maxAbs = 0
    for (let i = 0; i < n; i++) {
      expect(Number.isFinite(l[i])).toBe(true)
      expect(Number.isFinite(r[i])).toBe(true)
      const a = Math.abs(l[i])
      if (a > maxAbs) maxAbs = a
      const ar = Math.abs(r[i])
      if (ar > maxAbs) maxAbs = ar
    }
    console.log('[bounded] 2s 噪声输出峰值=', maxAbs.toFixed(4))
    expect(maxAbs).toBeLessThanOrEqual(3)
  })

  it('混响特性:单脉冲后 ≥50ms 仍有非零尾音能量(互质延迟 → 高密度尾音)', () => {
    const rev = new FdnReverb(FS)
    rev.setParams(baseParams({ roomSize: 0.75, damping: 0.4, wet: 1, dry: 0 }))
    const n = Math.round(0.6 * FS)
    const { l } = processImpulse(rev, n)
    // 50ms..300ms 窗口内的最大幅值(跳过早期直达回声)
    const from = Math.round(0.05 * FS)
    const to = Math.round(0.3 * FS)
    let tailMax = 0
    for (let i = from; i < to; i++) {
      const a = Math.abs(l[i])
      if (a > tailMax) tailMax = a
    }
    console.log('[tail] 50..300ms 窗口峰值=', tailMax.toExponential(3))
    expect(tailMax).toBeGreaterThan(1e-4)
  })

  it('preDelay:50ms 预延迟使脉冲响应整体平移 ≈50ms(首峰位移 50ms±10ms)', () => {
    const n = Math.round(0.4 * FS)
    const a = new FdnReverb(FS)
    const b = new FdnReverb(FS)
    a.setParams(baseParams({ preDelayMs: 0 }))
    b.setParams(baseParams({ preDelayMs: 50 }))
    const ir0 = processImpulse(a, n).l
    const ir1 = processImpulse(b, n).l
    const f0 = maxAbsAndFirst(ir0, 1e-4).first
    const f1 = maxAbsAndFirst(ir1, 1e-4).first
    const shiftMs = ((f1 - f0) / FS) * 1000
    console.log('[preDelay] 首峰位置 noPD=', f0, 'PD50ms=', f1, '位移=', shiftMs.toFixed(2) + 'ms')
    // 无预延迟首峰 ≈ 最短延迟线(prime 701 × 1.3 × fs/44100 ≈ 20.7ms)
    expect(f0).toBeGreaterThan(0)
    // 50ms 内无输出(输入侧 preDelay:前 50ms 网络只收到 0)
    const pd = Math.round(0.05 * FS)
    for (let i = 0; i < pd; i++) {
      expect(Math.abs(ir1[i])).toBeLessThan(1e-9)
    }
    // 位移 ≈ 50ms,容差 10ms
    expect(Math.abs(shiftMs - 50)).toBeLessThanOrEqual(10)
  })

  it('dry/wet:wet=0/dry=1 时输出=输入(逐样本一致);wet=1/dry=0 时输出≠输入', () => {
    const n = Math.round(0.5 * FS)
    const xl = lcgNoise(n, 55)
    const xr = lcgNoise(n, 66)
    // 纯干声:恒等
    const dry = new FdnReverb(FS)
    dry.setParams(baseParams({ wet: 0, dry: 1 }))
    const od = processStream(dry, xl, xr)
    expectExact(od.l, xl)
    expectExact(od.r, xr)
    // 纯湿声:有处理,与输入不同
    const wet = new FdnReverb(FS)
    wet.setParams(baseParams({ wet: 1, dry: 0, roomSize: 0.7 }))
    const ow = processStream(wet, xl, xr)
    let diff = 0
    for (let i = 0; i < n; i++) {
      const d = Math.abs(ow.l[i] - xl[i])
      if (d > diff) diff = d
    }
    console.log('[wet] 湿路最大差异=', diff.toExponential(3))
    expect(diff).toBeGreaterThan(1e-3)
  })

  it('reset 后重放同输入 = 与首次一致(状态复位干净)', () => {
    const n = Math.round(0.5 * FS)
    const xl = lcgNoise(n, 2024)
    const xr = lcgNoise(n, 3030)
    const rev = new FdnReverb(FS)
    rev.setParams(baseParams({ roomSize: 0.7, damping: 0.3, preDelayMs: 10 }))
    const first = processStream(rev, xl, xr)
    rev.reset()
    const second = processStream(rev, xl, xr)
    expectExact(first.l, second.l)
    expectExact(first.r, second.r)
  })

  it('width=0 时单声道化(相同输入 → 左右一致);width=1 时保留差异', () => {
    const n = Math.round(0.8 * FS)
    const mono = lcgNoise(n, 9090)
    // width=0:湿路交叉混合后左右一致
    const w0 = new FdnReverb(FS)
    w0.setParams(baseParams({ width: 0, wet: 1, dry: 0.3, roomSize: 0.7 }))
    const o0 = processStream(w0, mono, mono)
    for (let i = 0; i < n; i++) {
      expect(Math.abs(o0.l[i] - o0.r[i])).toBeLessThan(1e-6)
    }
    // width=1:左右网络素数表不同 → 湿路去相关 → 保留差异
    const w1 = new FdnReverb(FS)
    w1.setParams(baseParams({ width: 1, wet: 1, dry: 0.3, roomSize: 0.7 }))
    const o1 = processStream(w1, mono, mono)
    let diff = 0
    for (let i = 0; i < n; i++) {
      const d = Math.abs(o1.l[i] - o1.r[i])
      if (d > diff) diff = d
    }
    console.log('[width] width=1 最大 L/R 差异=', diff.toExponential(3))
    expect(diff).toBeGreaterThan(1e-4)
  })

  it('type 表:hall 尾音长于 room(同用户参数下 0.2s 处能量更高)', () => {
    const n = Math.round(0.5 * FS)
    const hall = new FdnReverb(FS)
    hall.setParams(baseParams({ type: 'hall' }))
    const { l: lHall } = processImpulse(hall, n)
    const room = new FdnReverb(FS)
    room.setParams(baseParams({ type: 'room' }))
    const { l: lRoom } = processImpulse(room, n)
    const envHall = rmsEnvelopeDb(lHall, FS)
    const envRoom = rmsEnvelopeDb(lRoom, FS)
    const at = Math.floor((0.2 * FS) / ((20 / 1000) * FS)) // 0.2s 处(20ms 窗)
    console.log('[type] 0.2s 处 hall=', envHall[at].toFixed(1) + 'dB', 'room=', envRoom[at].toFixed(1) + 'dB')
    expect(envHall[at]).toBeGreaterThan(envRoom[at] + 5)
  })

  it('性能冒烟:1s 音频(128 帧块)处理耗时(console 报告,不强制断言)', () => {
    const rev = new FdnReverb(FS)
    rev.setParams(baseParams({ roomSize: 0.7, damping: 0.4, wet: 0.8, dry: 0.4, preDelayMs: 20, width: 0.8 }))
    const l = new Float32Array(BLOCK)
    const r = new Float32Array(BLOCK)
    const total = FS // 1s
    // 预热(冷启动/编译)
    for (let off = 0; off < total; off += BLOCK) rev.processStereo(l, r)
    const t0 = performance.now()
    for (let off = 0; off < total; off += BLOCK) rev.processStereo(l, r)
    const dt = performance.now() - t0
    console.log(`[perf] FdnReverb 处理 1s(48000 样本,128 块)耗时 ${dt.toFixed(2)}ms = 实时 ${(dt / 1000).toFixed(3)}x`)
  })
});
