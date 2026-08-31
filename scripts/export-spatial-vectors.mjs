#!/usr/bin/env node
import { createHash } from 'node:crypto'
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { pathToFileURL, fileURLToPath } from 'node:url'
import Ajv from 'ajv'
import esbuild from 'esbuild'

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const spatialVectorDir = path.join(repoRoot, 'specs', 'spatial', 'vectors')
const worldFixturePath = path.join(spatialVectorDir, 'world-listener.v1.json')
const worldSchemaPath = path.join(repoRoot, 'specs', 'schema', 'world-listener.schema.json')
const rendererFixturePath = path.join(spatialVectorDir, 'renderer-abi.v1.json')
const rendererSchemaPath = path.join(repoRoot, 'specs', 'schema', 'spatial-renderer-abi.schema.json')
const dspVectorDir = path.join(repoRoot, 'specs', 'dsp', 'vectors')
const f32 = Math.fround

const listener = (position, yaw, pitch, roll) => ({
  position,
  yaw,
  ...(pitch === undefined ? {} : { pitch }),
  ...(roll === undefined ? {} : { roll }),
})
const expectedWorldFixture = {
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
    { id: 'pitch-up', listener: listener({ x: 0, y: 0, z: 0 }, 0, 30, 0), source: { x: 0, y: 0, z: 2 }, expected: { azimuthDeg: 0, elevationDeg: -30, distance: 2 } },
    { id: 'roll-right', listener: listener({ x: 0, y: 0, z: 0 }, 0, 0, 30), source: { x: 2, y: 0, z: 0 }, expected: { azimuthDeg: 90, elevationDeg: 30, distance: 2 } },
  ],
}

const azimuths = [-90, 0, 90]
const elevations = [-30, 0, 30]
const hrirLength = 4
const directions = []
for (let elevationIndex = 0; elevationIndex < elevations.length; elevationIndex++) {
  for (let azimuthIndex = 0; azimuthIndex < azimuths.length; azimuthIndex++) {
    directions.push({
      azimuthDeg: azimuths[azimuthIndex],
      elevationDeg: elevations[elevationIndex],
      left: [f32(0.45 + azimuthIndex * 0.08 + elevationIndex * 0.03), f32(0.12 - elevationIndex * 0.015), f32(-0.04 + azimuthIndex * 0.01), f32(0.01)],
      right: [f32(0.7 - azimuthIndex * 0.09 + elevationIndex * 0.02), f32(-0.06 + elevationIndex * 0.01), f32(0.03 - azimuthIndex * 0.005), f32(-0.015)],
    })
  }
}

function wrapAzimuth(value) {
  return ((value + 180) % 360 + 360) % 360 - 180
}

function nearestIndex(values, value, circular = false) {
  let best = 0
  let bestDistance = Infinity
  for (let index = 0; index < values.length; index++) {
    const difference = Math.abs(value - values[index])
    const distance = circular ? Math.min(difference, 360 - difference) : difference
    if (distance < bestDistance) {
      best = index
      bestDistance = distance
    }
  }
  return best
}

function nearest(azimuthDeg, elevationDeg) {
  const azimuthIndex = nearestIndex(azimuths, wrapAzimuth(azimuthDeg), true)
  const clampedElevation = Math.max(elevations[0], Math.min(elevations.at(-1), elevationDeg))
  const elevationIndex = nearestIndex(elevations, clampedElevation)
  const direction = directions[elevationIndex * azimuths.length + azimuthIndex]
  return { azimuthIndex, elevationIndex, left: direction.left, right: direction.right }
}

const distanceParams = { referenceDistance: 1, maximumDistance: 12, rolloffFactor: 1 }
function distanceGain(model, distance) {
  if (distance <= distanceParams.referenceDistance) return 1
  const d = Math.min(distance, distanceParams.maximumDistance)
  if (model === 'inverse') {
    return f32(distanceParams.referenceDistance / (distanceParams.referenceDistance + distanceParams.rolloffFactor * (d - distanceParams.referenceDistance)))
  }
  if (model === 'linear') {
    return f32(Math.max(0, 1 - distanceParams.rolloffFactor * (d - distanceParams.referenceDistance) / (distanceParams.maximumDistance - distanceParams.referenceDistance)))
  }
  return f32(Math.pow(d / distanceParams.referenceDistance, -distanceParams.rolloffFactor))
}

