/**
 * HyperSoundEngine v1 —— 分享串编解码（ShareCodec）
 *
 * 出处/许可：自研实现（设计文档 §4.10 / 映射表 #19，决策为 🔴 必须自研）。
 *   - FNV-1a 32 位哈希：Fowler–Noll–Vo 公开算法（公有领域，无版权限制）。
 *   - base64url：RFC 4648 §5（URL 安全字母表，无填充）。
 *   - 解码端"白名单字段 + 数值 clamp"语义对应既有"分享串防注入"修复。
 *
 * 序列化格式（两代并存，解码端全收）：
 *   v2（当前编码输出）：HSE2-<Crockford Base32 分组串>
 *     - 载荷 = "<version>:<checksum>:<deltaJson>"，version=2；deltaJson 只存与默认参数
 *       的差异项（sampleRate 强制携带，作为还原骨架的采样率）；
 *     - Crockford Base32：0-9 + 去易混字符的大写字母（无 I/L/O/U），大小写不敏感，
 *       解码端把 I/L→1、O→0、U→V 归一；每 5 字符一组以 '-' 分隔，仅为可读性，
 *       解码前剥掉全部分隔符。
 *   v1（旧串，只解码）：base64url( "<version>:<checksum>:<json>" )，json 为全量参数快照。
 *   - 两代的 json 为"去 IR 数组"后的参数（卷积混响 IR 数组不参与序列化，仅保留 irName
 *     引用，解码后 ir 恒为 null，由调用方按 irName 重新加载）；
 *   - checksum 为覆盖 "<version>:<json>" 的 FNV-1a 32 位值（8 位小写十六进制），
 *     固定长度置于版本号之后，保证 json 内含 ':' 也能无歧义解析；
 *   - v2 解码：差异载荷以默认参数为骨架还原（未知键与 v1 白名单语义一致：静默丢弃），
 *     再走同一套白名单字段 + 数值 clamp 清洗。
 *
 * 确定性：序列化对象按固定字段顺序构造，同参数必得同串；同串解码结果唯一。
 * 纯 TS、零运行时依赖：仅使用平台全局（TextEncoder/TextDecoder/Math.imul）。
 */

import type { HyperSoundEngineParams, EqBand, ModulationRoute, SpatialSettings } from '../types'
import { createDefaultParams, createDefaultSpatialSettings } from '../types'

/** 当前分享串格式版本（编码输出）；载荷 2 = 仅存与默认参数的差异项 */
export const SHARE_CODEC_VERSION = 2
/** 旧版全量载荷版本（只解码不编码；旧分享串持续可导入） */
export const SHARE_CODEC_LEGACY_VERSION = 1

// ---------------------------------------------------------------------------
// base64url（RFC 4648 §5）
// ---------------------------------------------------------------------------
const B64_ALPHABET = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_'

function utf8Encode(s: string): Uint8Array {
  return new TextEncoder().encode(s)
}

function utf8Decode(bytes: Uint8Array): string {
  return new TextDecoder().decode(bytes)
}

/** base64url → 字节；非法字符/非法长度抛 Error（防注入入口） */
function base64UrlToBytes(s: string): Uint8Array {
  if (s.length % 4 === 1) throw new Error('invalid share code: bad base64url length')
  let t = s
  while (t.length % 4 !== 0) t += '=' // 兼容带填充输入（本实现生成时无填充）
  const out: number[] = []
  for (let i = 0; i < t.length; i += 4) {
    const c0 = B64_ALPHABET.indexOf(t[i])
    const c1 = B64_ALPHABET.indexOf(t[i + 1])
    const c2 = t[i + 2] === '=' ? 0 : B64_ALPHABET.indexOf(t[i + 2])
    const c3 = t[i + 3] === '=' ? 0 : B64_ALPHABET.indexOf(t[i + 3])
    if (c0 < 0 || c1 < 0 || c2 < 0 || c3 < 0) {
      throw new Error('invalid share code: bad base64url character')
    }
    out.push((c0 << 2) | (c1 >> 4))
    if (t[i + 2] !== '=') out.push(((c1 & 0x0f) << 4) | (c2 >> 2))
    if (t[i + 3] !== '=') out.push(((c2 & 0x03) << 6) | c3)
  }
  return Uint8Array.from(out)
}

// ---------------------------------------------------------------------------
// Crockford Base32（0-9 + 去易混字符的大写字母；比 base64url 长约 20%，
// 但大小写不敏感、无 I/L/O/U 易混字符、可分组——为"人手抄/口述/黏贴"场景服务）
// ---------------------------------------------------------------------------
const B32_ALPHABET = '0123456789ABCDEFGHJKMNPQRSTVWXYZ'
/** HSE2 前缀（v2 传输哨兵；纯 ASCII，跨输入法/剪贴板稳定） */
const SHARE_CODE_PREFIX = 'HSE2'

/** 字节 → Crockford Base32（大写；末尾不足 5 位补零） */
function bytesToBase32Crockford(bytes: Uint8Array): string {
  let acc = 0
  let bits = 0
  let out = ''
  for (let i = 0; i < bytes.length; i++) {
    acc = (acc << 8) | bytes[i]
    bits += 8
    while (bits >= 5) {
      out += B32_ALPHABET[(acc >>> (bits - 5)) & 0x1f]
      bits -= 5
    }
  }
  if (bits > 0) out += B32_ALPHABET[(acc << (5 - bits)) & 0x1f]
  return out
}

