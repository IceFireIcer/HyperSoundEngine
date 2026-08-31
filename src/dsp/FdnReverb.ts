/**
 * FdnReverb —— 算法混响(FDN,Feedback Delay Network;算法创新模块)
 *
 * 出处/许可:
 *  - 结构(N 条互质反馈延迟线 + 正交反馈矩阵 + 每线一阶低通阻尼)源自公开文献:
 *    · Jot, J.-M. (1991), "An improved digital reverberator using a feedback
 *      delay network", Proc. International Computer Music Conference (ICMC)。
 *      这是 FDN 的奠基论文:互质延迟 + 酉(正交)反馈矩阵保证稳定,并建议用
 *      Householder 反射阵构造"最大扩散"的正交混合。
 *    · Rocchesso, D. & Smith, J.O. (1997), "Circulant and elliptic feedback
 *      delay networks for artificial reverberation", IEEE Trans. Speech
 *      and Audio Processing。
 *    · Zölzer(编), "DAFX: Digital Audio Effects", 2nd ed., §2.5(FDN 小节)。
 *  - 反馈矩阵取 Householder 反射阵 H = I - (2/N)·u·uᵀ(u = [1,…,1]ᵀ):
 *    H 正交(HᵀH = I),能量不增长;且 H·v = v - (2/N)·(Σv)·u 只需 O(N),
 *    比显式矩阵乘法 O(N²) 快——正是 Jot 1991 采用的技巧。
 *    本实现为按公开思路独立编写的 TS 代码,不复制任何既有源码。
 *
 * 实现要点:
 *  - 默认 N=8 条延迟线,长度取素数(互质)→ 无共同周期 → 无金属共振音;左/右
 *    各用一套不同的素数表,构成真实立体声去相关。支持 2/4/8/16 线。
 *  - 反馈环路(每样本):各线输出 → 一阶低通(与 Freeverb 相同 damping 语义:
 *    filt = damp2·out + damp1·store)→ Householder 正交混合 → 乘反馈增益 g
 *    (roomSize clamp ≤ 0.98)→ 写回延迟线。环路往返增益 = g²·(低通 ≤1) < 1,
 *    而正交矩阵不增能 → 无条件稳定,任何输入不发散。
 *  - 输入按 1/√N 均匀注入各线(有源部分能量归一),输出取各线等权平均(1/N)。
 *  - preDelay 用独立延迟线(输入侧,与 ReverbSimple 一致),上限 1000ms。
 *  - width 立体声交叉混合公式与 ReverbSimple 一致(wet1/wet2)。
 *  - type 表提供 (roomSize, damping, delayScale) 基准,用户参数在基准附近
 *    ±0.25 微调(与 ReverbSimple 相同的"类型为基准"风格)。
 *  - 零分配:延迟缓冲 Float32Array[]、长度/位置 Int32Array 均在构造时按最大
 *    配置(16 线 × 最长延迟)预分配;setParams 只改长度系数;process 稳态零分配。
 *
 * 确定性:同输入同参数必同输出;无 Math.random、无 Date、无 console。
 */

import type { ReverbType } from '../types'

/** FDN 混响参数(与 ReverbSimple 对齐,便于未来接入引擎链) */
export interface FdnReverbParams {
  /** 空间大小 0..1(→ 反馈增益,clamp ≤0.98) */
  roomSize: number
  /** 阻尼 0..1(反馈回路一阶低通,与 Freeverb damping 同语义) */
  damping: number
  /** 湿声增益 0..4 */
  wet: number
  /** 干声增益 0..4 */
  dry: number
  /** 预延迟 ms,0..1000 */
  preDelayMs: number
  /** 立体声宽度 0..2(1=无交叉) */
  width: number
  /** 混响类型(提供基准参数表),默认 'hall' */
  type?: ReverbType
  /** 延迟线数:2/4/8/16,默认 8 */
  lines?: number
}

