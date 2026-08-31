/**
 * Convolver —— 非均匀分区卷积混响 + IR 去周期化（模块 9）
 *
 * 出处/许可：
 *  - 分区卷积（partitioned convolution, overlap-add）思路源自
 *    W. Gardner《Efficient Convolution without Input-Output Delay》(JAES 1995)
 *    与 DAFX-03 论文（Wefers 等，分区卷积混响经典方案，
 *    https://www.dafx.de/paper-archive/2003/DAFX03_Paper_Wefers.pdf），
 *    以及 Rust fft-convolver(MIT) 的分区调度思路（本实现为独立推导的自研代码）。
 *  - 非均匀分区（non-uniform partitioned convolution）：IR 前部（瞬态/听感关键段）
 *    用短分区保证低延迟与高频分辨率，后部（长尾）用长分区减少 FFT 次数；
 *    两级分区均按 Gardner 分区卷积语义在同一个 outAccum 上 overlap-add，
 *    数学上严格等价于完整线性卷积（本实现为自研推导）。
 *  - FFT 内核使用本项目 src/dsp/fft.ts（自研蝶形实现，参考 kissfft 思路，BSD-3；
 *    接口 fft(real, imag, inverse) 稳定，内部基-2/基-4 升级不影响本模块）。
 *  - IR 去周期化（尾部指数衰减窗）为本项目自研方法（技术文档 §2.1）。
 *
 * 实现要点：
 *  - 两级分区：最短分区长 Ls = partitionSize（默认 512，与旧版一致；getLatencySamples()
 *    与流式湿路延迟都等于 Ls），长分区长 Ll = longPartitionSize（默认 4096 = 8×Ls，为 Ls 的
 *    2 的幂整数倍，k = Ll/Ls）。IR 前 shortRegionMs（默认 100ms）用短分区（Ps 块，
 *    FFT 尺寸 Ns = 2·Ls），其余用长分区（Pl 块，FFT 尺寸 Nl = 2·Ll）。
 *  - 输入按 Ls 分组进入（湿路缓冲延迟 = Ls，getLatencySamples() 返回 Ls）；
 *    每 k 个短输入块累积出一个长输入块，才做一次长 FFT（频率为 1/(k·Ls) 块），
 *    长分区贡献写入 outAccum 的偏移 (Ps + p·k - k + 1)·Ls（前半）与
 *    (Ps + p·k + 1)·Ls（后半），与短分区贡献叠加后逐块左移输出。
 *    Ps ≥ k-1 时所有长分区偏移非负（loadIR 内保证）。
 *  - 分区数折算：P_total = Ps + Pl·k（按短块粒度），outAccum 长度 = (P_total+2)·Ls。
 *  - 去周期化：从 IR 能量包络峰值后 -60dB 点起乘 exp 衰减（τ≈50ms），
 *    消除尾部硬截断导致的周期伪影。
 *  - process() 用同一分区方案（短块 + 长块）直接 overlap-add 到输出数组，
 *    与 processStereo 数学等价（流式延迟 Ls 后逐样本一致）。
 *
 * 确定性：同输入同 IR 同参数必同输出；无随机、无 Date、无 console。
 * 实时安全：loadIR 允许分配（非实时路径）；processStereo 稳态零分配（全部缓冲预分配）。
 */

import { fft, nextPow2 } from './fft'

/** 构造选项：短分区长、长分区长、短区段时长（ms）与是否对 IR 做去周期化（默认 true） */
export interface ConvolverOptions {
  /** 最短分区长（默认 512，与旧版一致）；同时决定湿路延迟 getLatencySamples() */
  partitionSize?: number
  /** 长分区长（默认 4096 = 8×partitionSize），必须是 partitionSize 的 2 的幂整数倍，否则退化为均匀 */
  longPartitionSize?: number
  /** IR 前部用短分区的时长（默认 100ms） */
  shortRegionMs?: number
  dePeriodize?: boolean
}

export class Convolver {
  private readonly fs: number
  private readonly partitionSize: number
  private readonly longPartitionSize: number
  private readonly shortRegionSamples: number
  private readonly k: number
  private readonly dePeriodize: boolean

