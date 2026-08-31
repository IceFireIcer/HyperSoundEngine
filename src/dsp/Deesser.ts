/**
 * HyperSoundEngine v1 —— 动态齿音抑制（De-esser）
 *
 * 概念来源：Stanford EE264 数字音频处理课程公开的 de-esser 设计思路
 *   （侧链带通提取齿音频段 → 包络检测 → dB 域阈值压缩 → 分带/宽带增益施加），
 *   以及《音频算法技术文档.md》§4；本文件为全新自研实现。
 *
 * 信号流：
 *   1. 侧链提取：单声道和 (L+R)/2 → RBJ 带通(centerHz, q) 取出齿音频段 s[n]；
 *   2. 包络检测：对 s[n] 平方，按 attack(默认 1ms)/release(默认 80ms) 一阶平滑得到 env；
 *   3. dB 域阈值压缩：levelDb = 10·log10(env)，超过 thresholdDb 的部分按 ratio 衰减：
 *        reduction = (levelDb − thresholdDb)·(1 − 1/ratio)，g = 10^(−reduction/20)；
 *   4. 增益施加：
 *      - 分带式（splitBand=true，推荐）：Linkwitz-Riley 4 阶交叉（2 级 Q=0.7071
 *        低通/高通于同一截止），低频带 LP2(x) 原样、高频带 HP2(x) 乘以 g：
 *        out = LP2(x) + g·HP2(x)。g=1 时 LP2+HP2 恰为全通（幅度不变），
 *        中低频不受影响，齿音频段被精确压制；
 *      - 宽带式（splitBand=false）：整体乘以 g —— out = x·g；
 *   5. mix 干湿混合：out = x + mix·(processed − x)。
 */

import type { DeesserSettings } from '../types'
import { Biquad } from './biquad'

/** 数值钳制 */
function clamp(v: number, lo: number, hi: number): number {
  return v < lo ? lo : v > hi ? hi : v
}

/** 一阶平滑系数：α = 1 − exp(−1/(τ·fs))，τ 由毫秒换算 */
function onePoleCoef(timeMs: number, fs: number, floorMs: number): number {
  const ms = Math.max(timeMs, floorMs)
  return 1 - Math.exp(-1 / ((ms / 1000) * fs))
}

export class Deesser {
  private fs: number
  private enabled = true
  private centerHz = 6000
  private q = 0.7
  private thresholdDb = -30
  private ratio = 8
  private splitBand = true
  private mix = 1
  private attackCoef = 0
  private releaseCoef = 0
  private env = 0
  private readonly bp = new Biquad()
  // Linkwitz-Riley 4 阶交叉：每通道 2 级 LP + 2 级 HP（Q=0.7071）
  private readonly lpL1 = new Biquad()
  private readonly lpL2 = new Biquad()
  private readonly lpR1 = new Biquad()
  private readonly lpR2 = new Biquad()
  private readonly hpL1 = new Biquad()
  private readonly hpL2 = new Biquad()
  private readonly hpR1 = new Biquad()
  private readonly hpR2 = new Biquad()

  constructor(fs: number) {
    if (!(fs > 0) || !Number.isFinite(fs)) throw new Error('invalid sample rate')
    this.fs = fs
    this.applyParams({ enabled: true, centerHz: 6000, q: 0.7, thresholdDb: -30, ratio: 8, attackMs: 1, releaseMs: 80, splitBand: true, mix: 1 })
  }

  setParams(p: DeesserSettings): void {
    this.applyParams(p)
  }