/** type → 基准参数表(自行定义,注释见各条目) */
interface FdnTypeTable {
  /** 基准 roomSize(0..1,→ 反馈增益) */
  roomSize: number
  /** 基准 damping(0..1) */
  damping: number
  /** 延迟长度缩放(1.0 = 标准 FDN 调音;spring 特短、stage 特长) */
  delayScale: number
}

/** type → 房间参数表(自行定义) */
const TYPE_TABLE: Record<ReverbType, FdnTypeTable> = {
  // hall:大空间长尾,反馈强、阻尼适中,延迟拉长
  hall: { roomSize: 0.7, damping: 0.4, delayScale: 1.3 },
  // room:小房间短尾,反馈弱、阻尼高(偏闷),延迟短
  room: { roomSize: 0.4, damping: 0.6, delayScale: 0.6 },
  // plate:金属板混响,反馈中等、阻尼很低(明亮),延迟偏短密度高
  plate: { roomSize: 0.6, damping: 0.2, delayScale: 0.7 },
  // spring:弹簧混响,反馈弱、阻尼极高(独特"弹簧"音色),延迟特短
  spring: { roomSize: 0.3, damping: 0.8, delayScale: 0.35 },
  // stage:舞台/厅堂,反馈中等、阻尼适中,延迟最长获得更宽声场
  stage: { roomSize: 0.55, damping: 0.5, delayScale: 1.5 },
}

// 互质(素数)延迟基底 @44.1kHz。同一表内均为不同素数 → 两两互质 → 无共同周期
// 共振(无金属感);左右各一套不同素数,保证 L/R 湿路去相关。
const DELAYS_L: Record<number, number[]> = {
  2: [499, 547],
  4: [599, 641, 677, 709],
  8: [701, 719, 733, 757, 773, 797, 811, 823],
  16: [701, 719, 733, 757, 773, 797, 811, 823, 827, 839, 853, 857, 859, 863, 877, 881],
}
const DELAYS_R: Record<number, number[]> = {
  2: [521, 563],
  4: [607, 653, 683, 727],
  8: [709, 727, 739, 761, 787, 809, 821, 829],
  16: [709, 727, 739, 761, 787, 809, 821, 829, 839, 853, 857, 859, 863, 877, 881, 883],
}

// 构造期最大配置(预分配上限)
const MAX_LINES = 16
const MAX_DELAY_BASE = 883 // 所有素数表中的最大值
const MAX_DELAY_SCALE = 1.5 // type 表最大 delayScale(stage)
const MAX_FEEDBACK = 0.98 // 反馈增益安全上限(g² < 1 → 无条件稳定)
const MAX_PREDELAY_MS = 1000

/**
 * FDN 单声道网络(左/右各持有一个;左右素数表不同 → 立体声去相关)。
 * 状态方程(每样本,对每条线 j):
 *   s_j(t+1) = g·Σ_i H_ji·LPF(s_i(t)) + (1/√N)·x(t)
 *   y(t)     = (1/N)·Σ_j s_j(t)
 */
class FdnNetwork {
  private readonly fs: number
  // 延迟线缓冲:Float32Array[] 构造时按最大延迟预分配
  private readonly buf: Float32Array[] = []
  // 每条线的长度与读写位置:Int32Array
  private readonly len: Int32Array = new Int32Array(MAX_LINES)
  private readonly pos: Int32Array = new Int32Array(MAX_LINES)
  // 每线阻尼滤波器状态(store = 上一拍低通输出,Freeverb 语义)
  private readonly store: Float32Array = new Float32Array(MAX_LINES)
  // 过程暂存(预分配复用 → process 零分配)
  private readonly out: Float32Array = new Float32Array(MAX_LINES)
  private readonly filt: Float32Array = new Float32Array(MAX_LINES)

  private n = 0
  private g = 0
  private damp1 = 0
  private damp2 = 1
  private inject = 0
  private outGain = 0

