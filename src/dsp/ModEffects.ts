/**
 * ModEffects.ts —— 调制类效果：Delay / Chorus / Flanger / Phaser / Tremolo
 *
 * 全部为自研实现：
 * - Delay：环形延迟线 + 反馈 + 干湿混合；
 * - Chorus / Flanger：LFO 调制分数延迟（线性插值）；
 * - Phaser：多级一阶全通滤波器 + LFO 调制中心频率 + 反馈；
 * - Tremolo：LFO 幅度调制。
 *
 * 约定：processStereo 就地处理；构造时预分配缓冲，process 内零分配。
 * 确定性：无随机、无 Date、无 console；同输入同参数同输出。
 */

import type {
  DelaySettings,
  ChorusSettings,
  FlangerSettings,
  PhaserSettings,
  TremoloSettings,
} from '../types'

function clamp(v: number, lo: number, hi: number): number {
  return v < lo ? lo : v > hi ? hi : v
}

/** 环形延迟线读取（线性插值） */
function readDelay(buf: Float32Array, pos: number, delaySamples: number): number {
  const size = buf.length
  const d = clamp(delaySamples, 0, size - 1)
  const i0 = Math.floor(d)
  const frac = d - i0
  const idx0 = (pos - i0 + size) % size
  const idx1 = (idx0 - 1 + size) % size
  return buf[idx0] * (1 - frac) + buf[idx1] * frac
}

function writeDelay(buf: Float32Array, pos: number, value: number): void {
  buf[pos] = value
}

export class DelayEffect {
  private readonly fs: number
  private readonly bufL: Float32Array
  private readonly bufR: Float32Array
  private pos = 0
  private delaySamples = 0
  private feedback = 0.3
  private mix = 0.3

  constructor(fs: number) {
    if (!(fs > 0)) throw new Error('invalid sample rate')
    this.fs = fs
    const maxDelay = Math.ceil(fs * 2) + 1 // 最大 2s
    this.bufL = new Float32Array(maxDelay)
    this.bufR = new Float32Array(maxDelay)
  }

  setParams(p: DelaySettings): void {
    this.delaySamples = (clamp(p.delayMs, 0, 2000) / 1000) * this.fs
    this.feedback = clamp(p.feedback, 0, 0.98)
    this.mix = clamp(p.mix, 0, 1)
  }

  processStereo(l: Float32Array, r: Float32Array, frameCount?: number): void {
    const n = Math.max(0, Math.min(Math.floor(frameCount ?? l.length), l.length, r.length))
    const bufL = this.bufL
    const bufR = this.bufR
    const size = bufL.length
    const d = this.delaySamples
    const fb = this.feedback
    const mix = this.mix
    let pos = this.pos
    for (let i = 0; i < n; i++) {
      const xl = l[i]
      const xr = r[i]
      const wetL = readDelay(bufL, pos, d)
      const wetR = readDelay(bufR, pos, d)
      writeDelay(bufL, pos, xl + wetL * fb)
      writeDelay(bufR, pos, xr + wetR * fb)
      l[i] = xl * (1 - mix) + wetL * mix
      r[i] = xr * (1 - mix) + wetR * mix
      pos = (pos + 1) % size
    }
    this.pos = pos
  }

  reset(): void {
    this.bufL.fill(0)
    this.bufR.fill(0)
    this.pos = 0
  }
}

class ModulatedDelay {
  protected readonly fs: number
  protected readonly bufL: Float32Array
  protected readonly bufR: Float32Array
  protected pos = 0
  protected phase = 0
  protected baseDelay = 0
  protected depthSamples = 0
  protected rateHz = 1

  constructor(fs: number, maxDelaySec: number) {
    if (!(fs > 0)) throw new Error('invalid sample rate')
    this.fs = fs
    const len = Math.ceil(fs * maxDelaySec) + 2
    this.bufL = new Float32Array(len)
    this.bufR = new Float32Array(len)
  }

  protected setCommon(baseMs: number, depthMs: number, rateHz: number): void {
    this.baseDelay = (clamp(baseMs, 0, 100) / 1000) * this.fs
    this.depthSamples = (clamp(depthMs, 0, 50) / 1000) * this.fs
    this.rateHz = clamp(rateHz, 0.01, 20)
  }

  protected lfoValue(): number {
    // 双极性正弦
    return Math.sin(2 * Math.PI * this.phase)
  }

  protected advance(n: number): void {
    this.phase = (this.phase + (this.rateHz * n) / this.fs) % 1
  }

  protected processCore(l: Float32Array, r: Float32Array, feedback: number, mix: number, frameCount?: number): void {
    const n = Math.max(0, Math.min(Math.floor(frameCount ?? l.length), l.length, r.length))
    const bufL = this.bufL
    const bufR = this.bufR
    const size = bufL.length
    let pos = this.pos
    for (let i = 0; i < n; i++) {
      const xl = l[i]
      const xr = r[i]
      const mod = this.lfoValue()
      const d = this.baseDelay + this.depthSamples * mod
      const wetL = readDelay(bufL, pos, d)
      const wetR = readDelay(bufR, pos, d)
      writeDelay(bufL, pos, xl + wetL * feedback)
      writeDelay(bufR, pos, xr + wetR * feedback)
      l[i] = xl * (1 - mix) + wetL * mix
      r[i] = xr * (1 - mix) + wetR * mix
      pos = (pos + 1) % size
    }
    this.pos = pos
    this.advance(n)
  }

