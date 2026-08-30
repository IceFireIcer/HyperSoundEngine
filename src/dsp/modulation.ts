/**
 * modulation.ts —— 参数调制矩阵（LFO / Envelope Follower）
 *
 * 出处/许可：自研基础件。LFO 波形（sine/triangle/square/saw）与包络跟随
 * （峰值检测 + 一阶平滑）为音频处理公有知识，无第三方代码。
 *
 * 设计：
 * - `Lfo`：产生双极性归一化 LFO 波形（sine/triangle/square/saw）；
 * - `EnvelopeFollower`：跟踪输入信号包络（0..1）；
 * - `ModulationMatrix`：把 LFO/Envelope 路由到内置目标（masterGain / stereoWidth），
 *   按块更新目标值，供引擎在处理块时使用。
 *
 * 确定性：无随机、无 Date；同输入同参数同输出。
 */

import type { LfoShape, ModulationRoute } from '../types'

function clamp(v: number, lo: number, hi: number): number {
  return v < lo ? lo : v > hi ? hi : v
}

/** LFO 低频振荡器（双极性 -1..1） */
export class Lfo {
  private readonly sampleRate: number
  private shape: LfoShape = 'sine'
  private rateHz = 1
  private depth = 1
  private phase = 0

  constructor(sampleRate: number, shape: LfoShape = 'sine', rateHz = 1, depth = 1) {
    if (!(sampleRate > 0)) throw new Error('invalid sample rate')
    this.sampleRate = sampleRate
    this.setParams(shape, rateHz, depth)
  }

  setParams(shape: LfoShape, rateHz: number, depth: number): void {
    this.shape = shape
    this.rateHz = Math.max(0, rateHz)
    this.depth = clamp(depth, 0, 1)
  }

  /** 推进 n 个样本并返回当前归一化输出（-1..1） */
  processBlock(n: number): number {
    const dt = n / this.sampleRate
    this.phase = (this.phase + this.rateHz * dt) % 1
    return this.value() * this.depth
  }

  reset(): void {
    this.phase = 0
  }

  private value(): number {
    const p = this.phase
    switch (this.shape) {
      case 'sine':
        return Math.sin(2 * Math.PI * p)
      case 'triangle':
        return 4 * Math.abs(p - 0.5) - 1
      case 'square':
        return p < 0.5 ? 1 : -1
      case 'saw':
        return 2 * p - 1
      default:
        return Math.sin(2 * Math.PI * p)
    }
  }
}

/** 包络跟随器（峰值检测 + 一阶平滑，输出 0..1） */
export class EnvelopeFollower {
  private readonly sampleRate: number
  private attackCoef = 0
  private releaseCoef = 0
  private amount = 1
  private env = 0

  constructor(sampleRate: number, attackMs = 10, releaseMs = 200, amount = 1) {
    if (!(sampleRate > 0)) throw new Error('invalid sample rate')
    this.sampleRate = sampleRate
    this.setParams(attackMs, releaseMs, amount)
  }

  setParams(attackMs: number, releaseMs: number, amount: number): void {
    const a = Math.max(attackMs, 0.05)
    const r = Math.max(releaseMs, 0.05)
    this.attackCoef = 1 - Math.exp(-1 / ((a / 1000) * this.sampleRate))
    this.releaseCoef = 1 - Math.exp(-1 / ((r / 1000) * this.sampleRate))
    this.amount = clamp(amount, 0, 1)
  }

  /** 处理一个块，返回块尾包络（已乘 amount） */
  processBlock(l: Float32Array, r: Float32Array, n: number): number {
    const attack = this.attackCoef
    const release = this.releaseCoef
    for (let i = 0; i < n; i++) {
      const e = Math.abs(l[i]) > Math.abs(r[i]) ? Math.abs(l[i]) : Math.abs(r[i])
      if (e > this.env) this.env += attack * (e - this.env)
      else this.env += release * (e - this.env)
    }
    return this.env * this.amount
  }

  reset(): void {
    this.env = 0
  }
}

/** 调制矩阵：把 LFO/Envelope 路由到 masterGain / stereoWidth */
export class ModulationMatrix {
  private readonly lfo: Lfo
  private readonly env: EnvelopeFollower
  private readonly result = { masterGain: 1, stereoWidth: 1 }
  private routes: ModulationRoute[] = []

  constructor(
    sampleRate: number,
    routes: ModulationRoute[] = [],
    lfo?: { shape: LfoShape; rateHz: number; depth: number },
    envelope?: { attackMs: number; releaseMs: number; amount: number },
  ) {
    this.lfo = new Lfo(sampleRate, lfo?.shape ?? 'sine', lfo?.rateHz ?? 1, lfo?.depth ?? 0.5)
    this.env = new EnvelopeFollower(
      sampleRate,
      envelope?.attackMs ?? 10,
      envelope?.releaseMs ?? 200,
      envelope?.amount ?? 0.5,
    )
    this.routes = routes.slice()
  }

  setRoutes(routes: ModulationRoute[]): void {
    this.routes = routes.slice()
  }

  setLfoParams(shape: LfoShape, rateHz: number, depth: number): void {
    this.lfo.setParams(shape, rateHz, depth)
  }

  setEnvelopeParams(attackMs: number, releaseMs: number, amount: number): void {
    this.env.setParams(attackMs, releaseMs, amount)
  }

  /** 处理一个块并返回独立结果快照。实时路径应使用 processBlockInto。 */
  processBlock(l: Float32Array, r: Float32Array, n: number): { masterGain: number; stereoWidth: number } {
    this.processBlockInto(l, r, n, this.result)
    return { masterGain: this.result.masterGain, stereoWidth: this.result.stereoWidth }
  }

  /** 把结果写入调用方提供的对象，供实时路径避免每块分配。 */
  processBlockInto(
    l: Float32Array,
    r: Float32Array,
    n: number,
    output: { masterGain: number; stereoWidth: number },
  ): void {
    const lfoVal = this.lfo.processBlock(n)
    const envVal = this.env.processBlock(l, r, n)

    let masterGain = 1
    let stereoWidth = 1
    for (const route of this.routes) {
      const src = route.source === 'lfo' ? lfoVal : envVal
      const v = src * route.amount + (route.offset ?? 0)
      if (route.target === 'masterGain') masterGain += v
      else stereoWidth += v
    }
    output.masterGain = clamp(masterGain, 0, 4)
    output.stereoWidth = clamp(stereoWidth, 0, 2)
  }

  reset(): void {
    this.lfo.reset()
    this.env.reset()
  }
}
