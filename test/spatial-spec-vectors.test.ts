import { readFileSync } from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import Ajv from 'ajv'
import { describe, expect, it } from 'vitest'
import { computeRelativeDirection } from '../src/spatial/controller'
import type { Vec3, WorldListenerPose } from '../src/spatial/types'

type WorldFixture = {
  schemaVersion: number
  fixture: string
  tolerance: { angleAbs: number; distanceAbs: number }
  cases: Array<{
    id: string
    listener: WorldListenerPose
    source: Vec3
    expected: { azimuthDeg: number; elevationDeg: number; distance: number }
  }>
}

type DistanceModel = 'inverse' | 'linear' | 'exponential'
type RendererFixture = {
  schemaVersion: number
  fixture: string
  scope: { excludedNumericParity: string[] }
  abi: {
    inputLayout: string
    inputStrideUnit: string
    objectSlotsUnit: string
    objectParams: string[]
    outputLayout: string
    lengthUnit: string
    successCode: number
  }
  tolerance: { value: number; floor: number }
  grid: {
    sampleRate: number
    azimuths: number[]
    elevations: number[]
    hrirLength: number
    directions: Array<{ azimuthDeg: number; elevationDeg: number; left: number[]; right: number[] }>
  }
  distance: {
    params: { referenceDistance: number; maximumDistance: number; rolloffFactor: number }
    cases: Array<{ id: string; model: DistanceModel; distance: number; expectedGain: number; expectedAirCoefficient: number }>
  }
  nearestCases: Array<{
    id: string
    azimuthDeg: number
    elevationDeg: number
    expected: { azimuthIndex: number; elevationIndex: number; left: number[]; right: number[] }
  }>
  rendererCases: Array<{
    id: string
    input: number[]
    inputStride: number
    objectSlots: number[]
    blockSizes: number[]
    azimuthDeg: number
    elevationDeg: number
    distance: number
    gain: number
    distanceModel: DistanceModel
    roomMode: 'off' | 'configured-zero'
    resetReplay: boolean
    expected: { left: number[]; right: number[] }
  }>
}

const root = path.join(path.dirname(fileURLToPath(import.meta.url)), '..')
const readJson = (relativePath: string): any => JSON.parse(readFileSync(path.join(root, relativePath), 'utf8'))
const worldJson = readJson('specs/spatial/vectors/world-listener.v1.json')
const worldSchema = readJson('specs/schema/world-listener.schema.json')
const world = worldJson as WorldFixture
const rendererJson = readJson('specs/spatial/vectors/renderer-abi.v1.json')
const rendererSchema = readJson('specs/schema/spatial-renderer-abi.schema.json')
const renderer = rendererJson as RendererFixture
const f32 = Math.fround

function close(got: number, want: number): boolean {
  return Math.abs(got - want) <= renderer.tolerance.value * Math.max(Math.abs(want), renderer.tolerance.floor)
}

function nearest(azimuthDeg: number, elevationDeg: number) {
  const wrap = ((azimuthDeg + 180) % 360 + 360) % 360 - 180
  let azimuthIndex = 0
  let azimuthDistance = Infinity
  renderer.grid.azimuths.forEach((candidate, index) => {
    const difference = Math.abs(wrap - candidate)
    const distance = Math.min(difference, 360 - difference)
    if (distance < azimuthDistance) {
      azimuthIndex = index
      azimuthDistance = distance
    }
  })
  const elevations = renderer.grid.elevations
  const elevation = Math.max(elevations[0], Math.min(elevations[elevations.length - 1], elevationDeg))
  let elevationIndex = 0
  let elevationDistance = Infinity
  elevations.forEach((candidate, index) => {
    const distance = Math.abs(elevation - candidate)
    if (distance < elevationDistance) {
      elevationIndex = index
      elevationDistance = distance
    }
  })
  const direction = renderer.grid.directions[elevationIndex * renderer.grid.azimuths.length + azimuthIndex]
  return { azimuthIndex, elevationIndex, left: direction.left, right: direction.right }
}

function distanceGain(model: DistanceModel, distance: number): number {
  const params = renderer.distance.params
  if (distance <= params.referenceDistance) return 1
  const d = Math.min(distance, params.maximumDistance)
  if (model === 'inverse') return f32(params.referenceDistance / (params.referenceDistance + params.rolloffFactor * (d - params.referenceDistance)))
  if (model === 'linear') return f32(Math.max(0, 1 - params.rolloffFactor * (d - params.referenceDistance) / (params.maximumDistance - params.referenceDistance)))
  return f32(Math.pow(d / params.referenceDistance, -params.rolloffFactor))
}

function airCoefficient(distance: number): number {
  const cutoff = f32(Math.min(f32(4000 / f32(1 + distance)), f32(renderer.grid.sampleRate * 0.5)))
  return f32(1 - Math.exp(f32(f32(-f32(2 * Math.PI) * cutoff) / renderer.grid.sampleRate)))
}

function render(caseDef: RendererFixture['rendererCases'][number]) {
  const hrir = nearest(caseDef.azimuthDeg, caseDef.elevationDeg)
  const history = new Float32Array(renderer.grid.hrirLength)
  const left = new Float32Array(caseDef.input.length)
  const right = new Float32Array(caseDef.input.length)
  const gain = f32(caseDef.gain * distanceGain(caseDef.distanceModel, caseDef.distance))
  const coefficient = airCoefficient(caseDef.distance)
  let position = 0
  let airState = 0
  let frame = 0
  for (const blockSize of caseDef.blockSizes) {
    for (let blockFrame = 0; blockFrame < blockSize; blockFrame++, frame++) {
      airState = f32(airState + f32(coefficient * f32(f32(caseDef.input[frame]) - airState)))
      history[position] = f32(airState * gain)
      let readPosition = position
      let leftSample = 0
      let rightSample = 0
      for (let tap = 0; tap < renderer.grid.hrirLength; tap++) {
        leftSample = f32(leftSample + f32(history[readPosition] * hrir.left[tap]))
        rightSample = f32(rightSample + f32(history[readPosition] * hrir.right[tap]))
        readPosition = readPosition === 0 ? renderer.grid.hrirLength - 1 : readPosition - 1
      }
      left[frame] = leftSample
      right[frame] = rightSample
      position = (position + 1) % renderer.grid.hrirLength
    }
  }
  return { left: Array.from(left), right: Array.from(right) }
}