  private irLoaded = false
  /** IR 去周期化后的长度 M */
  private irLength = 0
  private irName: string | null = null
  /** 短分区数 Ps（覆盖 IR 前部） */
  private numShort = 0
  /** 长分区数 Pl（覆盖 IR 尾部；Pl=0 时退化为均匀分区） */
  private numLong = 0
  /** 长分区起点（IR 样本索引 = Ps·Ls） */
  private longStart = 0
  /** 短 FFT 长度 Ns = nextPow2(2·Ls) */
  private shortFftSize = 0
  /** 长 FFT 长度 Nl = nextPow2(2·Ll) */
  private longFftSize = 0

  /** 短分区预计算频谱（实部/虚部），长度 Ps*Ns */
  private shortSpecReal: Float32Array = new Float32Array(0)
  private shortSpecImag: Float32Array = new Float32Array(0)
  /** 长分区预计算频谱（实部/虚部），长度 Pl*Nl */
  private longSpecReal: Float32Array = new Float32Array(0)
  private longSpecImag: Float32Array = new Float32Array(0)

  /** 干湿混合 0..1：out = (1-mix)·dry + mix·wet */
  private mix = 1
  private preDelaySamples = 0

  // ---- 流式（processStereo）状态，全部预分配 ----
  private inputBlockL: Float32Array = new Float32Array(0)
  private inputBlockR: Float32Array = new Float32Array(0)
  private inputPos = 0
  /** 长输入块累积（每 k 个短块填满 Ll 样本），每通道独立 */
  private longInL: Float32Array = new Float32Array(0)
  private longInR: Float32Array = new Float32Array(0)
  // outAccum 每通道独立：两通道串行处理且各左移一次，
  // 共用累加器会导致分区历史被后处理通道提前消耗（湿路内容错位/丢失）
  private outAccumL: Float32Array = new Float32Array(0)
  private outAccumR: Float32Array = new Float32Array(0)
  private pendingWetL: Float32Array = new Float32Array(0)
  private pendingWetR: Float32Array = new Float32Array(0)
  private pendingLen = 0
  private pendingPos = 0
  private wetDelayL: Float32Array = new Float32Array(0)
  private wetDelayR: Float32Array = new Float32Array(0)
  private wetDelayPos = 0
  /** 已送入的输入样本总数（累计，仅统计用） */
  private totalIn = 0
  /** 已放行的湿路样本总数 */
  private totalWetOut = 0
  /** 已完成的输入块数（块完成时 +1）：湿路放行的"已产出"依据 */
  private completedBlocks = 0
  /** 已输出的样本总数（跨调用累计）：湿路放行的"位置"依据（逐样本，支持任意块长） */
  private totalOut = 0
  private maxFrames = 0
  private explicitlyPrepared = false

  // ---- 工作缓冲（复用，零分配） ----
  /** 短输入 FFT 工作缓冲（Ns） */
  private shortWorkReal: Float32Array = new Float32Array(0)
  private shortWorkImag: Float32Array = new Float32Array(0)
  /** 短分区复乘/IFFT 缓冲（Ns） */
  private prodShortReal: Float32Array = new Float32Array(0)
  private prodShortImag: Float32Array = new Float32Array(0)
  /** 长输入 FFT 工作缓冲（Nl） */
  private longWorkReal: Float32Array = new Float32Array(0)
  private longWorkImag: Float32Array = new Float32Array(0)
  /** 长分区复乘/IFFT 缓冲（Nl） */
  private prodLongReal: Float32Array = new Float32Array(0)
  private prodLongImag: Float32Array = new Float32Array(0)

