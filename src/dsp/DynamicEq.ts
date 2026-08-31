/**
 * DynamicEq.ts —— 自适应动态均衡器(频谱包络驱动的自动混音 / 动态均衡,自研)
 *
 * 出处/许可:
 *  - "动态均衡 / 多段动态" 公开概念:按实时频谱包络对各频带自动施加增益修正
 *    (某频带能量持续偏高 → 自动压低,防掩蔽 / 自动修整;能量低 → 保持或轻微
 *    提升),类似节目跟随的"智能 EQ";dB 域软拐点压缩曲线为 DAW 压缩器通用
 *    公式(与 Compressor.ts 同公式族,行业公开形式,未复制第三方代码);
 *  - 分带网络采用"全通交叉"结构:LP = (1+A)/2、HP = (1−A)/2 共享同一个
 *    一阶全通 A,代数恒等 LP+HP = 1 ⇒ 各带信号之和恒等于输入(单位增益时
 *    精确重建、无静态染色)。该结构为经典分频思路的自研组合实现。
 *
 * 实现要点(信号流,控制延迟一个分析块):
 *   1. 分带:固定 5 带(默认交叉频率 200/800/2500/8000 Hz,每带可开关),
 *      每通道 8 个 Biquad 组成 LP/HP 交叉树,逐样本得到 5 路带信号;
 *   2. 分析:按块(blockSize 默认 128)累加各带平方能量
 *      sumsq_b = Σ(bandL_b² + bandR_b²),块末换算电平
 *      levelDb_b = 10·log10(sumsq_b / (2N) + 1e-12);
 *   3. 控制(每块一次):levelDb 与 thresholdDb 比较,软拐点 knee 内压缩
 *      reduction = over·(1 − 1/ratio)(拐点内二次插值),
 *      targetDb = targetGainDb_b − reduction,
 *      targetLin = 10^(targetDb/20),再经 strength 干湿混合为目标增益
 *      (strength=0 → 目标恒 1 → 直通);
 *   4. 增益平滑:目标增益逐样本一阶平滑(下降走 attackCoef、恢复走
 *      releaseCoef),块间无跳变、无抽吸;
 *   5. 输出:out = Σ_b gain_b·band_b 就地写回 L/R(单位增益时精确重建输入)。
 *
 * 确定性 / 实时约束:
 *  - 同输入同参数必同输出;无 Math.random / Date / console;
 *  - processStereo 内零分配(全部缓冲在构造期预分配);
 *  - strength=0 或 enabled=false 为硬直通:输出逐样本等于输入;
 *  - fs<=0 抛 Error('invalid sample rate');所有参数 clamp 防 NaN/Inf;
 *  - 输出有界:每带增益钳制在 [0, 3],交叉树单位增益时精确重建输入。
 */

import { Biquad } from './biquad'

/** 固定频带数(5 带:low / low-mid / mid / high-mid / high) */
const BAND_COUNT = 5
/** 默认交叉频率(4 个:带 i 与带 i+1 的分界) */
const DEFAULT_CROSSOVER_HZ: readonly number[] = [200, 800, 2500, 8000]
/** 频带名称(UI / 调试用) */
export const DYNAMIC_EQ_BAND_NAMES: readonly string[] = ['low', 'low-mid', 'mid', 'high-mid', 'high']
/** 每带增益钳制范围(线性,防任意参数组合下输出无界) */
const GAIN_MIN = 0
const GAIN_MAX = 3

/** 数值钳制 */
function clamp(v: number, lo: number, hi: number): number {
  return v < lo ? lo : v > hi ? hi : v
}

/** 一阶平滑系数:α = 1 − exp(−1/(τ·fs)),τ 由毫秒换算 */
function onePoleCoef(timeMs: number, fs: number, floorMs: number): number {
  const ms = Math.max(timeMs, floorMs)
  return 1 - Math.exp(-1 / ((ms / 1000) * fs))
}

