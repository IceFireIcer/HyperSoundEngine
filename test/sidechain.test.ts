/**
 * Sidechain 输入测试
 *
 * 覆盖：
 * - Compressor 由外部 sidechain 驱动包络
 * - Deesser 由外部 sidechain 驱动齿音检测
 * - HyperSoundEngine.process 的 sidechain 参数在 sidechainEnabled 时生效
 */

import { describe, it, expect } from 'vitest'
import { Compressor } from '../src/dsp/Compressor'
import { Deesser } from '../src/dsp/Deesser'
import { HyperSoundEngine, createDefaultParams } from '../src/index'

function rms(x: Float32Array): number {
  let s = 0
  for (let i = 0; i < x.length; i++) s += x[i] * x[i]
  return Math.sqrt(s / x.length)
}

describe('sidechain', () => {
  it('Compressor uses external sidechain to drive gain reduction', () => {
    const fs = 48000
    const c = new Compressor(fs)
    c.setParams({
      enabled: true,
      thresholdDb: -30,
      ratio: 8,
      kneeDb: 0,
      attackMs: 1,
      releaseMs: 100,
      makeupDb: 0,
      outputGain: 1,
      sidechainEnabled: true,
    })

    const n = 4800
    const mainL = new Float32Array(n).fill(0.01) // -40dB，低于阈值
    const mainR = new Float32Array(n).fill(0.01)
    const sideL = new Float32Array(n).fill(1.0) // 0dB，远高于阈值
    const sideR = new Float32Array(n).fill(1.0)
    const outL = mainL.slice()
    const outR = mainR.slice()

    // 不使用 sidechain：主信号低于阈值，几乎不衰减
    c.reset()
    c.processStereo(outL, outR)
    expect(outL[outL.length - 1]).toBeGreaterThan(0.009)

    // 使用 sidechain：强 sidechain 驱动衰减
    c.reset()
    c.processStereo(outL, outR, sideL, sideR)
    expect(outL[outL.length - 1]).toBeLessThan(0.005)
  })

  it('Deesser uses external sidechain to trigger high-frequency reduction', () => {
    const fs = 48000
    const d = new Deesser(fs)
    d.setParams({
      enabled: true,
      centerHz: 8000,
      q: 0.7,
      thresholdDb: -30,
      ratio: 8,
      attackMs: 1,
      releaseMs: 80,
      splitBand: true,
      mix: 1,
      sidechainEnabled: true,
    })

    const n = 48000
    // 主信号：较轻的 8kHz 齿音
    const mainL = new Float32Array(n)
    const mainR = new Float32Array(n)
    // sidechain：很强的 8kHz 齿音
    const sideL = new Float32Array(n)
    const sideR = new Float32Array(n)
    for (let i = 0; i < n; i++) {
      const v = Math.sin((2 * Math.PI * 8000 * i) / fs)
      mainL[i] = v * 0.05
      mainR[i] = v * 0.05
      sideL[i] = v * 1.0
      sideR[i] = v * 1.0
    }

    const outNoSide = mainL.slice()
    const outRNoSide = mainR.slice()
    d.reset()
    d.processStereo(outNoSide, outRNoSide)
    const rmsNoSide = rms(outNoSide)

    const outSide = mainL.slice()
    const outRSide = mainR.slice()
    d.reset()
    d.processStereo(outSide, outRSide, sideL, sideR)
    const rmsSide = rms(outSide)

    expect(rmsSide).toBeLessThan(rmsNoSide * 0.7)
  })

  it('Engine applies sidechain only when compressor.sidechainEnabled is true', () => {
    const fs = 48000
    const engine = new HyperSoundEngine(fs, 2)
    const params = createDefaultParams(fs)
    params.limiter.enabled = false
    params.compressor.enabled = true
    params.compressor.thresholdDb = -30
    params.compressor.ratio = 8
    params.compressor.attackMs = 1
    params.compressor.releaseMs = 100
    params.compressor.sidechainEnabled = true
    engine.setParams(params)

    const n = 4800
    const inL = new Float32Array(n).fill(0.01)
    const inR = new Float32Array(n).fill(0.01)
    const sideL = new Float32Array(n).fill(1.0)
    const sideR = new Float32Array(n).fill(1.0)
    const outL = new Float32Array(n)
    const outR = new Float32Array(n)

    engine.process([inL, inR], [outL, outR], [sideL, sideR])
    expect(outL[outL.length - 1]).toBeLessThan(0.005)

    // 关闭 sidechain 后，主信号低于阈值，不再被 sidechain 压缩
    params.compressor.sidechainEnabled = false
    engine.setParams(params)
    engine.reset() // 清掉上一次 sidechain 留下的包络
    engine.process([inL, inR], [outL, outR], [sideL, sideR])
    expect(outL[outL.length - 1]).toBeGreaterThan(0.009)
  })
})