  constructor(fs: number, opts?: ConvolverOptions) {
    if (fs <= 0 || !Number.isFinite(fs)) {
      throw new Error('invalid sample rate')
    }
    this.fs = fs
    // 最短分区长（湿路延迟）= 传入的 partitionSize；默认 512（与旧版一致，勿改为 256）
    let L = opts && opts.partitionSize !== undefined ? Math.round(opts.partitionSize) : 512
    if (!Number.isFinite(L) || L < 1) L = 512
    // 分区长取合理范围 [32, 8192]（过小 FFT 开销大、过大延迟高）
    this.partitionSize = Math.min(8192, Math.max(32, L))
    // 长分区长：默认 4096（= 8×Ls，Ls 的 2 的幂倍数，k>=1；k=1 退化为均匀分区）。
    // 长分区必须是最短分区长的整数倍（约束：延迟=partitionSize 语义不变）
    let wantLl = opts && opts.longPartitionSize !== undefined ? Math.round(opts.longPartitionSize) : 4096
    if (!Number.isFinite(wantLl) || wantLl < 1) wantLl = 4096
    let k = 1
    if (wantLl > this.partitionSize) {
      // k = 2^ceil(log2(wantLl / Ls))，保证 Ll = Ls·k 且 k 为 2 的幂
      let ratio = wantLl / this.partitionSize
      let pow = 1
      while (pow < ratio) pow <<= 1
      k = Math.max(1, pow)
    }
    this.k = k
    this.longPartitionSize = this.partitionSize * k
    // 短区段样本数（默认 100ms）
    let sms = opts && opts.shortRegionMs !== undefined ? Math.round(opts.shortRegionMs) : 100
    if (!Number.isFinite(sms) || sms < 0) sms = 100
    this.shortRegionSamples = Math.round((Math.min(5000, sms) / 1000) * fs)
    this.dePeriodize = opts ? opts.dePeriodize !== false : true

    // 延迟线按最大 1s 预分配（preDelayMs 上限 1000ms）
    const maxDelay = fs
    this.wetDelayL = new Float32Array(maxDelay)
    this.wetDelayR = new Float32Array(maxDelay)
  }

  /**
   * 载入单声道 IR。dePeriodize=true 时先做去周期化（尾部指数衰减窗）。
   * 空 / 全零 / 非法 IR 抛 Error。
   */
  loadIR(ir: Float32Array, irName?: string): void {
    if (!ir || ir.length === 0) {
      throw new Error('invalid impulse response: empty')
    }
    // 校验：有限值且非全零
    let anyNonZero = false
    for (let i = 0; i < ir.length; i++) {
      const v = ir[i]
      if (!Number.isFinite(v)) {
        throw new Error('invalid impulse response: contains NaN/Infinity')
      }
      if (v !== 0) anyNonZero = true
    }
    if (!anyNonZero) {
      throw new Error('invalid impulse response: all zero')
    }

    const Ls = this.partitionSize
    const k = this.k
    const src = this.dePeriodize ? this.dePeriodizeIR(ir) : ir
    const M = src.length

    // ---- 非均匀分区规划 ----
    // 短分区覆盖 IR 前部（shortRegionSamples 或全部 IR，取较短者）
    let Ps = Math.max(1, Math.ceil(this.shortRegionSamples / Ls))
    // 保证长分区贡献偏移非负：Ps >= k-1（长分区前半写入偏移 = (Ps - k + 1)·Ls）
    if (Ps < k - 1) Ps = Math.max(1, k - 1)
    const longStart = Ps * Ls
    // 长分区覆盖 IR[longStart..M)
    let Pl = 0
    if (longStart < M) {
      Pl = Math.max(1, Math.ceil((M - longStart) / this.longPartitionSize))
    }
    // 若 IR 实际短于短区段规划，收敛 Ps 到实际分区数（Pl=0 时退化为均匀）
    if (Pl === 0) {
      Ps = Math.max(1, Math.ceil(M / Ls))
    }
    // longStart 随 Ps 收敛同步更新（Pl=0 时长分区路径不使用，但保持状态一致）
    const longStartFinal = Ps * Ls

    const Ns = nextPow2(2 * Ls)
    const Nl = nextPow2(2 * this.longPartitionSize)
    const PTotal = Ps + Pl * k // 按短块粒度折算的总分区数

    this.irLength = M
    this.irName = irName !== undefined ? irName : null
    this.numShort = Ps
    this.numLong = Pl
    this.longStart = longStartFinal
    this.shortFftSize = Ns
    this.longFftSize = Nl

    // 预计算短分区频谱（loadIR 非实时路径，允许分配）
    this.shortSpecReal = new Float32Array(Ps * Ns)
    this.shortSpecImag = new Float32Array(Ps * Ns)
    const workR = new Float32Array(Math.max(Ns, Nl))
    const workI = new Float32Array(Math.max(Ns, Nl))
    for (let p = 0; p < Ps; p++) {
      workR.fill(0)
      workI.fill(0)
      const base = p * Ls
      const count = Math.min(Ls, M - base)
      for (let j = 0; j < count; j++) workR[j] = src[base + j]
      fft(workR.subarray(0, Ns), workI.subarray(0, Ns), false)
      this.shortSpecReal.set(workR.subarray(0, Ns), p * Ns)
      this.shortSpecImag.set(workI.subarray(0, Ns), p * Ns)
    }
    // 预计算长分区频谱
    this.longSpecReal = new Float32Array(Pl * Nl)
    this.longSpecImag = new Float32Array(Pl * Nl)
    for (let p = 0; p < Pl; p++) {
      workR.fill(0)
      workI.fill(0)
      const base = longStart + p * this.longPartitionSize
      const count = Math.min(this.longPartitionSize, M - base)
      for (let j = 0; j < count; j++) workR[j] = src[base + j]
      fft(workR.subarray(0, Nl), workI.subarray(0, Nl), false)
      this.longSpecReal.set(workR.subarray(0, Nl), p * Nl)
      this.longSpecImag.set(workI.subarray(0, Nl), p * Nl)
    }

    // （重新）分配流式缓冲与工作缓冲
    const accLen = Math.max((PTotal + 2) * Ls, this.pendingCapacity(Ls))
    this.inputBlockL = new Float32Array(Ls)
    this.inputBlockR = new Float32Array(Ls)
    this.longInL = new Float32Array(this.longPartitionSize)
    this.longInR = new Float32Array(this.longPartitionSize)
    this.outAccumL = new Float32Array(accLen)
    this.outAccumR = new Float32Array(accLen)
    // pending 缓冲：队列容量 = (PTotal+2)·Ls（在途块 + 一个正在生成的块 + 余量）
    this.pendingWetL = new Float32Array(accLen)
    this.pendingWetR = new Float32Array(accLen)
    this.shortWorkReal = new Float32Array(Ns)
    this.shortWorkImag = new Float32Array(Ns)
    this.prodShortReal = new Float32Array(Ns)
    this.prodShortImag = new Float32Array(Ns)
    this.longWorkReal = new Float32Array(Nl)
    this.longWorkImag = new Float32Array(Nl)
    this.prodLongReal = new Float32Array(Nl)
    this.prodLongImag = new Float32Array(Nl)

    this.inputPos = 0
    this.pendingLen = 0
    this.pendingPos = 0
    this.totalIn = 0
    this.totalWetOut = 0
    this.completedBlocks = 0
    this.totalOut = 0
    this.outAccumL.fill(0)
    this.outAccumR.fill(0)
    this.irLoaded = true
  }

