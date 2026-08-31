/**
 * HyperSoundEngine v1 —— 前瞻限幅器（Lookahead Limiter + True Peak）
 *
 * 概念来源：《音频算法技术文档.md》§3.3 —— 输入延迟 L → 检测窗峰值 →
 * 平滑增益 g = min(1, 10^(thresholdDb/20)/peak)，施加到延迟后的音频上（brickwall 零过冲）；
 * 真峰值检测采用 ITU-R BS.1770 的 4× 过采样思路（窗函数 sinc 插值取峰）。本文件为自研实现。
 *
 * 时序（流式逐样本）：在输入时刻 idx 已知 x[0..idx]，输出 y[idx] = x[idx−L]·g[idx]，
 *   g[idx] 由检测窗 [idx−L, idx]（真峰值模式为 [idx−L−3, idx−3]，检测值延迟 3 样本
 *   以便居中 sinc 插值）的峰值决定 —— 瞬时峰值在到达输出前约 L 个样本即被检测并
 *   预先压低增益，因此输出不会过冲，也不产生增益跳变咔哒声。
 *
 * 增益平滑：target < gain 时用 attack（快，默认 0.5ms），否则用 release（慢，默认 150ms）。
 * 延迟线 / 检测队列 / 插值历史均预分配，process 内零分配。
 *
 * 真峰值插值优化：
 *  - 3 相位 × 8 taps 双声道内联展开，tap 索引与历史样本一次性取入局部变量
 *    （消除逐 tap 的 &7 索引与数组重取）；
 *  - 相位 2（frac=1/2）利用窗函数 sinc 对称性合并系数相同的 tap 对
 *    （(t2,t7)、(t3,t6)、(t4,t5) 共用系数），每声道 8 次乘减为 5 次；
 *  - 检测值与优化前数值一致（浮点容差 ≤1e-6，不改变限幅行为）。
 *
 * 确定性：同输入同输出；无 Math.random / Date / console。
 */

import type { LimiterSettings } from '../types'

function clamp(v: number, lo: number, hi: number): number {
  return v < lo ? lo : v > hi ? hi : v
}

function onePoleCoef(timeMs: number, fs: number): number {
  const ms = Math.max(timeMs, 0.05)
  return 1 - Math.exp(-1 / ((ms / 1000) * fs))
}

export class Limiter {
  private fs: number
  private enabled = true
  private thresholdLin = Math.pow(10, -1 / 20)
  private lookahead = 0
  private attackCoef = 0
  private releaseCoef = 0
  private truePeak = false

  // 延迟线（尺寸 lookahead+1，读取最旧样本 = 延迟 L）
  private delayL = new Float32Array(1)
  private delayR = new Float32Array(1)
  private delayW = 0

  // 单调递减队列（环形）—— 滑动窗口峰值检测
  private qIdx = new Int32Array(8)
  private qVal = new Float32Array(8)
  private qHead = 0
  private qTail = 0
  private qLen = 0
  private qCap = 8

  // 真峰值：每通道 8 样本历史（环形）+ 3 相位 × 8 taps 插值系数
  private histL = new Float32Array(8)
  private histR = new Float32Array(8)
  private histW = 0
  private interp = new Float32Array(24)

  private gain = 1
  private reductionDb = 0
  private sampleIndex = 0

  constructor(fs: number) {
    if (!(fs > 0) || !Number.isFinite(fs)) throw new Error('invalid sample rate')
    this.fs = fs
    this.applyParams({ enabled: true, thresholdDb: -1, lookaheadMs: 5, attackMs: 0.5, releaseMs: 150, truePeak: true })
  }

  setParams(p: LimiterSettings): void {
    this.applyParams(p)
  }