  constructor(fs: number, maxDelay: number) {
    this.fs = fs
    for (let j = 0; j < MAX_LINES; j++) this.buf.push(new Float32Array(maxDelay))
  }

  /** 配置线数、延迟长度、反馈/阻尼/注入/输出增益(只改系数,不重新分配) */
  configure(
    n: number,
    baseDelays: number[],
    delayScale: number,
    g: number,
    damp1: number,
    damp2: number,
  ): void {
    this.n = n
    this.g = g
    this.damp1 = damp1
    this.damp2 = damp2
    // 注入:能量归一(||b||=1);输出:等权平均(||c||=1/N → 稳态湿声功率
    // ≈ E(x²)/(N·(1-g²)),与输入同量级且安全有界)
    this.inject = 1 / Math.sqrt(n)
    this.outGain = 1 / n
    const scale = (delayScale * this.fs) / 44100
    for (let j = 0; j < n; j++) {
      this.len[j] = Math.max(1, Math.round(baseDelays[j] * scale))
    }
  }

  /** 单样本处理:输入 x,返回该线网络的湿输出(就地更新状态) */
  process(x: number): number {
    const n = this.n
    const { buf, len, pos, store, out, filt } = this
    // 1) 读出各线输出 + 反馈回路一阶低通(与 Freeverb damping 相同语义)
    let sum = 0
    for (let j = 0; j < n; j++) {
      const o = buf[j][pos[j]]
      out[j] = o
      const f = o * this.damp2 + store[j] * this.damp1
      filt[j] = f
      store[j] = f
      sum += f
    }
    // 2) Householder 正交混合 + 反馈增益:(H·filt)_j = filt_j - (2/N)·Σfilt
    const u = (2 / n) * sum
    for (let j = 0; j < n; j++) {
      const b = buf[j]
      const p = pos[j]
      b[p] = this.inject * x + this.g * (filt[j] - u)
      let np = p + 1
      if (np >= len[j]) np = 0
      pos[j] = np
    }
    // 3) 输出:各线等权平均
    let y = 0
    for (let j = 0; j < n; j++) y += out[j]
    return y * this.outGain
  }

  reset(): void {
    for (let j = 0; j < MAX_LINES; j++) {
      this.buf[j].fill(0)
      this.pos[j] = 0
      this.store[j] = 0
    }
  }
}

/** FDN 算法混响(立体声,就地处理) */
export class FdnReverb {
  private readonly fs: number
  private readonly left: FdnNetwork
  private readonly right: FdnNetwork

  // preDelay 独立延迟线(输入侧,左右各一;注意左右各持独立位置指针——
  // 若共用一个位置,每样本会被推进两次,有效延迟将减半)
  private readonly preDelayL: Float32Array
  private readonly preDelayR: Float32Array
  private preDelayPosL = 0
  private preDelayPosR = 0
  private preDelayLen = 0

  // 混音参数(wet/dry + width 交叉,与 ReverbSimple 相同公式)
  private wet1 = 0
  private wet2 = 0
  private dry = 0
  private lineCount = 8

  constructor(fs: number) {
    if (fs <= 0 || !Number.isFinite(fs)) {
      throw new Error('invalid sample rate')
    }
    this.fs = fs
    // 预分配最大延迟:最长素数基底 × 最大 delayScale × fs/44100
    const maxDelay = Math.ceil((MAX_DELAY_BASE * MAX_DELAY_SCALE * fs) / 44100) + 2
    this.left = new FdnNetwork(fs, maxDelay)
    this.right = new FdnNetwork(fs, maxDelay)
    // preDelay 上限 1000ms
    this.preDelayL = new Float32Array(Math.ceil(fs) + 1)
    this.preDelayR = new Float32Array(Math.ceil(fs) + 1)
  }