  prepare(maxFrames: number): void {
    const frames = Number.isFinite(maxFrames) ? Math.max(0, Math.floor(maxFrames)) : 0
    this.explicitlyPrepared = frames > 0
    this.ensurePendingCapacity(frames)
  }

  private ensurePendingCapacity(frames: number): void {
    if (frames <= this.maxFrames) return
    this.maxFrames = frames
    if (!this.irLoaded) return
    const capacity = Math.max(this.pendingWetL.length, this.pendingCapacity(this.partitionSize))
    if (this.pendingWetL.length < capacity) {
      this.pendingWetL = new Float32Array(capacity)
      this.pendingWetR = new Float32Array(capacity)
      this.pendingLen = 0
      this.pendingPos = 0
    }
  }

  private pendingCapacity(partitionSize: number): number {
    const produced = Math.ceil(Math.max(this.maxFrames, partitionSize) / partitionSize)
    return Math.max(3, produced + 2) * partitionSize
  }

  /** 设置干湿混合 0..1（1=纯湿） */
  setMix(mix: number): void {
    this.mix = Math.min(1, Math.max(0, mix))
  }

  /** 设置湿路预延迟 ms（0..1000） */
  setPreDelayMs(ms: number): void {
    const clamped = Math.min(1000, Math.max(0, ms))
    this.preDelaySamples = Math.round((clamped * this.fs) / 1000)
  }

