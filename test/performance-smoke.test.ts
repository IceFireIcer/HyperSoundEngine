/**
 * 性能冒烟测试（宽松阈值，防灾难性回归）
 *
 * 不追求精确基准（那由 scripts/benchmark.mjs 负责），只保证默认参数全链
 * 处理 1 秒 48kHz 立体声音频不会慢到不可接受。
 */

import { describe, it, expect } from 'vitest'
import { createEngine, createDefaultParams } from '../src/index'

describe('performance smoke', () => {
  it('processes 1s of 48kHz stereo within a generous budget', () => {
    const fs = 48000
    const block = 128
    const engine = createEngine(fs, 2)
    engine.setParams(createDefaultParams(fs))
    engine.prepare(block)

    const inL = new Float32Array(block).fill(0.1)
    const inR = new Float32Array(block).fill(0.1)
    const outL = new Float32Array(block)
    const outR = new Float32Array(block)

    // 预热
    for (let i = 0; i < 50; i++) engine.process([inL, inR], [outL, outR])

    const totalBlocks = fs / block
    const start = performance.now()
    for (let i = 0; i < totalBlocks; i++) engine.process([inL, inR], [outL, outR])
    const elapsedMs = performance.now() - start

    // 1 秒音频正常应在几十毫秒内完成；这里留 100 倍余量，只拦灾难性回退。
    expect(elapsedMs).toBeLessThan(5000)
  })
})