/** Crockford Base32 → 字节；剥离分隔符、大小写归一（I/L→1、O→0、U→V），非法字符抛 Error */
function base32CrockfordToBytes(s: string): Uint8Array {
  let acc = 0
  let bits = 0
  const out: number[] = []
  for (const ch0 of s.toUpperCase()) {
    const ch = ch0 === 'I' || ch0 === 'L' ? '1' : ch0 === 'O' ? '0' : ch0 === 'U' ? 'V' : ch0
    const v = B32_ALPHABET.indexOf(ch)
    if (v < 0) throw new Error('invalid share code: bad base32 character ' + ch0)
    acc = (acc << 5) | v
    bits += 5
    if (bits >= 8) {
      out.push((acc >>> (bits - 8)) & 0xff)
      bits -= 8
    }
  }
  return Uint8Array.from(out)
}

/** 每 5 字符一组以 '-' 分隔（纯可读性；解码前剥离） */
function groupCode(s: string): string {
  return (s.match(/.{1,5}/g) ?? []).join('-')
}

/**
 * 深比较：full 相对 base 的差异子树（plain JSON 语义；数组与叶子整体比较）。
 * 返回 undefined 表示该子树与 base 完全一致（不参与序列化）。
 */
function shareDelta(base: unknown, full: unknown): unknown {
  if (
    base === null || full === null || Array.isArray(base) || Array.isArray(full) ||
    typeof base !== 'object' || typeof full !== 'object'
  ) {
    return JSON.stringify(base) === JSON.stringify(full) ? undefined : full
  }
  const b = base as Record<string, unknown>
  const f = full as Record<string, unknown>
  const out: Record<string, unknown> = {}
  let changed = false
  for (const k of Object.keys(f)) {
    if (!(k in b)) {
      out[k] = f[k]
      changed = true
      continue
    }
    const sub = shareDelta(b[k], f[k])
    if (sub !== undefined) {
      out[k] = sub
      changed = true
    }
  }
  return changed ? out : undefined
}

/** 差异载荷还原：以 base（默认参数骨架）为底、delta 覆盖；数组与叶子整体替换；
 *  未知键与 v1 白名单语义一致——静默丢弃（篡改另有校验和兜底）。 */
function shareRehydrate(base: unknown, delta: unknown): unknown {
  if (
    delta === null || Array.isArray(delta) || typeof delta !== 'object' ||
    base === null || Array.isArray(base) || typeof base !== 'object'
  ) {
    return delta
  }
  const b = base as Record<string, unknown>
  const d = delta as Record<string, unknown>
  const out: Record<string, unknown> = { ...b }
  for (const k of Object.keys(d)) {
    if (!(k in b)) continue
    out[k] = shareRehydrate(b[k], d[k])
  }
  return out
}

// ---------------------------------------------------------------------------
// FNV-1a 32 位（公开算法，公有领域）
// ---------------------------------------------------------------------------
function fnv1a32(str: string): number {
  let h = 0x811c9dc5
  for (let i = 0; i < str.length; i++) {
    h ^= str.charCodeAt(i)
    h = Math.imul(h, 0x01000193)
  }
  return h >>> 0
}

function checksumOf(version: number, json: string): string {
  return fnv1a32(version + ':' + json).toString(16).padStart(8, '0')
}

// ---------------------------------------------------------------------------
// 解码：白名单字段 + 数值 clamp
// ---------------------------------------------------------------------------
type Raw = Record<string, unknown>

function isObj(v: unknown): v is Raw {
  return typeof v === 'object' && v !== null && !Array.isArray(v)
}

/** 数值 clamp：非有限数/类型不符 → 默认值；越界 → 钳到 [min,max] */
function num(v: unknown, min: number, max: number, def: number): number {
  if (typeof v !== 'number' || !Number.isFinite(v)) return def
  return v < min ? min : v > max ? max : v
}

function bool(v: unknown, def: boolean): boolean {
  return typeof v === 'boolean' ? v : def
}

/** 字符串白名单：仅接受 string（防注入），超长截断 */
function str(v: unknown, def: string | null, maxLen: number): string | null {
  if (v === null || v === undefined) return def
  if (typeof v !== 'string') return def
  return v.length > maxLen ? v.slice(0, maxLen) : v
}

/** 字符串枚举白名单 */
function oneOf<T extends string>(v: unknown, allowed: readonly T[], def: T): T {
  return typeof v === 'string' && (allowed as readonly string[]).includes(v) ? (v as T) : def
}

/** 数字枚举白名单（bandCount / direction） */
function numOneOf<T extends number>(v: unknown, allowed: readonly T[], def: T): T {
  return typeof v === 'number' && (allowed as readonly number[]).includes(v) ? (v as T) : def
}

/** 数值数组（clamp 后截断到 maxLen）；缺失时返回默认值副本 */
function numArray(v: unknown, min: number, max: number, def: number[], maxLen: number): number[] {
  if (!Array.isArray(v)) return def.slice()
  const out: number[] = []
  for (const x of v) {
    if (out.length >= maxLen) break
    if (typeof x === 'number' && Number.isFinite(x)) out.push(x < min ? min : x > max ? max : x)
  }
  return out
}

