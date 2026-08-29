#!/usr/bin/env node
/**
 * export-vectors.mjs —— Phase 0「向量基建」DSP 对拍向量导出工具
 *
 * 用法：node scripts/export-vectors.mjs
 *
 * 职责：
 *  - 以 TS 支线（src/）为行为事实标准，为 biquad / limiter / reverb-simple /
 *    compressor / bass-enhancer / mid-side / eq-chain / fdn-reverb / deesser /
 *    loudness-comp / dynamic-eq / mod-effects 十二个流式立体声模块，以及两个特殊
 *    驱动形态的模块——fft（非流式变换，(L,R)=(Re,Im) 平面，specs/dsp/fft.md §三）与
 *    convolver（IR 配方驱动，specs/dsp/convolver.md §4.2）——
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
 * 模块加载方案：优先 Node 原生 type-stripping 直接 import src/*.ts（Node >=23.6 默认
 * 支持）；bass-enhancer 含运行时相对导入（'./biquad'，无扩展名），原生加载会失败，
 * 此时整体回退用 devDependencies 中已有的 esbuild 把所需模块打包成临时 mjs 再动态
 * 导入。不新增任何依赖。两种策略对同一 TS 源的浮点语义逐位等价，不影响向量字节一致性。
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
  { id: 'compressor', file: 'Compressor.ts' },
  { id: 'bass-enhancer', file: 'BassEnhancer.ts' },
  { id: 'mid-side', file: 'MidSide.ts' },
  { id: 'eq-chain', file: 'EqChain.ts' },
  { id: 'fdn-reverb', file: 'FdnReverb.ts' },
  { id: 'deesser', file: 'Deesser.ts' },
  { id: 'loudness-comp', file: 'LoudnessComp.ts' },
  { id: 'dynamic-eq', file: 'DynamicEq.ts' },
  { id: 'mod-effects', file: 'ModEffects.ts' },
  // 特殊驱动形态（非 StereoProcessor 流式模块，见各自规格的驱动模型章节）：
  { id: 'fft', file: 'fft.ts' },               // 非流式变换：(L,R)=(Re,Im) 平面（specs/dsp/fft.md §三）
  { id: 'convolver', file: 'Convolver.ts' },   // IR 配方驱动（specs/dsp/convolver.md §4.2）
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

/**
 * 合成齿音：4–8kHz 频带限定的确定性带限噪声——9 个固定频点正弦（4000..8000Hz，步进 500Hz）
 * 幅度统一，相位由固定种子 LCG 派生（禁 Math.random，同机同版本逐位可复现）。
 */
function bandNoise(frames, sampleRate, ampPerComp, seed) {
  const comps = []
  let s = seed >>> 0
  for (let k = 0; k < 9; k++) {
    s = (Math.imul(s, 1664525) + 1013904223) >>> 0
    comps.push({ freqHz: 4000 + k * 500, amp: ampPerComp, phaseRad: (s / 4294967296) * 2 * Math.PI })
  }
  return sineSum(frames, sampleRate, comps)
}

/**
 * IR 配方 → 确定性冲激响应（specs/dsp/convolver.md §4.2，两支线逐字一致）。
 * 双精度求值、存入 Float32Array 时一次量化为 f32；LCG 与 lcgNoise 同族（禁 Math.random）。
 * delta    ：length = delay+1，ir[delay] = 1，其余全 0（单点冲激，逐位锚点用）。
 * expNoise ：ir[i] = ((u*2-1)*amp) * exp((-decay*i)/(length-1))，u 为固定种子 LCG
 *            推进后的状态（先推进再取值）；表达式结合序逐字固化，不得重排。
 */
