/**
 * Node 离线处理示例
 *
 * 运行前先构建：
 *   npm run build
 * 然后：
 *   node examples/node-offline.mjs
 */

import { createEngine, createDefaultParams } from '../dist/index.js'

const fs = 48000
const engine = createEngine(fs, 2)
const params = createDefaultParams(fs)

// 打开 EQ 与限幅器，做一点简单处理
params.eq.enabled = true
params.eq.simpleBands = [2, 0, 0, 0, 2]
engine.setParams(params)

// 生成 0.1s 440Hz 正弦 + 0.1s 静音
const seconds = 0.2
const n = Math.floor(fs * seconds)
const inL = new Float32Array(n)
const inR = new Float32Array(n)
for (let i = 0; i < n; i++) {
  const t = i / fs
  const v = i < fs * 0.1 ? Math.sin(2 * Math.PI * 440 * t) * 0.5 : 0
  inL[i] = v
  inR[i] = v
}

const outL = new Float32Array(n)
const outR = new Float32Array(n)
engine.process([inL, inR], [outL, outR])

const stats = engine.getStats()
console.log('processed samples:', n)
console.log('peakDb:', stats.peakDb.toFixed(2))
console.log('lufsIntegrated:', Number.isFinite(stats.lufsIntegrated) ? stats.lufsIntegrated.toFixed(2) : 'NaN (too short)')
console.log('engineLatencySamples:', stats.engineLatencySamples)