/** 专业 10 段默认频点（与 src/types.ts PRO_EQ_DEFAULT_BANDS 一致） */
function defaultEqProBands(): EqBand[] {
  return [31.5, 63, 125, 250, 500, 1000, 2000, 4000, 8000, 16000].map((f) => ({
    frequency: f,
    gain: 0,
    q: 1.1,
  }))
}

// ---------------------------------------------------------------------------
// 空间音频分享串编解码：plain-data JSON 块整体往返
// 子结构形状由 spatial/types 定义、由引擎的 spatialConfigFromSettings 消费，
// share-code 层不做逐字段白名单（避免与 spatial/types 双份维护漂移）——编码端原样
// 深拷贝为 plain object，解码端递归过滤只保留 boolean/有限 number/string/plain array/
// plain object（防 prototype 污染 + 剔除函数/Symbol/循环引用等异常 payload）。
// ---------------------------------------------------------------------------

/** 编码：SpatialSettings → plain object 深拷贝（剔除 undefined 字段，保 JSON 可序列化） */
function encodeSpatial(s: SpatialSettings): unknown {
  return JSON.parse(JSON.stringify(s))
}

/** 递归深度上限（防恶意深度嵌套栈溢出） */
const SPATIAL_MAX_DEPTH = 12
/** 单对象键数上限（防超大 payload） */
const SPATIAL_MAX_KEYS = 256

/**
 * 解码端深度清洗：仅保留 boolean/有限 number/string/plain array/plain object，
 * 递归限深 SPATIAL_MAX_DEPTH、单对象键数限 SPATIAL_MAX_KEYS。任何非法/超限 → 返回 null
 * （调用方回退 createDefaultSpatialSettings）。纯函数、无原型污染（不触发 setter、
 * 不用 eval、不引用 __proto__/constructor 等危险键——全部丢弃）。
 */
function deepSanitizeSpatial(v: unknown, depth: number): unknown {
  if (depth > SPATIAL_MAX_DEPTH) return null
  if (v === null || v === undefined) return null
  if (typeof v === 'boolean') return v
  if (typeof v === 'string') return v
  if (typeof v === 'number') return Number.isFinite(v) ? v : null
  if (Array.isArray(v)) {
    if (v.length > SPATIAL_MAX_KEYS) return null
    const out: unknown[] = []
    for (const item of v) {
      const cleaned = deepSanitizeSpatial(item, depth + 1)
      if (cleaned === null) return null // 子项非法 → 整体拒绝（保守，防半残结构）
      out.push(cleaned)
    }
    return out
  }
  if (typeof v === 'object') {
    const obj = v as Record<string, unknown>
    const keys = Object.keys(obj)
    if (keys.length > SPATIAL_MAX_KEYS) return null
    const out: Record<string, unknown> = {}
    for (const k of keys) {
      // 危险键一律丢弃（防原型污染）
      if (k === '__proto__' || k === 'constructor' || k === 'prototype') continue
      const cleaned = deepSanitizeSpatial(obj[k], depth + 1)
      if (cleaned === null) return null
      out[k] = cleaned
    }
    return out
  }
  return null
}

/**
 * 解码 SpatialSettings：raw 为对象 → 深度清洗 → 期望形状校验（关键字段 mode 在白名单内，
 * 子对象存在性）→ 强类型断言；非法/缺省 → createDefaultSpatialSettings()（mode:'off'）。
 * 形状校验保持宽松（仅 mode 白名单 + 各子对象为对象），完整字段有效性由引擎消费时
 * 的 spatialConfigFromSettings 兜底（布局/场景助手自带钳位）。
 */
function decodeSpatial(raw: unknown): SpatialSettings {
  const cleaned = deepSanitizeSpatial(raw, 0)
  if (!cleaned || typeof cleaned !== 'object' || Array.isArray(cleaned)) {
    return createDefaultSpatialSettings()
  }
  const o = cleaned as Record<string, unknown>
  const mode = oneOf(
    o.mode,
    ['off', 'instant', 'headLocked', 'world', 'stage'] as const,
    'off',
  )
  // 关键子对象存在性校验：缺任一 → 默认（防半残结构进入引擎）
  const sub = ['instant', 'headLocked', 'world', 'stage', 'ambience']
  for (const k of sub) {
    if (!isObj(o[k])) return createDefaultSpatialSettings()
  }
  // 以默认为骨架、清洗后的值覆盖（保证新增子字段有默认；旧分享串缺字段不崩）
  const def = createDefaultSpatialSettings()
  return {
    mode,
    masterGain: typeof o.masterGain === 'number' && Number.isFinite(o.masterGain)
      ? o.masterGain
      : def.masterGain,
    instant: { ...def.instant, ...(o.instant as object) },
    headLocked: { ...def.headLocked, ...(o.headLocked as object) },
    world: { ...def.world, ...(o.world as object) },
    stage: { ...def.stage, ...(o.stage as object) },
    ambience: { ...def.ambience, ...(o.ambience as object) },
    convolution: oneOf(o.convolution, ['partitioned', 'time'] as const, def.convolution),
    hrtfInterp: oneOf(o.hrtfInterp, ['nearest', 'spherical'] as const, def.hrtfInterp),
    distanceModel: oneOf(o.distanceModel, ['inverse', 'linear', 'exponential'] as const, def.distanceModel),
    refDistance: num(o.refDistance, 0.1, 100, def.refDistance),
    maxDistance: num(o.maxDistance, 1, 200, def.maxDistance),
  }
}