  setParams(p: FdnReverbParams): void {
    const t = TYPE_TABLE[p.type ?? 'hall'] ?? TYPE_TABLE.hall
    const n = normalizeLines(p.lines)

    // type 提供基准,用户参数在基准附近 ±0.25 微调(中性 0.5 时即类型本身)
    const effRoom = Math.min(MAX_FEEDBACK, Math.max(0, t.roomSize + (clamp01(p.roomSize) - 0.5) * 0.5))
    const effDamp = Math.min(0.99, Math.max(0.01, t.damping + (clamp01(p.damping) - 0.5) * 0.5))

    // wet/dry + width 交叉混合(与 ReverbSimple 相同)
    const wet = Math.min(4, Math.max(0, p.wet))
    const width = Math.min(2, Math.max(0, p.width))
    this.wet1 = wet * (width / 2 + 0.5)
    this.wet2 = wet * ((1 - width) / 2)
    this.dry = Math.min(4, Math.max(0, p.dry))

    // preDelay
    const pdMs = Math.min(MAX_PREDELAY_MS, Math.max(0, p.preDelayMs))
    this.preDelayLen = Math.round((pdMs * this.fs) / 1000)

    // 延迟长度:素数基底 × type.delayScale × fs/44100(互质属性在取整后依然近似保持)
    this.left.configure(n, DELAYS_L[n], t.delayScale, effRoom, effDamp, 1 - effDamp)
    this.right.configure(n, DELAYS_R[n], t.delayScale, effRoom, effDamp, 1 - effDamp)

    // 线数结构变化:清空状态,避免残留数据
    if (n !== this.lineCount) {
      this.lineCount = n
      this.reset()
    }
  }

  /** 就地处理立体声;out = dry·in + 湿路交叉混合(FDN 结构) */
  processStereo(l: Float32Array, r: Float32Array, frameCount?: number): void {
    const B = Math.max(0, Math.min(Math.floor(frameCount ?? l.length), l.length, r.length))
    for (let i = 0; i < B; i++) {
      const xl = l[i]
      const xr = r[i]
      // preDelay(输入侧)
      const dl = this.delayPush(this.preDelayL, xl, this.preDelayPosL)
      this.preDelayPosL = this.advancePos(this.preDelayPosL, this.preDelayL.length)
      const dr = this.delayPush(this.preDelayR, xr, this.preDelayPosR)
      this.preDelayPosR = this.advancePos(this.preDelayPosR, this.preDelayR.length)
      // 左右独立 FDN 网络(不同素数表 → 去相关)
      const wetL = this.left.process(dl)
      const wetR = this.right.process(dr)
      // wet/dry + width 交叉混合
      l[i] = xl * this.dry + wetL * this.wet1 + wetR * this.wet2
      r[i] = xr * this.dry + wetR * this.wet1 + wetL * this.wet2
    }
  }

  reset(): void {
    this.left.reset()
    this.right.reset()
    this.preDelayL.fill(0)
    this.preDelayR.fill(0)
    this.preDelayPosL = 0
    this.preDelayPosR = 0
  }

  /** 环形延迟线:写入 x,返回 preDelayLen 前的样本(preDelay=0 时恒等) */
  private delayPush(line: Float32Array, x: number, pos: number): number {
    if (this.preDelayLen === 0) return x
    const size = line.length
    let readPos = pos - this.preDelayLen
    if (readPos < 0) readPos += size
    const out = line[readPos]
    line[pos] = x
    return out
  }

  /** 环形延迟线写后位置前进(带环绕) */
  private advancePos(pos: number, size: number): number {
    let np = pos + 1
    if (np >= size) np = 0
    return np
  }
}

/** 线数校验:仅允许 2/4/8/16(素数表齐备),默认 8 */
function normalizeLines(v: number | undefined): number {
  const n = v === undefined ? 8 : Math.trunc(v)
  if (n !== 2 && n !== 4 && n !== 8 && n !== 16) {
    throw new Error(`FdnReverb: lines 必须为 2/4/8/16,收到 ${v}`)
  }
  return n
}

function clamp01(v: number): number {
  return v < 0 ? 0 : v > 1 ? 1 : v
}
