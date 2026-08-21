/**
 * HyperSoundEngine 简单性能基准
 *
 * 运行前先构建：
 *   npm run build
 * 然后：
 *   npm run benchmark
 *
 * 指标：48kHz / 128 帧 / 立体声 / 默认参数全链处理 5 秒音频的耗时。
 */
import { createEngine, createDefaultParams } from '../dist/index.js'

const fs = 48000
const block = 128
const seconds = 5
const engine = createEngine(fs, 2)
engine.setParams(createDefaultParams(fs))
engine.prepare(block)

const n = block
const inL = new Float32Array(n).fill(0.1)
const inR = new Float32Array(n).fill(0.1)
const outL = new Float32Array(n)
const outR = new Float32Array(n)

// 预热，确保 JIT 生效
for (let i = 0; i < 200; i++) engine.process([inL, inR], [outL, outR])

const totalBlocks = Math.floor((fs * seconds) / block)
const start = performance.now()
for (let i = 0; i < totalBlocks; i++) engine.process([inL, inR], [outL, outR])
const elapsedMs = performance.now() - start

const audioMs = (totalBlocks * block / fs) * 1000
const pct = (elapsedMs / audioMs) * 100
console.log(`processed ${audioMs.toFixed(0)} ms audio in ${elapsedMs.toFixed(2)} ms (${pct.toFixed(2)}% realtime)`)
console.log(`block=${block} fs=${fs} blocks=${totalBlocks}`)