  /**
   * 单声道一次完整卷积（有限块语义，从零状态开始）：
   * 返回新 Float32Array，长度 = 输入长度 + IR 尾 + preDelay 样本。
   * 未载入 IR 时抛错（调用方应先 loadIR）。
   * 用同一非均匀分区方案（短块 + 长块）直接 overlap-add，数学等价于完整线性卷积。
   */
  process(x: Float32Array): Float32Array {
    if (!this.irLoaded) {
      throw new Error('no impulse response loaded')
    }
    const Ls = this.partitionSize
    const Ll = this.longPartitionSize
    const Ps = this.numShort
    const Pl = this.numLong
    const Ns = this.shortFftSize
    const Nl = this.longFftSize
    const convLen = x.length + this.irLength - 1
    const total = convLen + this.preDelaySamples
    const out = new Float32Array(total)

    // ---- 短分区贡献（输入按 Ls 分块） ----
    const I = Math.ceil(x.length / Ls)
    for (let i = 0; i < I; i++) {
      this.shortWorkReal.fill(0)
      this.shortWorkImag.fill(0)
      const start = i * Ls
      const end = Math.min(start + Ls, x.length)
      for (let j = start; j < end; j++) this.shortWorkReal[j - start] = x[j]
      fft(this.shortWorkReal, this.shortWorkImag, false)
      for (let p = 0; p < Ps; p++) {
        const specBase = p * Ns
        for (let k = 0; k < Ns; k++) {
          const r1 = this.shortWorkReal[k]
          const i1 = this.shortWorkImag[k]
          const r2 = this.shortSpecReal[specBase + k]
          const i2 = this.shortSpecImag[specBase + k]
          this.prodShortReal[k] = r1 * r2 - i1 * i2
          this.prodShortImag[k] = r1 * i2 + i1 * r2
        }
        fft(this.prodShortReal, this.prodShortImag, true) // 逆变换（含 ÷N）
        const base1 = (i + p) * Ls
        const base2 = base1 + Ls
        for (let j = 0; j < Ls; j++) {
          const idx1 = base1 + j
          if (idx1 < total) out[idx1] += this.prodShortReal[j]
          const idx2 = base2 + j
          if (idx2 < total) out[idx2] += this.prodShortReal[Ls + j]
        }
      }
    }

    // ---- 长分区贡献（输入按 Ll 分块，每 k 个短块） ----
    const J = Math.ceil(x.length / Ll)
    for (let i = 0; i < J; i++) {
      this.longWorkReal.fill(0)
      this.longWorkImag.fill(0)
      const start = i * Ll
      const end = Math.min(start + Ll, x.length)
      for (let j = start; j < end; j++) this.longWorkReal[j - start] = x[j]
      fft(this.longWorkReal, this.longWorkImag, false)
      for (let p = 0; p < Pl; p++) {
        const specBase = p * Nl
        for (let k = 0; k < Nl; k++) {
          const r1 = this.longWorkReal[k]
          const i1 = this.longWorkImag[k]
          const r2 = this.longSpecReal[specBase + k]
          const i2 = this.longSpecImag[specBase + k]
          this.prodLongReal[k] = r1 * r2 - i1 * i2
          this.prodLongImag[k] = r1 * i2 + i1 * r2
        }
        fft(this.prodLongReal, this.prodLongImag, true)
        const base1 = this.longStart + (i + p) * Ll
        const base2 = base1 + Ll
        for (let j = 0; j < Ll; j++) {
          const idx1 = base1 + j
          if (idx1 < total) out[idx1] += this.prodLongReal[j]
          const idx2 = base2 + j
          if (idx2 < total) out[idx2] += this.prodLongReal[Ll + j]
        }
      }
    }

    // 施加 preDelay：卷积结果整体右移
    if (this.preDelaySamples > 0) {
      for (let i = convLen - 1; i >= 0; i--) out[i + this.preDelaySamples] = out[i]
      out.fill(0, 0, this.preDelaySamples)
    }
    return out
  }

