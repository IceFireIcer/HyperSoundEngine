#!/usr/bin/env node
import { existsSync, mkdirSync, writeFileSync } from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const output = path.join(repoRoot, 'specs', 'engine', 'vectors', 'frame-count.v1.json')
const fixture = {
  schemaVersion: 1,
  cases: [{
    id: 'prepared-short-tail-does-not-advance-capacity',
    sampleRate: 48000,
    preparedCapacity: 128,
    blockFrames: [7, 128, 31, 128],
    seeds: [501, 502, 503, 504],
    params: { tremolo: { rateHz: 7, depth: 0.8, mix: 1 } },
  }],
}

if (existsSync(output)) throw new Error('夹具已存在，生成器拒绝覆盖：' + output)
mkdirSync(path.dirname(output), { recursive: true })
writeFileSync(output, JSON.stringify(fixture, null, 2) + '\n')
console.log('已生成 ' + path.relative(repoRoot, output))