/** 白名单重建：只读取已知字段，未知字段（含 __proto__ 等注入键）一律丢弃 */
function sanitizeParams(raw: unknown): HyperSoundEngineParams {
  if (!isObj(raw)) throw new Error('invalid share code payload')
  const sampleRate = Math.round(num(raw.sampleRate, 8000, 192000, 48000))

  const eqRaw = isObj(raw.eq) ? raw.eq : {}
  const deesserRaw = isObj(raw.deesser) ? raw.deesser : {}
  const compRaw = isObj(raw.compressor) ? raw.compressor : {}
  const nightRaw = isObj(raw.nightMode) ? raw.nightMode : {}
  const bassRaw = isObj(raw.bassEnhancer) ? raw.bassEnhancer : {}
  const revRaw = isObj(raw.reverb) ? raw.reverb : {}
  const revAlgRaw = isObj(revRaw.algorithmic) ? revRaw.algorithmic : {}
  const revConvRaw = isObj(revRaw.convolution) ? revRaw.convolution : {}
  const surRaw = isObj(raw.surround3d) ? raw.surround3d : {}
  const lcRaw = isObj(raw.loudnessCompensation) ? raw.loudnessCompensation : {}
  const lnRaw = isObj(raw.loudnessNormalization) ? raw.loudnessNormalization : {}
  const limRaw = isObj(raw.limiter) ? raw.limiter : {}
  const ieqRaw = isObj(raw.ieq) ? raw.ieq : {}
  const dynamicEqRaw = isObj(raw.dynamicEq) ? raw.dynamicEq : {}
  const pitchRaw = isObj(raw.pitch) ? raw.pitch : {}
  const modRaw = isObj(raw.modulation) ? raw.modulation : {}
  const modLfoRaw = isObj(modRaw.lfo) ? modRaw.lfo : {}
  const modEnvRaw = isObj(modRaw.envelope) ? modRaw.envelope : {}
  const modRoutesRaw = Array.isArray(modRaw.routes) ? modRaw.routes : []
  const modFxRaw = isObj(raw.modEffects) ? raw.modEffects : {}
  const delayRaw = isObj(modFxRaw.delay) ? modFxRaw.delay : {}
  const chorusRaw = isObj(modFxRaw.chorus) ? modFxRaw.chorus : {}
  const flangerRaw = isObj(modFxRaw.flanger) ? modFxRaw.flanger : {}
  const phaserRaw = isObj(modFxRaw.phaser) ? modFxRaw.phaser : {}
  const tremoloRaw = isObj(modFxRaw.tremolo) ? modFxRaw.tremolo : {}
  const hearingRaw = isObj(raw.hearing) ? raw.hearing : {}

  // eq.simpleBands：5 段，缺失补 0
  const simple = numArray(eqRaw.simpleBands, -20, 20, [0, 0, 0, 0, 0], 5)
  while (simple.length < 5) simple.push(0)

  // eq.proBands：白名单 {frequency,gain,q}；字段缺失 → 默认 10 段；显式空数组 → 保留空
  let proBands: EqBand[] = defaultEqProBands()
  if (Array.isArray(eqRaw.proBands)) {
    const parsed: EqBand[] = []
    for (const b of eqRaw.proBands) {
      if (parsed.length >= 20) break
      if (isObj(b)) {
        parsed.push({
          frequency: num(b.frequency, 20, 20000, 1000),
          gain: num(b.gain, -20, 20, 0),
          q: num(b.q, 0.1, 10, 1.1),
        })
      }
    }
    proBands = parsed
  }

  // loudnessCompensation.bands：白名单 {frequency,gain}，最多 32 段
  const lcBands: { frequency: number; gain: number }[] = []
  if (Array.isArray(lcRaw.bands)) {
    for (const b of lcRaw.bands) {
      if (lcBands.length >= 32) break
      if (isObj(b)) {
        lcBands.push({ frequency: num(b.frequency, 20, 20000, 1000), gain: num(b.gain, -20, 20, 0) })
      }
    }
  }

  // modulation.routes：白名单路由，最多 16 条
  const modRoutes: ModulationRoute[] = []
  for (const r of modRoutesRaw) {
    if (modRoutes.length >= 16) break
    if (!isObj(r)) continue
    const source = oneOf(r.source, ['lfo', 'envelope'] as const, 'lfo')
    const target = oneOf(r.target, ['masterGain', 'stereoWidth'] as const, 'masterGain')
    modRoutes.push({
      source,
      target,
      amount: num(r.amount, -1, 1, 0),
      offset: num(r.offset, -2, 2, 0),
    })
  }

  return {
    sampleRate,
    eq: {
      enabled: bool(eqRaw.enabled, true),
      mode: oneOf(eqRaw.mode, ['simple', 'pro'] as const, 'pro'),
      simpleBands: simple,
      proBands,
      bandCount: numOneOf(eqRaw.bandCount, [10, 20] as const, 10),
      qCompensation: bool(eqRaw.qCompensation, true),
      locked: bool(eqRaw.locked, false),
    },
    deesser: {
      enabled: bool(deesserRaw.enabled, false),
      centerHz: num(deesserRaw.centerHz, 100, 16000, 6000),
      q: num(deesserRaw.q, 0.1, 10, 0.7),
      thresholdDb: num(deesserRaw.thresholdDb, -60, 0, -30),
      ratio: num(deesserRaw.ratio, 1, 50, 8),
      attackMs: num(deesserRaw.attackMs, 0, 100, 1),
      releaseMs: num(deesserRaw.releaseMs, 0, 2000, 80),
      splitBand: bool(deesserRaw.splitBand, true),
      mix: num(deesserRaw.mix, 0, 1, 1),
      sidechainEnabled: bool(deesserRaw.sidechainEnabled, false),
    },
    compressor: {
      enabled: bool(compRaw.enabled, false),
      thresholdDb: num(compRaw.thresholdDb, -60, 0, -20),
      ratio: num(compRaw.ratio, 1, 50, 4),
      kneeDb: num(compRaw.kneeDb, 0, 24, 6),
      attackMs: num(compRaw.attackMs, 0, 500, 10),
      releaseMs: num(compRaw.releaseMs, 0, 3000, 150),
      makeupDb: num(compRaw.makeupDb, -24, 24, 0),
      outputGain: num(compRaw.outputGain, 0, 2, 1),
      sidechainEnabled: bool(compRaw.sidechainEnabled, false),
    },
    nightMode: {
      enabled: bool(nightRaw.enabled, false),
      amount: num(nightRaw.amount, 0, 10, 0),
    },
    bassEnhancer: {
      enabled: bool(bassRaw.enabled, false),
      cutoffHz: num(bassRaw.cutoffHz, 20, 500, 90),
      q: num(bassRaw.q, 0.1, 10, 0.7),
      harmonicType: oneOf(bassRaw.harmonicType, ['odd', 'even', 'atan', 'soft'] as const, 'odd'),
      harmonicGain: num(bassRaw.harmonicGain, 0, 1, 0.6),
      mix: num(bassRaw.mix, 0, 1, 0.5),
      levelDb: num(bassRaw.levelDb, -6, 6, 0),
      lowBoostDb: num(bassRaw.lowBoostDb, -6, 12, 0),
    },
    reverb: {
      enabled: bool(revRaw.enabled, false),
      mode: oneOf(revRaw.mode, ['convolution', 'algorithmic', 'fdn', 'off'] as const, 'algorithmic'),
      algorithmic: {
        type: oneOf(revAlgRaw.type, ['hall', 'room', 'plate', 'spring', 'stage'] as const, 'hall'),
        roomSize: num(revAlgRaw.roomSize, 0, 1, 0.5),
        damping: num(revAlgRaw.damping, 0, 1, 0.5),
        wet: num(revAlgRaw.wet, 0, 1, 0.3),
        dry: num(revAlgRaw.dry, 0, 1, 0.7),
        preDelayMs: num(revAlgRaw.preDelayMs, 0, 500, 0),
        width: num(revAlgRaw.width, 0, 2, 1),
      },
      convolution: {
        ir: null, // IR 数组不进入分享串；解码后恒为 null，由调用方按 irName 重新加载
        irName: str(revConvRaw.irName, null, 256),
        mix: num(revConvRaw.mix, 0, 1, 0.3),
        preDelayMs: num(revConvRaw.preDelayMs, 0, 500, 0),
        dePeriodize: bool(revConvRaw.dePeriodize, true),
      },
    },
    surround3d: {
      enabled: bool(surRaw.enabled, false),
      distance: num(surRaw.distance, 0, 10, 0.5),
      speed: num(surRaw.speed, 0, 10, 1),
      angle: num(surRaw.angle, -360, 360, 0),
      direction: numOneOf(surRaw.direction, [1, -1] as const, 1),
    },
    loudnessCompensation: {
      enabled: bool(lcRaw.enabled, false),
      mode: oneOf(lcRaw.mode, ['auto', 'preset', 'custom'] as const, 'auto'),
      preset: oneOf(lcRaw.preset, ['flat', 'bass', 'vocal', 'warm', 'bright', 'night'] as const, 'flat'),
      bands: lcBands,
      volumePercent: num(lcRaw.volumePercent, 0, 100, 80),
      maxBoostDb: num(lcRaw.maxBoostDb, 0, 24, 12),
      smoothingSeconds: num(lcRaw.smoothingSeconds, 0.01, 10, 0.2),
    },
    loudnessNormalization: {
      enabled: bool(lnRaw.enabled, false),
      targetLufs: num(lnRaw.targetLufs, -40, 0, -14),
      maxGainDb: num(lnRaw.maxGainDb, 0, 24, 9),
      minGainDb: num(lnRaw.minGainDb, -24, 0, -9),
      useRealtimeMeter: bool(lnRaw.useRealtimeMeter, true),
      externalGainDb: num(lnRaw.externalGainDb, -24, 24, 0),
    },
    limiter: {
      enabled: bool(limRaw.enabled, true),
      thresholdDb: num(limRaw.thresholdDb, -60, 0, -1),
      lookaheadMs: num(limRaw.lookaheadMs, 0, 50, 5),
      attackMs: num(limRaw.attackMs, 0, 50, 0.5),
      releaseMs: num(limRaw.releaseMs, 0, 2000, 150),
      truePeak: bool(limRaw.truePeak, true),
    },
    ieq: {
      enabled: bool(ieqRaw.enabled, false),
      strength: num(ieqRaw.strength, 0, 1, 0.5),
      targetCurve: oneOf(ieqRaw.targetCurve, ['flat', 'warm', 'bright', 'vocal'] as const, 'flat'),
      timeConstantSec: num(ieqRaw.timeConstantSec, 0.1, 10, 3),
    },
    dynamicEq: {
      enabled: bool(dynamicEqRaw.enabled, false),
      strength: num(dynamicEqRaw.strength, 0, 1, 0.5),
      thresholdDb: num(dynamicEqRaw.thresholdDb, -80, 0, -20),
      ratio: num(dynamicEqRaw.ratio, 1, 20, 2),
      attackMs: num(dynamicEqRaw.attackMs, 1, 500, 20),
      releaseMs: num(dynamicEqRaw.releaseMs, 10, 2000, 200),
      bands: Array.isArray(dynamicEqRaw.bands)
        ? (dynamicEqRaw.bands as Array<{ enabled?: unknown; targetGainDb?: unknown }>).slice(0, 5).map((b) => ({
            enabled: bool(b?.enabled, true),
            targetGainDb: num(b?.targetGainDb, -12, 12, 0),
          }))
        : [0, 1, 2, 3, 4].map(() => ({ enabled: true, targetGainDb: 0 })),
    },
    pitch: {
      enabled: bool(pitchRaw.enabled, false),
      semitones: num(pitchRaw.semitones, -10, 10, 0),
      rate: num(pitchRaw.rate, 0.25, 3, 1),
      voiceBalance: num(pitchRaw.voiceBalance, -1, 1, 0),
    },
    modulation: {
      enabled: bool(modRaw.enabled, false),
      lfo: {
        enabled: bool(modLfoRaw.enabled, false),
        shape: oneOf(modLfoRaw.shape, ['sine', 'triangle', 'square', 'saw'] as const, 'sine'),
        rateHz: num(modLfoRaw.rateHz, 0.01, 20, 1),
        depth: num(modLfoRaw.depth, 0, 1, 0.5),
      },
      envelope: {
        enabled: bool(modEnvRaw.enabled, false),
        attackMs: num(modEnvRaw.attackMs, 0.1, 1000, 10),
        releaseMs: num(modEnvRaw.releaseMs, 0.1, 5000, 200),
        amount: num(modEnvRaw.amount, 0, 1, 0.5),
      },
      routes: modRoutes,
    },
    modEffects: {
      delay: {
        enabled: bool(delayRaw.enabled, false),
        delayMs: num(delayRaw.delayMs, 0, 2000, 250),
        feedback: num(delayRaw.feedback, 0, 0.98, 0.3),
        mix: num(delayRaw.mix, 0, 1, 0.3),
      },
      chorus: {
        enabled: bool(chorusRaw.enabled, false),
        rateHz: num(chorusRaw.rateHz, 0.01, 20, 1),
        depthMs: num(chorusRaw.depthMs, 0, 50, 3),
        mix: num(chorusRaw.mix, 0, 1, 0.4),
      },
      flanger: {
        enabled: bool(flangerRaw.enabled, false),
        rateHz: num(flangerRaw.rateHz, 0.01, 20, 0.5),
        depthMs: num(flangerRaw.depthMs, 0, 50, 2),
        feedback: num(flangerRaw.feedback, 0, 0.98, 0.4),
        mix: num(flangerRaw.mix, 0, 1, 0.5),
      },
      phaser: {
        enabled: bool(phaserRaw.enabled, false),
        rateHz: num(phaserRaw.rateHz, 0.01, 20, 0.5),
        depth: num(phaserRaw.depth, 0, 1, 0.5),
        feedback: num(phaserRaw.feedback, 0, 0.98, 0.4),
        mix: num(phaserRaw.mix, 0, 1, 0.5),
        stages: num(phaserRaw.stages, 2, 8, 4),
      },
      tremolo: {
        enabled: bool(tremoloRaw.enabled, false),
        rateHz: num(tremoloRaw.rateHz, 0.01, 30, 5),
        depth: num(tremoloRaw.depth, 0, 1, 0.5),
        mix: num(tremoloRaw.mix, 0, 1, 1),
      },
    },
    hearing: {
      enabled: bool(hearingRaw.enabled, false),
    },
    // 空间音频：raw 缺 spatial 字段 → decodeSpatial 返回默认 off；
    // 旧分享串（无 spatial）往返得默认 off，行为与历史一致
    spatial: decodeSpatial(raw.spatial),
    stereoWidth: num(raw.stereoWidth, 0, 2, 1),
    sceneId: str(raw.sceneId, null, 64),
    customized: bool(raw.customized, false),
  }
}

