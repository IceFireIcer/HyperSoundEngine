/**
 * 优化对比基准：非均匀分区卷积 + FFT基4 + ReverbSimple内联 + Limiter插值
 *
 * 运行前先构建：npm run build
 * 运行：node scripts/benchmark-optimized.mjs
 *
 * 场景：
 *  - 默认全链（无混响）
 *  - 卷积混响场景（2s IR @48k，mode='convolution'）
 *  - FDN 混响场景（mode='fdn'）
 *  - DynamicEq 场景
 */
import { createEngine, createDefaultParams } from '../dist/index.js'

const fs = 48000
const block = 128
const seconds = 5

function run(label, patchParams) {
  const engine = createEngine(fs, 2)
  const params = createDefaultParams(fs)
  if (patchParams) patchParams(params)
  engine.setParams(params)
  engine.prepare(block)

  const n = block
  const inL = new Float32Array(n).fill(0.1)
  const inR = new Float32Array(n).fill(0.1)
  const outL = new Float32Array(n)
  const outR = new Float32Array(n)

  for (let i = 0; i < 200; i++) engine.process([inL, inR], [outL, outR])

  const totalBlocks = Math.floor((fs * seconds) / block)
  const start = performance.now()
  for (let i = 0; i < totalBlocks; i++) engine.process([inL, inR], [outL, outR])
  const elapsedMs = performance.now() - start

  const audioMs = (totalBlocks * block / fs) * 1000
  const pct = (elapsedMs / audioMs) * 100
  console.log(`${label}: ${elapsedMs.toFixed(2)} ms (${pct.toFixed(2)}% realtime)`)
}

console.log('48kHz / 128 帧 / 5 秒音频 / 各场景全链:')
console.log('----------------------------------------------')

// 默认全链
run('默认全链               ', null)

// 卷积混响:2s IR
const irLen = fs * 2
const ir = new Float32Array(irLen)
for (let i = 0; i < irLen; i++) ir[i] = Math.exp(-i / (0.5 * fs)) * (i % 97 === 0 ? 1 : 0.01)
run('卷积混响(2s IR)        ', (p) => {
  p.reverb.enabled = true
  p.reverb.mode = 'convolution'
  p.reverb.convolution.ir = ir
  p.reverb.convolution.mix = 0.3
})

// FDN 混响
run('FDN 混响(algorithmic)  ', (p) => {
  p.reverb.enabled = true
  p.reverb.mode = 'fdn'
  p.reverb.algorithmic.wet = 0.3
})

// DynamicEq
run('DynamicEq              ', (p) => {
  p.dynamicEq.enabled = true
  p.dynamicEq.strength = 0.5
})