/**
 * 一阶全通交叉系数:给定交叉频率 fc 返回共享全通系数 a1,以及
 * LP = (1+A)/2 → b0=b1=lp;HP = (1−A)/2 → b0=hp、b1=−hp。
 * 代数上 LP+HP = 1(精确重建);fc ∈ (0, fs/2) 时 |a1| < 1 恒稳定。
 */
function crossoverCoeffs(fc: number, fs: number): { lp: number, hp: number, a1: number } {
  const wc = (2 * Math.PI * fc) / fs
  const a1 = -Math.tan(Math.PI / 4 - wc / 2)
  return { lp: 0.5 * (1 + a1), hp: 0.5 * (1 - a1), a1 }
}

/** 单带配置(固定 5 带,不足用默认补齐) */
export interface DynamicEqBandParam {
  /** 该带是否参与动态处理(关闭则该带目标增益恒为 1,静态直通) */
  enabled: boolean
  /** 交叉频率(Hz):该带与下一带的分界;末带忽略(默认 200/800/2500/8000) */
  frequency: number
  /** 静态目标偏移 dB(目标曲线,默认 0):低于阈值时该带保持此静态增益 */
  targetGainDb?: number
}

export interface DynamicEqParams {
  /** 总开关:false 时硬直通 */
  enabled?: boolean
  /** 整体强度 0..1:0=直通,1=完全生效 */
  strength?: number
  /** 触发阈值 dB(默认 -20) */
  thresholdDb?: number
  /** 每带压缩比(默认 2):超过阈值部分按 1/ratio 斜率输出 */
  ratio?: number
  /** 软拐点 dB(默认 6):阈值附近平滑过渡 */
  kneeDb?: number
  /** 增益平滑 attack ms(默认 20):增益下降速度 */
  attackMs?: number
  /** 增益平滑 release ms(默认 200):增益恢复速度 */
  releaseMs?: number
  /** 分析块大小(默认 128):每块计算一次各带能量,控制延迟一个块 */
  blockSize?: number
  /** 固定 5 带配置(数组不足 5 项时其余带保持当前/默认配置,超出忽略) */
  bands?: DynamicEqBandParam[]
}

export class DynamicEq {
  private readonly fs: number
  private enabled = true
  private strength = 1
  private thresholdDb = -20
  private ratio = 2
  private kneeDb = 6
  private attackCoef = 0
  private releaseCoef = 0
  private blockSize = 128
  /** 每带状态(全部在构造期初始化,process 内零分配) */
  private readonly bandEnabled: boolean[] = new Array<boolean>(BAND_COUNT).fill(true)
  private readonly crossFreqs = Float64Array.from(DEFAULT_CROSSOVER_HZ)
  private readonly staticDb = new Float64Array(BAND_COUNT)
  private readonly sumsq = new Float64Array(BAND_COUNT)
  private readonly levelsDb = new Float64Array(BAND_COUNT)
  private readonly targetGains = new Float64Array(BAND_COUNT).fill(1)
  private readonly gains = new Float64Array(BAND_COUNT).fill(1)
  /** 交叉树:每通道 LP1,HP1,LP2,HP2,LP3,HP3,LP4,HP4(共 8 个 Biquad) */
  private readonly treeL: Biquad[] = []
  private readonly treeR: Biquad[] = []

  constructor(fs: number, params?: Partial<DynamicEqParams>) {
    if (!(fs > 0) || !Number.isFinite(fs)) throw new Error('invalid sample rate')
    this.fs = fs
    for (let i = 0; i < 8; i++) {
      this.treeL.push(new Biquad())
      this.treeR.push(new Biquad())
    }
    this.applyParams(params ?? {})
  }

  setParams(p: Partial<DynamicEqParams>): void {
    this.applyParams(p)
  }