describe('world-listener 共享空间夹具', () => {
  it('夹具通过严格 JSON Schema 且保留 14 个唯一 case', () => {
    const validate = new Ajv({ allErrors: true, strict: true }).compile(worldSchema)
    expect(validate(worldJson), JSON.stringify(validate.errors)).toBe(true)
    expect(world.schemaVersion).toBe(1)
    expect(world.fixture).toBe('world-listener')
    expect(world.cases).toHaveLength(14)
    expect(new Set(world.cases.map(testCase => testCase.id)).size).toBe(world.cases.length)
  })

  for (const testCase of world.cases) {
    it(testCase.id, () => {
      const got = computeRelativeDirection(testCase.listener, testCase.source)
      expect(Math.abs(got.azimuthDeg - testCase.expected.azimuthDeg)).toBeLessThanOrEqual(world.tolerance.angleAbs)
      expect(Math.abs(got.elevationDeg - testCase.expected.elevationDeg)).toBeLessThanOrEqual(world.tolerance.angleAbs)
      expect(Math.abs(got.distance - testCase.expected.distance)).toBeLessThanOrEqual(world.tolerance.distanceAbs)
      expect(got.azimuthDeg).toBeGreaterThanOrEqual(-180)
      expect(got.azimuthDeg).toBeLessThan(180)
    })
  }
})

describe('renderer-abi 共享空间夹具', () => {
  it('通过严格 schema 并固定 ABI 布局与数值范围', () => {
    const validate = new Ajv({ allErrors: true, strict: true }).compile(rendererSchema)
    expect(validate(rendererJson), JSON.stringify(validate.errors)).toBe(true)
    expect(renderer.abi).toEqual({
      inputLayout: 'object-major-planar-mono',
      inputStrideUnit: 'f32-elements',
      objectSlotsUnit: 'u32-elements',
      objectParams: ['azimuthDeg', 'elevationDeg', 'distance', 'gain'],
      outputLayout: 'planar-stereo',
      lengthUnit: 'f32-elements',
      successCode: 0,
    })
    expect(renderer.scope.excludedNumericParity).toEqual(['spherical', 'room-nonzero'])
    expect(renderer.distance.cases.map(value => value.id)).toEqual([
      'inverse-reference', 'inverse-far', 'linear-far', 'exponential-clamped',
    ])
    expect(renderer.nearestCases.map(value => value.id)).toEqual([
      'exact-center', 'wrap-positive', 'wrap-negative', 'nearest-right-upper', 'tie-keeps-first',
    ])
    expect(renderer.rendererCases.map(value => value.id)).toEqual([
      'delta-right-asymmetric', 'distance-air-step', 'short-final-block',
      'reset-replays-initial-state', 'configured-room-zero-bypass',
    ])
    expect(renderer.distance.cases.length + renderer.nearestCases.length + renderer.rendererCases.length).toBe(14)
    expect(renderer.grid.directions).toHaveLength(renderer.grid.azimuths.length * renderer.grid.elevations.length)
    expect(new Set([...renderer.nearestCases, ...renderer.distance.cases, ...renderer.rendererCases].map(value => value.id)).size)
      .toBe(renderer.nearestCases.length + renderer.distance.cases.length + renderer.rendererCases.length)
    for (const testCase of renderer.rendererCases) {
      expect(testCase.objectSlots).toEqual([0])
      expect(testCase.blockSizes.reduce((sum, size) => sum + size, 0)).toBe(testCase.input.length)
      expect(testCase.inputStride).toBeGreaterThanOrEqual(Math.max(...testCase.blockSizes))
      expect(testCase.expected.left).toHaveLength(testCase.input.length)
      expect(testCase.expected.right).toHaveLength(testCase.input.length)
    }
  })

  it('覆盖 nearest wrap、clamp 与稳定 tie-break', () => {
    for (const testCase of renderer.nearestCases) expect(nearest(testCase.azimuthDeg, testCase.elevationDeg)).toEqual(testCase.expected)
  })

  it('覆盖 distance 与 air absorption 冻结值', () => {
    for (const testCase of renderer.distance.cases) {
      expect(close(distanceGain(testCase.model, testCase.distance), testCase.expectedGain), testCase.id).toBe(true)
      expect(close(airCoefficient(testCase.distance), testCase.expectedAirCoefficient), testCase.id).toBe(true)
    }
  })

  for (const testCase of renderer.rendererCases) {
    it(testCase.id, () => {
      const got = render(testCase)
      for (let frame = 0; frame < testCase.input.length; frame++) {
        expect(close(got.left[frame], testCase.expected.left[frame]), `left[${frame}]`).toBe(true)
        expect(close(got.right[frame], testCase.expected.right[frame]), `right[${frame}]`).toBe(true)
      }
      if (testCase.id === 'delta-right-asymmetric') expect(got.left).not.toEqual(got.right)
      if (testCase.resetReplay) expect(render(testCase)).toEqual(got)
    })
  }
})
