#!/usr/bin/env node
/**
 * export-vectors.mjs —— Phase 0「向量基建」DSP 对拍向量导出工具
 *
 * 用法：node scripts/export-vectors.mjs
 *
 * 职责：
 *  - 以 TS 支线（src/）为行为事实标准，为 biquad / limiter / reverb-simple 三个模块
 *    导出确定性对拍向量到 specs/dsp/vectors/<module>.<case>.json 与同名 .f32；
 *  - 向量格式契约（两支线共享，见 specs/ 目录规划文档）：
 *      JSON：schemaVersion=1 / module / case / sampleRate / blockSize / channels=2 /
 *            frames / params（与模块 setParams 实际消费的字段完全一致）/ tolerance /
 *            notes（可选）；
 *      f32 ：小端、非交错 [输入左][输入右][期望输出左][期望输出右]，各 frames 帧；
 *      语义：输入按 blockSize 顺序分块调用模块处理（末块可短），状态跨块保持，
 *            期望输出 = 逐块输出按序拼接；
 *      容差：|got-want| <= value * max(|want|, floor)。
 *
 * 模块加载方案：优先 Node 原生 type-stripping 直接 import src/*.ts（三个模块仅含
 * 可擦除语法且运行时零相对导入，Node >=23.6 默认支持）；若失败则回退用 devDependencies
 * 中已有的 esbuild 把所需模块打包成临时 mjs 再动态导入。不新增任何依赖。
 *
 * 冻结纪律：向量一旦生成即为冻结基线。本脚本重复运行时逐字节比对既有文件——
 * 内容一致则跳过写入；不一致则直接报错拒绝覆盖（防止任何一方单方面修改基线）。
 * 若确认某行为变更属有意，须人工删除对应向量文件后重新导出，并在变更记录中说明。
 *
 * 确定性：输入只用固定系数正弦叠加 + 固定种子 LCG 伪噪声（禁 Math.random /
 * Date / console 随机源）；同机同版本重复导出字节级一致。
 */

