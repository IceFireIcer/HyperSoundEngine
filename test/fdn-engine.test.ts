/**
 * FDN 混响引擎接线测试（mode='fdn'）
 *
 * 覆盖：
 * - reverb.mode='fdn' 时引擎走 FDN 网络混响,处理无 NaN、输出有界
 * - FDN 湿路有处理(与关闭混响的输出不同)
 * - mode='off' 完全直通
 * - ShareCodec 往返保留 mode='fdn'
 */

import { describe, it, expect } from 'vitest'
import { HyperSoundEngine, createDefaultParams, encodeShareCode, decodeShareCode } from '../src/index'

describe('FDN 引擎接线', () => {
  it('mode=fdn 启用后处理无 NaN、输出有界且与关混响不同', () => {
    const fs = 48000
    const engine = new HyperSoundEngine(fs, 2)
    const p = createDefaultParams(fs)
    p.reverb.enabled = true
    p.reverb.mode = 'fdn'
    p.reverb.algorithmic.type = 'hall'
    p.reverb.algorithmic.roomSize = 0.6
    p.reverb.algorithmic.wet = 0.4
    p.reverb.algorithmic.dry = 0.6
    engine.setParams(p)

    const n = 128
    const inL = new Float32Array(n).fill(0.3)
    const inR = new Float32Array(n).fill(0.3)
    const outL = new Float32Array(n)
    const outR = new Float32Array(n)
    let maxPeak = 0
    for (let blk = 0; blk < 200; blk++) {
      engine.process([inL, inR], [outL, outR])
      for (let i = 0; i < n; i++) {
        expect(Number.isFinite(outL[i])).toBe(true)
        expect(Number.isFinite(outR[i])).toBe(true)
        maxPeak = Math.max(maxPeak, Math.abs(outL[i]), Math.abs(outR[i]))
      }
    }
    expect(maxPeak).toBeLessThanOrEqual(3)

    // FDN 湿路有处理:输出与关闭混响不同
    const e2 = new HyperSoundEngine(fs, 2)
    const p2 = createDefaultParams(fs)
    p2.reverb.enabled = false
    e2.setParams(p2)
    const o2 = [new Float32Array(n), new Float32Array(n)]
    e2.process([new Float32Array(n).fill(0.3), new Float32Array(n).fill(0.3)], o2)
    expect(Math.abs(outL[127] - o2[0][127])).toBeGreaterThan(1e-4)
  })

  it('mode=off 完全直通（禁用限幅器消除 lookahead 延迟）', () => {
    const fs = 48000
    const engine = new HyperSoundEngine(fs, 2)
    const p = createDefaultParams(fs)
    p.limiter.enabled = false
    p.reverb.enabled = true
    p.reverb.mode = 'off'
    engine.setParams(p)
    const n = 128
    const inL = new Float32Array(n).fill(0.2)
    const inR = new Float32Array(n).fill(0.2)
    const outL = new Float32Array(n)
    const outR = new Float32Array(n)
    engine.process([inL, inR], [outL, outR])
    for (let i = 0; i < n; i++) {
      expect(outL[i]).toBeCloseTo(inL[i], 6)
      expect(outR[i]).toBeCloseTo(inR[i], 6)
    }
  })

  it('ShareCodec 往返保留 mode=fdn', () => {
    const fs = 48000
    const p = createDefaultParams(fs)
    p.reverb.enabled = true
    p.reverb.mode = 'fdn'
    const code = encodeShareCode(p)
    const back = decodeShareCode(code)
    expect(back.reverb.mode).toBe('fdn')
  })

  it('确定性:同参数同输入两次处理逐样本一致', () => {
    const fs = 48000
    const mk = () => {
      const e = new HyperSoundEngine(fs, 2)
      const p = createDefaultParams(fs)
      p.reverb.enabled = true
      p.reverb.mode = 'fdn'
      p.reverb.algorithmic.roomSize = 0.5
      e.setParams(p)
      return e
    }
    const e1 = mk()
    const e2 = mk()
    const n = 256
    const i1 = new Float32Array(n).fill(0.15)
    const i2 = new Float32Array(n).fill(0.15)
    const o1 = [new Float32Array(n), new Float32Array(n)]
    const o2 = [new Float32Array(n), new Float32Array(n)]
    e1.process([i1, i1], o1)
    e2.process([i2, i2], o2)
    for (let i = 0; i < n; i++) {
      expect(o1[0][i]).toBe(o2[0][i])
      expect(o1[1][i]).toBe(o2[1][i])
    }
  })
})
