#!/usr/bin/env node
/**
 * 导出 engine 参数、场景与分享串结构化契约快照。
 *
 * 既有目标逐字节相同则保持不写；内容漂移时拒绝覆盖。脚本还会对
 * specs/dsp/vectors 的 72 组冻结音频向量做前后摘要，确保本导出器不触碰它们。
 */
import { createHash } from 'node:crypto'
import { existsSync, mkdtempSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { pathToFileURL, fileURLToPath } from 'node:url'
import esbuild from 'esbuild'

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const outDir = path.join(repoRoot, 'specs', 'engine', 'vectors')
const dspVectorDir = path.join(repoRoot, 'specs', 'dsp', 'vectors')
const SAMPLE_RATE = 48000
const SCHEMA_VERSION = 1

async function loadFacts() {
  const tempDir = mkdtempSync(path.join(tmpdir(), 'hse-engine-contracts-'))
  const outfile = path.join(tempDir, 'facts.mjs')
  try {
    await esbuild.build({
      stdin: {
        contents: [
          "export { createDefaultParams } from './src/types.ts'",
          "export { SCENE_IDS, SCENE_PRESETS } from './src/engine/ScenePresets.ts'",
          "export { SHARE_CODEC_VERSION, encodeShareCode } from './src/engine/ShareCodec.ts'",
        ].join('\n'),
        resolveDir: repoRoot,
        sourcefile: 'engine-contract-facts.ts',
        loader: 'ts',
      },
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

function serialize(value) {
  return Buffer.from(JSON.stringify(value, null, 2) + '\n', 'utf8')
}

function writeFrozen(fileName, value) {
  const filePath = path.join(outDir, fileName)
  const content = serialize(value)
  if (existsSync(filePath)) {
    const existing = readFileSync(filePath)
    if (existing.equals(content)) return 'unchanged'
    throw new Error(
      '冻结基线冲突：' + filePath + ' 已存在且与 TS 事实源不一致。' +
      '禁止静默改写；确认兼容契约变更后须人工删除旧文件再导出。',
    )
  }
  mkdirSync(outDir, { recursive: true })
  writeFileSync(filePath, content)
  return 'written'
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
  if (before.count !== 144) {
    throw new Error('既有 DSP 冻结向量文件应为 144 个（72 组），实际为 ' + before.count)
  }

  const facts = await loadFacts()
  const params = facts.createDefaultParams(SAMPLE_RATE)
  const sceneIds = Array.from(facts.SCENE_IDS)
  const scenes = facts.SCENE_PRESETS
  if (sceneIds.length !== 12 || scenes.length !== 12) {
    throw new Error('内置场景应固定为 12 个')
  }
  if (scenes.some((scene, index) => scene.id !== sceneIds[index])) {
    throw new Error('SCENE_IDS 与 SCENE_PRESETS 顺序不一致')
  }

  const rawOutOfRange = structuredClone(params)
  rawOutOfRange.sampleRate = 999999
  rawOutOfRange.stereoWidth = 9
  rawOutOfRange.limiter.thresholdDb = -99

  const documents = [
    ['default-params.48000.json', {
      schemaVersion: SCHEMA_VERSION,
      sampleRate: SAMPLE_RATE,
      params,
    }],
    ['scenes.48000.json', {
      schemaVersion: SCHEMA_VERSION,
      sampleRate: SAMPLE_RATE,
      sceneIds,
      scenes,
    }],
    ['share-codes.48000.json', {
      schemaVersion: SCHEMA_VERSION,
      codecVersion: facts.SHARE_CODEC_VERSION,
      sampleRate: SAMPLE_RATE,
      cases: [
        { id: 'default', code: facts.encodeShareCode(params) },
        ...scenes.map((scene) => ({ id: scene.id, code: facts.encodeShareCode(scene.params) })),
        { id: 'raw-out-of-range', code: facts.encodeShareCode(rawOutOfRange) },
      ],
    }],
  ]

  let written = 0
  let unchanged = 0
  for (const [fileName, document] of documents) {
    const result = writeFrozen(fileName, document)
    if (result === 'written') written++
    else unchanged++
    console.log('[' + result.padEnd(9) + '] specs/engine/vectors/' + fileName)
  }

  const after = directoryDigest(dspVectorDir)
  if (after.count !== before.count || after.digest !== before.digest) {
    throw new Error('specs/dsp/vectors 在 engine 契约导出期间发生变化')
  }
  console.log('完成：新写 ' + written + ' 个文件，字节级一致跳过 ' + unchanged + ' 个；既有 72 组 DSP 向量未变。')
}

main().catch((error) => {
  console.error(error instanceof Error ? error.message : String(error))
  process.exitCode = 1
})
