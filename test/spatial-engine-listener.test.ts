import { describe, expect, it } from 'vitest'
import { HyperSoundEngine, createDefaultParams } from '../src/index'

const SAMPLE_RATE = 48000
const FRAMES = 4096

function renderWorld(
  yaw: number,
  orientation: { pitch?: number; roll?: number } = {},
  ambience: { enabled: boolean; amount: number } = { enabled: false, amount: 0 },
): { left: Float32Array; right: Float32Array; latency: number } {
  const engine = new HyperSoundEngine(SAMPLE_RATE, 2)
  const params = createDefaultParams(SAMPLE_RATE)
  params.eq.enabled = false
  params.deesser.enabled = false
  params.compressor.enabled = false
  params.nightMode.enabled = false
  params.reverb.enabled = false
  params.bassEnhancer.enabled = false
  params.loudnessCompensation.enabled = false
  params.dynamicEq.enabled = false
  params.limiter.enabled = false
  params.modulation.enabled = false
  const spatial = params.spatial
  if (!spatial) throw new Error('default params must include spatial settings')
  spatial.mode = 'world'
  spatial.masterGain = 1
  spatial.instant.amount = 1
  spatial.instant.room = 'off'
  spatial.instant.roomAmount = 0
  spatial.world.listener = {
    position: { x: 0, y: 0, z: 0 },
    yaw,
    pitch: orientation.pitch ?? 0,
    roll: orientation.roll ?? 0,
  }
  spatial.ambience = ambience
  spatial.world.sources = [{
    id: 'source',
    position: { x: 0, y: 0, z: 2 },
    gain: 1,
    size: 0,
  }]
  engine.setParams(params)
  engine.prepare(FRAMES)

  const input = new Float32Array(FRAMES)
  input[0] = 1
  const left = new Float32Array(FRAMES)
  const right = new Float32Array(FRAMES)
  engine.process([input, input], [left, right])
  return { left, right, latency: engine.getLatencySamples() }
}

describe('HyperSoundEngine world listener 接线', () => {
  it('yaw 跨整圈后第 22 级输出保持逐样本等价', () => {
    const wrapped = renderWorld(30)
    const fullTurn = renderWorld(390)
    const front = renderWorld(0)

    expect(fullTurn.left).toEqual(wrapped.left)
    expect(fullTurn.right).toEqual(wrapped.right)
    let difference = 0
    for (let i = wrapped.latency; i < FRAMES; i++) {
      difference += Math.abs(wrapped.left[i] - front.left[i]) + Math.abs(wrapped.right[i] - front.right[i])
    }
    expect(difference).toBeGreaterThan(1e-4)
  })

  it('ambience enabled/amount 接入实际空间输出，关闭或 amount=0 保持基线', () => {
    const disabled = renderWorld(0)
    const zero = renderWorld(0, {}, { enabled: true, amount: 0 })
    const enabled = renderWorld(0, {}, { enabled: true, amount: 1 })
    expect(zero.left).toEqual(disabled.left)
    expect(zero.right).toEqual(disabled.right)
    let difference = 0
    for (let i = disabled.latency; i < FRAMES; i++) {
      difference += Math.abs(disabled.left[i] - enabled.left[i]) + Math.abs(disabled.right[i] - enabled.right[i])
    }
    expect(difference).toBeGreaterThan(1e-4)
  })
})
