/**
 * HyperSoundEngine v1 —— 虚拟低频增强（Virtual Bass / Bass Enhancer）
 *
 * 概念来源：IEEE 虚拟低音论文思路（Gerstle et al. / "Synthesis of polynomial-based
 *   nonlinear device..." / "Virtual Bass Enhancement Based on Harmonics Control"，
 *   Missing Fundamental 心理声学：大脑从 2/3/4 次谐波重建基频音高），
 *   以及《音频算法技术文档.md》§5；本文件为自研实现。
 *
 * 信号流（每通道）：
 *   1. 低通提取：LPF(cutoffHz, q) 取出低频段 x_bass（默认 90Hz）；
 *   2. 谐波生成：非线性函数 NL ——
 *        odd  : x³（奇次谐波：3 次为主）
 *        even : |x| 全波整流（偶次谐波：2 次为主 + DC，由后级 HPF 去除）
 *        atan : atan(√|x|)·sign(x)（ATSR 器件，奇次谐波，解析可控）
 *        soft : tanh(2·x)（软削波，奇次谐波，幅度衰减快）
 *   3. 高通整形：HPF(max(150, cutoffHz·1.5) Hz) 只保留基频整数倍谐波，去除 DC 与互调；
 *   4. 混合：out = x + 10^(levelDb/20)·mix·harmonicGain·shaped
 *      （levelDb 作用于谐波路径电平；dry 信号保持不变）；
 *   5. 低音下潜：out += (10^(lowBoostDb/20) − 1)·x_bass —— 低通提取的低频带按增益
 *      混回，等价于以 cutoffHz 为中心的 low-shelf 真实低频提升（lowBoostDb 默认
 *      0=关闭，此时输出与不含该路径时逐位一致；谐波路径只提供心理声学感知，
 *      真实低频能量靠这条路径）。
 */

import type { BassEnhancerSettings, HarmonicType } from '../types'
import { Biquad } from './biquad'

function clamp(v: number, lo: number, hi: number): number {
  return v < lo ? lo : v > hi ? hi : v
}

export class BassEnhancer {
  private fs: number
  private enabled = true
  private cutoffHz = 90
  private q = 0.7
  private harmonicType: HarmonicType = 'odd'
  private harmonicGain = 0.6
  private mix = 0.5
  private levelLin = 1
  private lowBoostDb = 0
  private lowLin = 0 // 10^(lowBoostDb/20) − 1，低频带混回增益（真实能量提升）
  private readonly lpL = new Biquad()
  private readonly lpR = new Biquad()
  private readonly hpL = new Biquad()
  private readonly hpR = new Biquad()

  constructor(fs: number) {
    if (!(fs > 0) || !Number.isFinite(fs)) throw new Error('invalid sample rate')
    this.fs = fs
    this.applyParams({ enabled: true, cutoffHz: 90, q: 0.7, harmonicType: 'odd', harmonicGain: 0.6, mix: 0.5, levelDb: 0 })
  }

  setParams(p: BassEnhancerSettings): void {
    this.applyParams(p)
  }

  /** 参数即时生效：钳制 + 系数重算 */
  private applyParams(p: BassEnhancerSettings): void {
    this.enabled = p.enabled
    this.cutoffHz = clamp(p.cutoffHz, 20, this.fs * 0.45)
    this.q = clamp(p.q, 0.1, 20)
    this.harmonicType = p.harmonicType
    this.harmonicGain = clamp(p.harmonicGain, 0, 1)
    this.mix = clamp(p.mix, 0, 1)
    this.levelLin = Math.pow(10, clamp(p.levelDb, -6, 6) / 20)
    // 防御旧参数快照缺字段（undefined/非有限值直接 clamp 会得 NaN 污染输出）
    const lb = p.lowBoostDb
    this.lowBoostDb = clamp(typeof lb === 'number' && Number.isFinite(lb) ? lb : 0, -6, 12)
    this.lowLin = Math.pow(10, this.lowBoostDb / 20) - 1
    // 谐波整形高通：≥150Hz 或 cutoffHz·1.5（取较大），上限 fs·0.45
    const hpCut = clamp(Math.max(150, this.cutoffHz * 1.5), 20, this.fs * 0.45)
    this.lpL.setParams('lowpass', this.cutoffHz, this.q, 0)
    this.lpR.setParams('lowpass', this.cutoffHz, this.q, 0)
    this.hpL.setParams('highpass', hpCut, 0.707, 0)
    this.hpR.setParams('highpass', hpCut, 0.707, 0)
  }

  /** 谐波非线性函数（仅作用于低频带，避免全频互调） */
  private nonlinearity(x: number): number {
    switch (this.harmonicType) {
      case 'odd':
        return x * x * x
      case 'even':
        return Math.abs(x)
      case 'atan':
        // ATSR：atan(√|x|)·sign(x)
        return Math.atan(Math.sqrt(Math.abs(x))) * Math.sign(x)
      case 'soft':
        // tanh(g·x)，g=2 常量驱动（软削波，奇次谐波快速衰减）
        return Math.tanh(2 * x)
      default:
        return x * x * x
    }
  }

  /** 就地处理立体声（l/r 原地改写） */
  processStereo(l: Float32Array, r: Float32Array, frameCount?: number): void {
    if (!this.enabled) return // 恒等直通
    const n = Math.max(0, Math.min(Math.floor(frameCount ?? l.length), l.length, r.length))
    const k = this.mix * this.harmonicGain * this.levelLin
    const low = this.lowLin
    for (let i = 0; i < n; i++) {
      const xl = l[i]
      const xr = r[i]
      // 1) 低通提取低频带
      const bl = this.lpL.process(xl)
      const br = this.lpR.process(xr)
      // 2) 非线性谐波生成 + 3) 高通整形
      const hl = this.hpL.process(this.nonlinearity(bl))
      const hr = this.hpR.process(this.nonlinearity(br))
      // 4) 混回（dry 不变，谐波路径按 mix×harmonicGain×level）
      // 5) 低音下潜：低频带按 lowBoostDb 混回（真实低频能量提升；low=0 时逐位等于原路径）
      l[i] = xl + k * hl + low * bl
      r[i] = xr + k * hr + low * br
    }
  }

  reset(): void {
    this.lpL.reset()
    this.lpR.reset()
    this.hpL.reset()
    this.hpR.reset()
  }
}
