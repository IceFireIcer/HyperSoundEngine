/**
 * MIDI 事件接口 / MIDI Learn 测试（引擎级行为 seam）
 *
 * 覆盖（PRD Testing Decisions）：
 * - 喂 CC 事件 → getParams() 目标参数按范围映射变化
 * - clamp：CC 全量程两端映射到 min/max
 * - 平滑：smoothMs=0 直接到位；smoothMs>0 多块收敛且单调
 * - note on/off 驱动布尔参数开关；note 驱动数值参数取 max/min
 * - learn 非法路径抛错；unlearn 后 CC 不再生效；未绑定 CC 无副作用
 * - 确定性：同事件序列两次处理结果一致
 */

import { describe, it, expect } from 'vitest'
import { HyperSoundEngine, createDefaultParams } from '../src/index'
import type { MidiEvent } from '../src/types'

function makeEngine(fs = 48000): HyperSoundEngine {
  const e = new HyperSoundEngine(fs, 2)
  const p = createDefaultParams(fs)
  p.limiter.enabled = false
  p.loudnessCompensation.enabled = false
  p.loudnessNormalization.enabled = false
  p.modulation.enabled = false
  p.eq.enabled = false
  p.ieq.enabled = false
  p.nightMode.enabled = false
  p.pitch.enabled = false
  e.setParams(p)
  return e
}

function inBuf(n: number, v: number): Float32Array[] {
  return [new Float32Array(n).fill(v), new Float32Array(n).fill(v)]
}
function outBuf(n: number): Float32Array[] {
  return [new Float32Array(n), new Float32Array(n)]
}

