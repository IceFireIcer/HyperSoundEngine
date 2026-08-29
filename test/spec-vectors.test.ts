/**
 * spec-vectors.test.ts —— 双支线共享 DSP 对拍门禁（TS 侧）
 *
 * 职责：
 *  - 扫描 specs/dsp/vectors/*.json 及同名 .f32 冻结基线向量；
 *  - 对每个 case 用 TS 支线实现按契约语义重跑（blockSize 分块、末块可短、状态跨块保持、
 *    输出按序拼接），并按共享容差公式断言：|got-want| <= value * max(|want|, floor)；
 *  - 防呆：向量目录缺失或为空时显式失败（门禁不允许静默空过）；
 *    元数据不符合契约（schemaVersion/channels/tolerance/f32 长度等）时显式失败。
 *
 * 纪律：
 *  - 本测试只读向量，绝不改写；期望值修改须走"新增向量"流程；
 *  - 纯 Node 环境（无 jsdom 文件头注释）；不新增依赖——仓库未引入 @types/node，
 *    文件底部内联本测试所需的最小 node:fs / node:path / node:url 类型声明。
 */
import { describe, expect, it } from 'vitest'
import { Biquad, type BiquadType } from '../src/dsp/biquad'
import { Limiter } from '../src/dsp/Limiter'
import { ReverbSimple, type ReverbSimpleParams } from '../src/dsp/ReverbSimple'
import { Compressor } from '../src/dsp/Compressor'
import { BassEnhancer } from '../src/dsp/BassEnhancer'
import { MidSide } from '../src/dsp/MidSide'
import { EqChain } from '../src/dsp/EqChain'
import type { LimiterSettings, CompressorSettings, BassEnhancerSettings } from '../src/types'
import { existsSync, readdirSync, readFileSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const VECTOR_DIR = resolve(fileURLToPath(import.meta.url), '..', '..', 'specs', 'dsp', 'vectors')
const SUPPORTED_MODULES = ['biquad', 'limiter', 'reverb-simple', 'compressor', 'bass-enhancer', 'mid-side', 'eq-chain'] as const

/** 向量 JSON 元数据（与 specs/dsp/vectors 契约一致） */
interface VectorMeta {
  schemaVersion: number
  module: string
  case: string
  sampleRate: number
  blockSize: number
  channels: number
  frames: number
  params: Record<string, unknown>
  tolerance: { kind: string; value: number; floor: number }
  notes?: string
}

/** 一个已发现的对拍 case */
interface DiscoveredCase {
  /** 展示名：<module>.<case> */
  label: string
  jsonPath: string
  f32Path: string
  meta: VectorMeta
  /** 原始 f32 字节（小端四段布局） */
  f32Bytes: Uint8Array
}

/** 扫描向量目录；目录不存在返回空表（由防呆用例显式失败） */
function discoverCases(): DiscoveredCase[] {
  if (!existsSync(VECTOR_DIR)) return []
  const cases: DiscoveredCase[] = []
  const jsonNames = readdirSync(VECTOR_DIR).filter((n) => n.endsWith('.json')).sort()
  for (const name of jsonNames) {
    const jsonPath = join(VECTOR_DIR, name)
    const f32Path = join(VECTOR_DIR, name.replace(/\.json$/, '.f32'))
    const label = name.replace(/\.json$/, '')
    if (!existsSync(f32Path)) {
      throw new Error('向量配对损坏：缺少与 ' + name + ' 同名的 .f32 文件（' + f32Path + '）')
    }
    const meta = JSON.parse(readFileSync(jsonPath, 'utf8')) as VectorMeta
    const f32Bytes = readFileSync(f32Path)
    cases.push({ label, jsonPath, f32Path, meta, f32Bytes })
  }
  return cases
}

/** 元数据契约校验：任何不符都以显式错误失败 */
function validateMeta(found: DiscoveredCase): void {
  const m = found.meta
  if (m.schemaVersion !== 1) throw new Error(found.label + ': schemaVersion 必须=1，实际 ' + m.schemaVersion)
  if (!(SUPPORTED_MODULES as readonly string[]).includes(m.module)) {
    throw new Error(found.label + ': 未知模块 id "' + m.module + '"（支持：' + SUPPORTED_MODULES.join('/') + '）')
  }
  if (m.channels !== 2) throw new Error(found.label + ': channels 必须=2，实际 ' + m.channels)
  if (!(m.frames > 0)) throw new Error(found.label + ': frames 必须为正数')
  if (!(m.blockSize > 0)) throw new Error(found.label + ': blockSize 必须为正数')
  if (m.tolerance.kind !== 'relative') throw new Error(found.label + ': 容差类型必须为 relative')
  if (!(m.tolerance.value > 0) || !(m.tolerance.floor >= 0)) {
    throw new Error(found.label + ': 容差 value/floor 非法')
  }
  // f32 四段布局：[输入左][输入右][期望输出左][期望输出右] × frames × 4 字节
  const expectedBytes = m.frames * 4 * 4
  if (found.f32Bytes.byteLength !== expectedBytes) {
    throw new Error(
      found.label + ': .f32 字节数应为 ' + expectedBytes + '（frames×4 段×4 字节），实际 ' + found.f32Bytes.byteLength,
    )
  }
}

/** 小端读出四段布局，返回非交错的输入/期望输出数组 */
function readSegments(bytes: Uint8Array, frames: number): {
  inputL: Float32Array
  inputR: Float32Array
  wantL: Float32Array
  wantR: Float32Array
} {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength)
  const total = bytes.byteLength / 4
  const all = new Float32Array(total)
  for (let i = 0; i < total; i++) all[i] = view.getFloat32(i * 4, true)
  return {
    inputL: all.slice(0, frames),
    inputR: all.slice(frames, frames * 2),
    wantL: all.slice(frames * 2, frames * 3),
    wantR: all.slice(frames * 3, frames * 4),
  }
}

