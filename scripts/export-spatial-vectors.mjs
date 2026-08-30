#!/usr/bin/env node
import { createHash } from 'node:crypto'
import { existsSync, mkdtempSync, readFileSync, readdirSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { pathToFileURL, fileURLToPath } from 'node:url'
import Ajv from 'ajv'
import esbuild from 'esbuild'

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const fixturePath = path.join(repoRoot, 'specs', 'spatial', 'vectors', 'world-listener.v1.json')
const schemaPath = path.join(repoRoot, 'specs', 'schema', 'world-listener.schema.json')
const dspVectorDir = path.join(repoRoot, 'specs', 'dsp', 'vectors')

const listener = (position, yaw) => ({ position, yaw })
const expectedFixture = {
  schemaVersion: 1,
  fixture: 'world-listener',
  coordinateSystem: {
    handedness: 'right', rightAxis: '+x', upAxis: '+y', forwardAxis: '+z',
    angleUnit: 'degree', distanceUnit: 'meter', azimuthRange: '[-180,180)',
  },
  tolerance: { angleAbs: 1e-9, distanceAbs: 1e-9 },
  cases: [
    { id: 'front', listener: listener({ x: 0, y: 0, z: 0 }, 0), source: { x: 0, y: 0, z: 5 }, expected: { azimuthDeg: 0, elevationDeg: 0, distance: 5 } },
    { id: 'right', listener: listener({ x: 0, y: 0, z: 0 }, 0), source: { x: 5, y: 0, z: 0 }, expected: { azimuthDeg: 90, elevationDeg: 0, distance: 5 } },
    { id: 'left', listener: listener({ x: 0, y: 0, z: 0 }, 0), source: { x: -5, y: 0, z: 0 }, expected: { azimuthDeg: -90, elevationDeg: 0, distance: 5 } },
    { id: 'behind', listener: listener({ x: 0, y: 0, z: 0 }, 0), source: { x: 0, y: 0, z: -5 }, expected: { azimuthDeg: -180, elevationDeg: 0, distance: 5 } },
    { id: 'above', listener: listener({ x: 0, y: 0, z: 0 }, 0), source: { x: 0, y: 5, z: 0 }, expected: { azimuthDeg: 0, elevationDeg: 90, distance: 5 } },
    { id: 'below', listener: listener({ x: 0, y: 0, z: 0 }, 0), source: { x: 0, y: -5, z: 0 }, expected: { azimuthDeg: 0, elevationDeg: -90, distance: 5 } },
    { id: 'translated', listener: listener({ x: 10, y: -2, z: 3 }, 0), source: { x: 10, y: -2, z: 8 }, expected: { azimuthDeg: 0, elevationDeg: 0, distance: 5 } },
    { id: 'yaw-positive', listener: listener({ x: 0, y: 0, z: 0 }, 30), source: { x: 0, y: 0, z: 5 }, expected: { azimuthDeg: -30, elevationDeg: 0, distance: 5 } },
    { id: 'yaw-negative', listener: listener({ x: 0, y: 0, z: 0 }, -30), source: { x: 0, y: 0, z: 5 }, expected: { azimuthDeg: 30, elevationDeg: 0, distance: 5 } },
    { id: 'yaw-wrap', listener: listener({ x: 0, y: 0, z: 0 }, 30), source: { x: -0.17364817766693033, y: 0, z: -0.984807753012208 }, expected: { azimuthDeg: 160, elevationDeg: 0, distance: 1 } },
    { id: 'yaw-full-turn', listener: listener({ x: 0, y: 0, z: 0 }, 390), source: { x: -0.17364817766693033, y: 0, z: -0.984807753012208 }, expected: { azimuthDeg: 160, elevationDeg: 0, distance: 1 } },
    { id: 'coincident', listener: listener({ x: 1, y: 2, z: 3 }, 725), source: { x: 1, y: 2, z: 3 }, expected: { azimuthDeg: 0, elevationDeg: 0, distance: 0 } },
  ],
}

async function loadFacts() {
  const tempDir = mkdtempSync(path.join(tmpdir(), 'hse-spatial-contracts-'))
  const outfile = path.join(tempDir, 'facts.mjs')
  try {
    await esbuild.build({
      entryPoints: [path.join(repoRoot, 'src', 'spatial', 'controller.ts')],
      bundle: true,
      format: 'esm',
      platform: 'node',
      target: 'node18',
      outfile,
      logLevel: 'silent',
    })
    return await import(pathToFileURL(outfile).href)
  } finally {
    rmSync(tempDir, { recursive: true, force: true })
  }
}

function directoryDigest(directory) {
  const files = readdirSync(directory).sort()
  const hash = createHash('sha256')
  for (const file of files) {
    hash.update(file)
    hash.update('\0')
    hash.update(readFileSync(path.join(directory, file)))
    hash.update('\0')
  }
  return { count: files.length, digest: hash.digest('hex') }
}

async function main() {
  const before = directoryDigest(dspVectorDir)
  if (before.count !== 144) throw new Error(`既有 DSP 冻结文件应为 144 个，实际为 ${before.count}`)
  if (!existsSync(fixturePath)) throw new Error(`缺少冻结夹具：${fixturePath}`)

  const schema = JSON.parse(readFileSync(schemaPath, 'utf8'))
  const validate = new Ajv({ allErrors: true, strict: true }).compile(schema)
  if (!validate(expectedFixture)) throw new Error(`内置 canonical fixture 不符合 schema：${JSON.stringify(validate.errors)}`)

  const actualBytes = readFileSync(fixturePath)
  const actual = JSON.parse(actualBytes.toString('utf8'))
  const actualCanonical = Buffer.from(JSON.stringify(actual, null, 2) + '\n', 'utf8')
  const expectedBytes = Buffer.from(JSON.stringify(expectedFixture, null, 2) + '\n', 'utf8')
  if (!actualCanonical.equals(expectedBytes)) {
    throw new Error(`冻结基线冲突：${fixturePath} 与独立 canonical case 源不一致，禁止静默改写`)
  }
  if (!validate(actual)) throw new Error(`冻结夹具不符合 schema：${JSON.stringify(validate.errors)}`)

  const { computeRelativeDirection } = await loadFacts()
  for (const testCase of actual.cases) {
    const got = computeRelativeDirection(testCase.listener, testCase.source)
    if (Math.abs(got.azimuthDeg - testCase.expected.azimuthDeg) > actual.tolerance.angleAbs
      || Math.abs(got.elevationDeg - testCase.expected.elevationDeg) > actual.tolerance.angleAbs
      || Math.abs(got.distance - testCase.expected.distance) > actual.tolerance.distanceAbs) {
      throw new Error(`冻结基线冲突：${testCase.id} 与 TS 事实实现不一致`)
    }
  }

  const after = directoryDigest(dspVectorDir)
  if (before.count !== after.count || before.digest !== after.digest) {
    throw new Error('空间夹具校验期间既有 DSP 冻结资产发生变化')
  }
  console.log(`完成：world-listener ${actual.cases.length}/${actual.cases.length} PASS；既有 DSP 144 文件逐字节未变。`)
}

main().catch(error => {
  console.error(error instanceof Error ? error.message : String(error))
  process.exitCode = 1
})