// ---------------------------------------------------------------------------
// 编码：固定字段顺序构造可序列化快照（去 IR 数组 → 仅 irName）
// ---------------------------------------------------------------------------
function toShareObject(p: HyperSoundEngineParams): unknown {
  return {
    sampleRate: p.sampleRate,
    eq: {
      enabled: p.eq.enabled,
      mode: p.eq.mode,
      simpleBands: p.eq.simpleBands.slice(0, 5),
      proBands: p.eq.proBands.slice(0, 20).map((b) => ({ frequency: b.frequency, gain: b.gain, q: b.q })),
      bandCount: p.eq.bandCount,
      qCompensation: p.eq.qCompensation,
      locked: p.eq.locked,
    },
    deesser: {
      enabled: p.deesser.enabled,
      centerHz: p.deesser.centerHz,
      q: p.deesser.q,
      thresholdDb: p.deesser.thresholdDb,
      ratio: p.deesser.ratio,
      attackMs: p.deesser.attackMs,
      releaseMs: p.deesser.releaseMs,
      splitBand: p.deesser.splitBand,
      mix: p.deesser.mix,
      sidechainEnabled: p.deesser.sidechainEnabled ?? false,
    },
    compressor: {
      enabled: p.compressor.enabled,
      thresholdDb: p.compressor.thresholdDb,
      ratio: p.compressor.ratio,
      kneeDb: p.compressor.kneeDb,
      attackMs: p.compressor.attackMs,
      releaseMs: p.compressor.releaseMs,
      makeupDb: p.compressor.makeupDb,
      outputGain: p.compressor.outputGain,
      sidechainEnabled: p.compressor.sidechainEnabled ?? false,
    },
    nightMode: {
      enabled: p.nightMode.enabled,
      amount: p.nightMode.amount,
    },
    bassEnhancer: {
      enabled: p.bassEnhancer.enabled,
      cutoffHz: p.bassEnhancer.cutoffHz,
      q: p.bassEnhancer.q,
      harmonicType: p.bassEnhancer.harmonicType,
      harmonicGain: p.bassEnhancer.harmonicGain,
      mix: p.bassEnhancer.mix,
      levelDb: p.bassEnhancer.levelDb,
      lowBoostDb: p.bassEnhancer.lowBoostDb ?? 0,
    },
    reverb: {
      enabled: p.reverb.enabled,
      mode: p.reverb.mode,
      algorithmic: {
        type: p.reverb.algorithmic.type,
        roomSize: p.reverb.algorithmic.roomSize,
        damping: p.reverb.algorithmic.damping,
        wet: p.reverb.algorithmic.wet,
        dry: p.reverb.algorithmic.dry,
        preDelayMs: p.reverb.algorithmic.preDelayMs,
        width: p.reverb.algorithmic.width,
      },
      // 去 IR 数组：卷积 IR 只保留 irName 引用，ir 不参与序列化
      convolution: {
        irName: p.reverb.convolution.irName,
        mix: p.reverb.convolution.mix,
        preDelayMs: p.reverb.convolution.preDelayMs,
        dePeriodize: p.reverb.convolution.dePeriodize,
      },
    },
    surround3d: {
      enabled: p.surround3d.enabled,
      distance: p.surround3d.distance,
      speed: p.surround3d.speed,
      angle: p.surround3d.angle,
      direction: p.surround3d.direction,
    },
    loudnessCompensation: {
      enabled: p.loudnessCompensation.enabled,
      mode: p.loudnessCompensation.mode,
      preset: p.loudnessCompensation.preset,
      bands: p.loudnessCompensation.bands.slice(0, 32).map((b) => ({ frequency: b.frequency, gain: b.gain })),
      volumePercent: p.loudnessCompensation.volumePercent,
      maxBoostDb: p.loudnessCompensation.maxBoostDb,
      smoothingSeconds: p.loudnessCompensation.smoothingSeconds,
    },
    loudnessNormalization: {
      enabled: p.loudnessNormalization.enabled,
      targetLufs: p.loudnessNormalization.targetLufs,
      maxGainDb: p.loudnessNormalization.maxGainDb,
      minGainDb: p.loudnessNormalization.minGainDb,
      useRealtimeMeter: p.loudnessNormalization.useRealtimeMeter,
      externalGainDb: p.loudnessNormalization.externalGainDb,
    },
    limiter: {
      enabled: p.limiter.enabled,
      thresholdDb: p.limiter.thresholdDb,
      lookaheadMs: p.limiter.lookaheadMs,
      attackMs: p.limiter.attackMs,
      releaseMs: p.limiter.releaseMs,
      truePeak: p.limiter.truePeak,
    },
    ieq: {
      enabled: p.ieq.enabled,
      strength: p.ieq.strength,
      targetCurve: p.ieq.targetCurve,
      timeConstantSec: p.ieq.timeConstantSec,
    },
    dynamicEq: {
      enabled: p.dynamicEq.enabled,
      strength: p.dynamicEq.strength,
      thresholdDb: p.dynamicEq.thresholdDb,
      ratio: p.dynamicEq.ratio,
      attackMs: p.dynamicEq.attackMs,
      releaseMs: p.dynamicEq.releaseMs,
      bands: p.dynamicEq.bands.map((b) => ({ enabled: b.enabled, targetGainDb: b.targetGainDb })),
    },
    pitch: {
      enabled: p.pitch.enabled,
      semitones: p.pitch.semitones,
      rate: p.pitch.rate,
      voiceBalance: p.pitch.voiceBalance,
    },
    modulation: {
      enabled: p.modulation.enabled,
      lfo: {
        enabled: p.modulation.lfo.enabled,
        shape: p.modulation.lfo.shape,
        rateHz: p.modulation.lfo.rateHz,
        depth: p.modulation.lfo.depth,
      },
      envelope: {
        enabled: p.modulation.envelope.enabled,
        attackMs: p.modulation.envelope.attackMs,
        releaseMs: p.modulation.envelope.releaseMs,
        amount: p.modulation.envelope.amount,
      },
      routes: p.modulation.routes.map((r) => ({ ...r })),
    },
    modEffects: {
      delay: { ...p.modEffects.delay },
      chorus: { ...p.modEffects.chorus },
      flanger: { ...p.modEffects.flanger },
      phaser: { ...p.modEffects.phaser },
      tremolo: { ...p.modEffects.tremolo },
    },
    hearing: {
      enabled: p.hearing.enabled,
    },
    // 空间音频：作为清洗后的 JSON 块整体序列化（子结构形状由 spatial/types 定义，
    // 不做逐字段白名单避免双份维护漂移）；缺省/undefined 时不写入该字段
    spatial: p.spatial ? encodeSpatial(p.spatial) : undefined,
    stereoWidth: p.stereoWidth,
    sceneId: p.sceneId,
    customized: p.customized,
  }
}