/** 按 module id 用 TS 支线实现构造分块处理器（状态在闭包实例上跨块保持） */
function instantiate(
  moduleId: string,
  sampleRate: number,
  params: Record<string, unknown>,
): (l: Float32Array, r: Float32Array) => [Float32Array, Float32Array] {
  switch (moduleId) {
    case 'biquad': {
      // 单声道模块的立体声扩展语义（与导出脚本一致）：左右各一个独立 TDF2 实例，
      // 相同系数、状态独立跨块保持。
      const type = params.type as BiquadType
      const f0 = params.f0 as number
      const q = params.q as number
      const gainDb = params.gainDb as number
      const left = new Biquad(type, f0, q, gainDb, sampleRate)
      const right = new Biquad(type, f0, q, gainDb, sampleRate)
      return (l, r) => {
        const outL = new Float32Array(l.length)
        const outR = new Float32Array(r.length)
        left.processBlock(l, outL)
        right.processBlock(r, outR)
        return [outL, outR]
      }
    }
    case 'limiter': {
      const limiter = new Limiter(sampleRate)
      limiter.setParams(params as unknown as LimiterSettings)
      return (l, r) => {
        const outL = l.slice()
        const outR = r.slice()
        limiter.processStereo(outL, outR)
        return [outL, outR]
      }
    }
    case 'reverb-simple': {
      const reverb = new ReverbSimple(sampleRate)
      reverb.setParams(params as unknown as ReverbSimpleParams)
      return (l, r) => {
        const outL = l.slice()
        const outR = r.slice()
        reverb.processStereo(outL, outR)
        return [outL, outR]
      }
    }
    case 'compressor': {
      const comp = new Compressor(sampleRate)
      comp.setParams(params as unknown as CompressorSettings)
      const useSidechain = params.sidechainEnabled === true
      return (l, r) => {
        const outL = l.slice()
        const outR = r.slice()
        if (useSidechain) {
          // sidechain 向量语义（specs/dsp/compressor.md §4.5）：本块原始输入的
          // 单声道和派生，双精度加法、就地处理前快照；sideL 与 sideR 内容相同。
          const side = new Float32Array(l.length)
          for (let i = 0; i < side.length; i++) side[i] = l[i] + r[i]
          comp.processStereo(outL, outR, side, side)
        } else {
          comp.processStereo(outL, outR)
        }
        return [outL, outR]
      }
    }
    case 'bass-enhancer': {
      const bass = new BassEnhancer(sampleRate)
      bass.setParams(params as unknown as BassEnhancerSettings)
      return (l, r) => {
        const outL = l.slice()
        const outR = r.slice()
        bass.processStereo(outL, outR)
        return [outL, outR]
      }
    }
    case 'mid-side': {
      // MidSide 无采样率概念（构造无参）；setParams 为位置参数接口
      const ms = new MidSide()
      ms.setParams(params.width as number, params.voiceBalance as number)
      return (l, r) => {
        const outL = l.slice()
        const outR = r.slice()
        ms.processStereo(outL, outR)
        return [outL, outR]
      }
    }
    case 'eq-chain': {
      // 驱动顺序采用引擎接线顺序（HyperSoundEngine.ts：先 setBands 后 setQCompensation；
      // specs/dsp/eq-chain.md §4.3 实证两种顺序终态逐位一致）。立体声语义（§4.4）：
      // 左右声道共享同一条级联滤波状态，每块内先整条处理 L、再整条处理 R；
      // 输出依赖 blockSize，由向量固定，与导出脚本及 Rust 门禁按同一块长回放。
      const eq = new EqChain(sampleRate, params.bandCount as number)
      eq.setBands(params.bands as { frequency: number; gain: number; q: number }[])
      eq.setQCompensation(params.qCompensation === true)
      return (l, r) => {
        const outL = l.slice()
        const outR = r.slice()
        eq.processStereo(outL, outR)
        return [outL, outR]
      }
    }
    default:
      throw new Error('未知模块 id：' + moduleId)
  }
}