  /** 参数即时生效：钳制 + 系数重算；缓冲尺寸变化或从禁用切回启用时清空管线 */
  private applyParams(p: LimiterSettings): void {
    const wasEnabled = this.enabled
    this.enabled = p.enabled
    this.thresholdLin = Math.pow(10, clamp(p.thresholdDb, -60, 0) / 20)
    this.lookahead = Math.max(0, Math.min(Math.round((p.lookaheadMs * this.fs) / 1000), Math.floor(this.fs * 0.1)))
    this.attackCoef = onePoleCoef(p.attackMs, this.fs)
    this.releaseCoef = onePoleCoef(p.releaseMs, this.fs)
    this.truePeak = p.truePeak

    const size = Math.max(this.lookahead + 1, 1)
    const cap = Math.max(this.lookahead + 8, 8)
    if (size !== this.delayL.length || cap !== this.qCap) {
      this.delayL = new Float32Array(size)
      this.delayR = new Float32Array(size)
      this.qIdx = new Int32Array(cap)
      this.qVal = new Float32Array(cap)
      this.qCap = cap
      this.qHead = 0
      this.qTail = 0
      this.qLen = 0
      this.histL.fill(0)
      this.histR.fill(0)
      this.histW = 0
      this.gain = 1
      this.sampleIndex = 0
      this.reductionDb = 0
    }
    if (this.enabled && !wasEnabled) {
      // 禁用期间延迟线未更新，恢复时清空避免陈旧样本
      this.delayL.fill(0)
      this.delayR.fill(0)
      this.qHead = 0
      this.qTail = 0
      this.qLen = 0
      this.histL.fill(0)
      this.histR.fill(0)
      this.histW = 0
      this.gain = 1
      this.sampleIndex = 0
      this.reductionDb = 0
    }
    // 4× 过采样 sinc 插值系数（Blackman 窗，3 相位 × 8 taps，窗支撑 [-5, 5]）
    if (this.truePeak) {
      for (let ph = 0; ph < 3; ph++) {
        const frac = (ph + 1) / 4
        for (let k = -4; k <= 3; k++) {
          const x = frac - k
          const sx = x === 0 ? 1 : Math.sin(Math.PI * x) / (Math.PI * x)
          const u = (x + 5) / 10
          const w = 0.42 - 0.5 * Math.cos(2 * Math.PI * u) + 0.08 * Math.cos(4 * Math.PI * u)
          this.interp[ph * 8 + (k + 4)] = sx * w
        }
      }
    }
  }

