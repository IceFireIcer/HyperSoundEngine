import { existsSync, readFileSync } from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import Ajv from 'ajv'
import { describe, expect, it } from 'vitest'
import { HyperSoundEngine } from '../src/engine/HyperSoundEngine'
import { createDefaultParams, type HyperSoundEngineParams } from '../src/types'

interface ScanSummary {
  finiteRatio: number
  nonZeroRatio: number
  peakOrder: number
  rmsOrder: number
}

interface ScanCase {
  id: string
  kind: 'seed' | 'minimum' | 'maximum'
  sampleRate: number
  blockSize: number
  frames: number
  inputSeed: number
  params: { overrides: Record<string, unknown> }
  expected: { left: ScanSummary; right: ScanSummary }
}

interface ScanFixture {
  schemaVersion: number
  generator: { algorithm: string; caseCount: number }
  tolerance: { kind: 'relative'; value: number; floor: number }
  outputFile: string
  cases: ScanCase[]
}

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const jsonPath = path.join(repoRoot, 'specs', 'engine', 'vectors', 'phase4-param-scan.json')
const schemaPath = path.join(repoRoot, 'specs', 'schema', 'phase4-param-scan.schema.json')

function loadFixture(): ScanFixture {
  if (!existsSync(jsonPath)) throw new Error('缺少 Phase 4 参数扫描夹具：' + jsonPath)
  if (!existsSync(schemaPath)) throw new Error('缺少 Phase 4 参数扫描 Schema：' + schemaPath)
  const fixture = JSON.parse(readFileSync(jsonPath, 'utf8')) as ScanFixture
  const schema = JSON.parse(readFileSync(schemaPath, 'utf8')) as object
  const validate = new Ajv({ allErrors: true, strict: true }).compile(schema)
  if (!validate(fixture)) throw new Error('Phase 4 参数扫描夹具不符合 Schema：' + JSON.stringify(validate.errors))
  return fixture
}

function merge(base: HyperSoundEngineParams, patch: Record<string, unknown>): HyperSoundEngineParams {
  const apply = (target: Record<string, unknown>, source: Record<string, unknown>): void => {
    for (const [key, value] of Object.entries(source)) {
      const current = target[key]
      if (value && typeof value === 'object' && !Array.isArray(value) && !ArrayBuffer.isView(value) &&
          current && typeof current === 'object' && !Array.isArray(current) && !ArrayBuffer.isView(current)) {
        apply(current as Record<string, unknown>, value as Record<string, unknown>)
      } else {
        target[key] = value
      }
    }
  }
  apply(base as unknown as Record<string, unknown>, patch)
  return base
}

function input(frames: number, seed: number): [Float32Array, Float32Array] {
  const left = new Float32Array(frames)
  const right = new Float32Array(frames)
  let state = seed >>> 0
  const sample = (): number => {
    state = (Math.imul(state, 1664525) + 1013904223) >>> 0
    return -0.95 + 1.9 * (state / 4294967296)
  }
  for (let i = 0; i < frames; i++) {
    left[i] = sample()
    right[i] = sample()
  }
  return [left, right]
}

function magnitudeOrder(value: number): number {
  return value > 0 && Number.isFinite(value) ? Math.floor(Math.log10(value)) : 0
}

function summarize(samples: Float32Array): [number, number, number, number] {
  let finiteCount = 0
  let nonZeroCount = 0
  let peakAbs = 0
  let sumSquares = 0
  for (const sample of samples) {
    if (Number.isFinite(sample)) finiteCount++
    if (sample !== 0) nonZeroCount++
    peakAbs = Math.max(peakAbs, Math.abs(sample))
    sumSquares += sample * sample
  }
  const rms = Math.sqrt(sumSquares / samples.length)
  return [finiteCount / samples.length, nonZeroCount / samples.length, magnitudeOrder(peakAbs), magnitudeOrder(rms)]
}

function assertSummaryWithinTolerance(id: string, channel: string, got: Float32Array, want: ScanSummary, value: number, floor: number): void {
  const metricNames = ['finiteRatio', 'nonZeroRatio', 'peakOrder', 'rmsOrder']
  const gotSummary = summarize(got)
  const wantSummary = [want.finiteRatio, want.nonZeroRatio, want.peakOrder, want.rmsOrder]
  for (let metric = 0; metric < metricNames.length; metric++) {
    const error = Math.abs(gotSummary[metric] - wantSummary[metric])
    const bound = value * Math.max(Math.abs(wantSummary[metric]), floor)
    if (!(error <= bound)) {
      throw new Error(`${id} ${channel} ${metricNames[metric]} 超差：got=${gotSummary[metric]} want=${wantSummary[metric]} error=${error} bound=${bound}`)
    }
  }
}

const fixture = loadFixture()

describe('Phase 4 固定种子全链跨语言参数扫描', () => {
  it('夹具固定 40 case、指定矩阵、边界快照与 17 帧短尾', () => {
    expect(fixture.generator.caseCount).toBe(40)
    expect(fixture.cases).toHaveLength(40)
    const groups = new Map<string, ScanCase[]>()
    for (const testCase of fixture.cases) {
      expect(testCase.frames % testCase.blockSize).toBe(17)
      const key = `${testCase.sampleRate}/${testCase.blockSize}`
      groups.set(key, [...(groups.get(key) ?? []), testCase])
      expect(testCase.params.overrides.spatial).toEqual({ mode: 'off' })
    }
    expect(Array.from(groups.keys())).toEqual(['44100/63', '48000/128', '48000/257', '96000/512'])
    for (const cases of groups.values()) {
      expect(cases).toHaveLength(10)
      expect(cases.filter(({ kind }) => kind === 'seed')).toHaveLength(8)
      expect(cases.filter(({ kind }) => kind === 'minimum')).toHaveLength(1)
      expect(cases.filter(({ kind }) => kind === 'maximum')).toHaveLength(1)
    }
  })

  for (const testCase of fixture.cases) {
    it(testCase.id + ' 重放落在 1e-6 相对容差内', () => {
      const [left, right] = input(testCase.frames, testCase.inputSeed)
      const engine = new HyperSoundEngine(testCase.sampleRate, 2)
      engine.setParams(merge(createDefaultParams(testCase.sampleRate), testCase.params.overrides))
      engine.prepare(testCase.blockSize)
      const gotL = new Float32Array(testCase.frames)
      const gotR = new Float32Array(testCase.frames)
      for (let offset = 0; offset < testCase.frames; offset += testCase.blockSize) {
        const end = Math.min(offset + testCase.blockSize, testCase.frames)
        const blockL = new Float32Array(end - offset)
        const blockR = new Float32Array(end - offset)
        engine.process([left.subarray(offset, end), right.subarray(offset, end)], [blockL, blockR])
        gotL.set(blockL, offset)
        gotR.set(blockR, offset)
      }
      assertSummaryWithinTolerance(testCase.id, 'left', gotL, testCase.expected.left, fixture.tolerance.value, fixture.tolerance.floor)
      assertSummaryWithinTolerance(testCase.id, 'right', gotR, testCase.expected.right, fixture.tolerance.value, fixture.tolerance.floor)
    })
  }
})