  reset(): void {
    this.bufL.fill(0)
    this.bufR.fill(0)
    this.pos = 0
    this.phase = 0
  }
}

export class ChorusEffect extends ModulatedDelay {
  constructor(fs: number) {
    super(fs, 0.1)
  }

  setParams(p: ChorusSettings): void {
    this.setCommon(20, p.depthMs, p.rateHz)
    this.mix = clamp(p.mix, 0, 1)
  }

  private mix = 0.4

  processStereo(l: Float32Array, r: Float32Array, frameCount?: number): void {
    this.processCore(l, r, 0, this.mix, frameCount)
  }
}

export class FlangerEffect extends ModulatedDelay {
  constructor(fs: number) {
    super(fs, 0.05)
  }

  private feedback = 0.4
  private mix = 0.5

  setParams(p: FlangerSettings): void {
    this.setCommon(1, p.depthMs, p.rateHz)
    this.feedback = clamp(p.feedback, 0, 0.98)
    this.mix = clamp(p.mix, 0, 1)
  }

  processStereo(l: Float32Array, r: Float32Array, frameCount?: number): void {
    this.processCore(l, r, this.feedback, this.mix, frameCount)
  }
}

export class PhaserEffect {
  private readonly fs: number
  private rateHz = 0.5
  private depth = 0.5
  private feedback = 0.4
  private mix = 0.5
  private stages = 4
  private phase = 0
  // 每通道每级状态：x1, y1
  private stateL: Float32Array
  private stateR: Float32Array

  constructor(fs: number) {
    if (!(fs > 0)) throw new Error('invalid sample rate')
    this.fs = fs
    this.stateL = new Float32Array(8 * 2)
    this.stateR = new Float32Array(8 * 2)
  }

  setParams(p: PhaserSettings): void {
    this.rateHz = clamp(p.rateHz, 0.01, 20)
    this.depth = clamp(p.depth, 0, 1)
    this.feedback = clamp(p.feedback, 0, 0.98)
    this.mix = clamp(p.mix, 0, 1)
    this.stages = Math.max(2, Math.min(8, Math.round(p.stages)))
  }

  processStereo(l: Float32Array, r: Float32Array, frameCount?: number): void {
    const n = Math.max(0, Math.min(Math.floor(frameCount ?? l.length), l.length, r.length))
    const stages = this.stages
    const fs = this.fs
    const depth = this.depth
    const fb = this.feedback
    const mix = this.mix
    let phase = this.phase
    for (let i = 0; i < n; i++) {
      const xl = l[i]
      const xr = r[i]
      // LFO 调制中心频率 200..2000Hz
      const lfo = 0.5 + 0.5 * Math.sin(2 * Math.PI * phase)
      const fc = 200 + 1800 * (0.2 + 0.8 * lfo * depth)
      const a = (1 - Math.tan((Math.PI * fc) / fs)) / (1 + Math.tan((Math.PI * fc) / fs))

      // 简单反馈：用上一个样本的输出反馈叠加
      const inL = xl + fb * this.lastOutL
      const inR = xr + fb * this.lastOutR
      let yl = inL
      let yr = inR
      for (let s = 0; s < stages; s++) {
        const base = s * 2
        yl = this.allpass(inL, this.stateL, base, a)
        yr = this.allpass(inR, this.stateR, base, a)
      }
      this.lastOutL = yl
      this.lastOutR = yr
      l[i] = xl * (1 - mix) + yl * mix
      r[i] = xr * (1 - mix) + yr * mix
      phase = (phase + this.rateHz / fs) % 1
    }
    this.phase = phase
  }

  private lastOutL = 0
  private lastOutR = 0

  private allpass(x: number, state: Float32Array, base: number, a: number): number {
    const x1 = state[base]
    const y1 = state[base + 1]
    const y = -a * x + x1 + a * y1
    state[base] = x
    state[base + 1] = y
    return y
  }

  reset(): void {
    this.stateL.fill(0)
    this.stateR.fill(0)
    this.phase = 0
    this.lastOutL = 0
    this.lastOutR = 0
  }
}

export class TremoloEffect {
  private readonly fs: number
  private rateHz = 5
  private depth = 0.5
  private mix = 1
  private phase = 0

  constructor(fs: number) {
    if (!(fs > 0)) throw new Error('invalid sample rate')
    this.fs = fs
  }

  setParams(p: TremoloSettings): void {
    this.rateHz = clamp(p.rateHz, 0.01, 30)
    this.depth = clamp(p.depth, 0, 1)
    this.mix = clamp(p.mix, 0, 1)
  }

  processStereo(l: Float32Array, r: Float32Array, frameCount?: number): void {
    const n = Math.max(0, Math.min(Math.floor(frameCount ?? l.length), l.length, r.length))
    const fs = this.fs
    const depth = this.depth
    const mix = this.mix
    let phase = this.phase
    for (let i = 0; i < n; i++) {
      const g = 1 - depth * (0.5 + 0.5 * Math.sin(2 * Math.PI * phase))
      const wet = g
      l[i] = l[i] * (1 - mix + mix * wet)
      r[i] = r[i] * (1 - mix + mix * wet)
      phase = (phase + this.rateHz / fs) % 1
    }
    this.phase = phase
  }

  reset(): void {
    this.phase = 0
  }
}