  /** 参数即时生效:钳制 + 系数重算;增益/包络状态保留(避免参数变化时爆音) */
  private applyParams(p: Partial<DynamicEqParams>): void {
    const fs = this.fs
    const nyq = fs / 2
    this.enabled = p.enabled ?? this.enabled
    this.strength = clamp(p.strength ?? this.strength, 0, 1)
    this.thresholdDb = clamp(p.thresholdDb ?? this.thresholdDb, -80, 0)
    this.ratio = clamp(p.ratio ?? this.ratio, 1, 100)
    this.kneeDb = clamp(p.kneeDb ?? this.kneeDb, 0, 40)
    this.attackCoef = onePoleCoef(p.attackMs ?? this.currentAttackMs(), fs, 0.05)
    this.releaseCoef = onePoleCoef(p.releaseMs ?? this.currentReleaseMs(), fs, 1)
    this.blockSize = Math.max(16, Math.min(2048, Math.floor(p.blockSize ?? this.blockSize)))
    const bands = p.bands
    if (bands !== undefined) {
      for (let i = 0; i < BAND_COUNT; i++) {
        const b = bands[i]
        if (b !== undefined) {
          this.bandEnabled[i] = b.enabled
          this.staticDb[i] = clamp(b.targetGainDb ?? this.staticDb[i], -12, 12)
          if (i < BAND_COUNT - 1) this.crossFreqs[i] = clamp(b.frequency, 30, nyq * 0.9)
        }
      }
    }
    // 无论是否改频带都重算交叉树(构造期也必须生效,否则树保持直通系数)
    this.updateCrossover()
  }

  /** 反解当前 attack/release 毫秒(供 setParams 未指定时保持原平滑时间) */
  private currentAttackMs(): number {
    return this.attackCoef === 0 ? 20 : (-1000 / (this.fs * Math.log(1 - this.attackCoef)))
  }
  private currentReleaseMs(): number {
    return this.releaseCoef === 0 ? 200 : (-1000 / (this.fs * Math.log(1 - this.releaseCoef)))
  }

  /** 按当前交叉频率重算交叉树系数(仅 setParams 时调用) */
  private updateCrossover(): void {
    const fs = this.fs
    for (let i = 0; i < BAND_COUNT - 1; i++) {
      const { lp, hp, a1 } = crossoverCoeffs(this.crossFreqs[i], fs)
      const cl = { b0: lp, b1: lp, b2: 0, a1, a2: 0 }
      const ch = { b0: hp, b1: -hp, b2: 0, a1, a2: 0 }
      this.treeL[2 * i].setCoeffs(cl)
      this.treeL[2 * i + 1].setCoeffs(ch)
      this.treeR[2 * i].setCoeffs(cl)
      this.treeR[2 * i + 1].setCoeffs(ch)
    }
  }

