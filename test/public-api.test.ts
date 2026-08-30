/**
 * 独立包公共 API 契约测试
 *
 * 目标：
 * - 核心入口 `src/index.ts` 不导出浏览器宿主（保持核心纯净）；
 * - 浏览器入口 `src/browser.ts` 提供 HyperSoundEngineHost / createHyperSoundEngineHost；
 * - `createEngine` 工厂返回符合 AudioEngine 形态的实例。
 */

import { describe, it, expect } from 'vitest'
import * as core from '../src/index'
import * as browser from '../src/browser'
import { createEngine } from '../src/index'
import { createDefaultParams } from '../src/types'

describe('public API (standalone core)', () => {
  it('core entry exposes engine factory and core modules', () => {
    expect(typeof core.createEngine).toBe('function')
    expect(typeof core.createHyperSoundEngine).toBe('function')
    expect(typeof core.HyperSoundEngine).toBe('function')
    expect(typeof core.SCENE_PRESETS).toBe('object')
    expect(typeof core.encodeShareCode).toBe('function')
    expect(typeof core.encodeWav).toBe('function')
    expect(typeof core.decodeWav).toBe('function')
    expect(typeof core.computeRelativeDirection).toBe('function')
    expect(typeof core.wrapAzimuthDeg).toBe('function')
    expect(typeof core.SpectrumAnalyzer).toBe('function')
  })

  it('core entry does not expose browser host', () => {
    expect((core as unknown as { HyperSoundEngineHost?: unknown }).HyperSoundEngineHost).toBeUndefined()
  })

  it('browser entry exposes host and factory', () => {
    expect(typeof browser.HyperSoundEngineHost).toBe('function')
    expect(typeof browser.createHyperSoundEngineHost).toBe('function')
  })

  it('createEngine returns a working AudioEngine', () => {
    const fs = 48000
    const engine = createEngine(fs, 2)
    const params = createDefaultParams(fs)
    engine.setParams(params)

    const n = 128
    const inL = new Float32Array(n).fill(0.1)
    const inR = new Float32Array(n).fill(0.1)
    const outL = new Float32Array(n)
    const outR = new Float32Array(n)
    engine.process([inL, inR], [outL, outR])

    expect(engine.getLatencySamples()).toBeGreaterThanOrEqual(0)
    expect(engine.getStats()).toBeTruthy()
    expect(engine.getAnalysis()).toBeTruthy()
    expect(typeof engine.reset).toBe('function')
  })

  it('getParams returns a detached snapshot', () => {
    const fs = 48000
    const engine = createEngine(fs, 2)
    const params = createDefaultParams(fs)
    params.eq.simpleBands[0] = 3
    engine.setParams(params)

    const snap = engine.getParams()
    expect(snap.eq.simpleBands[0]).toBe(3)
    snap.eq.simpleBands[0] = 99
    expect(engine.getParams().eq.simpleBands[0]).toBe(3)
  })

  it('prepare preallocates and process still works', () => {
    const fs = 48000
    const engine = createEngine(fs, 2)
    engine.setParams(createDefaultParams(fs))
    engine.prepare(512)

    const n = 512
    const inL = new Float32Array(n).fill(0.1)
    const inR = new Float32Array(n).fill(0.1)
    const outL = new Float32Array(n)
    const outR = new Float32Array(n)
    expect(() => engine.process([inL, inR], [outL, outR])).not.toThrow()
  })
})