function airCoefficient(sampleRate, distance) {
  const cutoff = f32(Math.min(f32(4000 / f32(1 + distance)), f32(sampleRate * 0.5)))
  return f32(1 - Math.exp(f32(f32(-f32(2 * Math.PI) * cutoff) / sampleRate)))
}

function renderSingleSource(caseDef) {
  const selected = nearest(caseDef.azimuthDeg, caseDef.elevationDeg)
  const gain = f32(caseDef.gain * distanceGain(caseDef.distanceModel, caseDef.distance))
  const coefficient = airCoefficient(48000, caseDef.distance)
  const history = new Float32Array(hrirLength)
  const left = new Float32Array(caseDef.input.length)
  const right = new Float32Array(caseDef.input.length)
  let writePosition = 0
  let airState = 0
  for (let frame = 0; frame < caseDef.input.length; frame++) {
    airState = f32(airState + f32(coefficient * f32(f32(caseDef.input[frame]) - airState)))
    history[writePosition] = f32(airState * gain)
    let leftSample = 0
    let rightSample = 0
    let readPosition = writePosition
    for (let tap = 0; tap < hrirLength; tap++) {
      leftSample = f32(leftSample + f32(history[readPosition] * selected.left[tap]))
      rightSample = f32(rightSample + f32(history[readPosition] * selected.right[tap]))
      readPosition = readPosition === 0 ? hrirLength - 1 : readPosition - 1
    }
    left[frame] = leftSample
    right[frame] = rightSample
    writePosition = (writePosition + 1) % hrirLength
  }
  return { left: Array.from(left), right: Array.from(right) }
}

function buildRendererFixture() {
  const nearestCases = [
    ['exact-center', 0, 0], ['wrap-positive', 271, 80], ['wrap-negative', -271, -80],
    ['nearest-right-upper', 68, 22], ['tie-keeps-first', 45, 15],
  ].map(([id, azimuthDeg, elevationDeg]) => ({ id, azimuthDeg, elevationDeg, expected: nearest(azimuthDeg, elevationDeg) }))
  const distanceCases = [
    ['inverse-reference', 'inverse', 1], ['inverse-far', 'inverse', 6],
    ['linear-far', 'linear', 6], ['exponential-clamped', 'exponential', 20],
  ].map(([id, model, distance]) => ({ id, model, distance, expectedGain: distanceGain(model, distance), expectedAirCoefficient: airCoefficient(48000, distance) }))
  const rendererCases = [
    { id: 'delta-right-asymmetric', input: [1, 0, 0, 0, 0, 0, 0, 0], inputStride: 10, blockSizes: [8], azimuthDeg: 90, elevationDeg: 0, distance: 1, gain: 1, distanceModel: 'inverse', roomMode: 'off', resetReplay: false },
    { id: 'distance-air-step', input: [1, 1, 1, 1, 1, 1, 1, 1], inputStride: 8, blockSizes: [8], azimuthDeg: 0, elevationDeg: 0, distance: 6, gain: 0.75, distanceModel: 'inverse', roomMode: 'off', resetReplay: false },
    { id: 'short-final-block', input: [0.5, -0.25, 0.75, 0, -0.5, 0.125, 0.25], inputStride: 9, blockSizes: [3, 3, 1], azimuthDeg: -90, elevationDeg: 30, distance: 2, gain: 0.8, distanceModel: 'linear', roomMode: 'off', resetReplay: false },
    { id: 'reset-replays-initial-state', input: [0.25, 0.5, -0.5, 1, 0, -0.25, 0.125], inputStride: 7, blockSizes: [2, 3, 2], azimuthDeg: 0, elevationDeg: -30, distance: 3, gain: 1, distanceModel: 'exponential', roomMode: 'off', resetReplay: true },
    { id: 'configured-room-zero-bypass', input: [1, 0.25, -0.5, 0, 0.75, -0.25], inputStride: 6, blockSizes: [4, 2], azimuthDeg: 0, elevationDeg: 0, distance: 1, gain: 0.6, distanceModel: 'inverse', roomMode: 'configured-zero', resetReplay: false },
  ].map(caseDef => ({ ...caseDef, objectSlots: [0], expected: renderSingleSource(caseDef) }))
  return {
    schemaVersion: 1,
    fixture: 'renderer-abi',
    scope: { interpolation: 'nearest', convolution: 'time-domain', sourceCount: 1, room: 'off-only', excludedNumericParity: ['spherical', 'room-nonzero'] },
    abi: { inputLayout: 'object-major-planar-mono', inputStrideUnit: 'f32-elements', objectSlotsUnit: 'u32-elements', objectParams: ['azimuthDeg', 'elevationDeg', 'distance', 'gain'], outputLayout: 'planar-stereo', lengthUnit: 'f32-elements', successCode: 0 },
    tolerance: { kind: 'relative', value: 0.00001, floor: 1e-7 },
    grid: { sampleRate: 48000, azimuths, elevations, hrirLength, directions },
    distance: { params: distanceParams, cases: distanceCases },
    nearestCases,
    rendererCases,
  }
}