  /**
   * 流式立体声就地处理（引擎实时路径）。
   * 湿路 = 非均匀分区卷积 + preDelay；干路 = 输入本身（不延迟）。
   * out[i] = (1-mix)·dry[i] + mix·wet[i]；wet 相对 dry 延迟 Ls + preDelay 样本。
   * 未载入 IR 时抛错。
   */
  processStereo(l: Float32Array, r: Float32Array, frameCount?: number): void {
    if (!this.irLoaded) {
      throw new Error('no impulse response loaded')
    }
    const B = Math.max(0, Math.min(Math.floor(frameCount ?? l.length), l.length, r.length))
    if (!this.explicitlyPrepared && B > this.maxFrames) this.ensurePendingCapacity(B)
    const Ls = this.partitionSize
    const dryGain = 1 - this.mix
    const wetGain = this.mix

    // 先喂入输入块；块满时跑分区卷积产出湿块。
    // 注意：左右声道队列是并行的（同一 pendPos/pendLen 记账），
    // 写位置必须两声道共用同一 writeAt，且 pendingLen 每块只加一次 Ls。
    // pending 是"滑动窗口"队列：写前若队尾越界则把未读内容压缩到头部（copyWithin），
    // 保证 pendingPos 永远 < 容量、未读内容连续（修复长流越界读 undefined → NaN）。
    for (let i = 0; i < B; i++) {
      this.inputBlockL[this.inputPos] = l[i]
      this.inputBlockR[this.inputPos] = r[i]
      this.inputPos++
      if (this.inputPos >= Ls) {
        const cap = this.pendingWetL.length
        if (this.pendingPos + this.pendingLen + Ls > cap) {
          // 压缩：未读内容移到头部（摊还 O(Ls)/块）
          const remain = this.pendingLen
          if (remain > 0 && this.pendingPos > 0) {
            this.pendingWetL.copyWithin(0, this.pendingPos, this.pendingPos + remain)
            this.pendingWetR.copyWithin(0, this.pendingPos, this.pendingPos + remain)
          }
          this.pendingPos = 0
          // 突发（单次调用多块产出）超出容量时动态扩容——修复 B>容量 时的越界写（静默丢块 → NaN）
          if (this.pendingLen + Ls > this.pendingWetL.length) {
            if (this.explicitlyPrepared) {
              throw new Error(`Convolver block ${B} exceeds prepared pending capacity`)
            }
            this.ensurePendingCapacity(B)
          }
        }
        const writeAt = this.pendingPos + this.pendingLen
        this.processWetBlock(this.inputBlockL, this.longInL, this.pendingWetL, writeAt, this.outAccumL)
        this.processWetBlock(this.inputBlockR, this.longInR, this.pendingWetR, writeAt, this.outAccumR)
        this.pendingLen += Ls
        this.completedBlocks++
        this.inputPos = 0
      }
    }
    this.totalIn += B

    // 再按序取出 B 个湿样本（不足补零）并与干路混合。
    // 放行约束（逐样本，支持任意块长——修复 B>Ls 丢块/发散）：
    //   位置 i 的湿输出 = 输入位置 i−Ls 的卷积 → 湿序 wetIdx = totalOut − Ls；
    //   放行条件：wetIdx ≥ 0（延迟 Ls）且 wetIdx < completedBlocks·Ls（对应输入块已产出）
    //   且 totalWetOut === wetIdx（严格按序放行，防止同一次调用内多块提前放行）。
    for (let i = 0; i < B; i++) {
      let wetL = 0
      let wetR = 0
      const wetIdx = this.totalOut - Ls
      if (
        this.pendingLen > 0 &&
        wetIdx >= 0 &&
        wetIdx < this.completedBlocks * Ls &&
        this.totalWetOut === wetIdx
      ) {
        wetL = this.pendingWetL[this.pendingPos]
        wetR = this.pendingWetR[this.pendingPos]
        this.pendingPos++
        this.pendingLen--
        this.totalWetOut++
        if (this.pendingLen === 0) this.pendingPos = 0
      }
      this.totalOut++
      // preDelay
      wetL = this.pushDelay(this.wetDelayL, wetL)
      wetR = this.pushDelay(this.wetDelayR, wetR)
      l[i] = dryGain * l[i] + wetGain * wetL
      r[i] = dryGain * r[i] + wetGain * wetR
    }
  }

  /** 湿路引入的延迟（样本数）= 短分区长 Ls（块缓冲延迟），引擎可据此补偿 */
  getLatencySamples(): number {
    return this.partitionSize
  }

