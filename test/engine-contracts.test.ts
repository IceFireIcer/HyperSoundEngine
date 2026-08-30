/**
 * 引擎参数、场景与分享串冻结契约。
 * JSON 夹具由 scripts/export-engine-contracts.mjs 从 TS 事实源生成。
 */
import { readFileSync } from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'
import { SCENE_IDS, SCENE_PRESETS } from '../src/engine/ScenePresets'
import { decodeShareCode, encodeShareCode, SHARE_CODEC_VERSION } from '../src/engine/ShareCodec'
import { createDefaultParams } from '../src/types'
import type { HyperSoundEngineParams, ScenePreset } from '../src/types'

interface DefaultParamsFixture {
  schemaVersion: number
  sampleRate: number
  params: HyperSoundEngineParams
}

interface ScenesFixture {
  schemaVersion: number
  sampleRate: number
  sceneIds: string[]
  scenes: ScenePreset[]
}

interface ShareCodesFixture {
  schemaVersion: number
  codecVersion: number
  sampleRate: number
  cases: Array<{ id: string; code: string }>
}

const fixtureDir = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  '../specs/engine/vectors',
)

function readFixture<T>(fileName: string): T {
  return JSON.parse(readFileSync(path.join(fixtureDir, fileName), 'utf8')) as T
}

function fieldTree(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(fieldTree)
  if (value !== null && typeof value === 'object') {
    return Object.fromEntries(
      Object.entries(value as Record<string, unknown>).map(([key, child]) => [key, fieldTree(child)]),
    )
  }
  return typeof value
}

const defaultFixture = readFixture<DefaultParamsFixture>('default-params.48000.json')
const scenesFixture = readFixture<ScenesFixture>('scenes.48000.json')
const shareFixture = readFixture<ShareCodesFixture>('share-codes.48000.json')

describe('引擎参数冻结契约', () => {
  it('锁定 schema、48 kHz 默认值与完整字段树', () => {
    const actual = createDefaultParams(48000)
    expect(defaultFixture.schemaVersion).toBe(1)
    expect(defaultFixture.sampleRate).toBe(48000)
    expect(actual).toEqual(defaultFixture.params)
    expect(fieldTree(actual)).toEqual(fieldTree(defaultFixture.params))
  })

  it('默认工厂仅参数化 sampleRate，且每次返回独立快照', () => {
    const at44100 = createDefaultParams(44100)
    const expected = structuredClone(defaultFixture.params)
    expected.sampleRate = 44100
    expect(at44100).toEqual(expected)

    at44100.eq.proBands[0].gain = 12
    at44100.dynamicEq.bands[0].enabled = false
    expect(createDefaultParams(44100)).toEqual(expected)
  })
})

describe('内置场景冻结契约', () => {
  it('锁定 12 个场景的数量、顺序和完整内容', () => {
    expect(scenesFixture.schemaVersion).toBe(1)
    expect(scenesFixture.sampleRate).toBe(48000)
    expect(scenesFixture.sceneIds).toHaveLength(12)
    expect(scenesFixture.sceneIds).toEqual(Array.from(SCENE_IDS))
    expect(SCENE_PRESETS.map((scene) => scene.id)).toEqual(scenesFixture.sceneIds)
    expect(SCENE_PRESETS).toEqual(scenesFixture.scenes)
  })

  it('每个场景保持完整快照身份字段且不共享参数对象', () => {
    for (const [index, scene] of SCENE_PRESETS.entries()) {
      expect(scene.builtin).toBe(true)
      expect(scene.params.sampleRate).toBe(48000)
      expect(scene.params.sceneId).toBe(scene.id)
      expect(scene.params.customized).toBe(false)
      expect(scene.params.reverb.convolution.ir).toBeNull()
      expect(scene.params.spatial?.mode).toBe('off')
      for (let other = index + 1; other < SCENE_PRESETS.length; other++) {
        expect(scene.params).not.toBe(SCENE_PRESETS[other].params)
        expect(scene.params.eq.proBands).not.toBe(SCENE_PRESETS[other].params.eq.proBands)
      }
    }
  })
})

describe('分享串 golden 契约', () => {
  it('锁定默认参数及 12 个场景的规范 v2 编码', () => {
    expect(shareFixture.schemaVersion).toBe(1)
    expect(shareFixture.codecVersion).toBe(SHARE_CODEC_VERSION)
    expect(shareFixture.sampleRate).toBe(48000)
    expect(shareFixture.cases.map(({ id }) => id)).toEqual([
      'default',
      ...scenesFixture.sceneIds,
      'raw-out-of-range',
    ])

    const rawOutOfRange = structuredClone(defaultFixture.params)
    rawOutOfRange.sampleRate = 999999
    rawOutOfRange.stereoWidth = 9
    rawOutOfRange.limiter.thresholdDb = -99
    const snapshots = [
      defaultFixture.params,
      ...scenesFixture.scenes.map((scene) => scene.params),
      rawOutOfRange,
    ]
    expect(snapshots.map((params) => encodeShareCode(params))).toEqual(
      shareFixture.cases.map(({ code }) => code),
    )
  })

  it('冻结分享串解码为对应快照，并可规范地再次编码', () => {
    const snapshots = [defaultFixture.params, ...scenesFixture.scenes.map((scene) => scene.params)]
    for (const [index, item] of shareFixture.cases.slice(0, snapshots.length).entries()) {
      const decoded = decodeShareCode(item.code)
      expect(decoded).toEqual(snapshots[index])
      expect(encodeShareCode(decoded)).toBe(item.code)
    }
  })

  it('编码保留越界原值，解码阶段才按白名单钳制', () => {
    const edge = shareFixture.cases.find(({ id }) => id === 'raw-out-of-range')
    expect(edge).toBeDefined()
    const raw = structuredClone(defaultFixture.params)
    raw.sampleRate = 999999
    raw.stereoWidth = 9
    raw.limiter.thresholdDb = -99
    expect(encodeShareCode(raw)).toBe(edge!.code)

    const decoded = decodeShareCode(edge!.code)
    expect(decoded.sampleRate).toBe(192000)
    expect(decoded.stereoWidth).toBe(2)
    expect(decoded.limiter.thresholdDb).toBe(-60)
  })
})
