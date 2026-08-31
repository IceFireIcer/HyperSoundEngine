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
import { FdnReverb, type FdnReverbParams } from '../src/dsp/FdnReverb'
import { Deesser } from '../src/dsp/Deesser'
import { LoudnessComp, type LoudnessCompParams } from '../src/dsp/LoudnessComp'
import { DynamicEq, type DynamicEqParams } from '../src/dsp/DynamicEq'
import { DelayEffect, ChorusEffect, FlangerEffect, PhaserEffect, TremoloEffect } from '../src/dsp/ModEffects'
import { Convolver, type ConvolverOptions } from '../src/dsp/Convolver'
import { ModulationMatrix } from '../src/dsp/modulation'
import { HseStretch } from '../src/dsp/HseStretch'
import { LufsMeter } from '../src/dsp/LufsMeter'
import { fft } from '../src/dsp/fft'
import { HyperSoundEngine } from '../src/engine/HyperSoundEngine'
import { createEngine } from '../src/engine/factory'
import { createDefaultParams, type HyperSoundEngineParams, type LimiterSettings, type CompressorSettings, type BassEnhancerSettings, type DeesserSettings, type ModEffectsSettings, type ModulationRoute, type LfoShape } from '../src/types'
import { existsSync, readdirSync, readFileSync } from 'node:fs'
import { join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const VECTOR_DIR = resolve(fileURLToPath(import.meta.url), '..', '..', 'specs', 'dsp', 'vectors')
const FRAME_COUNT_VECTOR = resolve(fileURLToPath(import.meta.url), '..', '..', 'specs', 'engine', 'vectors', 'frame-count.v1.json')
const SUPPORTED_MODULES = ['biquad', 'limiter', 'reverb-simple', 'compressor', 'bass-enhancer', 'mid-side', 'eq-chain', 'fdn-reverb', 'deesser', 'loudness-comp', 'dynamic-eq', 'mod-effects', 'fft', 'convolver', 'modulation-matrix', 'hse-stretch', 'lufs-meter', 'engine-chain'] as const

/** 计量读数条目（specs/dsp/lufs-meter.md §三/§五）：want 为数值或非有限哨兵字符串 */
interface ReadingEntry {
  want: number | 'NaN' | '+Infinity' | '-Infinity'
  tol: number
}

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
  /** 可选模块驱动形态（缺省视为 'stream'）；meter = 计量型读数驱动（specs/dsp/lufs-meter.md §三） */
  moduleKind?: 'stream' | 'meter'
  /** 计量读数契约（仅 moduleKind='meter'）：读数名 → { want, tol } */
  readings?: Record<string, ReadingEntry>
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

interface FrameCountCase {
  id: string
  sampleRate: number
  preparedCapacity: number
  blockFrames: number[]
  seeds: number[]
  params: {
    tremolo: { rateHz: number; depth: number; mix: number }
  }
}

interface FrameCountFixture {
  schemaVersion: number
  cases: FrameCountCase[]
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
  // moduleKind / readings 双向绑定（Schema allOf 的加载器侧对偶校验）
  if (m.moduleKind !== undefined && m.moduleKind !== 'stream' && m.moduleKind !== 'meter') {
    throw new Error(found.label + ': moduleKind 必须为 stream/meter，实际 ' + String(m.moduleKind))
  }
  if (m.readings !== undefined && m.moduleKind !== 'meter') {
    throw new Error(found.label + ': readings 仅允许出现在 moduleKind="meter" 的计量型向量')
  }
  if (m.moduleKind === 'meter') {
    if (!m.readings || Object.keys(m.readings).length === 0) {
      throw new Error(found.label + ': 计量型向量（moduleKind="meter"）必须携带非空 readings')
    }
    for (const [name, entry] of Object.entries(m.readings)) {
      const w = entry.want
      if (typeof w !== 'number' && w !== 'NaN' && w !== '+Infinity' && w !== '-Infinity') {
        throw new Error(found.label + ': 读数 ' + name + ' 的 want 必须为数值或 NaN/±Infinity 哨兵')
      }
      if (typeof entry.tol !== 'number' || !(entry.tol >= 0) || !Number.isFinite(entry.tol)) {
        throw new Error(found.label + ': 读数 ' + name + ' 的 tol 必须为非负有限数')
      }
    }
  }
  // f32 布局：流式 = [输入左][输入右][期望输出左][期望输出右] × frames × 4 字节；
  // 计量型（moduleKind='meter'）= [输入左][输入右] 两段 × frames × 4 字节（无期望输出段，
  // specs/dsp/lufs-meter.md §三）。
  const expectedBytes = m.moduleKind === 'meter' ? m.frames * 4 * 2 : m.frames * 4 * 4
  if (found.f32Bytes.byteLength !== expectedBytes) {
    throw new Error(
      found.label + ': .f32 字节数应为 ' + expectedBytes +
      '（' + (m.moduleKind === 'meter' ? 'frames×2 段×4 字节，计量型' : 'frames×4 段×4 字节') + '），实际 ' + found.f32Bytes.byteLength,
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

/** IR 配方（specs/dsp/convolver.md §4.2；与导出脚本 buildIrRecipe 逐字一致） */
interface IrRecipe {
  kind: 'delta' | 'expNoise'
  delay?: number
  length?: number
  seed?: number
  decay?: number
  amp?: number
}

/** convolver 向量 params 形状（specs/dsp/convolver.md §三） */
interface ConvolverVectorParams {
  partitionSize: number
  longPartitionSize: number
  shortRegionMs: number
  dePeriodize: boolean
  mix: number
  preDelayMs: number
  ir: IrRecipe
}

/**
 * IR 配方 → 确定性冲激响应（specs/dsp/convolver.md §4.2，两支线逐字一致）。
 * 双精度求值、存入 Float32Array 时一次量化为 f32；LCG 与导出工具 lcgNoise 同族。
 * 表达式结合序逐字固化（f64 乘法不可交换结合），不得重排。
 */
function buildIrRecipe(recipe: IrRecipe): Float32Array {
  if (recipe.kind === 'delta') {
    const delay = Math.round(recipe.delay as number)
    if (!(delay >= 0)) throw new Error('delta IR 配方 delay 非法')
    const ir = new Float32Array(delay + 1)
    ir[delay] = 1
    return ir
  }
  if (recipe.kind === 'expNoise') {
    const length = Math.round(recipe.length as number)
    const seed = (recipe.seed as number) >>> 0
    const decay = recipe.decay as number
    const amp = recipe.amp as number
    if (!(length >= 2) || !(decay > 0)) throw new Error('expNoise IR 配方 length/decay 非法')
    const ir = new Float32Array(length)
    let s = seed
    for (let i = 0; i < length; i++) {
      s = (Math.imul(s, 1664525) + 1013904223) >>> 0
      const u = s / 4294967296
      ir[i] = ((u * 2 - 1) * amp) * Math.exp((-decay * i) / (length - 1))
    }
    return ir
  }
  throw new Error('未知 IR 配方 kind：' + recipe.kind)
}

/** 分块处理器形态：每块调用一次，返回该块输出（计量型模块返回零长数组） */
type ProcessFn = (l: Float32Array, r: Float32Array) => [Float32Array, Float32Array]

/** 按 module id 用 TS 支线实现构造分块处理器（状态在闭包实例上跨块保持）；
 *  计量型模块（moduleKind='meter'）在返回的处理器上附加 meter 实例，供读数对拍取回 */
function instantiate(
  moduleId: string,
  sampleRate: number,
  params: Record<string, unknown>,
): ProcessFn & { meter?: LufsMeter } {
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
    case 'fdn-reverb': {
      const reverb = new FdnReverb(sampleRate)
      reverb.setParams(params as unknown as FdnReverbParams)
      return (l, r) => {
        const outL = l.slice()
        const outR = r.slice()
        reverb.processStereo(outL, outR)
        return [outL, outR]
      }
    }
    case 'deesser': {
      const dss = new Deesser(sampleRate)
      dss.setParams(params as unknown as DeesserSettings)
      const useSidechain = params.sidechainEnabled === true
      return (l, r) => {
        const outL = l.slice()
        const outR = r.slice()
        if (useSidechain) {
          // sidechain 向量语义（specs/dsp/deesser.md §4.6）：本块原始输入的
          // 单声道和派生，双精度加法、就地处理前快照；sideL 与 sideR 内容相同
          // （本批向量不含该形态，规则与 compressor §4.5 同构）。
          const side = new Float32Array(l.length)
          for (let i = 0; i < side.length; i++) side[i] = l[i] + r[i]
          dss.processStereo(outL, outR, side, side)
        } else {
          dss.processStereo(outL, outR)
        }
        return [outL, outR]
      }
    }
    case 'loudness-comp': {
      // 平滑 alpha 逐块计算（specs/dsp/loudness-comp.md §4.3）：输出依赖 blockSize，
      // 与导出脚本及 Rust 门禁按同一块长回放。
      const comp = new LoudnessComp(sampleRate)
      comp.setParams(params as unknown as LoudnessCompParams)
      return (l, r) => {
        const outL = l.slice()
        const outR = r.slice()
        comp.processStereo(outL, outR)
        return [outL, outR]
      }
    }
    case 'dynamic-eq': {
      // 向量 params 为模块完整形状（DynamicEqParams，specs/dsp/dynamic-eq.md §三）；
      // 输出依赖顶层驱动分块与 params.blockSize 的控制节奏耦合（§4.5 实证），
      // 与导出脚本及 Rust 门禁按同一块长回放。
      const eq = new DynamicEq(sampleRate, params as unknown as DynamicEqParams)
      return (l, r) => {
        const outL = l.slice()
        const outR = r.slice()
        eq.processStereo(outL, outR)
        return [outL, outR]
      }
    }
    case 'mod-effects': {
      // 五效果按引擎接线顺序级联（HyperSoundEngine buildStages：delay→chorus→flanger→
      // phaser→tremolo，specs/dsp/mod-effects.md §4.1）。引擎语义：五效果无条件 setParams
      // （enabled 字段被效果类自身忽略），仅 enabled 的效果参与链路，禁用级逐位旁路。
      const me = params as unknown as ModEffectsSettings
      const delay = new DelayEffect(sampleRate)
      const chorus = new ChorusEffect(sampleRate)
      const flanger = new FlangerEffect(sampleRate)
      const phaser = new PhaserEffect(sampleRate)
      const tremolo = new TremoloEffect(sampleRate)
      delay.setParams(me.delay)
      chorus.setParams(me.chorus)
      flanger.setParams(me.flanger)
      phaser.setParams(me.phaser)
      tremolo.setParams(me.tremolo)
      const chain = [
        { enabled: me.delay.enabled, run: (l: Float32Array, r: Float32Array) => delay.processStereo(l, r) },
        { enabled: me.chorus.enabled, run: (l: Float32Array, r: Float32Array) => chorus.processStereo(l, r) },
        { enabled: me.flanger.enabled, run: (l: Float32Array, r: Float32Array) => flanger.processStereo(l, r) },
        { enabled: me.phaser.enabled, run: (l: Float32Array, r: Float32Array) => phaser.processStereo(l, r) },
        { enabled: me.tremolo.enabled, run: (l: Float32Array, r: Float32Array) => tremolo.processStereo(l, r) },
      ]
      return (l, r) => {
        const outL = l.slice()
        const outR = r.slice()
        for (const stage of chain) {
          if (stage.enabled) stage.run(outL, outR)
        }
        return [outL, outR]
      }
    }
    case 'fft': {
      // FFT 非流式变换特例（specs/dsp/fft.md §三）：输入 (L,R) = 复数平面 (Re,Im)，
      // 每块独立做原位复 FFT（无跨块状态），输出 = 变换后的两个平面。
      // 与导出脚本同构；块长必须为 2 的幂，本批向量固定 blockSize = frames = N（单块驱动）。
      const inverse = params.inverse === true
      return (l, r) => {
        const re = l.slice()
        const im = r.slice()
        fft(re, im, inverse)
        return [re, im]
      }
    }
    case 'convolver': {
      // 驱动顺序采用引擎接线顺序（HyperSoundEngine.ts 卷积混响阶段：构造(dePeriodize 选项)
      // → loadIR → setMix → setPreDelayMs → 逐块 processStereo）。
      // IR 配方与导出脚本逐字一致（specs/dsp/convolver.md §4.2）。
      const p = params as unknown as ConvolverVectorParams
      const opts: ConvolverOptions = {
        partitionSize: p.partitionSize,
        longPartitionSize: p.longPartitionSize,
        shortRegionMs: p.shortRegionMs,
        dePeriodize: p.dePeriodize,
      }
      const cv = new Convolver(sampleRate, opts)
      cv.loadIR(buildIrRecipe(p.ir), 'vector-ir')
      cv.setMix(p.mix)
      cv.setPreDelayMs(p.preDelayMs)
      return (l, r) => {
        const outL = l.slice()
        const outR = r.slice()
        cv.processStereo(outL, outR)
        return [outL, outR]
      }
    }
    case 'modulation-matrix': {
      // 控制率 Stage 驱动（specs/dsp/modulation-matrix.md §4.4）：实例化按引擎接线顺序
      // setRoutes → setLfoParams → setEnvelopeParams；每块先 processBlock 推进矩阵
      // （包络读取增益前输入，对应引擎「矩阵在块头推进、增益在链尾应用」），再把
      // masterGain 逐样本乘到 L/R（引擎 mod-master-gain 阶段语义）。stereoWidth 产物
      // 不入向量。输出依赖 blockSize（LFO 相位按块推进、增益按块常量）。
      const p = params as unknown as {
        routes: ModulationRoute[]
        lfo: { shape: LfoShape; rateHz: number; depth: number }
        envelope: { attackMs: number; releaseMs: number; amount: number }
      }
      const mm = new ModulationMatrix(sampleRate)
      mm.setRoutes(p.routes)
      mm.setLfoParams(p.lfo.shape, p.lfo.rateHz, p.lfo.depth)
      mm.setEnvelopeParams(p.envelope.attackMs, p.envelope.releaseMs, p.envelope.amount)
      return (l, r) => {
        const outL = l.slice()
        const outR = r.slice()
        const mod = mm.processBlock(outL, outR, outL.length)
        const g = mod.masterGain
        for (let i = 0; i < outL.length; i++) {
          outL[i] *= g
          outR[i] *= g
        }
        return [outL, outR]
      }
    }
    case 'hse-stretch': {
      // 块窗映射驱动（specs/dsp/hse-stretch.md §4.6）：processStereo 非就地、输出长度随
      // 参数变化——每块取输出的前 len 个样本（超出截断、不足补零）。载荷含 initialParams
      // 时先以初参 setParams，处理第 switchAtBlock 块之前以终参 setParams（参数突变序列，
      // §4.6.3）。驱动器从不调用 isSignalsmithAvailable()（恒为自研相位声码器路径）。
      // 输出依赖 blockSize（每块独立 STFT 分帧）。
      const p = params as unknown as {
        semitones: number
        rate: number
        initialParams?: { semitones: number; rate: number }
        switchAtBlock?: number
      }
      const stretch = new HseStretch(sampleRate, 2)
      const finalParams = { semitones: p.semitones, rate: p.rate }
      const sequenced = Boolean(p.initialParams)
      if (p.initialParams) {
        stretch.setParams({ semitones: p.initialParams.semitones, rate: p.initialParams.rate })
      } else {
        stretch.setParams(finalParams)
      }
      let blockIndex = 0
      return (l, r) => {
        const len = l.length
        if (sequenced && blockIndex === p.switchAtBlock) stretch.setParams(finalParams)
        blockIndex++
        const out = stretch.processStereo(l, r)
        const outL = new Float32Array(len)
        const outR = new Float32Array(len)
        outL.set(out.l.subarray(0, Math.min(out.l.length, len)))
        outR.set(out.r.subarray(0, Math.min(out.r.length, len)))
        return [outL, outR]
      }
    }
    case 'lufs-meter': {
      // 计量型读数驱动（specs/dsp/lufs-meter.md §三）：processStereo 就地分析——不改写
      // 缓冲、无音频输出（返回零长数组，f32 为两段输入布局）；实例附在 process.meter 上，
      // 全部块馈入完成后由读数判定取回（assertReadingsWithinTolerance）。
      // 无参数模块：构造仅 fs，params 恒为 {}，不调用 setParams。
      const meter = new LufsMeter(sampleRate)
      const process = ((l: Float32Array, r: Float32Array) => {
        meter.processStereo(l, r)
        return [new Float32Array(0), new Float32Array(0)] as [Float32Array, Float32Array]
      }) as ProcessFn & { meter?: LufsMeter }
      process.meter = meter
      return process
    }
    case 'engine-chain': {
      // 真实引擎 1–21 级驱动（specs/engine/chain.md）：默认参数事实源 + overrides 深合并，
      // 一次 setParams 后按向量 blockSize 重放 process；spatial 必须 off，HseStretch 链外。
      const engine = new HyperSoundEngine(sampleRate, 2, { legacyPaddedTail: true })
      const p = mergeEngineParams(createDefaultParams(sampleRate), (params.overrides ?? {}) as Record<string, unknown>)
      if (!p.spatial || p.spatial.mode !== 'off') throw new Error('engine-chain 必须设置 spatial.mode="off"')
      engine.setParams(p)
      return (l, r) => {
        const outL = new Float32Array(l.length)
        const outR = new Float32Array(r.length)
        engine.process([l, r], [outL, outR])
        return [outL, outR]
      }
    }
    default:
      throw new Error('未知模块 id：' + moduleId)
  }
}

/** engine-chain overrides 深合并；数组与 typed array 整体替换，与导出器逐字同构。 */
function mergeEngineParams(base: HyperSoundEngineParams, overrides: Record<string, unknown>): HyperSoundEngineParams {
  const merge = (target: Record<string, unknown>, patch: Record<string, unknown>): void => {
    for (const [key, value] of Object.entries(patch)) {
      const current = target[key]
      if (value && typeof value === 'object' && !Array.isArray(value) && !ArrayBuffer.isView(value) &&
          current && typeof current === 'object' && !Array.isArray(current) && !ArrayBuffer.isView(current)) {
        merge(current as Record<string, unknown>, value as Record<string, unknown>)
      } else {
        target[key] = value
      }
    }
  }
  merge(base as unknown as Record<string, unknown>, overrides)
  return base
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

/** 计量读数 getter 表（读数名与导出工具 METER_READINGS 逐字一致，specs/dsp/lufs-meter.md §二/§四） */
const READING_GETTERS: Record<string, (m: LufsMeter) => number> = {
  integratedLufs: (m) => m.getIntegratedLufs(),
  momentaryLufs: (m) => m.getMomentaryLufs(),
  shortTermLufs: (m) => m.getShortTermLufs(),
  lra: (m) => m.getLra(),
  peakDb: (m) => m.getPeakDb(),
  truePeakDb: (m) => m.getTruePeakDb(),
}

/** 单条计量读数判定（specs/dsp/lufs-meter.md §三/§五）：
 *  want 为有限数 → 绝对容差 |got-want| <= tol；want 为哨兵 → 等值判定（tol 不参与） */
function assertReadingWithinTolerance(label: string, name: string, entry: ReadingEntry, got: number): void {
  const w = entry.want
  if (w === 'NaN') {
    if (!Number.isNaN(got)) {
      throw new Error(label + ' 读数 ' + name + ' 应为 NaN，实际 ' + got)
    }
    return
  }
  if (w === '+Infinity') {
    if (got !== Infinity) {
      throw new Error(label + ' 读数 ' + name + ' 应为 +Infinity，实际 ' + got)
    }
    return
  }
  if (w === '-Infinity') {
    if (got !== -Infinity) {
      throw new Error(label + ' 读数 ' + name + ' 应为 -Infinity，实际 ' + got)
    }
    return
  }
  const err = Math.abs(got - w)
  if (!(err <= entry.tol)) {
    throw new Error(
      label + ' 读数 ' + name + ' 超出容差：got=' + got + ' want=' + w +
      ' |err|=' + err + ' 允许上限=' + entry.tol,
    )
  }
}

/** 计量型读数对拍：与冻结 readings 逐项判定（未知名视为向量非法） */
function assertReadingsWithinTolerance(label: string, readings: Record<string, ReadingEntry>, meter: LufsMeter): void {
  for (const name of Object.keys(readings)) {
    const getter = READING_GETTERS[name]
    if (!getter) {
      throw new Error(label + ': 未知读数名 "' + name + '"（合法集合见 specs/dsp/lufs-meter.md §二）')
    }
    assertReadingWithinTolerance(label, name, readings[name], getter(meter))
  }
}

const discovered = discoverCases()

function frameCountSignal(seed: number, frames: number): Float32Array {
  const out = new Float32Array(frames)
  let state = seed >>> 0
  for (let i = 0; i < frames; i++) {
    state = (Math.imul(state, 1664525) + 1013904223) >>> 0
    out[i] = (state / 4294967296) * 2 - 1
  }
  return out
}

function loadFrameCountFixture(): FrameCountFixture {
  if (!existsSync(FRAME_COUNT_VECTOR)) {
    throw new Error('默认 frameCount 共享夹具缺失：' + FRAME_COUNT_VECTOR)
  }
  const fixture = JSON.parse(readFileSync(FRAME_COUNT_VECTOR, 'utf8')) as FrameCountFixture
  if (fixture.schemaVersion !== 1 || !Array.isArray(fixture.cases) || fixture.cases.length === 0) {
    throw new Error('默认 frameCount 共享夹具必须是 schemaVersion=1 且包含非空 cases')
  }
  for (const testCase of fixture.cases) {
    if (typeof testCase.id !== 'string' || testCase.id.length === 0) throw new Error('frameCount case.id 必须为非空字符串')
    if (!Number.isFinite(testCase.sampleRate) || testCase.sampleRate <= 0) throw new Error(testCase.id + ': sampleRate 非法')
    if (!Number.isInteger(testCase.preparedCapacity) || testCase.preparedCapacity <= 0) throw new Error(testCase.id + ': preparedCapacity 非法')
    if (!Array.isArray(testCase.blockFrames) || !Array.isArray(testCase.seeds) || testCase.blockFrames.length !== testCase.seeds.length) {
      throw new Error(testCase.id + ': blockFrames/seeds 必须为等长数组')
    }
    if (testCase.blockFrames.length < 2 || !testCase.blockFrames.some((frames) => frames < testCase.preparedCapacity)) {
      throw new Error(testCase.id + ': 必须包含短于预分配容量的块及后继观察块')
    }
    if (testCase.blockFrames.some((frames) => !Number.isInteger(frames) || frames <= 0 || frames > testCase.preparedCapacity)) {
      throw new Error(testCase.id + ': blockFrames 必须是容量范围内的正整数')
    }
    if (testCase.seeds.some((seed) => !Number.isInteger(seed))) throw new Error(testCase.id + ': seeds 必须为整数')
    const tremolo = testCase.params?.tremolo
    if (!tremolo || !Number.isFinite(tremolo.rateHz) || !Number.isFinite(tremolo.depth) || !Number.isFinite(tremolo.mix)) {
      throw new Error(testCase.id + ': tremolo 参数必须为有限数')
    }
  }
  return fixture
}

describe('默认 createEngine frameCount 共享门禁', () => {
  it('预分配容量不允许短尾块推进容量尾部状态', () => {
    const fixture = loadFrameCountFixture()
    expect(fixture.schemaVersion).toBe(1)
    expect(fixture.cases.length).toBeGreaterThan(0)

    for (const testCase of fixture.cases) {
      expect(testCase.blockFrames.length).toBe(testCase.seeds.length)
      expect(testCase.blockFrames.some((frames) => frames < testCase.preparedCapacity)).toBe(true)
      const prepared = createEngine(testCase.sampleRate)
      const natural = createEngine(testCase.sampleRate)
      const params = createDefaultParams(testCase.sampleRate)
      params.eq.enabled = false
      params.limiter.enabled = false
      params.modEffects.tremolo.enabled = true
      Object.assign(params.modEffects.tremolo, testCase.params.tremolo)
      prepared.setParams(params)
      natural.setParams(params)
      prepared.prepare(testCase.preparedCapacity)

      for (let block = 0; block < testCase.blockFrames.length; block++) {
        const frames = testCase.blockFrames[block]
        expect(frames).toBeGreaterThan(0)
        expect(frames).toBeLessThanOrEqual(testCase.preparedCapacity)
        const inL = frameCountSignal(testCase.seeds[block], frames)
        const inR = frameCountSignal(testCase.seeds[block] ^ 0x9e3779b9, frames)
        const preparedOut = [new Float32Array(frames), new Float32Array(frames)]
        const naturalOut = [new Float32Array(frames), new Float32Array(frames)]
        prepared.process([inL, inR], preparedOut)
        natural.process([inL, inR], naturalOut)
        expect(preparedOut[0], testCase.id + ' left block ' + block).toEqual(naturalOut[0])
        expect(preparedOut[1], testCase.id + ' right block ' + block).toEqual(naturalOut[1])
      }
    }
  })
})

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
      // 计量型（moduleKind='meter'）：音频段为零长（无期望输出段），行为契约由
      // 冻结 readings 标量承载（specs/dsp/lufs-meter.md §三/§五）。
      if (m.moduleKind === 'meter') {
        assertReadingsWithinTolerance(found.label, m.readings as Record<string, ReadingEntry>, process.meter as LufsMeter)
      }
    })
  })
}
