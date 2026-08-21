/**
 * DynamicEq 单元测试 —— 自适应动态均衡器(频谱包络驱动的自动混音)
 *
 * 覆盖:构造 / setParams、strength=0 硬直通、确定性、无 NaN / 有界、
 * 动态行为(低带被压低、高带不受影响、输出 RMS 降低)、包络平滑
 * (相邻块增益差有界)、reset 干净、性能冒烟,以及交叉树单位增益
 * 精确重建(安静输入下输出≈输入,即无静态染色)。
 * 测试内使用固定种子伪随机(确定性);仅测试文件允许 console 报告性能。
 */
import { describe, expect, it } from 'vitest'
import { DynamicEq, DYNAMIC_EQ_BAND_NAMES } from '../src/dsp/DynamicEq'

const FS = 48000

function makeSine(n: number, f: number, amp: number): Float32Array {
  const x = new Float32Array(n)
  for (let i = 0; i < n; i++) x[i] = amp * Math.sin((2 * Math.PI * f * i) / FS)
  return x
}

/** 固定种子伪随机白噪声(确定性,幅度 ±amp) */
function makeNoise(n: number, amp: number): Float32Array {
  const x = new Float32Array(n)
  let s = 0x9e3779b9
  for (let i = 0; i < n; i++) {
    s ^= s << 13
    s ^= s >>> 17
    s ^= s << 5
    x[i] = amp * (((s >>> 0) / 0xffffffff) * 2 - 1)
  }
  return x
}

function peak(x: Float32Array): number {
  let m = 0
  for (let i = 0; i < x.length; i++) {
    const a = Math.abs(x[i])
    if (a > m) m = a
  }
  return m
}

function rms(x: Float32Array): number {
  let s = 0
  for (let i = 0; i < x.length; i++) s += x[i] * x[i]
  return Math.sqrt(s / x.length)
}

function maxAbsDiff(a: Float32Array, b: Float32Array): number {
  let m = 0
  const n = Math.min(a.length, b.length)
  for (let i = 0; i < n; i++) {
    const d = Math.abs(a[i] - b[i])
    if (d > m) m = d
  }
  return m
}

function hasNonFinite(x: Float32Array): boolean {
  for (let i = 0; i < x.length; i++) if (!Number.isFinite(x[i])) return true
  return false
}

