/**
 * ReverbSimple —— 算法混响（Freeverb 类，模块 10）
 *
 * 出处/许可：
 *  - 结构（8 立体声梳状 + 4 全通、damping 反馈低通、wet/dry/width 混音）源自
 *    Jezar 的 Freeverb（公有领域，https://freeverb3vst.osdn.jp/ 及
 *    musicdsp.org 上的原始实现），并参考 Perry Cook 的 stk FreeVerb（MIT）
 *    思路。本实现为按公开结构独立编写的 TS 代码。
 *  - type 房间参数表为本项目自行定义并注释（hall/room/plate/spring/stage），
 *    表中 (roomSize, damping) 为各类型的基准特性，与用户参数按 50% 权重混合。
 *
 * 实现要点：
 *  - 8 个立体声梳状滤波器（左右各 4，延迟互质防共振）+ 4 个全通滤波器
 *    （反馈系数 0.5，级联于梳状和之后）。
 *  - 梳状反馈 = effRoomSize（clamp 0..0.98，保证稳定不发散）；
 *    反馈回路一阶低通系数 = effDamping。
 *  - 湿路宽度 width：wet1 = wet·(width/2+0.5)，wet2 = wet·((1-width)/2)，
 *    立体声交叉混合（width=1 无交叉，width=0 单声道化）。
 *  - preDelay 用独立延迟线实现。
 *  - 延迟长度按 fs/44100 缩放，type 提供 delayScale（spring 特短、stage 特长）。
 *  - 热路径优化：processStereo 内把 comb/allpass/preDelay 函数体全部内联，
 *    每样本 8 comb + 4 allpass + 2 preDelay 调用与属性访问消除；字段在循环外
 *    缓存为局部变量，运算顺序与内联前逐位一致（数学完全等价，无浮点差异）。
 *
 * 确定性：同输入同参数必同输出；无随机、无 Date、无 console。
 */

import type { ReverbType } from '../types'

/** 算法混响参数（5 种 type） */
export interface ReverbSimpleParams {
  roomSize: number
  damping: number
  wet: number
  dry: number
  preDelayMs: number
  width: number
  type: ReverbType
}

interface ReverbTypeTable {
  /** 基准 roomSize（0..1） */
  roomSize: number
  /** 基准 damping（0..1） */
  damping: number
  /** 延迟长度缩放（1.0=标准 Freeverb 调音） */
  delayScale: number
}

/** type → 房间参数表（自行定义，注释见各条目） */
const TYPE_TABLE: Record<ReverbType, ReverbTypeTable> = {
  // hall：大空间长尾，反馈较强、阻尼适中，标准延迟
  hall: { roomSize: 0.7, damping: 0.4, delayScale: 1.0 },
  // room：小房间短尾，反馈弱、阻尼高（偏闷）
  room: { roomSize: 0.4, damping: 0.6, delayScale: 0.8 },
  // plate：金属板混响，反馈中等、阻尼很低（明亮），延迟偏短密度高
  plate: { roomSize: 0.6, damping: 0.2, delayScale: 0.7 },
  // spring：弹簧混响，反馈弱、阻尼极高（独特"弹簧"音色），延迟特短
  spring: { roomSize: 0.3, damping: 0.8, delayScale: 0.5 },
  // stage：舞台/厅堂，反馈中等、阻尼适中，延迟拉长获得更宽声场
  stage: { roomSize: 0.5, damping: 0.5, delayScale: 1.2 },
}

// 标准 Freeverb 梳状延迟（左右各 4，@44.1kHz，互质）
const COMB_DELAYS_L = [1116, 1188, 1277, 1356]
const COMB_DELAYS_R = [1101, 1173, 1256, 1344]
// 标准 Freeverb 全通延迟（@44.1kHz，左右共用）
const ALLPASS_DELAYS = [556, 441, 341, 225]

export class ReverbSimple {
  private readonly fs: number