  /**
   * 就地处理立体声(l/r 原地改写),内部按 blockSize 分块:
   * 每块先以当前(上一块算出的)目标增益逐样本平滑处理,块末由本块能量
   * 更新目标增益 —— 控制延迟一个分析块,增益平滑掩盖块粒度。
   */
  processStereo(l: Float32Array, r: Float32Array, frameCount?: number): void {
    if (l.length !== r.length) throw new Error('dynamiceq: L/R length mismatch')
    if (!this.enabled || this.strength <= 0) return // 硬直通:输出逐样本等于输入
    const n = Math.max(0, Math.min(Math.floor(frameCount ?? l.length), l.length, r.length))
    const block = this.blockSize
    const attack = this.attackCoef
    const release = this.releaseCoef
    const invRatio = 1 - 1 / this.ratio
    const knee = this.kneeDb
    const kneeHalf = knee * 0.5
    const twoKnee = 2 * knee
    const thr = this.thresholdDb
    const strength = this.strength
    const gains = this.gains
    const targets = this.targetGains
    const sumsq = this.sumsq
    const levels = this.levelsDb
    const bandEn = this.bandEnabled
    const staticDb = this.staticDb
    const tL = this.treeL
    const tR = this.treeR
    let pos = 0
    while (pos < n) {
      const end = Math.min(pos + block, n)
      const len = end - pos
      for (let b = 0; b < BAND_COUNT; b++) sumsq[b] = 0
      const invN = 1 / (2 * len)
      for (let i = pos; i < end; i++) {
        const xl = l[i]
        const xr = r[i]
        // —— 交叉树(逐样本):band0=LP1(x);band1=HP1→LP2;…;band4=HP4(链式残差)
        const r1l = tL[1].process(xl)
        const b0l = tL[0].process(xl)
        const r2l = tL[3].process(r1l)
        const b1l = tL[2].process(r1l)
        const r3l = tL[5].process(r2l)
        const b2l = tL[4].process(r2l)
        const r4l = tL[7].process(r3l)
        const b3l = tL[6].process(r3l)
        const b4l = r4l
        const r1r = tR[1].process(xr)
        const b0r = tR[0].process(xr)
        const r2r = tR[3].process(r1r)
        const b1r = tR[2].process(r1r)
        const r3r = tR[5].process(r2r)
        const b2r = tR[4].process(r2r)
        const r4r = tR[7].process(r3r)
        const b3r = tR[6].process(r3r)
        const b4r = r4r
        // —— 能量累加(块内分析)
        sumsq[0] += b0l * b0l + b0r * b0r
        sumsq[1] += b1l * b1l + b1r * b1r
        sumsq[2] += b2l * b2l + b2r * b2r
        sumsq[3] += b3l * b3l + b3r * b3r
        sumsq[4] += b4l * b4l + b4r * b4r
        // —— 增益平滑(逐样本一阶:下降用 attack,恢复用 release)
        const t0 = targets[0], t1 = targets[1], t2 = targets[2], t3 = targets[3], t4 = targets[4]
        let g0 = gains[0], g1 = gains[1], g2 = gains[2], g3 = gains[3], g4 = gains[4]
        g0 += (t0 < g0 ? attack : release) * (t0 - g0)
        g1 += (t1 < g1 ? attack : release) * (t1 - g1)
        g2 += (t2 < g2 ? attack : release) * (t2 - g2)
        g3 += (t3 < g3 ? attack : release) * (t3 - g3)
        g4 += (t4 < g4 ? attack : release) * (t4 - g4)
        gains[0] = g0; gains[1] = g1; gains[2] = g2; gains[3] = g3; gains[4] = g4
        // —— 输出:Σ gain_b·band_b(单位增益时精确重建输入)
        l[i] = g0 * b0l + g1 * b1l + g2 * b2l + g3 * b3l + g4 * b4l
        r[i] = g0 * b0r + g1 * b1r + g2 * b2r + g3 * b3r + g4 * b4r
      }
      // —— 块末控制:由本块能量计算下一块的目标增益(软拐点压缩 + 静态曲线 + strength)
      for (let b = 0; b < BAND_COUNT; b++) {
        const levelDb = 10 * Math.log10(sumsq[b] * invN + 1e-12)
        levels[b] = levelDb
        const over = levelDb - thr
        let reduction: number
        if (knee <= 0) {
          reduction = over > 0 ? over * invRatio : 0
        } else if (over < -kneeHalf) {
          reduction = 0
        } else if (over > kneeHalf) {
          reduction = over * invRatio
        } else {
          const x = over + kneeHalf
          reduction = (invRatio * x * x) / twoKnee
        }
        const targetDb = staticDb[b] - reduction
        const targetLin = Math.pow(10, targetDb / 20)
        const mixed = 1 + strength * (targetLin - 1)
        targets[b] = bandEn[b] ? Math.min(Math.max(mixed, GAIN_MIN), GAIN_MAX) : 1
      }
      pos = end
    }
  }

  /** 当前每带平滑增益(线性,5 项;单位增益 = 1 = 无处理) */
  getBandGains(): number[] {
    return Array.from(this.gains)
  }

  /** 最近一次分析的各带电平 dB(5 项,调试 / UI 用) */
  getBandLevelsDb(): number[] {
    return Array.from(this.levelsDb)
  }

  /** 频带名称(5 项) */
  getBandNames(): string[] {
    return DYNAMIC_EQ_BAND_NAMES.slice()
  }

  /** 复位:清空全部滤波器状态与增益/目标/电平(重放与首次一致) */
  reset(): void {
    for (let i = 0; i < this.treeL.length; i++) {
      this.treeL[i].reset()
      this.treeR[i].reset()
    }
    this.sumsq.fill(0)
    this.levelsDb.fill(0)
    this.targetGains.fill(1)
    this.gains.fill(1)
  }
}