  /** 参数即时生效：钳制 + 系数重算；包络状态保留（避免参数变化时爆音） */
  private applyParams(p: DeesserSettings): void {
    this.enabled = p.enabled
    this.centerHz = clamp(p.centerHz, 100, this.fs * 0.45)
    this.q = clamp(p.q, 0.1, 20)
    this.thresholdDb = clamp(p.thresholdDb, -80, 0)
    this.ratio = clamp(p.ratio, 1, 100)
    this.splitBand = p.splitBand
    this.mix = clamp(p.mix, 0, 1)
    this.attackCoef = onePoleCoef(p.attackMs, this.fs, 0.05)
    this.releaseCoef = onePoleCoef(p.releaseMs, this.fs, 1)
    // 侧链带通：齿音频段
    this.bp.setParams('bandpass', this.centerHz, this.q, 0)
    // 分带式交叉：截止取 centerHz·0.6（下限 2.5kHz，上限 fs·0.45），LR-4（每级 Q=0.7071）
    const xo = clamp(this.centerHz * 0.6, 2500, this.fs * 0.45)
    this.lpL1.setParams('lowpass', xo, 0.7071, 0)
    this.lpL2.setParams('lowpass', xo, 0.7071, 0)
    this.lpR1.setParams('lowpass', xo, 0.7071, 0)
    this.lpR2.setParams('lowpass', xo, 0.7071, 0)
    this.hpL1.setParams('highpass', xo, 0.7071, 0)
    this.hpL2.setParams('highpass', xo, 0.7071, 0)
    this.hpR1.setParams('highpass', xo, 0.7071, 0)
    this.hpR2.setParams('highpass', xo, 0.7071, 0)
  }

  /** 就地处理立体声（l/r 原地改写） */
  processStereo(l: Float32Array, r: Float32Array, sideL?: Float32Array, sideR?: Float32Array, frameCount?: number): void {
    if (!this.enabled) return // 恒等直通
    const useSide = sideL !== undefined && sideR !== undefined
    const n = Math.max(0, Math.min(
      Math.floor(frameCount ?? l.length),
      l.length,
      r.length,
      useSide ? sideL.length : Infinity,
      useSide ? sideR.length : Infinity,
    ))
    const attack = this.attackCoef
    const release = this.releaseCoef
    const thresholdDb = this.thresholdDb
    const invRatio = 1 - 1 / this.ratio
    const mix = this.mix
    const split = this.splitBand
    for (let i = 0; i < n; i++) {
      const xl = l[i]
      const xr = r[i]
      // 1) 检测信号：外部 sidechain（若提供）或主信号单声道和 → 带通
      const dl = useSide ? sideL![i] : xl
      const dr = useSide ? sideR![i] : xr
      const s = this.bp.process(0.5 * (dl + dr))
      // 2) 平方包络 + attack/release 一阶平滑
      const p = s * s
      if (p > this.env) this.env += attack * (p - this.env)
      else this.env += release * (p - this.env)
      // 3) dB 域阈值压缩
      const levelDb = 10 * Math.log10(this.env + 1e-12)
      const over = levelDb - thresholdDb
      const reduction = over > 0 ? over * invRatio : 0
      const g = Math.pow(10, -reduction / 20)
      if (split) {
        // 分带式：LP2 + g·HP2；g=1 时 LR-4 交叉重建全通（幅度不变）
        const lowL = this.lpL2.process(this.lpL1.process(xl))
        const lowR = this.lpR2.process(this.lpR1.process(xr))
        const highL = this.hpL2.process(this.hpL1.process(xl))
        const highR = this.hpR2.process(this.hpR1.process(xr))
        const outL = lowL + g * highL
        const outR = lowR + g * highR
        l[i] = xl + mix * (outL - xl)
        r[i] = xr + mix * (outR - xr)
      } else {
        // 宽带式：整体增益
        l[i] = xl + mix * (xl * g - xl)
        r[i] = xr + mix * (xr * g - xr)
      }
    }
  }

  reset(): void {
    this.env = 0
    this.bp.reset()
    this.lpL1.reset()
    this.lpL2.reset()
    this.lpR1.reset()
    this.lpR2.reset()
    this.hpL1.reset()
    this.hpL2.reset()
    this.hpR1.reset()
    this.hpR2.reset()
  }
}
