/**
 * 参数调制矩阵测试
 *
 * 覆盖：
 * - LFO 输出范围与相位推进
 * - EnvelopeFollower 包络跟随
 * - ModulationMatrix 路由到 masterGain / stereoWidth
 * - HyperSoundEngine 中 LFO→masterGain 调制使输出幅度变化
 */

import { describe, it, expect } from 'vitest'
import { Lfo, EnvelopeFollower, ModulationMatrix } from '../src/dsp/modulation'
import { HyperSoundEngine, createDefaultParams } from '../src/index'

describe('modulation', () => {
  it('LFO produces bounded bipolar values and advances', () => {
    const lfo = new Lfo(48000, 'sine', 1, 1)
    const values: number[] = []
    for (let i = 0; i < 10; i++) values.push(lfo.processBlock(480)) // 10ms blocks
    for (const v of values) {
      expect(v).toBeGreaterThanOrEqual(-1)
      expect(v).toBeLessThanOrEqual(1)
    }
    expect(new Set(values.map((v) => v.toFixed(3))).size).toBeGreaterThan(1)
  })

  it('EnvelopeFollower tracks input amplitude', () => {
    const env = new EnvelopeFollower(48000, 1, 200, 1)
    const n = 4800
    const l = new Float32Array(n).fill(0.5)
    const r = new Float32Array(n).fill(0.5)
    const v = env.processBlock(l, r, n)
    expect(v).toBeGreaterThan(0.4)
    expect(v).toBeLessThanOrEqual(1)
  })

  it('ModulationMatrix routes LFO to masterGain', () => {
    const matrix = new ModulationMatrix(48000, [
      { source: 'lfo', target: 'masterGain', amount: 0.5 },
    ])
    const n = 480
    const l = new Float32Array(n).fill(0.1)
    const r = new Float32Array(n).fill(0.1)
    const gains = new Set<number>()
    for (let i = 0; i < 20; i++) {
      gains.add(matrix.processBlock(l, r, n).masterGain)
    }
    expect(gains.size).toBeGreaterThan(1)
  })

  it('ModulationMatrix preserves snapshot semantics and supports allocation-free output', () => {
    const matrix = new ModulationMatrix(48000)
    const l = new Float32Array(128)
    const r = new Float32Array(128)

    const first = matrix.processBlock(l, r, 128)
    const second = matrix.processBlock(l, r, 128)
    expect(second).not.toBe(first)
    expect(second).toEqual({ masterGain: 1, stereoWidth: 1 })

    const output = { masterGain: 0, stereoWidth: 0 }
    matrix.processBlockInto(l, r, 128, output)
    expect(output).toEqual({ masterGain: 1, stereoWidth: 1 })
  })

  it('Engine applies LFO master-gain modulation', () => {
    const fs = 48000
    const engine = new HyperSoundEngine(fs, 2)
    const params = createDefaultParams(fs)
    params.limiter.enabled = false
    params.modulation.enabled = true
    params.modulation.lfo = { enabled: true, shape: 'sine', rateHz: 5, depth: 0.5 }
    params.modulation.routes = [{ source: 'lfo', target: 'masterGain', amount: 0.5 }]
    engine.setParams(params)

    const block = 480 // 10ms
    const inL = new Float32Array(block).fill(0.1)
    const inR = new Float32Array(block).fill(0.1)
    const outL = new Float32Array(block)
    const outR = new Float32Array(block)

    const rmsValues: number[] = []
    for (let i = 0; i < 30; i++) {
      engine.process([inL, inR], [outL, outR])
      let sum = 0
      for (let j = 0; j < block; j++) sum += outL[j] * outL[j]
      rmsValues.push(Math.sqrt(sum / block))
    }
    const min = Math.min(...rmsValues)
    const max = Math.max(...rmsValues)
    expect(max - min).toBeGreaterThan(0.01)
  })
})