import { mkdirSync, readFileSync, writeFileSync, existsSync, mkdtempSync } from 'node:fs'
import { rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const srcDir = path.join(repoRoot, 'src', 'dsp')
const outDir = path.join(repoRoot, 'specs', 'dsp', 'vectors')

const SCHEMA_VERSION = 1
const CHANNELS = 2
const TOLERANCE = { kind: 'relative', value: 1e-6, floor: 1e-9 }

// ==================== TS 模块加载 ====================

/** 待加载的 TS 模块清单（kebab-case 模块 id → 源文件） */
const MODULE_SOURCES = [
  { id: 'biquad', file: 'biquad.ts' },
  { id: 'limiter', file: 'Limiter.ts' },
  { id: 'reverb-simple', file: 'ReverbSimple.ts' },
]

/**
 * 加载 TS 模块：先尝试 Node 原生 type-stripping 直接 import；
 * 失败则用 esbuild 打包成临时 mjs 再动态导入（临时目录随进程结束清理）。
 */
async function loadDspModules() {
  try {
    const loaded = {}
    for (const m of MODULE_SOURCES) {
      const url = pathToFileURL(path.join(srcDir, m.file)).href
      loaded[m.id] = await import(url)
    }
    return { modules: loaded, strategy: 'node-native-type-stripping' }
  } catch (nativeError) {
    const esbuild = (await import('esbuild')).default
    const tmp = mkdtempSync(path.join(tmpdir(), 'hse-export-vectors-'))
    try {
      const loaded = {}
      for (const m of MODULE_SOURCES) {
        const outfile = path.join(tmp, m.id + '.mjs')
        await esbuild.build({
          entryPoints: [path.join(srcDir, m.file)],
          bundle: true,
          format: 'esm',
          platform: 'neutral',
          outfile,
          logLevel: 'silent',
        })
        loaded[m.id] = await import(pathToFileURL(outfile).href)
      }
      return { modules: loaded, strategy: 'esbuild-temp-bundle' }
    } finally {
      rmSync(tmp, { recursive: true, force: true })
    }
  }
}

// ==================== 确定性信号生成 ====================

/**
 * 固定种子 LCG 伪噪声（均匀分布近似 -amp..amp）。
 * 与 test/reverbsimple.test.ts 的 lcgNoise 同族参数，保证仓库内一致。
 */
function lcgNoise(frames, seed, amp) {
  const x = new Float32Array(frames)
  let s = seed >>> 0
  for (let i = 0; i < frames; i++) {
    s = (Math.imul(s, 1664525) + 1013904223) >>> 0
    x[i] = ((s / 4294967296) * 2 - 1) * amp
  }
  return x
}

/** 多频正弦叠加：comps = [{ freqHz, amp, phaseRad }]，双精度计算后存入 f32 */
function sineSum(frames, sampleRate, comps) {
  const x = new Float32Array(frames)
  for (let i = 0; i < frames; i++) {
    let acc = 0
    for (let c = 0; c < comps.length; c++) {
      acc += comps[c].amp * Math.sin((2 * Math.PI * comps[c].freqHz * i) / sampleRate + comps[c].phaseRad)
    }
    x[i] = acc
  }
  return x
}

/** 单频正弦（满幅常用形态） */
function sine(frames, sampleRate, freqHz, amp, phaseRad = 0) {
  return sineSum(frames, sampleRate, [{ freqHz, amp, phaseRad }])
}

/** 全零静音 */
function silence(frames) {
  return new Float32Array(frames)
}

/** 阶跃方波：headSilentFrames 帧静音后按 periodFrames 半周期翻转 ±amp */
function squareAfterSilence(frames, headSilentFrames, periodFrames, amp) {
  const x = new Float32Array(frames)
  for (let i = headSilentFrames; i < frames; i++) {
    const half = Math.floor(((i - headSilentFrames) % periodFrames) / (periodFrames / 2))
    x[i] = half === 0 ? amp : -amp
  }
  return x
}

/** 有声段截断：保留前 activeFrames 帧，其余置零（能量突发→衰减尾） */
function burstThenSilence(source, activeFrames) {
  const x = source.slice()
  x.fill(0, activeFrames)
  return x
}

// ==================== case 定义（冻结基线的唯一来源） ====================
// 每个 case 声明：module / case / sampleRate / blockSize / params / notes /
// inputL(frames,fs) 与 inputR(frames,fs)。frames 由输入数组长度决定。

const CASES = [
  // ---------- biquad ----------
  {
    module: 'biquad',
    caseId: 'case1',
    sampleRate: 48000,
    blockSize: 256,
    params: { type: 'peaking', f0: 1000, q: 1.2, gainDb: 4 },
    notes: '典型参数：peaking +4dB@1kHz Q1.2；左=三频正弦叠加(100/1000/7500Hz)，右=固定种子LCG噪声。单声道模块：左右各一个独立 TDF2 实例，系数相同、状态独立跨块保持。',
    inputL: (n, fs) => sineSum(n, fs, [
      { freqHz: 100, amp: 0.4, phaseRad: 0 },
      { freqHz: 1000, amp: 0.3, phaseRad: Math.PI / 3 },
      { freqHz: 7500, amp: 0.2, phaseRad: Math.PI / 5 },
    ]),
    inputR: (n) => lcgNoise(n, 20250101, 0.5),
  },
  {
    module: 'biquad',
    caseId: 'case2',
    sampleRate: 48000,
    blockSize: 128,
    params: { type: 'lowpass', f0: 10, q: 0.707, gainDb: 0 },
    notes: '边界参数：f0=10Hz 触发 designBiquad 频率下限钳制（低频 BLT 病态防护）；左右分别为满幅 1kHz/5kHz 正弦。帧数对 blockSize 非整除（末块 56 帧），验证短块语义。',
    inputL: (n, fs) => sine(n, fs, 1000, 1.0),
    inputR: (n, fs) => sine(n, fs, 5000, 1.0),
  },
  {
    module: 'biquad',
    caseId: 'case3',
    sampleRate: 48000,
    blockSize: 480,
    params: { type: 'notch', f0: 60, q: 8, gainDb: 0 },
    notes: '极端参数：notch 60Hz Q8（市电嗡声抑制场景）；左=LCG 噪声叠加 60Hz 正弦，右=纯 LCG 噪声对照。blockSize=480 非二次幂，末块 200 帧。',
    inputL: (n, fs) => {
      const noise = lcgNoise(n, 777, 0.6)
      const hum = sine(n, fs, 60, 0.3)
      for (let i = 0; i < n; i++) noise[i] = noise[i] + hum[i]
      return noise
    },
    inputR: (n) => lcgNoise(n, 778, 0.6),
  },

  // ---------- limiter ----------
  {
    module: 'limiter',
    caseId: 'case1',
    sampleRate: 48000,
    blockSize: 128,
    params: { enabled: true, thresholdDb: -1, lookaheadMs: 5, attackMs: 0.5, releaseMs: 150, truePeak: true },
    notes: '典型 brickwall 场景：左右同相满幅 3kHz 正弦，阈值 -1dBFS、真峰值检测开启；前瞻 240 样本的预压增益应使输出峰值不超阈值。',
    inputL: (n, fs) => sine(n, fs, 3000, 1.0),
    inputR: (n, fs) => sine(n, fs, 3000, 1.0),
  },
  {
    module: 'limiter',
    caseId: 'case2',
    sampleRate: 48000,
    blockSize: 333,
    params: { enabled: true, thresholdDb: -12, lookaheadMs: 0, attackMs: 1, releaseMs: 50, truePeak: false },
    notes: '边界参数：lookahead=0（无前瞻）、阈值 -12dBFS、数字峰值检测；左声道前 1250 帧静音后 ±1 方波，右声道反相。blockSize=333 且帧数非整除（末块仅 5 帧），重点覆盖极短末块。',
    inputL: (n) => squareAfterSilence(n, 1250, 120, 1.0),
    inputR: (n) => squareAfterSilence(n, 1250, 120, -1.0),
  },
  {
    module: 'limiter',
    caseId: 'case3',
    sampleRate: 48000,
    blockSize: 512,
    params: { enabled: true, thresholdDb: -6, lookaheadMs: 10, attackMs: 0.5, releaseMs: 150, truePeak: true },
    notes: '输入形态：全静音双声道。限幅器开启但输入为零，期望输出全零（增益保持 1、真峰值插值历史无残留），用于捕获任何 NaN/状态泄漏回归。',
    inputL: (n) => silence(n),
    inputR: (n) => silence(n),
  },
  {
    module: 'limiter',
    caseId: 'case4',
    sampleRate: 48000,
    blockSize: 256,
    params: { enabled: false, thresholdDb: -1, lookaheadMs: 5, attackMs: 0.5, releaseMs: 150, truePeak: true },
    notes: '直通契约锚点：enabled=false 时输出必须与输入逐位一致（就地透传、衰减归零）。左=LCG 噪声，右=双频正弦叠加。',
    inputL: (n) => lcgNoise(n, 42, 0.9),
    inputR: (n, fs) => sineSum(n, fs, [
      { freqHz: 220, amp: 0.5, phaseRad: 0 },
      { freqHz: 3300, amp: 0.3, phaseRad: Math.PI / 4 },
    ]),
  },

  // ---------- reverb-simple ----------
  {
    module: 'reverb-simple',
    caseId: 'case1',
    sampleRate: 48000,
    blockSize: 128,
    params: { roomSize: 0.5, damping: 0.5, wet: 0.3, dry: 0.7, preDelayMs: 0, width: 1, type: 'hall' },
    notes: '冲激响应快照：仅左声道首帧冲激、右声道全零（不对称激励验证立体声网络去相关）。hall 典型参数 wet0.3/dry0.7。',
    inputL: (n) => {
      const x = silence(n)
      x[0] = 1
      return x
    },
    inputR: (n) => silence(n),
  },
  {
    module: 'reverb-simple',
    caseId: 'case2',
    sampleRate: 44100,
    blockSize: 99,
    params: { roomSize: 0, damping: 1, wet: 1, dry: 0, preDelayMs: 20, width: 0, type: 'room' },
    notes: '边界参数组合：roomSize=0/damping=1 参数下限、width=0 湿路单声道化（左右湿声一致）、preDelay 20ms；采样率 44100 使延迟长度缩放比为 1。输入前半段 LCG 噪声、后半段静音，覆盖衰减尾与非整除末块（40 帧）。',
    inputL: (n) => burstThenSilence(lcgNoise(n, 9001, 0.7), Math.floor(n / 2)),
    inputR: (n) => burstThenSilence(lcgNoise(n, 9002, 0.7), Math.floor(n / 2)),
  },
  {
    module: 'reverb-simple',
    caseId: 'case3',
    sampleRate: 48000,
    blockSize: 512,
    params: { roomSize: 1, damping: 0, wet: 0.5, dry: 0.5, preDelayMs: 50, width: 2, type: 'stage' },
    notes: '极端参数：roomSize=1/damping=0 上限附近、width=2 超宽立体声（负交叉混合）、stage 类型 delayScale=1.2、preDelay 50ms；输入为左右去相关的双频正弦叠加。',
    inputL: (n, fs) => sineSum(n, fs, [
      { freqHz: 220, amp: 0.5, phaseRad: 0 },
      { freqHz: 3300, amp: 0.25, phaseRad: Math.PI / 4 },
    ]),
    inputR: (n, fs) => sineSum(n, fs, [
      { freqHz: 440, amp: 0.5, phaseRad: Math.PI / 2 },
      { freqHz: 5200, amp: 0.25, phaseRad: (3 * Math.PI) / 4 },
    ]),
  },
]

// ==================== 模块实例化与分块处理 ====================

/**
 * 按 case 实例化模块并返回分块处理函数。
 * 返回 process(l, r) → [outL, outR]：每次调用处理一个块（就地实现模块会先复制输入，
 * 保证原始输入缓冲不被污染），状态由闭包内实例跨调用保持。
 */
function instantiateProcessor(modules, moduleId, sampleRate, params) {
  switch (moduleId) {
    case 'biquad': {
      // biquad 为单声道模块：左右各一个独立实例（相同系数、状态独立），这是
      // EqChain 等上层链路的既定用法，也是本向量固化的立体声扩展语义。
      if (!modules.biquad || !modules.biquad.Biquad) throw new Error('biquad 模块加载失败')
      const left = new modules.biquad.Biquad(params.type, params.f0, params.q, params.gainDb, sampleRate)
      const right = new modules.biquad.Biquad(params.type, params.f0, params.q, params.gainDb, sampleRate)
      return (l, r) => {
        const outL = new Float32Array(l.length)
        const outR = new Float32Array(r.length)
        left.processBlock(l, outL)
        right.processBlock(r, outR)
        return [outL, outR]
      }
    }
    case 'limiter': {
      if (!modules.limiter || !modules.limiter.Limiter) throw new Error('limiter 模块加载失败')
      const limiter = new modules.limiter.Limiter(sampleRate)
      limiter.setParams(params)
      return (l, r) => {
        const outL = l.slice()
        const outR = r.slice()
        limiter.processStereo(outL, outR)
        return [outL, outR]
      }
    }
    case 'reverb-simple': {
      if (!modules['reverb-simple'] || !modules['reverb-simple'].ReverbSimple) throw new Error('reverb-simple 模块加载失败')
      const reverb = new modules['reverb-simple'].ReverbSimple(sampleRate)
      reverb.setParams(params)
      return (l, r) => {
        const outL = l.slice()
        const outR = r.slice()
        reverb.processStereo(outL, outR)
        return [outL, outR]
      }
    }
    default:
      throw new Error('未知模块 id：' + moduleId)
  }
}

/** 按 blockSize 分块跑完整段输入，返回拼接后的期望输出（契约语义） */
function renderChunked(process, inL, inR, blockSize) {
  const frames = inL.length
  const outL = new Float32Array(frames)
  const outR = new Float32Array(frames)
  for (let offset = 0; offset < frames; offset += blockSize) {
    const len = Math.min(blockSize, frames - offset)
    const blockL = inL.subarray(offset, offset + len)
    const blockR = inR.subarray(offset, offset + len)
    const [chunkL, chunkR] = process(blockL, blockR)
    outL.set(chunkL, offset)
    outR.set(chunkR, offset)
  }
  return { outL, outR }
}

// ==================== 序列化 ====================

/** f32 小端四段布局：[输入左][输入右][期望输出左][期望输出右] */
function serializeF32(inL, inR, outL, outR) {
  const frames = inL.length
  const buffer = Buffer.alloc(frames * 4 * CHANNELS * 2)
  const view = new DataView(buffer.buffer, buffer.byteOffset, buffer.byteLength)
  let offset = 0
  for (const seg of [inL, inR, outL, outR]) {
    for (let i = 0; i < seg.length; i++) {
      view.setFloat32(offset, seg[i], true)
      offset += 4
    }
  }
  return buffer
}

/** JSON 固定键序序列化（幂等性的前提之一） */
function serializeJson(caseDef, meta) {
  const doc = {
    schemaVersion: SCHEMA_VERSION,
    module: caseDef.module,
    case: caseDef.caseId,
    sampleRate: caseDef.sampleRate,
    blockSize: caseDef.blockSize,
    channels: CHANNELS,
    frames: meta.frames,
    params: caseDef.params,
    tolerance: TOLERANCE,
    notes: caseDef.notes,
  }
  return Buffer.from(JSON.stringify(doc, null, 2) + '\n', 'utf8')
}

/**
 * 冻结守卫：目标文件已存在时逐字节比对。
 * 一致 → 跳过写入（幂等）；不一致 → 抛错拒写（禁止单方面修改冻结基线）。
 */
function writeFrozen(filePath, content) {
  if (existsSync(filePath)) {
    const existing = readFileSync(filePath)
    if (existing.equals(content)) return 'unchanged'
    throw new Error(
      '冻结基线冲突：' + filePath +
      ' 已存在且与新导出内容不一致。' +
      '向量一旦生成即为两支线共享的冻结基线，禁止由脚本静默改写；' +
      '若该行为变更确属有意，请人工删除该向量文件后重新导出，并在变更记录中说明。'
    )
  }
  mkdirSync(path.dirname(filePath), { recursive: true })
  writeFileSync(filePath, content)
  return 'written'
}

// ==================== 主流程 ====================

async function main() {
  const { modules, strategy } = await loadDspModules()
  console.log('TS 模块加载策略：' + strategy)

  let written = 0
  let unchanged = 0
  for (const caseDef of CASES) {
    // 先以完整长度生成输入（Float32Array 天然完成 f32 量化，
    // 保证门禁侧从 .f32 读回的输入与导出侧喂给模块的输入逐位一致）
    const frames = caseFrames(caseDef)
    const inL = caseDef.inputL(frames, caseDef.sampleRate)
    const inR = caseDef.inputR(frames, caseDef.sampleRate)
    if (inL.length !== frames || inR.length !== frames) {
      throw new Error('case ' + caseDef.module + '.' + caseDef.caseId + ' 输入长度与声明帧数不符')
    }

    const process = instantiateProcessor(modules, caseDef.module, caseDef.sampleRate, caseDef.params)
    const { outL, outR } = renderChunked(process, inL, inR, caseDef.blockSize)

    const baseName = caseDef.module + '.' + caseDef.caseId
    const jsonResult = writeFrozen(path.join(outDir, baseName + '.json'), serializeJson(caseDef, { frames }))
    const f32Result = writeFrozen(path.join(outDir, baseName + '.f32'), serializeF32(inL, inR, outL, outR))
    written += (jsonResult === 'written' ? 1 : 0) + (f32Result === 'written' ? 1 : 0)
    unchanged += (jsonResult === 'unchanged' ? 1 : 0) + (f32Result === 'unchanged' ? 1 : 0)
    console.log('[' + jsonResult.padEnd(9) + '] ' + baseName + '.json   [' + f32Result + '] ' + baseName + '.f32   (' + caseDef.sampleRate + 'Hz / ' + frames + ' 帧 / 块 ' + caseDef.blockSize + ')')
  }
  console.log('完成：新写 ' + written + ' 个文件，字节级一致跳过 ' + unchanged + ' 个，共 ' + CASES.length + ' 个 case。')
}

/** 各 case 的帧数 = 输入生成器自然长度；在此统一声明以便校验 */
function caseFrames(caseDef) {
  // 以左声道生成器在目标长度下的产物为准：这里显式声明每个 case 的帧数，
  // 避免“帧数隐含在生成器里”导致契约字段与数据漂移。
  const declared = FRAME_COUNTS[caseDef.module + '.' + caseDef.caseId]
  if (!declared) throw new Error('缺少帧数声明：' + caseDef.module + '.' + caseDef.caseId)
  return declared
}

const FRAME_COUNTS = {
  'biquad.case1': 4096,
  'biquad.case2': 3000,
  'biquad.case3': 2600,
  'limiter.case1': 9600,
  'limiter.case2': 5000,
  'limiter.case3': 2048,
  'limiter.case4': 4800,
  'reverb-simple.case1': 8192,
  'reverb-simple.case2': 4000,
  'reverb-simple.case3': 6000,
}

main().catch((err) => {
  console.error(String(err && err.message ? err.message : err))
  process.exitCode = 1
})
