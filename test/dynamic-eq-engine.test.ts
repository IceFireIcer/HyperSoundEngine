/**
 * DynamicEq 引擎接线测试（dynamicEq.enabled）
 *
 * 覆盖：
 * - dynamicEq.enabled=true 时引擎走动态均衡,无 NaN、输出有界
 * - strength=0 时直通
 * - 低频主导输入下低带增益被压低(getBandGains 不可达引擎,用输出电平对比)
 * - ShareCodec 往返保留 dynamicEq 设置
 * - 确定性
 */

import { describe, it, expect } from 'vitest'
import { HyperSoundEngine, createDefaultParams, encodeShareCode, decodeShareCode } from '../src/index'

function makeEngine(fs = 48000, patch?: (p: ReturnType<typeof createDefaultParams>) => void): HyperSoundEngine {
  const e = new HyperSoundEngine(fs, 2)
  const p = createDefaultParams(fs)
  p.limiter.enabled = false
  if (patch) patch(p)
  e.setParams(p)
  return e
}

describe('DynamicEq 引擎接线', () => {
  it('dynamicEq.enabled=true 处理无 NaN、输出有界', () => {
    const fs = 48000
    const engine = makeEngine(fs, (p) => {
      p.dynamicEq.enabled = true
      p.dynamicEq.strength = 1
      p.dynamicEq.thresholdDb = -30
    })
    const n = 128
    const inL = new Float32Array(n).fill(0.5)
    const inR = new Float32Array(n).fill(0.5)
    const outL = new Float32Array(n)
    const outR = new Float32Array(n)
    for (let blk = 0; blk < 100; blk++) {
      engine.process([inL, inR], [outL, outR])
      for (let i = 0; i < n; i++) {
        expect(Number.isFinite(outL[i])).toBe(true)
        expect(Number.isFinite(outR[i])).toBe(true)
        expect(Math.abs(outL[i])).toBeLessThanOrEqual(3)
      }
    }
  })

  it('enabled=false 完全直通', () => {
    const fs = 48000
    const engine = makeEngine(fs, (p) => { p.dynamicEq.enabled = false })
    const n = 128
    const inL = new Float32Array(n).fill(0.3)
    const inR = new Float32Array(n).fill(0.3)
    const outL = new Float32Array(n)
    const outR = new Float32Array(n)
    engine.process([inL, inR], [outL, outR])
    for (let i = 0; i < n; i++) {
      expect(outL[i]).toBeCloseTo(inL[i], 6)
      expect(outR[i]).toBeCloseTo(inR[i], 6)
    }
  })

  it('低频主导输入下动态均衡压低低带输出', () => {
    const fs = 48000
    const n = 4096
    // 100Hz 正弦(低频主导) + 少量白噪声
    const mk = (v: number) => {
      const a = new Float32Array(n)
      for (let i = 0; i < n; i++) a[i] = v * (Math.sin(2 * Math.PI * 100 * (i / fs)) * 0.8 + 0.05)
      return a
    }
    const engineOn = makeEngine(fs, (p) => {
      p.dynamicEq.enabled = true
      p.dynamicEq.strength = 1
      p.dynamicEq.thresholdDb = -35
      p.dynamicEq.ratio = 4
    })
    const engineOff = makeEngine(fs, (p) => { p.dynamicEq.enabled = false })
    const oOn = [new Float32Array(n), new Float32Array(n)]
    const oOff = [new Float32Array(n), new Float32Array(n)]
    engineOn.process([mk(0.5), mk(0.5)], oOn)
    engineOff.process([mk(0.5), mk(0.5)], oOff)
    // 动态均衡应在某段时间后压低低带:输出 RMS 低于直通
    const rms = (x: Float32Array) => {
      let s = 0
      for (let i = 0; i < x.length; i++) s += x[i] * x[i]
      return Math.sqrt(s / x.length)
    }
    expect(rms(oOn[0])).toBeLessThan(rms(oOff[0]) * 0.95)
  })

  it('ShareCodec 往返保留 dynamicEq 设置', () => {
    const fs = 48000
    const p = createDefaultParams(fs)
    p.dynamicEq.enabled = true
    p.dynamicEq.strength = 0.7
    p.dynamicEq.thresholdDb = -25
    p.dynamicEq.bands[0].targetGainDb = -3
    const code = encodeShareCode(p)
    const back = decodeShareCode(code)
    expect(back.dynamicEq.enabled).toBe(true)
    expect(back.dynamicEq.strength).toBeCloseTo(0.7, 6)
    expect(back.dynamicEq.thresholdDb).toBeCloseTo(-25, 6)
    expect(back.dynamicEq.bands.length).toBe(5)
    expect(back.dynamicEq.bands[0].targetGainDb).toBeCloseTo(-3, 6)
  })

  it('确定性:同参数同输入两次处理逐样本一致', () => {
    const fs = 48000
    const mk = () => makeEngine(fs, (p) => { p.dynamicEq.enabled = true; p.dynamicEq.strength = 0.8 })
    const e1 = mk()
    const e2 = mk()
    const n = 256
    const i1 = new Float32Array(n).fill(0.25)
    const i2 = new Float32Array(n).fill(0.25)
    const o1 = [new Float32Array(n), new Float32Array(n)]
    const o2 = [new Float32Array(n), new Float32Array(n)]
    e1.process([i1, i1], o1)
    e2.process([i2, i2], o2)
    for (let i = 0; i < n; i++) {
      expect(o1[0][i]).toBe(o2[0][i])
    }
  })
})