// ---------------------------------------------------------------------------
// 公开 API
// ---------------------------------------------------------------------------

/** 序列化（v2）：默认参数差异 JSON + FNV-1a 校验 → Crockford Base32 分组串（HSE2- 前缀）。
 *  sampleRate 强制携带（还原骨架的采样率随发送方，解码端据此重建默认骨架）。 */
export function encodeShareCode(p: HyperSoundEngineParams): string {
  const full = toShareObject(p) as Record<string, unknown>
  const base = toShareObject(createDefaultParams(p.sampleRate)) as Record<string, unknown>
  const delta = { ...(shareDelta(base, full) as Record<string, unknown> | undefined), sampleRate: full.sampleRate }
  const json = JSON.stringify(delta)
  const payload = SHARE_CODEC_VERSION + ':' + checksumOf(SHARE_CODEC_VERSION, json) + ':' + json
  return SHARE_CODE_PREFIX + '-' + groupCode(bytesToBase32Crockford(utf8Encode(payload)))
}

/** 反序列化：HSE2（v2 差异载荷）与 v1 旧串（base64url 全量载荷）双路全收；
 *  版本/校验和验证 + 白名单字段 + 数值 clamp；非法输入抛 Error */
export function decodeShareCode(s: string): HyperSoundEngineParams {
  if (typeof s !== 'string' || s.length === 0) {
    throw new Error('invalid share code: empty input')
  }
  const trimmed = s.trim()
  let text: string
  if (trimmed.length >= 4 && trimmed.slice(0, 4).toUpperCase() === SHARE_CODE_PREFIX) {
    // v2 传输：HSE2- 分组 Crockford（剥前缀与全部分隔符/空白）
    text = utf8Decode(base32CrockfordToBytes(trimmed.slice(4).replace(/[-\s]/g, '')))
  } else {
    // v1 传输：base64url
    try {
      text = utf8Decode(base64UrlToBytes(trimmed))
    } catch (e) {
      throw new Error('invalid share code: ' + (e instanceof Error ? e.message : 'bad base64url'))
    }
  }
  const firstColon = text.indexOf(':')
  if (firstColon <= 0) throw new Error('invalid share code: missing version')
  const version = text.slice(0, firstColon)
  // 布局：<version>:<8位校验和>:<json> —— json 起点 = 版本冒号 + 1 + 校验和长度(8) + 1
  const checksum = text.slice(firstColon + 1, firstColon + 9)
  const json = text.slice(firstColon + 10)
  if (version !== String(SHARE_CODEC_VERSION) && version !== String(SHARE_CODEC_LEGACY_VERSION)) {
    throw new Error('unsupported share code version: ' + version)
  }
  if (!/^[0-9a-f]{8}$/.test(checksum)) {
    throw new Error('invalid share code: bad checksum format')
  }
  if (checksumOf(Number(version), json) !== checksum) {
    throw new Error('share code checksum mismatch')
  }
  let raw: unknown
  try {
    raw = JSON.parse(json)
  } catch {
    throw new Error('invalid share code: malformed JSON')
  }
  if (version === String(SHARE_CODEC_LEGACY_VERSION)) {
    return sanitizeParams(raw) // v1：全量参数快照
  }
  // v2：差异载荷 → 以默认参数（发送方采样率）为骨架还原后走同一套白名单清洗
  // （v2 载荷强制携带 sampleRate，见 encodeShareCode；骨架采样率仅是缺省兜底，
  //   越界/缺失由 sanitizeParams 的白名单 clamp 最终兜底）
  const rawRate = raw !== null && typeof raw === 'object' && !Array.isArray(raw)
    ? (raw as Record<string, unknown>).sampleRate
    : undefined
  const sampleRate = typeof rawRate === 'number' && Number.isFinite(rawRate) ? rawRate : 48000
  const skeleton = toShareObject(createDefaultParams(sampleRate))
  return sanitizeParams(shareRehydrate(skeleton, raw))
}