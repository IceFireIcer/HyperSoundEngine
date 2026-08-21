/**
 * 自定义处理阶段注册/移除/复位测试
 *
 * 验证 HyperSoundEngine 的模块化扩展点：
 * - registerStage 插入自定义 stage 并参与处理
 * - unregisterStage 移除后恢复原行为
 * - reset() 会调用自定义 stage 的可选 reset()
 */

import { describe, it, expect } from 'vitest'
import { HyperSoundEngine, createDefaultParams, type ProcessingStage } from '../src/index'

describe('stage registry', () => {
  it('registers, processes, resets and unregisters a custom stage', () => {
    const fs = 48000
    const engine = new HyperSoundEngine(fs, 2)
    const params = createDefaultParams(fs)
    params.limiter.enabled = false // 关闭 lookahead 延迟，便于单块断言
    engine.setParams(params)

    let resetCount = 0
    const stage: ProcessingStage = {
      id: 'test-gain',
      active: () => true,
      run: (l, r) => {
        for (let i = 0; i < l.length; i++) {
          l[i] *= 2
          r[i] *= 2
        }
      },
      reset: () => {
        resetCount++
      },
    }

    engine.registerStage(stage)
    expect(engine.getStages().some((s) => s.id === 'test-gain')).toBe(true)

    const n = 128
    const inL = new Float32Array(n).fill(0.1)
    const inR = new Float32Array(n).fill(0.1)
    const outL = new Float32Array(n)
    const outR = new Float32Array(n)

    engine.process([inL, inR], [outL, outR])
    expect(outL[0]).toBeCloseTo(0.2, 6)
    expect(outR[0]).toBeCloseTo(0.2, 6)

    engine.reset()
    expect(resetCount).toBe(1)

    expect(engine.unregisterStage('test-gain')).toBe(true)
    expect(engine.getStages().some((s) => s.id === 'test-gain')).toBe(false)

    engine.process([inL, inR], [outL, outR])
    expect(outL[0]).toBeCloseTo(0.1, 6)
    expect(outR[0]).toBeCloseTo(0.1, 6)
  })

  it('replaces an existing stage with the same id', () => {
    const fs = 48000
    const engine = new HyperSoundEngine(fs, 2)
    const params = createDefaultParams(fs)
    params.limiter.enabled = false // 关闭 lookahead 延迟，便于单块断言
    engine.setParams(params)

    const first: ProcessingStage = {
      id: 'replace-me',
      active: () => true,
      run: (l) => {
        for (let i = 0; i < l.length; i++) l[i] *= 3
      },
    }
    const second: ProcessingStage = {
      id: 'replace-me',
      active: () => true,
      run: (l) => {
        for (let i = 0; i < l.length; i++) l[i] *= 5
      },
    }

    engine.registerStage(first)
    engine.registerStage(second)

    const n = 64
    const inL = new Float32Array(n).fill(0.1)
    const inR = new Float32Array(n).fill(0.1)
    const outL = new Float32Array(n)
    const outR = new Float32Array(n)
    engine.process([inL, inR], [outL, outR])
    expect(outL[0]).toBeCloseTo(0.5, 6)
  })
})