/** 按 blockSize 分块重跑整段输入，返回拼接后的左右输出 */
function renderChunked(
  process: (l: Float32Array, r: Float32Array) => [Float32Array, Float32Array],
  inputL: Float32Array,
  inputR: Float32Array,
  blockSize: number,
): { outL: Float32Array; outR: Float32Array } {
  const frames = inputL.length
  const outL = new Float32Array(frames)
  const outR = new Float32Array(frames)
  for (let offset = 0; offset < frames; offset += blockSize) {
    const len = Math.min(blockSize, frames - offset)
    const [chunkL, chunkR] = process(inputL.subarray(offset, offset + len), inputR.subarray(offset, offset + len))
    outL.set(chunkL, offset)
    outR.set(chunkR, offset)
  }
  return { outL, outR }
}

/** 共享容差公式判定：|got-want| <= value * max(|want|, floor)；违例即抛错并给出定位信息 */
function assertWithinTolerance(
  label: string,
  channel: string,
  got: Float32Array,
  want: Float32Array,
  tol: { kind: string; value: number; floor: number },
): void {
  let worstRatio = 0
  for (let i = 0; i < want.length; i++) {
    const w = want[i]
    const g = got[i]
    const bound = tol.value * Math.max(Math.abs(w), tol.floor)
    const err = Math.abs(g - w)
    if (!(err <= bound)) {
      throw new Error(
        label + ' [' + channel + '#' + i + '] 超出容差：got=' + g + ' want=' + w +
        ' |err|=' + err + ' 允许上限=' + bound,
      )
    }
    const ratio = err / bound
    if (ratio > worstRatio) worstRatio = ratio
  }
  expect(worstRatio).toBeLessThanOrEqual(1)
}

const discovered = discoverCases()

describe('spec-vectors 对拍门禁（TS 侧）', () => {
  it('向量目录必须存在且至少包含一个 case（防呆：门禁禁止静默空过）', () => {
    if (!existsSync(VECTOR_DIR)) {
      throw new Error('对拍向量目录不存在：' + VECTOR_DIR + '。请先运行 node scripts/export-vectors.mjs 生成冻结基线。')
    }
    if (discovered.length === 0) {
      throw new Error('对拍向量目录为空：' + VECTOR_DIR + '。请先运行 node scripts/export-vectors.mjs 生成冻结基线。')
    }
    expect(discovered.length).toBeGreaterThan(0)
  })
})

for (const found of discovered) {
  describe('对拍 ' + found.label, () => {
    it('元数据符合共享契约', () => {
      validateMeta(found)
    })

    it('TS 实现重跑结果落在共享容差内', () => {
      validateMeta(found)
      const m = found.meta
      const { inputL, inputR, wantL, wantR } = readSegments(found.f32Bytes, m.frames)
      const process = instantiate(m.module, m.sampleRate, m.params)
      const { outL, outR } = renderChunked(process, inputL, inputR, m.blockSize)
      assertWithinTolerance(found.label, '输出左', outL, wantL, m.tolerance)
      assertWithinTolerance(found.label, '输出右', outR, wantR, m.tolerance)
    })
  })
}