  /** 就地处理立体声（l/r 原地改写）。输出相对输入延迟 lookahead 样本。 */
  processStereo(l: Float32Array, r: Float32Array, frameCount?: number): void {
    if (!this.enabled) {
      this.reductionDb = 0
      return
    }
    const n = Math.max(0, Math.min(Math.floor(frameCount ?? l.length), l.length, r.length))
    const thr = this.thresholdLin
    const dsize = this.delayL.length
    const lookahead = this.lookahead
    const tp = this.truePeak
    const attack = this.attackCoef
    const release = this.releaseCoef
    const interp = this.interp
    const histL = this.histL
    const histR = this.histR
    const delayL = this.delayL
    const delayR = this.delayR
    const qIdx = this.qIdx
    const qVal = this.qVal
    const qCap = this.qCap
    let qHead = this.qHead
    let qTail = this.qTail
    let qLen = this.qLen
    let delayW = this.delayW
    let histW = this.histW
    let gain = this.gain
    let sampleIndex = this.sampleIndex

    for (let i = 0; i < n; i++) {
      const xl = l[i]
      const xr = r[i]
      const idx = sampleIndex

      // 1) 4× 过采样历史写入（位置 idx−7..idx）
      histL[histW] = xl
      histR[histW] = xr
      histW = (histW + 1) & 7

      // 2) 检测值：数字峰值 或 真峰值（4× sinc 插值，位置 p = idx−3）
      let det: number
      if (tp) {
        det = Math.abs(xl) > Math.abs(xr) ? Math.abs(xl) : Math.abs(xr)
        if (idx >= 7) {
          const w = histW // 最旧样本位置（x[idx−7]）
          // 8 个历史索引一次性取入局部变量（消除逐 tap 的 &7）
          const i0 = w
          const i1 = (w + 1) & 7
          const i2 = (w + 2) & 7
          const i3 = (w + 3) & 7
          const i4 = (w + 4) & 7
          const i5 = (w + 5) & 7
          const i6 = (w + 6) & 7
          const i7 = (w + 7) & 7
          const hL0 = histL[i0], hL1 = histL[i1], hL2 = histL[i2], hL3 = histL[i3]
          const hL4 = histL[i4], hL5 = histL[i5], hL6 = histL[i6], hL7 = histL[i7]
          const hR0 = histR[i0], hR1 = histR[i1], hR2 = histR[i2], hR3 = histR[i3]
          const hR4 = histR[i4], hR5 = histR[i5], hR6 = histR[i6], hR7 = histR[i7]
          let vL = 0
          let vR = 0
          // 相位 1（frac=1/4）：8 taps 顺序展开（累加顺序与循环版一致）
          {
            const sL = interp[0] * hL0 + interp[1] * hL1 + interp[2] * hL2 + interp[3] * hL3 +
              interp[4] * hL4 + interp[5] * hL5 + interp[6] * hL6 + interp[7] * hL7
            const sR = interp[0] * hR0 + interp[1] * hR1 + interp[2] * hR2 + interp[3] * hR3 +
              interp[4] * hR4 + interp[5] * hR5 + interp[6] * hR6 + interp[7] * hR7
            const aL = Math.abs(sL)
            const aR = Math.abs(sR)
            if (aL > vL) vL = aL
            if (aR > vR) vR = aR
          }
          // 相位 2（frac=1/2）：sinc 对称 → 系数相同 tap 对合并（8 乘 → 5 乘）
          {
            const sL = interp[8] * hL0 + interp[9] * hL1 + interp[10] * (hL2 + hL7) +
              interp[11] * (hL3 + hL6) + interp[12] * (hL4 + hL5)
            const sR = interp[8] * hR0 + interp[9] * hR1 + interp[10] * (hR2 + hR7) +
              interp[11] * (hR3 + hR6) + interp[12] * (hR4 + hR5)
            const aL = Math.abs(sL)
            const aR = Math.abs(sR)
            if (aL > vL) vL = aL
            if (aR > vR) vR = aR
          }
          // 相位 3（frac=3/4）：8 taps 顺序展开
          {
            const sL = interp[16] * hL0 + interp[17] * hL1 + interp[18] * hL2 + interp[19] * hL3 +
              interp[20] * hL4 + interp[21] * hL5 + interp[22] * hL6 + interp[23] * hL7
            const sR = interp[16] * hR0 + interp[17] * hR1 + interp[18] * hR2 + interp[19] * hR3 +
              interp[20] * hR4 + interp[21] * hR5 + interp[22] * hR6 + interp[23] * hR7
            const aL = Math.abs(sL)
            const aR = Math.abs(sR)
            if (aL > vL) vL = aL
            if (aR > vR) vR = aR
          }
          if (vL > det) det = vL
          if (vR > det) det = vR
        }
        // 弹出窗口外（索引 < oldest）的队首过期项
        const oldest = idx - 3 - lookahead
        while (qLen > 0 && qIdx[qHead] < oldest) {
          qHead = (qHead + 1) % qCap
          qLen--
        }
        // 单调递减入队（相等值保留最新）
        const qIdxVal = idx - 3
        while (qLen > 0) {
          const t = (qTail - 1 + qCap) % qCap
          if (qVal[t] > det) break
          qTail = t
          qLen--
        }
        qIdx[qTail] = qIdxVal
        qVal[qTail] = det
        qTail = (qTail + 1) % qCap
        qLen++
      } else {
        det = Math.abs(xl) > Math.abs(xr) ? Math.abs(xl) : Math.abs(xr)
        const oldest = idx - lookahead
        while (qLen > 0 && qIdx[qHead] < oldest) {
          qHead = (qHead + 1) % qCap
          qLen--
        }
        while (qLen > 0) {
          const t = (qTail - 1 + qCap) % qCap
          if (qVal[t] > det) break
          qTail = t
          qLen--
        }
        qIdx[qTail] = idx
        qVal[qTail] = det
        qTail = (qTail + 1) % qCap
        qLen++
      }

      // 3) 延迟线写入（写后游标自增，环回用比较代替取模）
      delayL[delayW] = xl
      delayR[delayW] = xr
      delayW++
      if (delayW >= dsize) delayW = 0

      // 4) 目标增益 = min(1, 阈值/峰值)，attack/release 一阶平滑
      const peak = qLen > 0 ? qVal[qHead] : 0
      const target = Math.min(1, thr / Math.max(peak, 1e-12))
      if (target < gain) gain += attack * (target - gain)
      else gain += release * (target - gain)

      // 5) 输出 = 延迟 L 样本 × 平滑增益
      l[i] = delayL[delayW] * gain
      r[i] = delayR[delayW] * gain
      sampleIndex++
    }
    this.delayW = delayW
    this.qHead = qHead
    this.qTail = qTail
    this.qLen = qLen
    this.histW = histW
    this.gain = gain
    this.sampleIndex = sampleIndex
    this.reductionDb = 20 * Math.log10(gain)
  }

  /** 当前增益衰减 dB（<= 0） */
  getReductionDb(): number {
    return this.reductionDb
  }

  /** 引入的延迟（样本数）= lookahead 样本 */
  getLatencySamples(): number {
    return this.lookahead
  }

  reset(): void {
    this.delayL.fill(0)
    this.delayR.fill(0)
    this.delayW = 0
    this.qHead = 0
    this.qTail = 0
    this.qLen = 0
    this.histL.fill(0)
    this.histR.fill(0)
    this.histW = 0
    this.gain = 1
    this.reductionDb = 0
    this.sampleIndex = 0
  }
}
