/**
 * 多通道 HseAudioBus 测试
 *
 * 覆盖：
 * - HseAudioBus 通道/帧访问与 downmix/upmix
 * - HyperSoundEngine.processBus 处理 4 通道输入
 * - processBus 支持 sidechain HseAudioBus
 */

import { describe, it, expect } from 'vitest'
import { HseAudioBus } from '../src/dsp/HseAudioBus'
import { HyperSoundEngine, createDefaultParams } from '../src/index'

describe('HseAudioBus', () => {
  it('downmixes to stereo and writes back to multichannel', () => {
    const n = 64
    const ch0 = new Float32Array(n).fill(0.1)
    const ch1 = new Float32Array(n).fill(0.2)
    const ch2 = new Float32Array(n).fill(0.3)
    const bus = new HseAudioBus([ch0, ch1, ch2])

    expect(bus.channelCount).toBe(3)
    expect(bus.frameCount).toBe(n)

    const { l, r } = bus.downmixToStereo()
    expect(l[0]).toBeCloseTo(0.1, 6)
    expect(r[0]).toBeCloseTo(0.2, 6)

    const outL = new Float32Array(n).fill(0.5)
    const outR = new Float32Array(n).fill(0.6)
    const outBus = new HseAudioBus([new Float32Array(n), new Float32Array(n), new Float32Array(n)])
    outBus.writeStereo(outL, outR)
    expect(outBus.getChannel(0)[0]).toBeCloseTo(0.5, 6)
    expect(outBus.getChannel(1)[0]).toBeCloseTo(0.6, 6)
    expect(outBus.getChannel(2)[0]).toBeCloseTo(0.5, 6)
  })

  it('engine processBus handles 4 channels', () => {
    const fs = 48000
    const engine = new HyperSoundEngine(fs, 2)
    const params = createDefaultParams(fs)
    params.limiter.enabled = false
    engine.setParams(params)

    const n = 128
    const input = new HseAudioBus([
      new Float32Array(n).fill(0.1),
      new Float32Array(n).fill(0.1),
      new Float32Array(n).fill(0.1),
      new Float32Array(n).fill(0.1),
    ])
    const output = new HseAudioBus([
      new Float32Array(n),
      new Float32Array(n),
      new Float32Array(n),
      new Float32Array(n),
    ])

    engine.processBus(input, output)
    expect(output.getChannel(0)[0]).toBeCloseTo(0.1, 6)
    expect(output.getChannel(1)[0]).toBeCloseTo(0.1, 6)
    expect(output.getChannel(2)[0]).toBeCloseTo(0.1, 6)
    expect(output.getChannel(3)[0]).toBeCloseTo(0.1, 6)
  })
})
describe('HseAudioBus 多通道工具', () => {
  it('create / fromInterleaved / toInterleaved 往返一致', () => {
    const bus = HseAudioBus.create(4, 3)
    bus.channels[0].set([0, 1, 2])
    bus.channels[1].set([3, 4, 5])
    bus.channels[2].set([6, 7, 8])
    bus.channels[3].set([9, 10, 11])
    const inter = bus.toInterleaved()
    expect(Array.from(inter)).toEqual([0, 3, 6, 9, 1, 4, 7, 10, 2, 5, 8, 11])
    const back = HseAudioBus.fromInterleaved(inter, 4)
    for (let c = 0; c < 4; c++) {
      expect(Array.from(back.channels[c])).toEqual(Array.from(bus.channels[c]))
    }
  })

  it('copyTo / fill / applyGain / mixFrom / downmixToMono / extract', () => {
    const a = HseAudioBus.create(3, 4)
    a.channels[0].set([1, 1, 1, 1])
    a.channels[1].set([2, 2, 2, 2])
    a.channels[2].set([3, 3, 3, 3])

    const b = HseAudioBus.create(3, 4)
    a.copyTo(b)
    expect(b.getChannel(0)[0]).toBe(1)
    expect(b.getChannel(2)[3]).toBe(3)

    b.fill(0.5)
    expect(b.getChannel(1)[2]).toBe(0.5)

    a.applyGain(2)
    expect(a.getChannel(0)[0]).toBe(2)
    expect(a.getChannel(2)[0]).toBe(6)

    a.mixFrom(b, 2)
    expect(a.getChannel(0)[0]).toBeCloseTo(3, 6) // 2 + 0.5*2
    expect(a.getChannel(1)[0]).toBeCloseTo(5, 6) // 4 + 0.5*2

    const mono = a.downmixToMono()
    expect(mono[0]).toBeCloseTo((3 + 5 + 7) / 3, 6) // (3 + 5 + 7)/3

    const sub = a.extract([0, 2])
    expect(sub.channelCount).toBe(2)
    expect(sub.getChannel(0)[0]).toBe(3)
    expect(sub.getChannel(1)[0]).toBe(7)
  })

  it('perChannelPair：5.1(6ch) 每对独立处理，不跨对串扰', () => {
    const fs = 48000
    const engine = new HyperSoundEngine(fs, 2)
    const params = createDefaultParams(fs)
    params.limiter.enabled = false
    // 只开启混响?不——用确定性的纯增益验证:开启 compressor 会引入包络耦合。
    // 用最简单的"同相但不同幅度"验证每对独立:引擎默认无效果时是直通?默认场景是原声监听,
    // 但 loudnessCompensation 等可能带增益。这里关掉所有可能改变幅度的效果,验证直通一致性。
    params.loudnessCompensation.enabled = false
    params.loudnessNormalization.enabled = false
    params.compressor.enabled = false
    params.deesser.enabled = false
    params.bassEnhancer.enabled = false
    params.nightMode.enabled = false
    params.eq.enabled = false
    params.reverb.enabled = false
    params.modulation.enabled = false
    params.ieq.enabled = false
    params.pitch.enabled = false
    engine.setParams(params)

    const n = 256
    const input = new HseAudioBus([
      new Float32Array(n).fill(0.1), // 对0 L
      new Float32Array(n).fill(0.1), // 对0 R
      new Float32Array(n).fill(0.2), // 对1 L
      new Float32Array(n).fill(0.2), // 对1 R
      new Float32Array(n).fill(0.3), // 对2 L
      new Float32Array(n).fill(0.3), // 对2 R
    ])
    const output = new HseAudioBus(HseAudioBus.create(6, n).channels)
    engine.processBus(input, output, undefined, { mode: 'perChannelPair' })
    // 直通下每对独立:输出应近似等于输入(无效果链)
    for (let c = 0; c < 6; c++) {
      expect(output.getChannel(c)[0]).toBeCloseTo(input.getChannel(c)[0], 6)
    }
  })

  it('perChannelPair：奇数通道(5ch) 剩余通道取 L 写回', () => {
    const fs = 48000
    const engine = new HyperSoundEngine(fs, 2)
    const params = createDefaultParams(fs)
    params.limiter.enabled = false
    params.loudnessCompensation.enabled = false
    params.loudnessNormalization.enabled = false
    params.compressor.enabled = false
    params.deesser.enabled = false
    params.bassEnhancer.enabled = false
    params.nightMode.enabled = false
    params.eq.enabled = false
    params.reverb.enabled = false
    params.modulation.enabled = false
    params.ieq.enabled = false
    params.pitch.enabled = false
    engine.setParams(params)

    const n = 128
    const input = new HseAudioBus([
      new Float32Array(n).fill(0.1),
      new Float32Array(n).fill(0.1),
      new Float32Array(n).fill(0.2),
      new Float32Array(n).fill(0.2),
      new Float32Array(n).fill(0.3), // 单声道剩余
    ])
    const output = new HseAudioBus(HseAudioBus.create(5, n).channels)
    engine.processBus(input, output, undefined, { mode: 'perChannelPair' })
    for (let c = 0; c < 5; c++) {
      expect(output.getChannel(c)[0]).toBeCloseTo(input.getChannel(c)[0], 6)
    }
  })

  it('perChannelPair：sidechain 按对驱动压缩，仅目标对衰减', () => {
    const fs = 48000
    const engine = new HyperSoundEngine(fs, 2)
    const params = createDefaultParams(fs)
    params.limiter.enabled = false
    params.loudnessCompensation.enabled = false
    params.loudnessNormalization.enabled = false
    params.deesser.enabled = false
    params.bassEnhancer.enabled = false
    params.nightMode.enabled = false
    params.eq.enabled = false
    params.reverb.enabled = false
    params.modulation.enabled = false
    params.ieq.enabled = false
    params.pitch.enabled = false
    params.compressor.enabled = true
    params.compressor.thresholdDb = -30
    params.compressor.ratio = 8
    params.compressor.attackMs = 1
    params.compressor.releaseMs = 100
    params.compressor.sidechainEnabled = true
    engine.setParams(params)

    const n = 4800
    // 4 通道:对0 有强 sidechain,对1 无 sidechain
    const input = new HseAudioBus([
      new Float32Array(n).fill(0.01),
      new Float32Array(n).fill(0.01),
      new Float32Array(n).fill(0.01),
      new Float32Array(n).fill(0.01),
    ])
    const side = new HseAudioBus([
      new Float32Array(n).fill(1.0),
      new Float32Array(n).fill(1.0),
      new Float32Array(n).fill(0.01), // 对1 的 sidechain 很弱
      new Float32Array(n).fill(0.01),
    ])
    const output = new HseAudioBus(HseAudioBus.create(4, n).channels)
    engine.processBus(input, output, side, { mode: 'perChannelPair' })
    // 对0 被强 sidechain 压缩
    expect(output.getChannel(0)[output.getChannel(0).length - 1]).toBeLessThan(0.005)
    expect(output.getChannel(1)[output.getChannel(1).length - 1]).toBeLessThan(0.005)
    // 对1 几乎不被压缩(主信号低于阈值,侧链也弱)
    expect(output.getChannel(2)[output.getChannel(2).length - 1]).toBeGreaterThan(0.008)
    expect(output.getChannel(3)[output.getChannel(3).length - 1]).toBeGreaterThan(0.008)
  })
})
