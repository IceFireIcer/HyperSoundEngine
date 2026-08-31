#!/usr/bin/env node
import { existsSync, mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import esbuild from 'esbuild'

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const jsonPath = path.join(repoRoot, 'specs', 'engine', 'vectors', 'phase4-param-scan.json')
const WRITE = process.argv.slice(2).includes('--write')
const MATRIX = [[44100, 63], [48000, 128], [48000, 257], [96000, 512]]
const SEEDS = [0x00000001, 0x12345678, 0x243f6a88, 0x5eedf00d, 0x7fffffff, 0x80000000, 0xdeadbeef, 0xfffffffe]
const TOLERANCE = { kind: 'relative', value: 1e-6, floor: 1e-9 }

class Lcg {
  constructor(seed) { this.state = seed >>> 0 }
  nextU32() { this.state = (Math.imul(this.state, 1664525) + 1013904223) >>> 0; return this.state }
  unit() { return this.nextU32() / 4294967296 }
  range(lo, hi) { return lo + (hi - lo) * this.unit() }
  bool() { return (this.nextU32() & 1) === 0 }
  pick(values) { return values[this.nextU32() % values.length] }
}

function randomOverrides(seed, fs) {
  const rng = new Lcg(seed)
  const reverbMode = rng.pick(['off', 'algorithmic', 'fdn', 'convolution'])
  const frequencies = [31.5, 63, 125, 250, 500, 1000, 2000, 4000, 8000, 16000]
  const proBands = frequencies.map((frequency) => ({ frequency: Math.min(frequency, fs * 0.45), gain: rng.range(-12, 12), q: rng.range(0.2, 8) }))
  const dynamicBands = Array.from({ length: 5 }, () => ({ enabled: rng.bool(), targetGainDb: rng.range(-12, 12) }))
  return {
    eq: { enabled: true, mode: rng.bool() ? 'pro' : 'simple', simpleBands: Array.from({ length: 5 }, () => rng.range(-12, 12)), proBands, bandCount: 1 + (rng.nextU32() % 10), qCompensation: rng.bool() },
    deesser: { enabled: true, centerHz: rng.range(100, fs * 0.45), q: rng.range(0.1, 20), thresholdDb: rng.range(-80, 0), ratio: rng.range(1, 100), attackMs: rng.range(0.05, 100), releaseMs: rng.range(1, 1000), splitBand: rng.bool(), mix: rng.unit(), sidechainEnabled: rng.bool() },
    compressor: { enabled: true, thresholdDb: rng.range(-80, 0), ratio: rng.range(1, 100), kneeDb: rng.range(0, 40), attackMs: rng.range(0.05, 100), releaseMs: rng.range(1, 1000), makeupDb: rng.range(-24, 24), outputGain: rng.range(0, 2), sidechainEnabled: rng.bool() },
    nightMode: { enabled: true, amount: rng.range(0, 10) },
    bassEnhancer: { enabled: true, cutoffHz: rng.range(20, fs * 0.4), q: rng.range(0.1, 20), harmonicType: rng.pick(['odd', 'even', 'atan', 'soft']), harmonicGain: rng.unit(), mix: rng.unit(), levelDb: rng.range(-6, 6), lowBoostDb: rng.range(-6, 12) },
    reverb: { enabled: reverbMode !== 'off', mode: reverbMode, algorithmic: { type: rng.pick(['hall', 'room', 'plate', 'spring', 'stage']), roomSize: rng.unit(), damping: rng.unit(), wet: rng.unit(), dry: rng.unit(), preDelayMs: rng.range(0, 250), width: rng.range(0, 2) }, convolution: { ir: reverbMode === 'convolution' ? [1, 0.25, -0.125, 0.0625] : null, mix: rng.unit(), preDelayMs: rng.range(0, 100), dePeriodize: rng.bool() } },
    surround3d: { enabled: true, distance: rng.unit(), speed: rng.range(0, 4), angle: rng.range(-180, 180), direction: rng.bool() ? 1 : -1 },
    loudnessCompensation: { enabled: true, mode: rng.pick(['auto', 'preset', 'custom']), preset: rng.pick(['flat', 'bass', 'vocal', 'warm', 'bright', 'night']), bands: [{ frequency: 80, gain: rng.range(-24, 24) }, { frequency: 4000, gain: rng.range(-24, 24) }], volumePercent: rng.range(0, 100), maxBoostDb: rng.range(0, 24), smoothingSeconds: rng.range(0.01, 2) },
    loudnessNormalization: { enabled: true, targetLufs: rng.range(-40, 0), maxGainDb: rng.range(0, 24), minGainDb: rng.range(-24, 0), useRealtimeMeter: rng.bool(), externalGainDb: rng.range(-24, 24) },
    limiter: { enabled: true, thresholdDb: rng.range(-60, 0), lookaheadMs: rng.range(0, 50), attackMs: rng.range(0.05, 100), releaseMs: rng.range(1, 1000), truePeak: rng.bool() },
    ieq: { enabled: rng.bool(), strength: rng.unit(), targetCurve: rng.pick(['flat', 'warm', 'bright', 'vocal']), timeConstantSec: rng.range(0.1, 10) },
    dynamicEq: { enabled: true, strength: rng.unit(), thresholdDb: rng.range(-80, 0), ratio: rng.range(1, 100), attackMs: rng.range(0.05, 100), releaseMs: rng.range(1, 1000), bands: dynamicBands },
    pitch: { enabled: true, voiceBalance: rng.range(-1, 1) },
    modulation: { enabled: true, lfo: { shape: rng.pick(['sine', 'triangle', 'square', 'saw']), rateHz: rng.range(0.01, 30), depth: rng.unit() }, envelope: { attackMs: rng.range(0.05, 100), releaseMs: rng.range(1, 1000), amount: rng.unit() }, routes: [{ source: 'lfo', target: 'masterGain', amount: rng.range(-2, 2), offset: rng.range(-1, 1) }, { source: 'envelope', target: 'stereoWidth', amount: rng.range(-2, 2), offset: rng.range(-1, 1) }] },
    modEffects: {
      delay: { enabled: true, delayMs: rng.range(0, 2000), feedback: rng.range(0, 0.98), mix: rng.unit() },
      chorus: { enabled: true, rateHz: rng.range(0.01, 20), depthMs: rng.range(0, 50), mix: rng.unit() },
      flanger: { enabled: true, rateHz: rng.range(0.01, 20), depthMs: rng.range(0, 50), feedback: rng.range(0, 0.98), mix: rng.unit() },
      phaser: { enabled: true, rateHz: rng.range(0.01, 20), depth: rng.unit(), feedback: rng.range(0, 0.98), mix: rng.unit(), stages: 1 + (rng.nextU32() % 8) },
      tremolo: { enabled: true, rateHz: rng.range(0.01, 30), depth: rng.unit(), mix: rng.unit() },
    },
    stereoWidth: rng.range(0, 2), spatial: { mode: 'off' },
  }
}

function boundaryOverrides(maximum, fs) {
  const x = (lo, hi) => maximum ? hi : lo
  const value = randomOverrides(maximum ? 0xffffffff : 0, fs)
  value.deesser.centerHz = x(100, fs * 0.45); value.deesser.q = x(0.1, 20)
  value.compressor.thresholdDb = x(-80, 0); value.compressor.ratio = x(1, 100); value.compressor.kneeDb = x(0, 40)
  value.bassEnhancer.lowBoostDb = x(-6, 12)
  value.reverb.enabled = true; value.reverb.mode = maximum ? 'fdn' : 'algorithmic'
  value.reverb.algorithmic.roomSize = x(0, 1); value.reverb.algorithmic.damping = x(0, 1)
  value.limiter.thresholdDb = x(-60, 0); value.limiter.lookaheadMs = x(0, 50)
  value.dynamicEq.strength = x(0, 1); value.ieq.enabled = !maximum; value.stereoWidth = x(0, 2)
  return value
}

function input(frames, seed) {
  const rng = new Lcg(seed); const left = new Float32Array(frames); const right = new Float32Array(frames)
  for (let i = 0; i < frames; i++) { left[i] = rng.range(-0.95, 0.95); right[i] = rng.range(-0.95, 0.95) }
  return [left, right]
}

function merge(base, patch) {
  for (const [key, value] of Object.entries(patch)) {
    const current = base[key]
    if (value && typeof value === 'object' && !Array.isArray(value) && !ArrayBuffer.isView(value) && current && typeof current === 'object' && !Array.isArray(current) && !ArrayBuffer.isView(current)) merge(current, value)
    else base[key] = value
  }
  return base
}

async function loadEngine() {
  const dir = mkdtempSync(path.join(tmpdir(), 'hse-phase4-scan-')); const outfile = path.join(dir, 'engine.mjs')
  await esbuild.build({ stdin: { contents: "export { HyperSoundEngine } from './src/engine/HyperSoundEngine.ts'; export { createDefaultParams } from './src/types.ts'", resolveDir: repoRoot, loader: 'ts' }, bundle: true, format: 'esm', platform: 'node', target: 'node20', outfile, logLevel: 'silent' })
  return { facts: await import(pathToFileURL(outfile).href), cleanup: () => rmSync(dir, { recursive: true, force: true }) }
}

function definitions() {
  const cases = []
  MATRIX.forEach(([sampleRate, blockSize], matrixIndex) => {
    SEEDS.forEach((seed, seedIndex) => cases.push({ id: `fs${sampleRate}-b${blockSize}-seed${seedIndex + 1}`, kind: 'seed', sampleRate, blockSize, frames: blockSize * 5 + 17, inputSeed: (seed ^ matrixIndex ^ 0xa5a55a5a) >>> 0, params: { overrides: randomOverrides(seed, sampleRate) } }))
    cases.push({ id: `fs${sampleRate}-b${blockSize}-minimum`, kind: 'minimum', sampleRate, blockSize, frames: blockSize * 5 + 17, inputSeed: 0xb4b44b4b, params: { overrides: boundaryOverrides(false, sampleRate) } })
    cases.push({ id: `fs${sampleRate}-b${blockSize}-maximum`, kind: 'maximum', sampleRate, blockSize, frames: blockSize * 5 + 17, inputSeed: 0x4b4bb4b4, params: { overrides: boundaryOverrides(true, sampleRate) } })
  })
  return cases
}

function magnitudeOrder(value) { return value > 0 && Number.isFinite(value) ? Math.floor(Math.log10(value)) : 0 }

function summarize(samples) {
  let finiteCount = 0; let nonZeroCount = 0; let peakAbs = 0; let sumSquares = 0
  for (const sample of samples) {
    if (Number.isFinite(sample)) finiteCount++
    if (sample !== 0) nonZeroCount++
    peakAbs = Math.max(peakAbs, Math.abs(sample)); sumSquares += sample * sample
  }
  const rms = Math.sqrt(sumSquares / samples.length)
  return { finiteRatio: finiteCount / samples.length, nonZeroRatio: nonZeroCount / samples.length, peakOrder: magnitudeOrder(peakAbs), rmsOrder: magnitudeOrder(rms) }
}

function verifyOrWrite(target, content) {
  if (!existsSync(target)) {
    if (!WRITE) throw new Error(`缺少冻结夹具：${target}；仅可显式运行 node scripts/phase4-param-scan.mjs --write 创建`)
    mkdirSync(path.dirname(target), { recursive: true }); writeFileSync(target, content); return 'written'
  }
  const old = readFileSync(target)
  if (!old.equals(content)) throw new Error(`冻结夹具不一致：${target}；禁止覆盖既有 Phase 4 基线`)
  return 'verified'
}

async function main() {
  const cases = definitions(); const { facts, cleanup } = await loadEngine()
  try {
    for (const testCase of cases) {
      const [left, right] = input(testCase.frames, testCase.inputSeed)
      const engine = new facts.HyperSoundEngine(testCase.sampleRate, 2)
      engine.setParams(merge(facts.createDefaultParams(testCase.sampleRate), testCase.params.overrides)); engine.prepare(testCase.blockSize)
      const outL = new Float32Array(testCase.frames); const outR = new Float32Array(testCase.frames)
      for (let offset = 0; offset < testCase.frames; offset += testCase.blockSize) {
        const end = Math.min(offset + testCase.blockSize, testCase.frames); const l = new Float32Array(end - offset); const r = new Float32Array(end - offset)
        engine.process([left.subarray(offset, end), right.subarray(offset, end)], [l, r]); outL.set(l, offset); outR.set(r, offset)
      }
      testCase.expected = { left: summarize(outL), right: summarize(outR) }
    }
  } finally { cleanup() }
  const document = { schemaVersion: 1, generator: { algorithm: 'phase4-lcg-1664525-1013904223', caseCount: 40 }, tolerance: TOLERANCE, cases }
  const json = Buffer.from(JSON.stringify(document, null, 2) + '\n')
  console.log(`[${verifyOrWrite(jsonPath, json)}] ${path.relative(repoRoot, jsonPath)}`)
  console.log(`Phase 4 参数扫描：${cases.length}/40 case。`)
}

main().catch((error) => { console.error(error instanceof Error ? error.message : String(error)); process.exitCode = 1 })