  // 梳状滤波器状态（8 组）
  private combBufL: Float32Array[] = []
  private combBufR: Float32Array[] = []
  private combPosL: Int32Array = new Int32Array(4)
  private combPosR: Int32Array = new Int32Array(4)
  private combLenL: Int32Array = new Int32Array(4)
  private combLenR: Int32Array = new Int32Array(4)
  private combStoreL: Float32Array = new Float32Array(4)
  private combStoreR: Float32Array = new Float32Array(4)

  // 全通滤波器状态（左右各 4）
  private apBufL: Float32Array[] = []
  private apBufR: Float32Array[] = []
  private apPosL: Int32Array = new Int32Array(4)
  private apPosR: Int32Array = new Int32Array(4)
  private apLen: Int32Array = new Int32Array(4)

  // preDelay 延迟线（左右各一）
  private preDelayL: Float32Array
  private preDelayR: Float32Array
  private preDelayPos = 0
  private preDelayLen = 0

  // 参数
  private feedback = 0
  private damp1 = 0
  private damp2 = 1
  private wet1 = 0
  private wet2 = 0
  private dry = 0

  constructor(fs: number) {
    if (fs <= 0 || !Number.isFinite(fs)) {
      throw new Error('invalid sample rate')
    }
    this.fs = fs

    // 预分配最大延迟缓冲：最长梳状 1356·1.2（stage）@fs
    const maxCombLen = Math.ceil((1356 * 1.2 * fs) / 44100) + 2
    const maxApLen = Math.ceil((556 * 1.2 * fs) / 44100) + 2
    for (let c = 0; c < 4; c++) {
      this.combBufL.push(new Float32Array(maxCombLen))
      this.combBufR.push(new Float32Array(maxCombLen))
      this.apBufL.push(new Float32Array(maxApLen))
      this.apBufR.push(new Float32Array(maxApLen))
    }
    // preDelay 上限 1000ms
    this.preDelayL = new Float32Array(Math.ceil(fs) + 1)
    this.preDelayR = new Float32Array(Math.ceil(fs) + 1)
  }

  setParams(p: ReverbSimpleParams): void {
    const t = TYPE_TABLE[p.type] || TYPE_TABLE.hall

    // type 提供基准，用户参数在基准附近 ±0.25 范围内微调（中性参数 0.5 时即类型本身）
    const effRoom = Math.min(0.98, Math.max(0, t.roomSize + (clamp01(p.roomSize) - 0.5) * 0.5))
    const effDamp = Math.min(0.99, Math.max(0.01, t.damping + (clamp01(p.damping) - 0.5) * 0.5))

    this.feedback = effRoom
    this.damp1 = effDamp
    this.damp2 = 1 - effDamp

    const wet = Math.min(4, Math.max(0, p.wet))
    const width = Math.min(2, Math.max(0, p.width))
    this.wet1 = wet * (width / 2 + 0.5)
    this.wet2 = wet * ((1 - width) / 2)
    this.dry = Math.min(4, Math.max(0, p.dry))

    // preDelay
    const pdMs = Math.min(1000, Math.max(0, p.preDelayMs))
    this.preDelayLen = Math.round((pdMs * this.fs) / 1000)

    // 延迟长度：标准调音 × type.delayScale × fs/44100
    const scale = (t.delayScale * this.fs) / 44100
    for (let c = 0; c < 4; c++) {
      this.combLenL[c] = Math.max(1, Math.round(COMB_DELAYS_L[c] * scale))
      this.combLenR[c] = Math.max(1, Math.round(COMB_DELAYS_R[c] * scale))
      this.apLen[c] = Math.max(1, Math.round(ALLPASS_DELAYS[c] * scale))
    }
  }