describe('MIDI Learn / 事件接口', () => {
  it('CC 事件按范围线性映射到目标参数（smoothMs=0 直接到位）', () => {
    const e = makeEngine()
    e.setParams({ ...e.getParams(), compressor: { ...e.getParams().compressor, enabled: true } })
    e.midiLearn(7, { kind: 'path', path: 'compressor.thresholdDb' }, { min: -60, max: 0, smoothMs: 0 })

    e.sendMidi([{ type: 'cc', channel: 0, cc: 7, value: 64 }])
    e.process(inBuf(128, 0.1), outBuf(128))
    expect(e.getParams().compressor.thresholdDb).toBeCloseTo(-60 + (64 / 127) * 60, 4)
  })

  it('clamp：CC 0 → min，CC 127 → max', () => {
    const e = makeEngine()
    e.setParams({ ...e.getParams(), compressor: { ...e.getParams().compressor, enabled: true } })
    e.midiLearn(1, { kind: 'path', path: 'compressor.thresholdDb' }, { min: -60, max: 0, smoothMs: 0 })

    e.sendMidi([{ type: 'cc', channel: 0, cc: 1, value: 0 }])
    e.process(inBuf(128, 0.1), outBuf(128))
    expect(e.getParams().compressor.thresholdDb).toBeCloseTo(-60, 4)

    e.sendMidi([{ type: 'cc', channel: 0, cc: 1, value: 127 }])
    e.process(inBuf(128, 0.1), outBuf(128))
    expect(e.getParams().compressor.thresholdDb).toBeCloseTo(0, 4)
  })

  it('invert：CC 0 → max，CC 127 → min', () => {
    const e = makeEngine()
    e.setParams({ ...e.getParams(), compressor: { ...e.getParams().compressor, enabled: true } })
    e.midiLearn(1, { kind: 'path', path: 'compressor.thresholdDb' }, { min: -60, max: 0, smoothMs: 0, invert: true })

    e.sendMidi([{ type: 'cc', channel: 0, cc: 1, value: 0 }])
    e.process(inBuf(128, 0.1), outBuf(128))
    expect(e.getParams().compressor.thresholdDb).toBeCloseTo(0, 4)

    e.sendMidi([{ type: 'cc', channel: 0, cc: 1, value: 127 }])
    e.process(inBuf(128, 0.1), outBuf(128))
    expect(e.getParams().compressor.thresholdDb).toBeCloseTo(-60, 4)
  })

  it('smoothMs>0：参数向目标单调收敛', () => {
    const e = makeEngine()
    e.setParams({ ...e.getParams(), compressor: { ...e.getParams().compressor, enabled: true } })
    e.midiLearn(7, { kind: 'path', path: 'compressor.thresholdDb' }, { min: -60, max: 0, smoothMs: 200 })

    e.sendMidi([{ type: 'cc', channel: 0, cc: 7, value: 127 }]) // target=0
    let prev = e.getParams().compressor.thresholdDb // 起始 ≈ -20（默认）
    let converged = false
    for (let blk = 0; blk < 80; blk++) {
      e.process(inBuf(256, 0.1), outBuf(256))
      const cur = e.getParams().compressor.thresholdDb
      expect(cur).toBeGreaterThanOrEqual(prev) // 单调非降
      prev = cur
      if (Math.abs(cur - 0) < 0.01) converged = true
    }
    expect(converged).toBe(true)
  })

  it('note on/off 驱动布尔参数开关', () => {
    const e = makeEngine()
    e.midiLearn(60, { kind: 'path', path: 'reverb.enabled' }, { eventType: 'note', min: 0, max: 1, smoothMs: 0 })
    expect(e.getParams().reverb.enabled).toBe(false)

    e.sendMidi([{ type: 'noteOn', channel: 0, note: 60, velocity: 100 }])
    e.process(inBuf(128, 0.1), outBuf(128))
    expect(e.getParams().reverb.enabled).toBe(true)

    e.sendMidi([{ type: 'noteOff', channel: 0, note: 60 }])
    e.process(inBuf(128, 0.1), outBuf(128))
    expect(e.getParams().reverb.enabled).toBe(false)
  })

  it('note 驱动数值参数：noteOn→max，noteOff→min', () => {
    const e = makeEngine()
    e.setParams({ ...e.getParams(), compressor: { ...e.getParams().compressor, enabled: true } })
    e.midiLearn(48, { kind: 'path', path: 'compressor.thresholdDb' }, { eventType: 'note', min: -60, max: 0, smoothMs: 0 })

    e.sendMidi([{ type: 'noteOn', channel: 0, note: 48, velocity: 100 }])
    e.process(inBuf(128, 0.1), outBuf(128))
    expect(e.getParams().compressor.thresholdDb).toBeCloseTo(0, 4)

    e.sendMidi([{ type: 'noteOff', channel: 0, note: 48 }])
    e.process(inBuf(128, 0.1), outBuf(128))
    expect(e.getParams().compressor.thresholdDb).toBeCloseTo(-60, 4)
  })

  it('learn 非法路径立即抛错', () => {
    const e = makeEngine()
    expect(() => e.midiLearn(7, { kind: 'path', path: 'compressor.nonexistent' })).toThrow(/unknown automatable path/)
    expect(() => e.midiLearn(7, { kind: 'path', path: 'fake.module.field' })).toThrow(/unknown automatable path/)
  })

  it('unlearn 后 CC 不再生效；未绑定 CC 无副作用', () => {
    const e = makeEngine()
    e.setParams({ ...e.getParams(), compressor: { ...e.getParams().compressor, enabled: true } })
    e.midiLearn(7, { kind: 'path', path: 'compressor.thresholdDb' }, { min: -60, max: 0, smoothMs: 0 })
    const before = e.getParams().compressor.thresholdDb

    expect(e.midiUnlearn(7)).toBe(true)
    e.sendMidi([{ type: 'cc', channel: 0, cc: 7, value: 127 }])
    e.process(inBuf(128, 0.1), outBuf(128))
    expect(e.getParams().compressor.thresholdDb).toBe(before)

    // 未绑定 CC（如 cc 99）无副作用
    e.sendMidi([{ type: 'cc', channel: 0, cc: 99, value: 127 }])
    e.process(inBuf(128, 0.1), outBuf(128))
    expect(e.getParams().compressor.thresholdDb).toBe(before)
  })

  it('getMidiBindings 返回当前绑定副本', () => {
    const e = makeEngine()
    e.midiLearn(7, { kind: 'path', path: 'compressor.thresholdDb' }, { min: -60, max: 0, smoothMs: 10 })
    e.midiLearn(60, { kind: 'path', path: 'reverb.enabled' }, { eventType: 'note', min: 0, max: 1, smoothMs: 0 })
    const b = e.getMidiBindings()
    expect(b.length).toBe(2)
    expect(b.some((x) => x.cc === 7)).toBe(true)
    expect(b.some((x) => x.cc === 60)).toBe(true)
  })

  it('builtin masterGain 绑定激活 mod-master-gain stage', () => {
    const e = makeEngine() // modulation.enabled=false
    e.midiLearn(7, { kind: 'builtin', param: 'masterGain' }, { min: 0, max: 2, smoothMs: 0 })
    e.sendMidi([{ type: 'cc', channel: 0, cc: 7, value: 64 }])
    const o = outBuf(128)
    e.process(inBuf(128, 0.5), o)
    // masterGain ≈ (64/127)*2 ≈ 1.008；stage 应激活并缩放输出
    expect(o[0][0]).toBeCloseTo(0.5 * (64 / 127) * 2, 4)
  })

  it('builtin stereoWidth 绑定改变立体声宽度', () => {
    const e = makeEngine()
    e.midiLearn(1, { kind: 'builtin', param: 'stereoWidth' }, { min: 0, max: 2, smoothMs: 0 })
    e.sendMidi([{ type: 'cc', channel: 0, cc: 1, value: 127 }])
    e.process(inBuf(128, 0.1), outBuf(128))
    expect(e.getParams().stereoWidth).toBeCloseTo(2, 4)
  })

  it('确定性：同事件序列两次处理结果一致', () => {
    const e1 = makeEngine()
    const e2 = makeEngine()
    e1.setParams({ ...e1.getParams(), compressor: { ...e1.getParams().compressor, enabled: true } })
    e2.setParams({ ...e2.getParams(), compressor: { ...e2.getParams().compressor, enabled: true } })
    e1.midiLearn(7, { kind: 'path', path: 'compressor.thresholdDb' }, { min: -60, max: 0, smoothMs: 50 })
    e2.midiLearn(7, { kind: 'path', path: 'compressor.thresholdDb' }, { min: -60, max: 0, smoothMs: 50 })

    const events: MidiEvent[] = [
      { type: 'cc', channel: 0, cc: 7, value: 30 },
      { type: 'cc', channel: 0, cc: 7, value: 100 },
      { type: 'cc', channel: 0, cc: 7, value: 60 },
    ]
    for (let blk = 0; blk < 5; blk++) {
      e1.sendMidi(blk === 0 ? events : [])
      e2.sendMidi(blk === 0 ? events : [])
      e1.process(inBuf(128, 0.1), outBuf(128))
      e2.process(inBuf(128, 0.1), outBuf(128))
    }
    expect(e1.getParams().compressor.thresholdDb).toBe(e2.getParams().compressor.thresholdDb)
  })

  it('reset 保留绑定但清空运行时队列与平滑状态', () => {
    const e = makeEngine()
    e.setParams({ ...e.getParams(), compressor: { ...e.getParams().compressor, enabled: true } })
    e.midiLearn(7, { kind: 'path', path: 'compressor.thresholdDb' }, { min: -60, max: 0, smoothMs: 0 })
    e.sendMidi([{ type: 'cc', channel: 0, cc: 7, value: 127 }])
    e.process(inBuf(128, 0.1), outBuf(128))
    expect(e.getMidiBindings().length).toBe(1)

    e.reset()
    expect(e.getMidiBindings().length).toBe(1)
    const before = e.getParams().compressor.thresholdDb
    e.process(inBuf(128, 0.1), outBuf(128))
    expect(e.getParams().compressor.thresholdDb).toBe(before)
  })

  it('队列溢出：getMidiDroppedCount 累计', () => {
    const e = makeEngine()
    e.midiLearn(7, { kind: 'path', path: 'compressor.thresholdDb' }, { min: -60, max: 0, smoothMs: 0 })
    expect(e.getMidiDroppedCount()).toBe(0)
    const flood: MidiEvent[] = []
    for (let i = 0; i < 5000; i++) flood.push({ type: 'cc', channel: 0, cc: 7, value: 64 })
    e.sendMidi(flood)
    expect(e.getMidiDroppedCount()).toBeGreaterThan(0)
  })
})