function buildIrRecipe(recipe) {
  if (!recipe || typeof recipe !== 'object') throw new Error('convolver 向量缺少 ir 配方')
  if (recipe.kind === 'delta') {
    const delay = Math.round(recipe.delay)
    if (!(delay >= 0)) throw new Error('delta IR 配方 delay 非法')
    const ir = new Float32Array(delay + 1)
    ir[delay] = 1
    return ir
  }
  if (recipe.kind === 'expNoise') {
    const length = Math.round(recipe.length)
    const seed = recipe.seed >>> 0
    const decay = recipe.decay
    const amp = recipe.amp
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
  {
    module: 'biquad',
    caseId: 'case4',
    sampleRate: 44100,
    blockSize: 441,
    params: { type: 'highshelf', f0: 8000, q: 0.707, gainDb: 3 },
    notes: '多采样率覆盖：采样率 44100，补齐既有三例全为 48000 的缺口；典型音乐性参数：highshelf +3dB@8kHz Q0.707（同时补齐 shelf 类型覆盖）。左=双频正弦叠加(200/6000Hz)，右=固定种子LCG噪声(种子 441001)。blockSize=441 恰为该采样率下 10ms，帧数非整除（末块 31 帧）。',
    inputL: (n, fs) => sineSum(n, fs, [
      { freqHz: 200, amp: 0.5, phaseRad: 0 },
      { freqHz: 6000, amp: 0.3, phaseRad: Math.PI / 6 },
    ]),
    inputR: (n) => lcgNoise(n, 441001, 0.5),
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

  // ---------- compressor ----------
  {
    module: 'compressor',
    caseId: 'case1',
    sampleRate: 48000,
    blockSize: 256,
    params: { enabled: true, thresholdDb: -6, ratio: 4, kneeDb: 6, attackMs: 10, releaseMs: 150, makeupDb: 0, outputGain: 1, sidechainEnabled: false },
    notes: '阈下直通锚点：输入峰值约 -34dBFS，包络稳态低于下膝点（-6-6/2=-9dBFS），压缩量恒 0、增益恰为 1，期望输出与输入逐位一致。左=低幅 LCG 噪声，右=低幅 440Hz 正弦。帧数非整除（末块 192 帧）。',
    inputL: (n) => lcgNoise(n, 31337, 0.02),
    inputR: (n, fs) => sine(n, fs, 440, 0.015),
  },
  {
    module: 'compressor',
    caseId: 'case2',
    sampleRate: 48000,
    blockSize: 384,
    params: { enabled: true, thresholdDb: -20, ratio: 20, kneeDb: 0, attackMs: 5, releaseMs: 100, makeupDb: 6, outputGain: 1, sidechainEnabled: false },
    notes: '重压缩稳态：硬拐点 knee=0、ratio 20、makeup +6dB；左右同频满幅正弦（0.9/1.0）联合包络驱动，稳态输出收敛于阈值+makeup 附近，覆盖 attack 进入段与 release 维持段。帧数恰整除（25 块）。',
    inputL: (n, fs) => sine(n, fs, 1000, 1.0),
    inputR: (n, fs) => sine(n, fs, 1000, 0.9),
  },
  {
    module: 'compressor',
    caseId: 'case3',
    sampleRate: 44100,
    blockSize: 441,
    params: { enabled: true, thresholdDb: -30, ratio: 4, kneeDb: 45, attackMs: 2, releaseMs: 80, makeupDb: 0, outputGain: 1, sidechainEnabled: false },
    notes: '软拐点膝区 + kneeDb 越上界钳制（45 按 40 生效）：输入包络稳态落于膝区内部，压缩量按二次曲线软化。多采样率覆盖：44100 下 attack/release 系数随 fs 变化（压缩器无内部采样率耦合，可安全多率）。blockSize=441 恰为该采样率 10ms，帧数非整除（末块 390 帧）。',
    inputL: (n, fs) => sine(n, fs, 220, 0.12),
    inputR: (n, fs) => sineSum(n, fs, [
      { freqHz: 220, amp: 0.08, phaseRad: 0 },
      { freqHz: 1760, amp: 0.05, phaseRad: Math.PI / 6 },
    ]),
  },
  {
    module: 'compressor',
    caseId: 'case4',
    sampleRate: 48000,
    blockSize: 256,
    params: { enabled: true, thresholdDb: -12, ratio: 8, kneeDb: 6, attackMs: 5, releaseMs: 120, makeupDb: 0, outputGain: 1, sidechainEnabled: true },
    notes: 'sidechain 外部驱动：sidechainEnabled=true，sidechain 取本块原始输入的单声道和（sideL=sideR=inL+inR，双精度派生，见 specs/dsp/compressor.md §4.5）。左右去相关双正弦（800/500Hz）使单声道和包络与内部联合峰值包络显著可区分——驱动器若误用内部包络或错误派生将显著超差。帧数非整除（末块 112 帧）。',
    inputL: (n, fs) => sine(n, fs, 800, 0.9),
    inputR: (n, fs) => sine(n, fs, 500, 0.9),
  },

  // ---------- bass-enhancer ----------
  {
    module: 'bass-enhancer',
    caseId: 'case1',
    sampleRate: 48000,
    blockSize: 256,
    params: { enabled: true, cutoffHz: 90, q: 0.7, harmonicType: 'even', harmonicGain: 0.8, mix: 0.6, levelDb: 0, lowBoostDb: 0 },
    notes: '偶次谐波生成：60Hz 正弦（低于截止 90Hz）经 |x| 全波整流生成偶次谐波，DC 由谐波高通 max(150, 90×1.5)=150Hz 去除。lowBoostDb=0（默认关闭）：下潜项逐位消失，输出与不含低音下潜路径的实现逐位一致。左右同相 60Hz。帧数非整除（末块 112 帧）。',
    inputL: (n, fs) => sine(n, fs, 60, 0.8),
    inputR: (n, fs) => sine(n, fs, 60, 0.8),
  },
  {
    module: 'bass-enhancer',
    caseId: 'case2',
    sampleRate: 48000,
    blockSize: 384,
    params: { enabled: true, cutoffHz: 90, q: 0.7, harmonicType: 'odd', harmonicGain: 0.8, mix: 0.6, levelDb: 0, lowBoostDb: 0 },
    notes: '奇次谐波生成（与 case1 同输入成对对照）：x³ 生成 3 次谐波为主（60Hz→180Hz），谐波高通同 case1。lowBoostDb=0 默认关闭。帧数非整除（末块 240 帧）。',
    inputL: (n, fs) => sine(n, fs, 60, 0.8),
    inputR: (n, fs) => sine(n, fs, 60, 0.8),
  },
  {
    module: 'bass-enhancer',
    caseId: 'case3',
    sampleRate: 48000,
    blockSize: 500,
    params: { enabled: true, cutoffHz: 90, q: 0.7, harmonicType: 'odd', harmonicGain: 0.2, mix: 0.3, levelDb: 0, lowBoostDb: 6 },
    notes: '低音下潜真实能量路径：lowBoostDb=+6 → lowLin=10^0.3-1≈0.995，低通提取的低频带近似翻倍混回；谐波路径低注入（harmonicGain 0.2 / mix 0.3）突出下潜路径。左右共享 55Hz 低音、去相关中频（440/660Hz）。blockSize=500 恰整除（12 块）。',
    inputL: (n, fs) => sineSum(n, fs, [
      { freqHz: 55, amp: 0.6, phaseRad: 0 },
      { freqHz: 440, amp: 0.25, phaseRad: Math.PI / 5 },
    ]),
    inputR: (n, fs) => sineSum(n, fs, [
      { freqHz: 55, amp: 0.6, phaseRad: 0 },
      { freqHz: 660, amp: 0.2, phaseRad: Math.PI / 3 },
    ]),
  },
  {
    module: 'bass-enhancer',
    caseId: 'case4',
    sampleRate: 48000,
    blockSize: 256,
    params: { enabled: true, cutoffHz: 90, q: 0.7, harmonicType: 'atan', harmonicGain: 1, mix: 1, levelDb: 6, lowBoostDb: -20 },
    notes: '极值钳制组合：lowBoostDb=-20 越下界按 -6 生效（lowLin≈-0.499，低频带被衰减）、levelDb=+6 上界、harmonicGain/mix=1 上界（k=mix×harmonicGain×10^0.3≈1.995）；harmonicType=atan ATSR 器件曲线覆盖。左右为 55/60Hz 低音 + 2kHz 高频对照。帧数非整除（末块 192 帧）。',
    inputL: (n, fs) => sineSum(n, fs, [
      { freqHz: 60, amp: 0.7, phaseRad: 0 },
      { freqHz: 2000, amp: 0.15, phaseRad: Math.PI / 4 },
    ]),
    inputR: (n, fs) => sineSum(n, fs, [
      { freqHz: 55, amp: 0.7, phaseRad: 0 },
      { freqHz: 2000, amp: 0.15, phaseRad: Math.PI / 4 },
    ]),
  },

  // ---------- mid-side ----------
  {
    module: 'mid-side',
    caseId: 'case1',
    sampleRate: 48000,
    blockSize: 256,
    params: { width: 2.5, voiceBalance: 0 },
    notes: '宽度展宽 + width 越上界钳制（2.5 按 2 生效）：midGain=1、sideGain=2，侧信号放大、中信号不变；左右去相关双正弦（330/550Hz）提供丰富侧成分。本模块无采样率概念，sampleRate 为契约字段（不传入模块）。帧数恰整除（16 块）。',
    inputL: (n, fs) => sine(n, fs, 330, 0.5),
    inputR: (n, fs) => sine(n, fs, 550, 0.5),
  },
  {
    module: 'mid-side',
    caseId: 'case2',
    sampleRate: 48000,
    blockSize: 333,
    params: { width: 0, voiceBalance: 0 },
    notes: '单声道塌缩：width=0 → sideGain=0，侧信号完全去除，左右输出均为中信号 M（左右逐位相等）。输入与 case1 相同（成对对照）。blockSize=333 非整除（末块 100 帧）。',
    inputL: (n, fs) => sine(n, fs, 330, 0.5),
    inputR: (n, fs) => sine(n, fs, 550, 0.5),
  },
  {
    module: 'mid-side',
    caseId: 'case3',
    sampleRate: 48000,
    blockSize: 256,
    params: { width: 1, voiceBalance: 0.75 },
    notes: '人声路径（voiceBalance>0 侧衰减）：midGain=1、sideGain=1×(1-0.75)=0.25；输入共享 220Hz 中成分（人声语义）+ 去相关高频侧成分（880/1320Hz），中成分保持、侧成分衰减至 1/4。帧数非整除（末块 136 帧）。',
    inputL: (n, fs) => sineSum(n, fs, [
      { freqHz: 220, amp: 0.4, phaseRad: 0 },
      { freqHz: 880, amp: 0.2, phaseRad: Math.PI / 6 },
    ]),
    inputR: (n, fs) => sineSum(n, fs, [
      { freqHz: 220, amp: 0.4, phaseRad: 0 },
      { freqHz: 1320, amp: 0.2, phaseRad: Math.PI / 3 },
    ]),
  },
  {
    module: 'mid-side',
    caseId: 'case4',
    sampleRate: 48000,
    blockSize: 480,
    params: { width: 1, voiceBalance: 0 },
    notes: '恒等锚点：width=1 且 voiceBalance=0，M/S 正逆变换在双精度中间量下精确还原 f32 输入，期望输出与输入逐位一致（跨实现对拍的最强精度锚点，捕获任何 f32 中间量精度纪律偏差）。输入与 case3 相同。帧数非整除（末块 200 帧）。',
    inputL: (n, fs) => sineSum(n, fs, [
      { freqHz: 220, amp: 0.4, phaseRad: 0 },
      { freqHz: 880, amp: 0.2, phaseRad: Math.PI / 6 },
    ]),
    inputR: (n, fs) => sineSum(n, fs, [
      { freqHz: 220, amp: 0.4, phaseRad: 0 },
      { freqHz: 1320, amp: 0.2, phaseRad: Math.PI / 3 },
    ]),
  },

  // ---------- eq-chain ----------
  {
    module: 'eq-chain',
    caseId: 'case1',
    sampleRate: 48000,
    blockSize: 256,
    params: {
      bandCount: 10,
      qCompensation: false,
      bands: [
        { frequency: 40, gain: 0, q: 0.5 },
        { frequency: 80, gain: 0, q: 0.8 },
        { frequency: 160, gain: 0, q: 1.0 },
        { frequency: 320, gain: 0, q: 1.2 },
        { frequency: 640, gain: 0, q: 1.4 },
        { frequency: 1280, gain: 0, q: 2.0 },
        { frequency: 2560, gain: 0, q: 3.0 },
        { frequency: 5120, gain: 0, q: 4.0 },
        { frequency: 10240, gain: 0, q: 0.707 },
        { frequency: 16000, gain: 0, q: 6.0 },
      ],
    },
    notes: '零增益全直通锚点：10 段全部 gain=0（frequency/q 取多变体），peaking A=1 时分子分母多项式解析恒等、TDF2 状态恒零，期望输出与输入逐位一致（左右声道皆然，含共享状态续跑的右声道）——最强跨实现精度锚点。左=三频正弦叠加(50/800/6000Hz)，右=固定种子LCG噪声。帧数非整除（末块 112 帧）。',
    inputL: (n, fs) => sineSum(n, fs, [
      { freqHz: 50, amp: 0.45, phaseRad: 0 },
      { freqHz: 800, amp: 0.35, phaseRad: Math.PI / 5 },
      { freqHz: 6000, amp: 0.25, phaseRad: Math.PI / 3 },
    ]),
    inputR: (n) => lcgNoise(n, 80801, 0.6),
  },
  {
    module: 'eq-chain',
    caseId: 'case2',
    sampleRate: 48000,
    blockSize: 384,
    params: {
      bandCount: 5,
      qCompensation: true,
      bands: [
        { frequency: 40, gain: 6, q: 1.4 },
        { frequency: 1000, gain: -4, q: 1.0 },
        { frequency: 8000, gain: 3, q: 0.8 },
      ],
    },
    notes: 'boost/cut 混合级联 + 级联 Q 补偿开启：3 个活动段（40Hz+6 / 1kHz−4 / 8kHz+3）+ 2 个尾部填充段（bands 短于 bandCount 的回退语义，填充段不参与补偿）。补偿迭代（Gauss-Seidel 逐段、0.8 阻尼、至多 5 轮、<0.05dB 提前终止）实测 2 轮收敛。左=活动段频点三频正弦叠加(40/1000/8000Hz)，右=固定种子LCG噪声（宽频激励）。与 case3 成对对照（仅 qCompensation 不同）。帧数非整除（末块 240 帧）。',
    inputL: (n, fs) => sineSum(n, fs, [
      { freqHz: 40, amp: 0.5, phaseRad: 0 },
      { freqHz: 1000, amp: 0.35, phaseRad: Math.PI / 4 },
      { freqHz: 8000, amp: 0.2, phaseRad: Math.PI / 2 },
    ]),
    inputR: (n) => lcgNoise(n, 80802, 0.5),
  },
  {
    module: 'eq-chain',
    caseId: 'case3',
    sampleRate: 48000,
    blockSize: 384,
    params: {
      bandCount: 5,
      qCompensation: false,
      bands: [
        { frequency: 40, gain: 6, q: 1.4 },
        { frequency: 1000, gain: -4, q: 1.0 },
        { frequency: 8000, gain: 3, q: 0.8 },
      ],
    },
    notes: '与 case2 完全成对的补偿关闭对照：参数与输入全同、仅 qCompensation=false——增益恰为用户目标、无补偿迭代，相邻段叠加使级联响应偏离控制点目标，输出与 case2 显著可区分（驱动器漏做/错做补偿迭代必然在此暴露）。帧数非整除（末块 240 帧）。',
    inputL: (n, fs) => sineSum(n, fs, [
      { freqHz: 40, amp: 0.5, phaseRad: 0 },
      { freqHz: 1000, amp: 0.35, phaseRad: Math.PI / 4 },
      { freqHz: 8000, amp: 0.2, phaseRad: Math.PI / 2 },
    ]),
    inputR: (n) => lcgNoise(n, 80802, 0.5),
  },
  {
    module: 'eq-chain',
    caseId: 'case4',
    sampleRate: 48000,
    blockSize: 480,
    params: {
      bandCount: 20,
      qCompensation: false,
      bands: [
        { frequency: 5, gain: 30, q: 0.05 },
        { frequency: 40, gain: 6, q: 1.4 },
        { frequency: 80, gain: -3, q: 1.0 },
        { frequency: 160, gain: 4.5, q: 0.9 },
        { frequency: 320, gain: -6, q: 1.2 },
        { frequency: 640, gain: 2, q: 1.5 },
        { frequency: 1000, gain: -1.5, q: 0.8 },
        { frequency: 1500, gain: 3.5, q: 1.1 },
        { frequency: 2000, gain: -2, q: 1.6 },
        { frequency: 3000, gain: 4, q: 0.85 },
        { frequency: 4000, gain: -5, q: 1.3 },
        { frequency: 5000, gain: 2.5, q: 0.95 },
        { frequency: 6000, gain: -3.5, q: 1.25 },
        { frequency: 8000, gain: 3, q: 1.0 },
        { frequency: 10000, gain: -1, q: 0.75 },
        { frequency: 12000, gain: 2, q: 1.45 },
        { frequency: 14000, gain: -2.5, q: 0.7 },
        { frequency: 16000, gain: 1.5, q: 1.15 },
        { frequency: 18000, gain: -1.5, q: 0.65 },
        { frequency: 30000, gain: -40, q: 50 },
      ],
    },
    notes: '满配钳制极值：bandCount=20、20 段全配；首段 frequency=5/gain=30/q=0.05 越下界（按 20Hz/+24dB/Q0.1 生效），末段 frequency=30000/gain=-40/q=50 越上界（按 20kHz/−24dB/Q18 生效），三参数双向钳制语义随载荷固化。左=三频正弦叠加(220/3300/11000Hz)，右=固定种子LCG噪声。帧数非整除（末块 240 帧）。',
    inputL: (n, fs) => sineSum(n, fs, [
      { freqHz: 220, amp: 0.4, phaseRad: 0 },
      { freqHz: 3300, amp: 0.25, phaseRad: Math.PI / 6 },
      { freqHz: 11000, amp: 0.15, phaseRad: Math.PI / 3 },
    ]),
    inputR: (n) => lcgNoise(n, 80804, 0.7),
  },

  // ---------- fdn-reverb ----------
  {
    module: 'fdn-reverb',
    caseId: 'case1',
    sampleRate: 48000,
    blockSize: 256,
    params: { roomSize: 0.5, damping: 0.5, wet: 0, dry: 1, preDelayMs: 1200, width: 1, type: 'hall', lines: 8 },
    notes: '纯干声恒等锚点：wet=0/dry=1 时湿路项精确为零、干路乘 1.0，期望输出与输入逐位一致（左右声道皆然，实证）；preDelayMs=1200 越上界按 1000ms 生效（仅作用于湿路输入侧，不影响逐位干路）。左=固定种子LCG噪声，右=双频正弦叠加(220/3300Hz)。帧数非整除（末块 112 帧）。',
    inputL: (n) => lcgNoise(n, 61001, 0.7),
    inputR: (n, fs) => sineSum(n, fs, [
      { freqHz: 220, amp: 0.5, phaseRad: 0 },
      { freqHz: 3300, amp: 0.25, phaseRad: Math.PI / 4 },
    ]),
  },
  {
    module: 'fdn-reverb',
    caseId: 'case2',
    sampleRate: 48000,
    blockSize: 128,
    params: { roomSize: 0.5, damping: 0.5, wet: 1, dry: 0, preDelayMs: 0, width: 1, type: 'hall', lines: 8 },
    notes: 'hall 冲激响应快照：纯湿声（dry=0）、用户 roomSize/damping=0.5 中性（即 type 基准 0.7/0.4、delayScale 1.3）；仅左声道首帧冲激、右声道全零（不对称激励验证左右 FDN 网络不同素数表的立体声去相关）。互质延迟线 → 高密度尾音，冲激响应整段冻结。帧数非整除（末块 64 帧）。',
    inputL: (n) => {
      const x = silence(n)
      x[0] = 1
      return x
    },
    inputR: (n) => silence(n),
  },
  {
    module: 'fdn-reverb',
    caseId: 'case3',
    sampleRate: 44100,
    blockSize: 441,
    params: { roomSize: 0.5, damping: 0.5, wet: 0.8, dry: 0.3, preDelayMs: 0, width: 0, type: 'room', lines: 8 },
    notes: 'width=0 湿路对半交叉对照：type room（基准 0.4/0.6/delayScale 0.6，与 case2 的 hall 形成类型对照）；输入为左右同源单声道 LCG 噪声——width=0 时 wet1=wet2=wet/2，湿路对半交叉后左右输出在 1 ulp f32 量级一致（实证约 7.5e-9，浮点加法结合序所致，不主张逐位一致）。多采样率 44100 覆盖延迟长度 fs/44100 缩放路径；blockSize=441 恰为该采样率 10ms，帧数非整除（末块 267 帧）。',
    inputL: (n) => lcgNoise(n, 61003, 0.6),
    inputR: (n) => lcgNoise(n, 61003, 0.6),
  },
  {
    module: 'fdn-reverb',
    caseId: 'case4',
    sampleRate: 48000,
    blockSize: 700,
    params: { roomSize: 1.5, damping: -0.3, wet: 4.5, dry: -0.5, preDelayMs: 80, width: 2.5, type: 'stage', lines: 16 },
    notes: 'stage 满配极值钳制：lines=16 满配素数表；roomSize=1.5/damping=-0.3 越界按 clamp01 后与 stage 基准（0.55/0.5/delayScale 1.5）混合生效、wet=4.5→4、dry=-0.5→0（纯湿）、width=2.5→2（wet2 为负的超宽反相交叉）；输入为前半段噪声突发→后半段静音，覆盖衰减尾与环路稳定性（反馈 g≤0.98、g²<1 有界不发散）。帧数非整除（末块 500 帧）。',
    inputL: (n) => burstThenSilence(lcgNoise(n, 61004, 0.7), Math.floor(n / 2)),
    inputR: (n) => burstThenSilence(lcgNoise(n, 61005, 0.7), Math.floor(n / 2)),
  },

  // ---------- deesser ----------
  {
    module: 'deesser',
    caseId: 'case1',
    sampleRate: 48000,
    blockSize: 256,
    params: { enabled: false, centerHz: 8000, q: 0.7, thresholdDb: -30, ratio: 8, attackMs: 1, releaseMs: 80, splitBand: true, mix: 1, sidechainEnabled: false },
    notes: '禁用恒等锚点：enabled=false 时 processStereo 首行直接返回、缓冲零改写，期望输出与输入逐位一致（左右声道皆然，包络与全部滤波器状态不推进）；其余参数取激进值随载荷固化。左=合成齿音（4–8kHz 九频点正弦族 + 固定种子 LCG 相位，带限确定性、无 Math.random），右=200Hz 正弦。帧数非整除（末块 112 帧）。',
    inputL: (n, fs) => bandNoise(n, fs, 0.05, 62001),
    inputR: (n, fs) => sine(n, fs, 200, 0.4),
  },
  {
    module: 'deesser',
    caseId: 'case2',
    sampleRate: 48000,
    blockSize: 384,
    params: { enabled: true, centerHz: 8000, q: 0.7, thresholdDb: -30, ratio: 8, attackMs: 1, releaseMs: 80, splitBand: true, mix: 1, sidechainEnabled: false },
    notes: '分带重衰减：splitBand=true，检测信号=左右单声道和经 8kHz/Q0.7 带通，齿音能量使包络稳态超阈、g 进入稳态压缩；左=持续合成齿音（4–8kHz 频带限定），右=200Hz 正弦——分带式只压高频带，低频带幅度保持（LR-4 全通仅相位旋转）。与 case3 成对对照（仅 splitBand 不同）。帧数非整除（末块 200 帧）。',
    inputL: (n, fs) => bandNoise(n, fs, 0.05, 62001),
    inputR: (n, fs) => sine(n, fs, 200, 0.4),
  },
  {
    module: 'deesser',
    caseId: 'case3',
    sampleRate: 48000,
    blockSize: 384,
    params: { enabled: true, centerHz: 8000, q: 0.7, thresholdDb: -30, ratio: 8, attackMs: 1, releaseMs: 80, splitBand: false, mix: 1, sidechainEnabled: false },
    notes: '宽带对照：与 case2 参数与输入完全相同、仅 splitBand=false——整体乘 g，右声道 200Hz 低频成分与齿音同被衰减（分带式则保持），两向量输出显著可区分（驱动器漏做交叉/误用宽带路径必然暴露）。帧数非整除（末块 200 帧）。',
    inputL: (n, fs) => bandNoise(n, fs, 0.05, 62001),
    inputR: (n, fs) => sine(n, fs, 200, 0.4),
  },
  {
    module: 'deesser',
    caseId: 'case4',
    sampleRate: 44100,
    blockSize: 441,
    params: { enabled: true, centerHz: 30000, q: 0.05, thresholdDb: -20, ratio: 8, attackMs: 0, releaseMs: 0, splitBand: true, mix: 1, sidechainEnabled: false },
    notes: '阈下不衰减 + 钳制极值：低电平输入使包络稳态低于阈值 -20dB → reduction 恒 0、g 恒 1，输出为 LR-4 全通重构（幅度不变、相位旋转——非逐位一致，幅度语义以冻结向量界定）；centerHz=30000 按 fs×0.45=19845 生效（44100 采样率上界）、q=0.05→0.1 下界、attackMs=0→0.05ms、releaseMs=0→1ms（release 下限与 attack 不同）。多采样率 44100 覆盖 attack/release/交叉系数随 fs 变化。帧数非整除（末块 126 帧）。',
    inputL: (n, fs) => bandNoise(n, fs, 0.008, 62004),
    inputR: (n, fs) => sine(n, fs, 200, 0.06),
  },

  // ---------- loudness-comp ----------
  {
    module: 'loudness-comp',
    caseId: 'case1',
    sampleRate: 48000,
    blockSize: 256,
    params: { volumePercent: 100, maxBoostDb: 12, preset: 'flat', bands: [], mode: 'auto', smoothingSeconds: 0.2 },
    notes: 'auto 满音量恒等锚点：volumePercent=100 → 全部目标增益为 0 → 恒等系数链 + 零状态，期望输出与输入逐位一致（左右声道皆然，实证为精确恒等而非近似）。左=四频正弦叠加(63/440/4000/10000Hz)覆盖等响度全频段，右=固定种子LCG噪声。帧数非整除（末块 112 帧）。',
    inputL: (n, fs) => sineSum(n, fs, [
      { freqHz: 63, amp: 0.4, phaseRad: 0 },
      { freqHz: 440, amp: 0.3, phaseRad: Math.PI / 5 },
      { freqHz: 4000, amp: 0.2, phaseRad: Math.PI / 3 },
      { freqHz: 10000, amp: 0.15, phaseRad: Math.PI / 2 },
    ]),
    inputR: (n) => lcgNoise(n, 63001, 0.6),
  },
  {
    module: 'loudness-comp',
    caseId: 'case2',
    sampleRate: 48000,
    blockSize: 384,
    params: { volumePercent: 20, maxBoostDb: 12, preset: 'flat', bands: [], mode: 'auto', smoothingSeconds: 0.05 },
    notes: 'auto 低音量提升（与 case1 构成 volumePercent 对照对）：volumePercent=20 → 5 个活动段（120Hz 低架、12kHz 高架、2500/4000/6300Hz peaking，ISO 226 简化近似低频 0-12dB/高频 0-6dB 曲线）；smoothingSeconds=0.05 逐块一阶爬升轨迹整段冻结——本模块输出依赖 blockSize，两支线必须按同一块长回放。左=五频正弦(63/120/1000/4000/10000Hz)，右=固定种子LCG噪声。帧数非整除（末块 200 帧）。',
    inputL: (n, fs) => sineSum(n, fs, [
      { freqHz: 63, amp: 0.35, phaseRad: 0 },
      { freqHz: 120, amp: 0.3, phaseRad: Math.PI / 6 },
      { freqHz: 1000, amp: 0.3, phaseRad: Math.PI / 4 },
      { freqHz: 4000, amp: 0.2, phaseRad: Math.PI / 3 },
      { freqHz: 10000, amp: 0.15, phaseRad: Math.PI / 2 },
    ]),
    inputR: (n) => lcgNoise(n, 63002, 0.5),
  },
  {
    module: 'loudness-comp',
    caseId: 'case3',
    sampleRate: 48000,
    blockSize: 333,
    params: {
      volumePercent: 42, maxBoostDb: 12, preset: 'flat', mode: 'custom', smoothingSeconds: 0.05,
      bands: [
        { frequency: 60, gain: 18 },
        { frequency: 30, gain: 200 },
        { frequency: 300, gain: 0.1 },
        { frequency: 1000, gain: -6 },
        { frequency: 4000, gain: 9 },
        { frequency: 20000, gain: 5 },
      ],
    },
    notes: 'custom 分组/钳制/丢弃语义：low 组(≤250Hz)取钳制后均值——{60,+18} 与 {30,+200→按+24 生效} 的均值 +21 → 120Hz 低架；mid 项 {300,+0.1} 因 |gain|<0.25 被丢弃；mid peaking -6@1kHz、+9@4kHz；high 组(≥6kHz) {20000,+5} → 12kHz 高架；volumePercent=42 不被 custom 模式消费（语义随载荷固化）。左=四频正弦(100/1000/4000/12000Hz)，右=固定种子LCG噪声。blockSize=333 非整除（末块 6 帧，极短短块覆盖）。',
    inputL: (n, fs) => sineSum(n, fs, [
      { freqHz: 100, amp: 0.4, phaseRad: 0 },
      { freqHz: 1000, amp: 0.3, phaseRad: Math.PI / 4 },
      { freqHz: 4000, amp: 0.25, phaseRad: Math.PI / 3 },
      { freqHz: 12000, amp: 0.2, phaseRad: Math.PI / 2 },
    ]),
    inputR: (n) => lcgNoise(n, 63003, 0.5),
  },
  {
    module: 'loudness-comp',
    caseId: 'case4',
    sampleRate: 48000,
    blockSize: 480,
    params: { volumePercent: 100, maxBoostDb: 12, preset: 'night', bands: [], mode: 'preset', smoothingSeconds: 0.05 },
    notes: 'preset 曲线 6 段满配：preset=night（含负增益中频控制点）→ 拟合达 6 段上限（120Hz 低架、12kHz 高架、315/630/4000/6300Hz peaking 正负混合）；volumePercent/maxBoostDb 不被 preset 模式消费。左=五频正弦(80/315/1000/6300/12000Hz)对准拟合频点，右=固定种子LCG噪声。帧数非整除（末块 160 帧）。',
    inputL: (n, fs) => sineSum(n, fs, [
      { freqHz: 80, amp: 0.4, phaseRad: 0 },
      { freqHz: 315, amp: 0.3, phaseRad: Math.PI / 5 },
      { freqHz: 1000, amp: 0.25, phaseRad: Math.PI / 4 },
      { freqHz: 6300, amp: 0.2, phaseRad: Math.PI / 3 },
      { freqHz: 12000, amp: 0.15, phaseRad: Math.PI / 2 },
    ]),
    inputR: (n) => lcgNoise(n, 63004, 0.5),
  },

  // ---------- dynamic-eq ----------
  {
    module: 'dynamic-eq',
    caseId: 'case1',
    sampleRate: 48000,
    blockSize: 256,
    params: {
      enabled: false,
      strength: 0.8,
      thresholdDb: -30,
      ratio: 8,
      kneeDb: 6,
      attackMs: 10,
      releaseMs: 200,
      blockSize: 128,
      bands: [
        { enabled: true, frequency: 200, targetGainDb: 6 },
        { enabled: true, frequency: 800, targetGainDb: -6 },
        { enabled: true, frequency: 2500, targetGainDb: 8 },
        { enabled: true, frequency: 8000, targetGainDb: -8 },
        { enabled: true, frequency: 0, targetGainDb: 3 },
      ],
    },
    notes: '禁用恒等锚点：enabled=false 时 processStereo 首行直接返回、缓冲零改写，期望输出与输入逐位一致（左右声道皆然，增益/目标/电平/交叉树全部状态不推进）；其余参数取激进值随载荷固化（bands 的 frequency 与引擎固定注入 DYNAMIC_EQ_CROSSOVERS=[200,800,2500,8000] 一致、末带 0 被模块忽略，见 specs/dsp/dynamic-eq.md §4.7）。左=三频正弦叠加(120/800/6000Hz)，右=固定种子LCG噪声。帧数非整除（末块 112 帧）。',
    inputL: (n, fs) => sineSum(n, fs, [
      { freqHz: 120, amp: 0.45, phaseRad: 0 },
      { freqHz: 800, amp: 0.3, phaseRad: Math.PI / 5 },
      { freqHz: 6000, amp: 0.2, phaseRad: Math.PI / 3 },
    ]),
    inputR: (n) => lcgNoise(n, 85001, 0.5),
  },
  {
    module: 'dynamic-eq',
    caseId: 'case2',
    sampleRate: 48000,
    blockSize: 333,
    params: {
      enabled: true,
      strength: 0.5,
      thresholdDb: -10,
      ratio: 2,
      kneeDb: 6,
      attackMs: 20,
      releaseMs: 200,
      blockSize: 128,
      bands: [
        { enabled: true, frequency: 200, targetGainDb: 6 },
        { enabled: true, frequency: 800, targetGainDb: 4 },
        { enabled: true, frequency: 2500, targetGainDb: 3 },
        { enabled: true, frequency: 8000, targetGainDb: 2 },
        { enabled: true, frequency: 0, targetGainDb: 1 },
      ],
    },
    notes: '全带静态提升 + strength 干湿混合：5 带全启用、目标曲线全正（+6/+4/+3/+2/+1 dB）、strength 0.5、阈值 -10dB 较高（激励电平多在阈下，静态曲线主导）——各带增益从 1 沿 release 路径爬升至 strength 混合目标。顶层 blockSize=333 非 params.blockSize=128 的整数倍：控制更新在每次驱动调用边界提前触发，输出依赖驱动分块（specs/dsp/dynamic-eq.md §4.5 实证），两支线必须按同一块长回放。帧数非整除（末块 6 帧）。',
    inputL: (n, fs) => sineSum(n, fs, [
      { freqHz: 110, amp: 0.35, phaseRad: 0 },
      { freqHz: 900, amp: 0.25, phaseRad: Math.PI / 4 },
      { freqHz: 3200, amp: 0.15, phaseRad: Math.PI / 3 },
    ]),
    inputR: (n) => lcgNoise(n, 85002, 0.35),
  },
  {
    module: 'dynamic-eq',
    caseId: 'case3',
    sampleRate: 48000,
    blockSize: 384,
    params: {
      enabled: true,
      strength: 0.7,
      thresholdDb: -20,
      ratio: 4,
      kneeDb: 6,
      attackMs: 10,
      releaseMs: 300,
      blockSize: 128,
      bands: [
        { enabled: true, frequency: 200, targetGainDb: 0 },
        { enabled: true, frequency: 800, targetGainDb: 0 },
        { enabled: true, frequency: 2500, targetGainDb: 0 },
        { enabled: true, frequency: 8000, targetGainDb: 0 },
        { enabled: true, frequency: 0, targetGainDb: 0 },
      ],
    },
    notes: '阈值/attack-release 行为：阈值 -20dB、ratio 4、strength 0.7——前半段强激励（0.9 幅度正弦叠加 + 0.9 噪声）使多带电平稳定超阈、增益沿 attack 路径下探至压缩稳态；后半段静音、增益沿 release 路径恢复；控制延迟一个分析块（128 样本）随轨迹整段冻结。顶层 blockSize=384 为 params.blockSize 的整数倍（=3×128，分块与整块处理逐位一致）。帧数非整除（末块 200 帧）。',
    inputL: (n, fs) => burstThenSilence(sineSum(n, fs, [
      { freqHz: 100, amp: 0.9, phaseRad: 0 },
      { freqHz: 3000, amp: 0.9, phaseRad: Math.PI / 4 },
    ]), Math.floor(n / 2)),
    inputR: (n) => burstThenSilence(lcgNoise(n, 85003, 0.9), Math.floor(n / 2)),
  },
  {
    module: 'dynamic-eq',
    caseId: 'case4',
    sampleRate: 44100,
    blockSize: 480,
    params: {
      enabled: true,
      strength: 1.5,
      thresholdDb: -90,
      ratio: 120,
      kneeDb: 45,
      attackMs: 0,
      releaseMs: 0,
      blockSize: 8,
      bands: [
        { enabled: true, frequency: 10, targetGainDb: 20 },
        { enabled: false, frequency: 30000, targetGainDb: -20 },
        { enabled: true, frequency: 2500, targetGainDb: 3 },
        { enabled: false, frequency: 8000, targetGainDb: 0 },
        { enabled: true, frequency: 99999, targetGainDb: 1 },
      ],
    },
    notes: '部分带禁用 + 钳制极值：bands[1]/bands[3] 禁用（目标恒 1、带信号仍以增益 1 参与求和）；strength=1.5→1、thresholdDb=-90→-80、ratio=120→100、kneeDb=45→40、attackMs=0→0.05ms、releaseMs=0→1ms、params.blockSize=8→16 双向越界钳制；bands[0].frequency=10→30 下界、bands[1].frequency=30000→19845（44100 下 nyq×0.9 上界）、bands[4].frequency=99999 完全被忽略（末带不读取）、targetGainDb ±20→±12。多采样率 44100 覆盖 crossover 上界与 attack/release 系数随 fs 变化。帧数非整除（末块 240 帧）。',
    inputL: (n, fs) => sineSum(n, fs, [
      { freqHz: 80, amp: 0.6, phaseRad: 0 },
      { freqHz: 1500, amp: 0.4, phaseRad: Math.PI / 5 },
      { freqHz: 9000, amp: 0.3, phaseRad: Math.PI / 3 },
    ]),
    inputR: (n) => lcgNoise(n, 85004, 0.5),
  },

  // ---------- mod-effects ----------
  {
    module: 'mod-effects',
    caseId: 'case1',
    sampleRate: 48000,
    blockSize: 256,
    params: {
      delay: { enabled: false, delayMs: 1500, feedback: 0.9, mix: 0.8 },
      chorus: { enabled: false, rateHz: 15, depthMs: 30, mix: 1 },
      flanger: { enabled: false, rateHz: 15, depthMs: 30, feedback: 0.9, mix: 1 },
      phaser: { enabled: false, rateHz: 15, depth: 1, feedback: 0.9, mix: 1, stages: 9 },
      tremolo: { enabled: false, rateHz: 25, depth: 1, mix: 1 },
    },
    notes: '全禁用恒等锚点：五效果 enabled 全 false，链路驱动器跳过全部五级（禁用级逐位旁路、状态不推进），期望输出与输入逐位一致；激进参数随载荷固化（含 phaser stages=9→8 越界钳制），驱动器对五效果无条件 setParams（enabled 字段被效果类自身忽略，引擎接线语义见 specs/dsp/mod-effects.md §4.1）。左=双频正弦叠加(220/3300Hz)，右=固定种子LCG噪声。帧数非整除（末块 112 帧）。',
    inputL: (n, fs) => sineSum(n, fs, [
      { freqHz: 220, amp: 0.5, phaseRad: 0 },
      { freqHz: 3300, amp: 0.25, phaseRad: Math.PI / 4 },
    ]),
    inputR: (n) => lcgNoise(n, 86001, 0.6),
  },
  {
    module: 'mod-effects',
    caseId: 'case2',
    sampleRate: 48000,
    blockSize: 333,
    params: {
      delay: { enabled: true, delayMs: 40, feedback: 0.55, mix: 0.4 },
      chorus: { enabled: false, rateHz: 1.2, depthMs: 5, mix: 0.4 },
      flanger: { enabled: false, rateHz: 0.8, depthMs: 4, feedback: 0.5, mix: 0.5 },
      phaser: { enabled: false, rateHz: 0.5, depth: 0.5, feedback: 0.4, mix: 0.5, stages: 4 },
      tremolo: { enabled: false, rateHz: 5, depth: 0.5, mix: 1 },
    },
    notes: 'Delay 单独激活（隔离 case）：仅 delay enabled（40ms≈1920 样本延迟线 + 反馈 0.55 + mix 0.4），左声道前 3000 帧有声后段静音——反馈回声族（40ms 间隔逐代衰减）与衰减尾清晰；delay 为纯逐样本递推（分块与整块逐位一致）；其余四级禁用逐位旁路。帧数非整除（末块 6 帧）。',
    inputL: (n, fs) => burstThenSilence(sineSum(n, fs, [
      { freqHz: 1000, amp: 0.8, phaseRad: 0 },
      { freqHz: 2500, amp: 0.3, phaseRad: Math.PI / 3 },
    ]), 3000),
    inputR: (n) => lcgNoise(n, 86002, 0.5),
  },
  {
    module: 'mod-effects',
    caseId: 'case3',
    sampleRate: 48000,
    blockSize: 333,
    params: {
      delay: { enabled: false, delayMs: 300, feedback: 0.5, mix: 0.3 },
      chorus: { enabled: true, rateHz: 4, depthMs: 5, mix: 0.5 },
      flanger: { enabled: true, rateHz: 2.5, depthMs: 4, feedback: 0.6, mix: 0.5 },
      phaser: { enabled: false, rateHz: 0.5, depth: 0.5, feedback: 0.4, mix: 0.5, stages: 4 },
      tremolo: { enabled: false, rateHz: 5, depth: 0.5, mix: 1 },
    },
    notes: 'Chorus + Flanger 组合（隔离 case）：chorus（4Hz/5ms/mix0.5，基础延迟固定 20ms、反馈恒 0）+ flanger（2.5Hz/4ms/反馈0.6/mix0.5，基础延迟固定 1ms）级联；flanger 调制负半周（1ms−4ms<0）触发 readDelay 下界钳制与 d<1 退化读取区（整环回绕前值，specs/dsp/mod-effects.md §4.2 实证连续、有限）。chorus/flanger 的 LFO 相位按整块步进（块内调制量常量）→ 输出依赖顶层 blockSize，两支线必须按同一块长回放。帧数非整除（末块 12 帧）。',
    inputL: (n, fs) => sineSum(n, fs, [
      { freqHz: 440, amp: 0.5, phaseRad: 0 },
      { freqHz: 1320, amp: 0.25, phaseRad: Math.PI / 5 },
    ]),
    inputR: (n) => lcgNoise(n, 86003, 0.5),
  },
  {
    module: 'mod-effects',
    caseId: 'case4',
    sampleRate: 44100,
    blockSize: 480,
    params: {
      delay: { enabled: false, delayMs: 300, feedback: 0.5, mix: 0.3 },
      chorus: { enabled: false, rateHz: 1.5, depthMs: 6, mix: 0.4 },
      flanger: { enabled: false, rateHz: 1, depthMs: 5, feedback: 0.5, mix: 0.5 },
      phaser: { enabled: true, rateHz: 1.5, depth: 0.8, feedback: 0.5, mix: 0.5, stages: 6 },
      tremolo: { enabled: true, rateHz: 8, depth: 0.7, mix: 1 },
    },
    notes: 'Phaser + Tremolo 组合（隔离 case，stages 变体）：phaser（1.5Hz/depth0.8/反馈0.5/mix0.5/stages=6 非默认级数；各级全通并行处理同一输入、仅末级输出被采用——非级联，specs/dsp/mod-effects.md §4.5）+ tremolo（8Hz/depth0.7/mix1 深度幅度调制）级联；两者 LFO 均逐样本步进（分块与整块逐位一致）。多采样率 44100 覆盖全通系数 tan(π·fc/fs) 与相位步进随 fs 变化。帧数非整除（末块 240 帧）。',
    inputL: (n, fs) => sineSum(n, fs, [
      { freqHz: 600, amp: 0.6, phaseRad: 0 },
      { freqHz: 2400, amp: 0.3, phaseRad: Math.PI / 4 },
    ]),
    inputR: (n) => lcgNoise(n, 86004, 0.5),
  },

  // ---------- fft（非流式变换特例，specs/dsp/fft.md §三） ----------
  // 驱动模型：(L,R) = 复数平面 (Re,Im)；blockSize = frames = N = fftSize（单块）；
  // 每块独立做原位复 FFT（无跨块状态），输出 = 变换后的两个平面。
  // 四条 case 覆盖 4 个块长，log2(N) 奇偶两类蝶形调度各两次（基-4 主路径 / 基-2 尾路径）。
  {
    module: 'fft',
    caseId: 'case1',
    sampleRate: 48000,
    blockSize: 4096,
    params: { inverse: false },
    notes: '脉冲平坦谱逐位锚点（GWT-FFT-01）：Re 平面=δ 脉冲（首样本 1）、Im 平面全零；N=4096（log2=12 偶数，纯基-4 蝶形路径）。期望输出：Re 谱逐位全 1、Im 谱逐位全 +0——蝶形只触碰精确零与 1 的加减，任何正确 DFT 实现均应逐位一致（最强跨实现精度锚点）。正变换不缩放（X[0]=Σx 尺度）。',
    inputL: (n) => {
      const x = silence(n)
      x[0] = 1
      return x
    },
    inputR: (n) => silence(n),
  },
  {
    module: 'fft',
    caseId: 'case2',
    sampleRate: 48000,
    blockSize: 2048,
    params: { inverse: false },
    notes: '整 bin 单频正弦 → 共轭对称双谱线（GWT-FFT-02）：Re 平面=单位幅度正弦、频率恰为整数 bin k0（f=k0·fs/N）；N=2048（log2=11 奇数，基-4 + 基-2 尾路径）。期望 Im 谱在 k0 与 N−k0 出现 ∓N/2·amp 谱线（∓1024，实证逐位精确）、Re 谱该两 bin 近零；其余 bin 的微小取值是逐级 f32 舍入的实现噪声（冻结值即 TS 噪声实现，移植须算术调度等价，specs/dsp/fft.md §四）。无窗变换：不整 bin 时呈矩形窗泄漏（case3/case4 覆盖）。',
    inputL: (n, fs) => sine(n, fs, (137 * fs) / n, 1.0),
    inputR: (n) => silence(n),
  },
  {
    module: 'fft',
    caseId: 'case3',
    sampleRate: 48000,
    blockSize: 1024,
    params: { inverse: false },
    notes: '双平面复输入（GWT-FFT-03）：Re 平面=440Hz/0.5 + 2900Hz/0.3(π/6)，Im 平面=700Hz/0.4(π/4) + 1800Hz/0.35(π/2)——两平面都有能量、频点互不相同且均非整数 bin（全谱泄漏形态）。N=1024（log2=10 偶数，纯基-4 路径）。复输入不具实信号共轭对称性，输出 Re/Im 两谱均有全谱能量；驱动器若把 Im 平面置零或拆成两次实变换必然超差。',
    inputL: (n, fs) => sineSum(n, fs, [
      { freqHz: 440, amp: 0.5, phaseRad: 0 },
      { freqHz: 2900, amp: 0.3, phaseRad: Math.PI / 6 },
    ]),
    inputR: (n, fs) => sineSum(n, fs, [
      { freqHz: 700, amp: 0.4, phaseRad: Math.PI / 4 },
      { freqHz: 1800, amp: 0.35, phaseRad: Math.PI / 2 },
    ]),
  },
  {
    module: 'fft',
    caseId: 'case4',
    sampleRate: 48000,
    blockSize: 8192,
    params: { inverse: false },
    notes: '直流分量 + 非整周期泄漏（GWT-FFT-04）：Re 平面=0.4 直流常量 + 777Hz/0.3 正弦（777Hz 非整数 bin，矩形窗泄漏裙摆连续分布——本变换不加窗）；Im 平面全零。N=8192（log2=13 奇数，基-2 尾路径的最大块长）。期望 X[0] 承载全部直流（解析尺度 DC·N），其余呈泄漏裙摆；全程无 NaN、有界。',
    inputL: (n, fs) => {
      const x = sine(n, fs, 777, 0.3)
      for (let i = 0; i < n; i++) x[i] = 0.4 + x[i]
      return x
    },
    inputR: (n) => silence(n),
  },

  // ---------- convolver（IR 配方驱动，specs/dsp/convolver.md §4.2） ----------
  // 驱动顺序 = 引擎接线顺序：new Convolver(fs, opts) → loadIR(buildIrRecipe) →
  // setMix → setPreDelayMs → 逐块 processStereo。
  {
    module: 'convolver',
    caseId: 'case1',
    sampleRate: 48000,
    blockSize: 512,
    params: { partitionSize: 512, longPartitionSize: 4096, shortRegionMs: 100, dePeriodize: true, mix: 1, preDelayMs: 0, ir: { kind: 'delta', delay: 0 } },
    notes: 'δ IR 延迟直通锚点（GWT-CV-01）：IR=单点冲激（delay=0，长度 1）→ 均匀单短分区（IR 短于短区段，Ps 收敛为 1、Pl=0）。mix=1 纯湿：首 Ls=512 个输出样本逐位 +0（湿路放行控制流锚点），其后湿路=输入延迟 Ls 的直通（偏差为分区卷积 FFT 往返舍入 1e-7 量级——输入带直流偏置使输出幅值远离零，非逐位一致语义以冻结向量界定）。dePeriodize=true 对 δ IR 为精确无操作（−60dB 后缀判定防误衰减，实证逐位一致）。getLatencySamples()=512，与 IR 长度解耦。帧数恰整除（12 块）。',
    inputL: (n, fs) => {
      const x = sine(n, fs, 220, 0.25)
      for (let i = 0; i < n; i++) x[i] = 0.6 + x[i]
      return x
    },
    inputR: (n, fs) => {
      const x = sine(n, fs, 330, 0.2, Math.PI / 6)
      for (let i = 0; i < n; i++) x[i] = 0.55 + x[i]
      return x
    },
  },
  {
    module: 'convolver',
    caseId: 'case2',
    sampleRate: 48000,
    blockSize: 384,
    params: { partitionSize: 512, longPartitionSize: 4096, shortRegionMs: 100, dePeriodize: false, mix: 1, preDelayMs: 0, ir: { kind: 'expNoise', length: 6000, seed: 12345, decay: 12, amp: 0.5 } },
    notes: 'expNoise 真实卷积尾 + 非均匀分区（GWT-CV-02）：IR=固定种子 LCG 指数衰减噪声（length=6000 超短区段 4800 → Ps=10、longStart=5120、Pl=1 长分区参与，每 k=8 个短块做一次 Nl=8192 长 FFT），dePeriodize=false（配方 IR 原样载入）、mix=1 纯湿。湿路=输入与 IR 的完整线性卷积延迟 Ls 对齐后的流式窗口（分区卷积数学等价线性卷积）；驱动器漏做长分区累加或错置写入偏移必然超差。左=三频正弦叠加，右=固定种子 LCG 噪声（宽频激励）。blockSize=384 非整除（末块 104 帧）。',
    inputL: (n, fs) => sineSum(n, fs, [
      { freqHz: 120, amp: 0.4, phaseRad: 0 },
      { freqHz: 1900, amp: 0.25, phaseRad: Math.PI / 5 },
      { freqHz: 7000, amp: 0.15, phaseRad: Math.PI / 3 },
    ]),
    inputR: (n) => lcgNoise(n, 99002, 0.5),
  },
  {
    module: 'convolver',
    caseId: 'case3',
    sampleRate: 48000,
    blockSize: 384,
    params: { partitionSize: 512, longPartitionSize: 4096, shortRegionMs: 100, dePeriodize: true, mix: 1, preDelayMs: 0, ir: { kind: 'expNoise', length: 6000, seed: 12345, decay: 12, amp: 0.5 } },
    notes: '与 case2 完全成对的去周期化开启对照（GWT-CV-03）：IR 配方、输入、块长全同，仅 dePeriodize=true——IR 尾部包络（−12 e-fold 衰减至 −104dB）跌破峰值 −60dB 触发尾部指数衰减（τ≈50ms），触发点之后尾段与 case2 显著可区分（实证差异样本近六成、最大相对差 1e-2 量级）；触发点之前逐样本一致。去周期化的包络窗（10ms RMS）、−60dB 后缀判定、τ 常数任何偏差都会造成可测差异。IR 长度不因去周期化改变（分区规划不变）。',
    inputL: (n, fs) => sineSum(n, fs, [
      { freqHz: 120, amp: 0.4, phaseRad: 0 },
      { freqHz: 1900, amp: 0.25, phaseRad: Math.PI / 5 },
      { freqHz: 7000, amp: 0.15, phaseRad: Math.PI / 3 },
    ]),
    inputR: (n) => lcgNoise(n, 99002, 0.5),
  },
  {
    module: 'convolver',
    caseId: 'case4',
    sampleRate: 48000,
    blockSize: 700,
    params: { partitionSize: 256, longPartitionSize: 2048, shortRegionMs: 100, dePeriodize: false, mix: 0.8, preDelayMs: 0, ir: { kind: 'expNoise', length: 1024, seed: 777, decay: 5, amp: 0.5 } },
    notes: '非整除块长 + mix 干湿混合 + 非默认分区（GWT-CV-04）：partitionSize=256（非默认；湿路延迟 Ls=256 随之变化）、longPartitionSize=2048（k=8，但 IR 短于短区段 → 均匀 4 短分区，Pl=0）、mix=0.8 → out=(1−mix)·dry+mix·wet（dryGain=1−mix 的 f64 语义，干路不延迟）。blockSize=700 > Ls：单次调用生产多个湿块（逐样本放行 + 突发扩容语义），帧数非整除（末块 400 帧）——任意块长无丢块/无 NaN（GWT-CV-05：本模块输出与驱动 blockSize 无关，实证逐位一致）。短 expNoise IR（均匀多短分区）、dePeriodize=false。',
    inputL: (n, fs) => {
      const x = sine(n, fs, 220, 0.3)
      for (let i = 0; i < n; i++) x[i] = 0.5 + x[i]
      return x
    },
    inputR: (n, fs) => {
      const x = sine(n, fs, 330, 0.25, Math.PI / 4)
      for (let i = 0; i < n; i++) x[i] = 0.4 + x[i]
      return x
    },
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
    case 'compressor': {
      if (!modules.compressor || !modules.compressor.Compressor) throw new Error('compressor 模块加载失败')
      const comp = new modules.compressor.Compressor(sampleRate)
      comp.setParams(params)
      const useSidechain = params.sidechainEnabled === true
      return (l, r) => {
        const outL = l.slice()
        const outR = r.slice()
        if (useSidechain) {
          // sidechain 向量语义（见 specs/dsp/compressor.md §4.5）：本块原始输入的
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
      if (!modules['bass-enhancer'] || !modules['bass-enhancer'].BassEnhancer) throw new Error('bass-enhancer 模块加载失败')
      const bass = new modules['bass-enhancer'].BassEnhancer(sampleRate)
      bass.setParams(params)
      return (l, r) => {
        const outL = l.slice()
        const outR = r.slice()
        bass.processStereo(outL, outR)
        return [outL, outR]
      }
    }
    case 'mid-side': {
      if (!modules['mid-side'] || !modules['mid-side'].MidSide) throw new Error('mid-side 模块加载失败')
      // MidSide 无采样率概念（构造无参）；setParams 为位置参数接口
      const ms = new modules['mid-side'].MidSide()
      ms.setParams(params.width, params.voiceBalance)
      return (l, r) => {
        const outL = l.slice()
        const outR = r.slice()
        ms.processStereo(outL, outR)
        return [outL, outR]
      }
    }
    case 'eq-chain': {
      if (!modules['eq-chain'] || !modules['eq-chain'].EqChain) throw new Error('eq-chain 模块加载失败')
      // 驱动顺序采用引擎接线顺序（HyperSoundEngine.ts：先 setBands 后 setQCompensation；
      // specs/dsp/eq-chain.md §4.3 实证两种顺序终态逐位一致）。立体声语义（§4.4）：
      // 左右声道共享同一条级联滤波状态，每块内先整条处理 L、再整条处理 R；
      // 输出依赖 blockSize，由向量固定，两支线须按同一块长回放。
      const eq = new modules['eq-chain'].EqChain(sampleRate, params.bandCount)
      eq.setBands(params.bands)
      eq.setQCompensation(params.qCompensation === true)
      return (l, r) => {
        const outL = l.slice()
        const outR = r.slice()
        eq.processStereo(outL, outR)
        return [outL, outR]
      }
    }
    case 'fdn-reverb': {
      if (!modules['fdn-reverb'] || !modules['fdn-reverb'].FdnReverb) throw new Error('fdn-reverb 模块加载失败')
      const reverb = new modules['fdn-reverb'].FdnReverb(sampleRate)
      reverb.setParams(params)
      return (l, r) => {
        const outL = l.slice()
        const outR = r.slice()
        reverb.processStereo(outL, outR)
        return [outL, outR]
      }
    }
    case 'deesser': {
      if (!modules.deesser || !modules.deesser.Deesser) throw new Error('deesser 模块加载失败')
      const dss = new modules.deesser.Deesser(sampleRate)
      dss.setParams(params)
      const useSidechain = params.sidechainEnabled === true
      return (l, r) => {
        const outL = l.slice()
        const outR = r.slice()
        if (useSidechain) {
          // sidechain 向量语义（specs/dsp/deesser.md §4.6）：本块原始输入的单声道和派生，
          // 双精度加法、就地处理前快照；sideL 与 sideR 内容相同（本批向量不含该形态）。
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
      if (!modules['loudness-comp'] || !modules['loudness-comp'].LoudnessComp) throw new Error('loudness-comp 模块加载失败')
      const comp = new modules['loudness-comp'].LoudnessComp(sampleRate)
      comp.setParams(params)
      return (l, r) => {
        const outL = l.slice()
        const outR = r.slice()
        comp.processStereo(outL, outR)
        return [outL, outR]
      }
    }
    case 'dynamic-eq': {
      if (!modules['dynamic-eq'] || !modules['dynamic-eq'].DynamicEq) throw new Error('dynamic-eq 模块加载失败')
      // 向量 params 为模块完整形状（DynamicEqParams，specs/dsp/dynamic-eq.md §三）；
      // 输出依赖顶层驱动分块与 params.blockSize 的控制节奏耦合（§4.5 实证），
      // 两支线必须按冻结向量的同一 blockSize 回放。
      const eq = new modules['dynamic-eq'].DynamicEq(sampleRate, params)
      return (l, r) => {
        const outL = l.slice()
        const outR = r.slice()
        eq.processStereo(outL, outR)
        return [outL, outR]
      }
    }
    case 'mod-effects': {
      if (!modules['mod-effects'] || !modules['mod-effects'].DelayEffect) throw new Error('mod-effects 模块加载失败')
      // 五效果按引擎接线顺序级联（HyperSoundEngine buildStages：delay→chorus→flanger→
      // phaser→tremolo，specs/dsp/mod-effects.md §4.1）。引擎语义：五效果无条件 setParams
      // （enabled 字段被效果类自身忽略），仅 enabled 的效果参与链路，禁用级逐位旁路。
      const M = modules['mod-effects']
      const delay = new M.DelayEffect(sampleRate)
      const chorus = new M.ChorusEffect(sampleRate)
      const flanger = new M.FlangerEffect(sampleRate)
      const phaser = new M.PhaserEffect(sampleRate)
      const tremolo = new M.TremoloEffect(sampleRate)
      delay.setParams(params.delay)
      chorus.setParams(params.chorus)
      flanger.setParams(params.flanger)
      phaser.setParams(params.phaser)
      tremolo.setParams(params.tremolo)
      const chain = [
        { enabled: params.delay.enabled, run: (l, r) => delay.processStereo(l, r) },
        { enabled: params.chorus.enabled, run: (l, r) => chorus.processStereo(l, r) },
        { enabled: params.flanger.enabled, run: (l, r) => flanger.processStereo(l, r) },
        { enabled: params.phaser.enabled, run: (l, r) => phaser.processStereo(l, r) },
        { enabled: params.tremolo.enabled, run: (l, r) => tremolo.processStereo(l, r) },
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
      // 块长必须为 2 的幂；本批向量固定 blockSize = frames = N（单块驱动）。
      if (!modules.fft || !modules.fft.fft) throw new Error('fft 模块加载失败')
      const inverse = params.inverse === true
      return (l, r) => {
        const re = l.slice()
        const im = r.slice()
        modules.fft.fft(re, im, inverse)
        return [re, im]
      }
    }
    case 'convolver': {
      if (!modules.convolver || !modules.convolver.Convolver) throw new Error('convolver 模块加载失败')
      // 驱动顺序采用引擎接线顺序（HyperSoundEngine.ts 卷积混响阶段：构造(dePeriodize 选项)
      // → loadIR → setMix → setPreDelayMs → 逐块 processStereo）。
      // IR 由确定性配方生成（buildIrRecipe，specs/dsp/convolver.md §4.2）。
      const cv = new modules.convolver.Convolver(sampleRate, {
        partitionSize: params.partitionSize,
        longPartitionSize: params.longPartitionSize,
        shortRegionMs: params.shortRegionMs,
        dePeriodize: params.dePeriodize,
      })
      cv.loadIR(buildIrRecipe(params.ir), 'vector-ir')
      cv.setMix(params.mix)
      cv.setPreDelayMs(params.preDelayMs)
      return (l, r) => {
        const outL = l.slice()
        const outR = r.slice()
        cv.processStereo(outL, outR)
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
  'biquad.case4': 4000,
  'limiter.case1': 9600,
  'limiter.case2': 5000,
  'limiter.case3': 2048,
  'limiter.case4': 4800,
  'reverb-simple.case1': 8192,
  'reverb-simple.case2': 4000,
  'reverb-simple.case3': 6000,
  'compressor.case1': 4800,
  'compressor.case2': 9600,
  'compressor.case3': 4800,
  'compressor.case4': 6000,
  'bass-enhancer.case1': 6000,
  'bass-enhancer.case2': 6000,
  'bass-enhancer.case3': 6000,
  'bass-enhancer.case4': 4800,
  'mid-side.case1': 4096,
  'mid-side.case2': 4096,
  'mid-side.case3': 5000,
  'mid-side.case4': 5000,
  'eq-chain.case1': 6000,
  'eq-chain.case2': 6000,
  'eq-chain.case3': 6000,
  'eq-chain.case4': 6000,
  'fdn-reverb.case1': 6000,
  'fdn-reverb.case2': 8000,
  'fdn-reverb.case3': 6000,
  'fdn-reverb.case4': 9600,
  'deesser.case1': 6000,
  'deesser.case2': 9800,
  'deesser.case3': 9800,
  'deesser.case4': 6300,
  'loudness-comp.case1': 6000,
  'loudness-comp.case2': 9800,
  'loudness-comp.case3': 6000,
  'loudness-comp.case4': 6400,
  'dynamic-eq.case1': 6000,
  'dynamic-eq.case2': 6000,
  'dynamic-eq.case3': 9800,
  'dynamic-eq.case4': 6000,
  'mod-effects.case1': 6000,
  'mod-effects.case2': 6000,
  'mod-effects.case3': 12000,
  'mod-effects.case4': 6000,
  // fft：frames = blockSize = N = fftSize（2 的幂，单块驱动，specs/dsp/fft.md §三）；
  // 四条 case 覆盖 log2(N) 奇偶两类蝶形调度。
  'fft.case1': 4096,
  'fft.case2': 2048,
  'fft.case3': 1024,
  'fft.case4': 8192,
  'convolver.case1': 6144,
  'convolver.case2': 9800,
  'convolver.case3': 9800,
  'convolver.case4': 6000,
}

main().catch((err) => {
  console.error(String(err && err.message ? err.message : err))
  process.exitCode = 1
})