describe('DynamicEq', () => {
  it('构造与 setParams 不抛错;非法 fs / 长度不匹配抛错', () => {
    expect(() => new DynamicEq(FS)).not.toThrow()
    expect(() => new DynamicEq(FS, {
      enabled: true, strength: 0.7, thresholdDb: -24, ratio: 3, kneeDb: 4,
      attackMs: 30, releaseMs: 250, blockSize: 256,
    })).not.toThrow()
    const eq = new DynamicEq(FS)
    expect(() => eq.setParams({
      bands: [
        { enabled: true, frequency: 150 },
        { enabled: false, frequency: 700, targetGainDb: 2 },
        { enabled: true, frequency: 2200 },
        { enabled: true, frequency: 7500 },
        { enabled: true, frequency: 12000 },
      ],
    })).not.toThrow()
    // 非法采样率
    expect(() => new DynamicEq(0)).toThrow()
    expect(() => new DynamicEq(NaN)).toThrow()
    // L/R 长度不匹配
    expect(() => eq.processStereo(new Float32Array(8), new Float32Array(9))).toThrow()
    expect(DYNAMIC_EQ_BAND_NAMES).toHaveLength(5)
    expect(eq.getBandNames()).toEqual(['low', 'low-mid', 'mid', 'high-mid', 'high'])
    expect(eq.getBandGains()).toHaveLength(5)
  })

  it('strength=0 与 enabled=false 硬直通:输出与输入逐样本一致', () => {
    const n = 9600
    const inL = makeSine(n, 220, 0.8)
    const inR = makeNoise(n, 0.3)
    for (const params of [{ strength: 0 }, { enabled: false }, { strength: 0, enabled: false }]) {
      const eq = new DynamicEq(FS)
      eq.setParams(params)
      const l = inL.slice()
      const r = inR.slice()
      eq.processStereo(l, r)
      expect(l).toEqual(inL)
      expect(r).toEqual(inR)
    }
  })

  it('确定性:同输入同参数两次处理逐样本一致', () => {
    const n = FS
    const inL = makeNoise(n, 0.8)
    const inR = makeSine(n, 440, 0.5)
    const run = (): { l: Float32Array, r: Float32Array } => {
      const eq = new DynamicEq(FS, { strength: 1, thresholdDb: -24, ratio: 2.5, attackMs: 15, releaseMs: 150 })
      const l = inL.slice()
      const r = inR.slice()
      eq.processStereo(l, r)
      return { l, r }
    }
    const a = run()
    const b = run()
    expect(a.l).toEqual(b.l)
    expect(a.r).toEqual(b.r)
  })

  it('无 NaN / 有界:2s 白噪声 + 2s 静音,无 NaN、峰值≤3、静音尾部收敛到 0', () => {
    const n = FS * 4
    const noise = makeNoise(FS * 2, 1.0)
    const inL = new Float32Array(n)
    const inR = new Float32Array(n)
    inL.set(noise, 0)
    inR.set(noise, 0)
    const eq = new DynamicEq(FS, { strength: 1 })
    const l = inL.slice()
    const r = inR.slice()
    eq.processStereo(l, r)
    expect(hasNonFinite(l)).toBe(false)
    expect(hasNonFinite(r)).toBe(false)
    expect(peak(l)).toBeLessThanOrEqual(3)
    expect(peak(r)).toBeLessThanOrEqual(3)
    // 静音尾部(最后 0.5s):滤波器状态与增益均已收敛,输出应为 0
    const tail = l.subarray(n - FS / 2)
    let maxTail = 0
    for (let i = 0; i < tail.length; i++) {
      const a = Math.abs(tail[i])
      if (a > maxTail) maxTail = a
    }
    expect(maxTail).toBeLessThan(1e-6)
  })

  it('动态行为:低频能量持续偏高 → 低带增益被压低,高频带不受影响,输出 RMS 降低', () => {
    const n = FS * 2
    const sine = makeSine(n, 100, 0.9)
    const nz = makeNoise(n, 0.05)
    const inL = new Float32Array(n)
    const inR = new Float32Array(n)
    for (let i = 0; i < n; i++) {
      inL[i] = sine[i] + nz[i]
      inR[i] = inL[i]
    }
    // 参考:strength=0 直通
    const bypass = new DynamicEq(FS, { strength: 0 })
    const refL = inL.slice()
    const refR = inR.slice()
    bypass.processStereo(refL, refR)
    const refRms = rms(refL.subarray(n >> 1))
    // 动态开启
    const eq = new DynamicEq(FS, { strength: 1, attackMs: 10 })
    const l = inL.slice()
    const r = inR.slice()
    eq.processStereo(l, r)
    const gains = eq.getBandGains()
    // 低带(100Hz 主导)被明显压低;高带(8k 以上,仅噪声)基本不动
    expect(gains[0]).toBeLessThan(0.8)
    expect(gains[0]).toBeGreaterThan(0.1)
    expect(gains[4]).toBeGreaterThan(0.95)
    // 稳态输出 RMS 低于直通
    const outRms = rms(l.subarray(n >> 1))
    expect(outRms).toBeLessThan(refRms * 0.7)
  })

  it('包络平滑:输入突变为高频能量,相邻块增益差有界且无瞬时跳变', () => {
    const n = FS // 1s:前 0.5s 低频正弦,后 0.5s 高频正弦(突变)
    const half = n >> 1
    const low = makeSine(half, 100, 0.9)
    const high = makeSine(half, 8000, 0.9)
    const inL = new Float32Array(n)
    const inR = new Float32Array(n)
    inL.set(low, 0)
    inL.set(high, half)
    inR.set(inL)
    const eq = new DynamicEq(FS, { strength: 1, attackMs: 20, releaseMs: 200, blockSize: 128 })
    const l = inL.slice()
    const r = inR.slice()
    const BLOCK = 128
    let maxDelta = 0
    let prev: number[] | null = null
    for (let pos = 0; pos < n; pos += BLOCK) {
      eq.processStereo(l.subarray(pos, pos + BLOCK), r.subarray(pos, pos + BLOCK))
      const g = eq.getBandGains()
      if (prev !== null) {
        for (let b = 0; b < 5; b++) {
          const d = Math.abs(g[b] - prev[b])
          if (d > maxDelta) maxDelta = d
        }
      }
      prev = g
    }
    expect(maxDelta).toBeLessThan(0.5) // 增益逐样本一阶平滑:块间变化有界
    const g = eq.getBandGains()
    expect(g[4]).toBeLessThan(0.8) // 高频带最终被压低
    // 低频带从 ~0.4 恢复:一阶 release(200ms)渐近收敛,0.5s 后 ≈0.95
    expect(g[0]).toBeGreaterThan(0.9) // 低频带已明显恢复
  })

  it('reset 后状态干净:reset 后重放与首次处理逐样本一致', () => {
    const n = FS * 1.5
    const s1 = makeSine(n, 100, 0.8)
    const s2 = makeSine(n, 3000, 0.35)
    const nz = makeNoise(n, 0.1)
    const inL = new Float32Array(n)
    const inR = new Float32Array(n)
    for (let i = 0; i < n; i++) {
      inL[i] = s1[i] + s2[i] + nz[i]
      inR[i] = s1[i] - s2[i] + nz[i]
    }
    const eq = new DynamicEq(FS, { strength: 1, attackMs: 15, releaseMs: 150 })
    const l1 = inL.slice()
    const r1 = inR.slice()
    eq.processStereo(l1, r1)
    eq.reset()
    const l2 = inL.slice()
    const r2 = inR.slice()
    eq.processStereo(l2, r2)
    expect(l2).toEqual(l1)
    expect(r2).toEqual(r1)
  })

  it('交叉树单位增益精确重建:安静输入(低于阈值)输出≈输入,无静态染色', () => {
    const n = FS
    const inL = makeSine(n, 100, 0.01) // -40dBFS,远低于阈值 → 增益保持 1
    const inR = inL.slice()
    const eq = new DynamicEq(FS, { strength: 1 })
    const l = inL.slice()
    const r = inR.slice()
    eq.processStereo(l, r)
    expect(maxAbsDiff(l, inL)).toBeLessThan(1e-5)
    expect(maxAbsDiff(r, inR)).toBeLessThan(1e-5)
  })

  it('性能冒烟:128 帧块处理 1s 立体声', () => {
    const n = FS
    const inL = makeNoise(n, 0.8)
    const inR = makeNoise(n, 0.8)
    const eq = new DynamicEq(FS, { strength: 1 })
    // 预热(系数/状态就绪)
    eq.processStereo(inL.slice(), inR.slice())
    const l = inL.slice()
    const r = inR.slice()
    const t0 = performance.now()
    const BLOCK = 128
    for (let pos = 0; pos < n; pos += BLOCK) {
      eq.processStereo(l.subarray(pos, pos + BLOCK), r.subarray(pos, pos + BLOCK))
    }
    const ms = performance.now() - t0
    console.log(`DynamicEq 性能:1s 立体声(48k,128 帧块)= ${ms.toFixed(2)} ms`)
    expect(ms).toBeLessThan(2000)
  })
})