async function loadFacts() {
  const tempDir = mkdtempSync(path.join(tmpdir(), 'hse-spatial-contracts-'))
  const outfile = path.join(tempDir, 'facts.mjs')
  try {
    await esbuild.build({ entryPoints: [path.join(repoRoot, 'src', 'spatial', 'controller.ts')], bundle: true, format: 'esm', platform: 'node', target: 'node18', outfile, logLevel: 'silent' })
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

function validateFixture(schemaPath, fixture, label) {
  const schema = JSON.parse(readFileSync(schemaPath, 'utf8'))
  const validate = new Ajv({ allErrors: true, strict: true }).compile(schema)
  if (!validate(fixture)) throw new Error(`${label} 不符合 schema：${JSON.stringify(validate.errors)}`)
}

const writeMissing = process.argv.includes('--write')

function freezeJson(filePath, fixture) {
  const bytes = Buffer.from(JSON.stringify(fixture, null, 2) + '\n', 'utf8')
  if (existsSync(filePath)) {
    if (!readFileSync(filePath).equals(bytes)) throw new Error(`冻结基线冲突：${filePath} 与 canonical 内容不一致，拒绝覆盖`)
    return 'unchanged'
  }
  if (!writeMissing) {
    throw new Error(`缺少冻结夹具：${filePath}；验证模式禁止自动生成，请显式使用 --write 创建新增基线`)
  }
  mkdirSync(path.dirname(filePath), { recursive: true })
  writeFileSync(filePath, bytes)
  return 'written'
}

async function main() {
  const before = directoryDigest(dspVectorDir)
  if (before.count !== 144) throw new Error(`既有 DSP 冻结文件应为 144 个，实际为 ${before.count}`)
  if (!existsSync(worldFixturePath)) throw new Error(`缺少冻结夹具：${worldFixturePath}`)

  const actualWorld = JSON.parse(readFileSync(worldFixturePath, 'utf8'))
  validateFixture(worldSchemaPath, expectedWorldFixture, 'world-listener canonical fixture')
  validateFixture(worldSchemaPath, actualWorld, 'world-listener frozen fixture')
  const actualCanonical = Buffer.from(JSON.stringify(actualWorld, null, 2) + '\n')
  const expectedCanonical = Buffer.from(JSON.stringify(expectedWorldFixture, null, 2) + '\n')
  if (!actualCanonical.equals(expectedCanonical)) {
    throw new Error(`冻结基线冲突：${worldFixturePath} 与独立 canonical case 源不一致，禁止静默改写`)
  }
  const { computeRelativeDirection } = await loadFacts()
  for (const testCase of expectedWorldFixture.cases) {
    const got = computeRelativeDirection(testCase.listener, testCase.source)
    if (Math.abs(got.azimuthDeg - testCase.expected.azimuthDeg) > expectedWorldFixture.tolerance.angleAbs
      || Math.abs(got.elevationDeg - testCase.expected.elevationDeg) > expectedWorldFixture.tolerance.angleAbs
      || Math.abs(got.distance - testCase.expected.distance) > expectedWorldFixture.tolerance.distanceAbs) {
      throw new Error(`冻结基线冲突：${testCase.id} 与 TS 事实实现不一致`)
    }
  }

  const rendererFixture = buildRendererFixture()
  validateFixture(rendererSchemaPath, rendererFixture, 'renderer-abi canonical fixture')
  const rendererStatus = freezeJson(rendererFixturePath, rendererFixture)

  const after = directoryDigest(dspVectorDir)
  if (before.count !== after.count || before.digest !== after.digest) throw new Error('空间夹具导出期间既有 DSP 冻结资产发生变化')
  console.log(`完成：world-listener 14/14 PASS；renderer-abi ${rendererStatus}；既有 DSP 144 文件逐字节未变。`)
}

main().catch(error => {
  console.error(error instanceof Error ? error.message : String(error))
  process.exitCode = 1
})
