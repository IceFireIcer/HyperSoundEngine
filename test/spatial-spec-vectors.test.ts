import { readFileSync } from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import Ajv from 'ajv'
import { describe, expect, it } from 'vitest'
import { computeRelativeDirection } from '../src/spatial/controller'
import type { Vec3, WorldListenerPose } from '../src/spatial/types'

type Fixture = {
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

const fixturePath = path.join(
  path.dirname(fileURLToPath(import.meta.url)),
  '..',
  'specs',
  'spatial',
  'vectors',
  'world-listener.v1.json',
)
const schemaPath = path.join(path.dirname(fileURLToPath(import.meta.url)), '..', 'specs', 'schema', 'world-listener.schema.json')
const fixtureJson = JSON.parse(readFileSync(fixturePath, 'utf8'))
const schema = JSON.parse(readFileSync(schemaPath, 'utf8'))
const fixture = fixtureJson as Fixture

describe('world-listener 共享空间夹具', () => {
  it('夹具通过严格 JSON Schema 且 case id 唯一', () => {
    const validate = new Ajv({ allErrors: true, strict: true }).compile(schema)
    expect(validate(fixtureJson), JSON.stringify(validate.errors)).toBe(true)
    expect(fixture.schemaVersion).toBe(1)
    expect(fixture.fixture).toBe('world-listener')
    expect(fixture.cases).toHaveLength(12)
    expect(new Set(fixture.cases.map(testCase => testCase.id)).size).toBe(fixture.cases.length)
  })

  for (const testCase of fixture.cases) {
    it(testCase.id, () => {
      const got = computeRelativeDirection(testCase.listener, testCase.source)
      expect(Math.abs(got.azimuthDeg - testCase.expected.azimuthDeg)).toBeLessThanOrEqual(fixture.tolerance.angleAbs)
      expect(Math.abs(got.elevationDeg - testCase.expected.elevationDeg)).toBeLessThanOrEqual(fixture.tolerance.angleAbs)
      expect(Math.abs(got.distance - testCase.expected.distance)).toBeLessThanOrEqual(fixture.tolerance.distanceAbs)
      expect(got.azimuthDeg).toBeGreaterThanOrEqual(-180)
      expect(got.azimuthDeg).toBeLessThan(180)
    })
  }
})