  /**
   * 就地处理立体声；out = dry·in + wet 混音（Freeverb 结构）。
   * 热路径：comb/allpass/preDelay 全部内联，字段缓存为局部变量；
   * 运算顺序与内联前逐位一致（数学完全等价）。
   */
  processStereo(l: Float32Array, r: Float32Array, frameCount?: number): void {
    const B = Math.max(0, Math.min(Math.floor(frameCount ?? l.length), l.length, r.length))
    // 湿路增益补偿（0.25 = 4 comb 平均）：无补偿时 4 路梳状求和的宽带稳态幅度
    // 可达输入 ~2-3 倍，wet 0.3 + dry 0.7 场景下总峰值超 1 → 削波炸音（用户实测）。
    const WET_GAIN = 0.25

    // 缓存字段到局部变量（循环内零属性访问）
    const cbL = this.combBufL
    const cbR = this.combBufR
    const posL = this.combPosL
    const posR = this.combPosR
    const lenL = this.combLenL
    const lenR = this.combLenR
    const stL = this.combStoreL
    const stR = this.combStoreR
    const abL = this.apBufL
    const abR = this.apBufR
    const apPosL = this.apPosL
    const apPosR = this.apPosR
    const apLen = this.apLen
    const pDL = this.preDelayL
    const pDR = this.preDelayR
    const feedback = this.feedback
    const damp1 = this.damp1
    const damp2 = this.damp2
    const wet1 = this.wet1
    const wet2 = this.wet2
    const dry = this.dry
    let pdLen = this.preDelayLen
    let pdPos = this.preDelayPos

    for (let i = 0; i < B; i++) {
      const xl = l[i]
      const xr = r[i]

      // preDelay（左右各一次 push，共享游标，顺序与内联前一致）
      let dl: number
      let dr: number
      if (pdLen === 0) {
        dl = xl
        dr = xr
      } else {
        const size = pDL.length
        let rp = pdPos - pdLen
        if (rp < 0) rp += size
        dl = pDL[rp]
        pDL[pdPos] = xl
        pdPos++
        if (pdPos >= size) pdPos = 0
        rp = pdPos - pdLen
        if (rp < 0) rp += size
        dr = pDR[rp]
        pDR[pdPos] = xr
        pdPos++
        if (pdPos >= size) pdPos = 0
      }

      // 8 梳状并联（每路：延迟 + 反馈低通，Jezar Freeverb 结构）
      let accL = 0
      let accR = 0
      {
        // comb 0
        const buf = cbL[0]
        const p = posL[0]
        const out = buf[p]
        const filt = out * damp2 + stL[0] * damp1
        stL[0] = filt
        buf[p] = dl + filt * feedback
        let np = p + 1
        if (np >= lenL[0]) np = 0
        posL[0] = np
        accL += out
      }
      {
        const buf = cbR[0]
        const p = posR[0]
        const out = buf[p]
        const filt = out * damp2 + stR[0] * damp1
        stR[0] = filt
        buf[p] = dr + filt * feedback
        let np = p + 1
        if (np >= lenR[0]) np = 0
        posR[0] = np
        accR += out
      }
      {
        const buf = cbL[1]
        const p = posL[1]
        const out = buf[p]
        const filt = out * damp2 + stL[1] * damp1
        stL[1] = filt
        buf[p] = dl + filt * feedback
        let np = p + 1
        if (np >= lenL[1]) np = 0
        posL[1] = np
        accL += out
      }
      {
        const buf = cbR[1]
        const p = posR[1]
        const out = buf[p]
        const filt = out * damp2 + stR[1] * damp1
        stR[1] = filt
        buf[p] = dr + filt * feedback
        let np = p + 1
        if (np >= lenR[1]) np = 0
        posR[1] = np
        accR += out
      }
      {
        const buf = cbL[2]
        const p = posL[2]
        const out = buf[p]
        const filt = out * damp2 + stL[2] * damp1
        stL[2] = filt
        buf[p] = dl + filt * feedback
        let np = p + 1
        if (np >= lenL[2]) np = 0
        posL[2] = np
        accL += out
      }
      {
        const buf = cbR[2]
        const p = posR[2]
        const out = buf[p]
        const filt = out * damp2 + stR[2] * damp1
        stR[2] = filt
        buf[p] = dr + filt * feedback
        let np = p + 1
        if (np >= lenR[2]) np = 0
        posR[2] = np
        accR += out
      }
      {
        const buf = cbL[3]
        const p = posL[3]
        const out = buf[p]
        const filt = out * damp2 + stL[3] * damp1
        stL[3] = filt
        buf[p] = dl + filt * feedback
        let np = p + 1
        if (np >= lenL[3]) np = 0
        posL[3] = np
        accL += out
      }
      {
        const buf = cbR[3]
        const p = posR[3]
        const out = buf[p]
        const filt = out * damp2 + stR[3] * damp1
        stR[3] = filt
        buf[p] = dr + filt * feedback
        let np = p + 1
        if (np >= lenR[3]) np = 0
        posR[3] = np
        accR += out
      }

      // 4 全通串联（反馈系数 0.5）
      {
        const buf = abL[0]
        const p = apPosL[0]
        const bufout = buf[p]
        const apOutL = -accL + bufout
        buf[p] = accL + bufout * 0.5
        accL = apOutL
        let np = p + 1
        if (np >= apLen[0]) np = 0
        apPosL[0] = np
      }
      {
        const buf = abR[0]
        const p = apPosR[0]
        const bufout = buf[p]
        const apOutR = -accR + bufout
        buf[p] = accR + bufout * 0.5
        accR = apOutR
        let np = p + 1
        if (np >= apLen[0]) np = 0
        apPosR[0] = np
      }
      {
        const buf = abL[1]
        const p = apPosL[1]
        const bufout = buf[p]
        const apOutL = -accL + bufout
        buf[p] = accL + bufout * 0.5
        accL = apOutL
        let np = p + 1
        if (np >= apLen[1]) np = 0
        apPosL[1] = np
      }
      {
        const buf = abR[1]
        const p = apPosR[1]
        const bufout = buf[p]
        const apOutR = -accR + bufout
        buf[p] = accR + bufout * 0.5
        accR = apOutR
        let np = p + 1
        if (np >= apLen[1]) np = 0
        apPosR[1] = np
      }
      {
        const buf = abL[2]
        const p = apPosL[2]
        const bufout = buf[p]
        const apOutL = -accL + bufout
        buf[p] = accL + bufout * 0.5
        accL = apOutL
        let np = p + 1
        if (np >= apLen[2]) np = 0
        apPosL[2] = np
      }
      {
        const buf = abR[2]
        const p = apPosR[2]
        const bufout = buf[p]
        const apOutR = -accR + bufout
        buf[p] = accR + bufout * 0.5
        accR = apOutR
        let np = p + 1
        if (np >= apLen[2]) np = 0
        apPosR[2] = np
      }
      {
        const buf = abL[3]
        const p = apPosL[3]
        const bufout = buf[p]
        const apOutL = -accL + bufout
        buf[p] = accL + bufout * 0.5
        accL = apOutL
        let np = p + 1
        if (np >= apLen[3]) np = 0
        apPosL[3] = np
      }
      {
        const buf = abR[3]
        const p = apPosR[3]
        const bufout = buf[p]
        const apOutR = -accR + bufout
        buf[p] = accR + bufout * 0.5
        accR = apOutR
        let np = p + 1
        if (np >= apLen[3]) np = 0
        apPosR[3] = np
      }

      accL *= WET_GAIN
      accR *= WET_GAIN

      // wet/dry + width 交叉混合
      l[i] = xl * dry + accL * wet1 + accR * wet2
      r[i] = xr * dry + accR * wet1 + accL * wet2
    }
    this.preDelayPos = pdPos
  }

  reset(): void {
    for (let c = 0; c < 4; c++) {
      this.combBufL[c].fill(0)
      this.combBufR[c].fill(0)
      this.apBufL[c].fill(0)
      this.apBufR[c].fill(0)
      this.combPosL[c] = 0
      this.combPosR[c] = 0
      this.apPosL[c] = 0
      this.apPosR[c] = 0
      this.combStoreL[c] = 0
      this.combStoreR[c] = 0
    }
    this.preDelayL.fill(0)
    this.preDelayR.fill(0)
    this.preDelayPos = 0
  }
}

function clamp01(v: number): number {
  return v < 0 ? 0 : v > 1 ? 1 : v
}