  reset(): void {
    this.inputPos = 0
    this.pendingLen = 0
    this.pendingPos = 0
    this.totalIn = 0
    this.totalWetOut = 0
    this.completedBlocks = 0
    this.totalOut = 0
    this.wetDelayPos = 0
    if (this.outAccumL.length > 0) {
      this.outAccumL.fill(0)
      this.outAccumR.fill(0)
    }
    if (this.pendingWetL.length > 0) {
      this.pendingWetL.fill(0)
      this.pendingWetR.fill(0)
    }
    if (this.wetDelayL.length > 0) {
      this.wetDelayL.fill(0)
      this.wetDelayR.fill(0)
    }
    if (this.inputBlockL.length > 0) {
      this.inputBlockL.fill(0)
      this.inputBlockR.fill(0)
    }
    if (this.longInL.length > 0) {
      this.longInL.fill(0)
      this.longInR.fill(0)
    }
  }

  /** 当前 IR 名称（未载入返回 null） */
  getIrName(): string | null {
    return this.irName
  }

  // ---------------------------------------------------------------- 内部

  /**
   * 处理一个完整短输入块（Ls 样本）：
   * 1) 短 FFT（Ns）→ 与 Ps 个短分区复乘、IFFT、overlap-add 到 outAccum（偏移 p·Ls / (p+1)·Ls）；
   * 2) 长输入块累积：本块复制进 longIn[blockIdx%k 位置]；当第 k 个短块（长块满）时
   *    做长 FFT（Nl）→ 与 Pl 个长分区复乘、IFFT，overlap-add 到 outAccum
   *    （偏移 (Ps+p·k−k+1)·Ls / (Ps+p·k+1)·Ls）；
   * 3) 取出 outAccum[0..Ls)（= 输出块）写入 pending[writeAt..writeAt+Ls)，左移 outAccum。
   * 注意：左右声道队列并行共享同一记账（pendingPos/pendingLen），
   * 写位置由调用方统一计算（writeAt = pendingPos + pendingLen），
   * pendingLen 由调用方在两次调用后只加一次 Ls（此处不记账）。
   * longIn 为通道独立长输入缓冲（长分区贡献跨 k 个短块累积）。
   * outAccum 为通道独立累加器，块处理前**不能** fill(0)——
   * 上一块左移后保留的 [0..(P_total)·Ls) 正是各分区历史贡献（Gardner 分区卷积语义）。
   */
  private processWetBlock(
    blk: Float32Array,
    longIn: Float32Array,
    pending: Float32Array,
    writeAt: number,
    outAccum: Float32Array,
  ): void {
    const Ls = this.partitionSize
    const Ps = this.numShort
    const Pl = this.numLong
    const Ns = this.shortFftSize
    const Nl = this.longFftSize
    const k = this.k
    const blockIdx = this.completedBlocks // 当前块号（0-based）

    // 长输入块累积（仅在存在长分区时）
    if (Pl > 0) {
      const longPos = (blockIdx % k) * Ls
      for (let j = 0; j < Ls; j++) longIn[longPos + j] = blk[j]
      // 长块满（第 k 个短块完成）：长 FFT + 长分区
      if (blockIdx % k === k - 1) {
        this.longWorkReal.fill(0)
        this.longWorkImag.fill(0)
        this.longWorkReal.set(longIn)
        fft(this.longWorkReal, this.longWorkImag, false)
        const Ll = this.longPartitionSize
        const longStart = this.longStart
        for (let p = 0; p < Pl; p++) {
          const specBase = p * Nl
          for (let kk = 0; kk < Nl; kk++) {
            const r1 = this.longWorkReal[kk]
            const i1 = this.longWorkImag[kk]
            const r2 = this.longSpecReal[specBase + kk]
            const i2 = this.longSpecImag[specBase + kk]
            this.prodLongReal[kk] = r1 * r2 - i1 * i2
            this.prodLongImag[kk] = r1 * i2 + i1 * r2
          }
          fft(this.prodLongReal, this.prodLongImag, true)
          // 长分区 p 贡献写入 outAccum：
          //   前半起点 = (Ps + p·k − k + 1)·Ls（长输入块 j 完成时输出块号 = (j+1)·k−1，
          //   卷积起点输出位置 = (j+p)·Ll + Ps·Ls = (j·k + p·k + Ps)·Ls）
          //   后半起点 = 前半 + Ll
          const base1 = (longStart + p * Ll) - (k - 1) * Ls // = (Ps + p·k − k + 1)·Ls
          const base2 = base1 + Ll
          for (let j = 0; j < Ll; j++) {
            outAccum[base1 + j] += this.prodLongReal[j]
            outAccum[base2 + j] += this.prodLongReal[Ll + j]
          }
        }
      }
    }

    // 短 FFT + 短分区（注意 outAccum 跨块累加，见上方注释）
    this.shortWorkReal.fill(0)
    this.shortWorkImag.fill(0)
    this.shortWorkReal.set(blk)
    fft(this.shortWorkReal, this.shortWorkImag, false)
    for (let p = 0; p < Ps; p++) {
      const specBase = p * Ns
      for (let kk = 0; kk < Ns; kk++) {
        const r1 = this.shortWorkReal[kk]
        const i1 = this.shortWorkImag[kk]
        const r2 = this.shortSpecReal[specBase + kk]
        const i2 = this.shortSpecImag[specBase + kk]
        this.prodShortReal[kk] = r1 * r2 - i1 * i2
        this.prodShortImag[kk] = r1 * i2 + i1 * r2
      }
      fft(this.prodShortReal, this.prodShortImag, true)
      const base1 = p * Ls
      const base2 = base1 + Ls
      for (let j = 0; j < Ls; j++) {
        outAccum[base1 + j] += this.prodShortReal[j]
        outAccum[base2 + j] += this.prodShortReal[Ls + j]
      }
    }

    for (let j = 0; j < Ls; j++) pending[writeAt + j] = outAccum[j]

    // 左移：块 1.. → 0..，尾部清零（为下一块保留各分区历史贡献）
    const len = outAccum.length
    outAccum.copyWithin(0, Ls, len)
    outAccum.fill(0, len - Ls, len)
  }

  /** 环形延迟线：写入 x，返回 preDelaySamples 前的样本（preDelay=0 直接返回 x） */
  private pushDelay(line: Float32Array, x: number): number {
    if (this.preDelaySamples === 0) return x
    const size = line.length
    let readPos = this.wetDelayPos - this.preDelaySamples
    if (readPos < 0) readPos += size
    const out = line[readPos]
    line[this.wetDelayPos] = x
    this.wetDelayPos++
    if (this.wetDelayPos >= size) this.wetDelayPos = 0
    return out
  }

  /**
   * IR 去周期化：检测能量包络峰值，从峰值后 -60dB 点起乘 exp 衰减（τ≈50ms）。
   * 返回新数组（不改动调用方传入的 IR）。
   */
  private dePeriodizeIR(ir: Float32Array): Float32Array {
    const M = ir.length
    const out = new Float32Array(M)
    out.set(ir)
    const W = Math.max(4, Math.round(0.01 * this.fs)) // 10ms 包络窗
    const half = W >> 1

    // 能量包络（移动平均 RMS）
    let peakIdx = 0
    let peakVal = -1
    for (let n = 0; n < M; n++) {
      let sum = 0
      const lo = Math.max(0, n - half)
      const hi = Math.min(M, n + half + 1)
      const cnt = hi - lo
      for (let j = lo; j < hi; j++) sum += ir[j] * ir[j]
      const env = Math.sqrt(sum / cnt)
      if (env > peakVal) {
        peakVal = env
        peakIdx = n
      }
    }
    if (peakVal <= 1e-12) return out // 极静 IR（loadIR 已挡全零，防御性分支）

    // -60dB 点：包络最后一次高于峰值-60dB 之后的第一个样本（此后包络保持低于阈值）。
    // 用"后缀"判定而非"首次低于"，避免稀疏 IR（如延迟冲激）被误衰减。
    const threshold = peakVal * 1e-3
    let lastAbove = peakIdx
    for (let n = peakIdx; n < M; n++) {
      let sum = 0
      const lo = Math.max(0, n - half)
      const hi = Math.min(M, n + half + 1)
      const cnt = hi - lo
      for (let j = lo; j < hi; j++) sum += ir[j] * ir[j]
      if (Math.sqrt(sum / cnt) > threshold) lastAbove = n
    }
    const n0 = lastAbove + 1
    if (n0 >= M) return out // 尾部未掉到 -60dB 以下，无需处理

    // 从 n0 起乘 exp 衰减，τ≈50ms
    const tau = 0.05 * this.fs
    for (let n = n0; n < M; n++) {
      out[n] *= Math.exp(-(n - n0) / tau)
    }
    return out
  }
}
