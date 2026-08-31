/**
 * ambienceMixer —— 环境声混合器纯函数/常量（HSE 空间音频，FOA 编解码渲染路径）
 *
 * 规划书 Phase 4「环境声/混响 send 用」完整版：多声道输入上混 → encodeSource →
 * decodeFoaToSpeakers 驱动环境扬声器增益。本模块是环境混合器的纯函数层——处理器
 * SpatialProcessor 持有平滑/延迟状态，每块调用 foaAmbienceGains 获取 4 路目标增益：
 *   立体声/多声道输入块 → stereoToFoa 能量提取（M/S：同相 → W 全向、反相 → Y 左右
 *   差分）→ decodeFoaToSpeakers 解码到 AMBIENCE_SPEAKERS 方位角（45/135/225/315）
 *   → 4 路目标增益 g_k（可负，Ambisonics 相位抵消语义），clamp 到 [-1,1] 后返回。
 *
 * 数学性质（单位正弦输入，RMS=1/√2，W=√2·rms=1、Y=√2·rms=1）：
 *   纯同相（L=R）：W=1、X=Y=0 → g_k = W/√2 ≈ 0.707（4 方向对全向分量等权，全相等）；
 *   纯反相（L=−R）：W=0、Y=1 → g_k = sin(az_k)·Y = ±1/√2——右半场（az∈(0°,180°)，
 *   sin>0，即 45°/135°）为正、左半场（225°/315°）为负（Y 只编码左右轴，M/S 矩阵
 *   无前后/上下信息；正负交替即 Ambisonics 相位抵消语义）；
 *   静音：全 0。
 *
 * 纯函数、确定性、无浏览器 API 依赖（AudioWorklet 安全；禁止 import 主线程模块）。
 */

import { AMBIENCE_SPEAKERS, decodeFoaToSpeakers, stereoToFoa } from './ambisonics'
import type { AmbisonicBuffer } from './ambisonics'

/** 环境扬声器数量（= AMBIENCE_SPEAKERS 4 方向 45/135/225/315） */
export const AMBIENCE_CHANNELS = AMBIENCE_SPEAKERS.length

/**
 * 环境扬声器方位角（只读数组，模块级常量——O1 审计 3.3 提取：
 * 原先 foaAmbienceGains 每块 `AMBIENCE_SPEAKERS.map(s => s.azimuthDeg)`
 * 分配临时数组 3 次（map 出 1 个数组 + decodeFoaToSpeakers 内部 map 出 1 个 +
 * 闭包封装），改为模块级共享只读数组复用——避免每块 GC 压力）。
 */
export const AMBIENCE_AZIMUTHS: readonly number[] = AMBIENCE_SPEAKERS.map((s) => s.azimuthDeg)

/** 去相关延迟基线（毫秒）：第 k 通道延迟 = BASE + k·STEP（20/28/36/44ms 量级，按 fs 换算） */
export const AMBIENCE_DELAY_BASE_MS = 20

/** 去相关延迟步进（毫秒）：相邻环境通道延迟间隔 8ms（通道间去相关 → 扩散感） */
export const AMBIENCE_DELAY_STEP_MS = 8

/** 第 k 环境通道的去相关延迟（样本，按采样率换算；k 越界按基线条数取模回绕） */
export function ambienceDelaySamples(sampleRate: number, k: number): number {
  const ms = AMBIENCE_DELAY_BASE_MS + (k % AMBIENCE_CHANNELS) * AMBIENCE_DELAY_STEP_MS
  return Math.round((ms / 1000) * sampleRate)
}

/**
 * FOA 环境增益：立体声块 → 4 路环境扬声器目标增益（stereoToFoa + decode 一步）。
 *
 * 处理器内每块调用（平滑/去相关在处理器侧，见 SpatialProcessor.renderAmbience）：
 * 平滑前先 clamp 到 [-1,1]（防御性——正常音乐输入下 |g_k| ≤ 1，见上数学性质）。
 *
 * @param l 左声道块（长度须 ≥ block）
 * @param r 右声道块
 * @param frameCount 本次有效帧数；可小于输入缓冲容量
 * @param foa 调用方预分配的 4 元素 FOA scratch
 * @param out 调用方预分配输出数组（长度 ≥ AMBIENCE_CHANNELS）
 * @returns out 原引用
 */
export function foaAmbienceGains(
  l: Float32Array,
  r: Float32Array,
  frameCount: number,
  foa: AmbisonicBuffer,
  out: AmbisonicBuffer,
): AmbisonicBuffer {
  if (out.length < AMBIENCE_CHANNELS) throw new Error('foaAmbienceGains: output buffer is too small')
  stereoToFoa(l, r, frameCount, foa)
  decodeFoaToSpeakers(foa, AMBIENCE_AZIMUTHS, out)
  for (let k = 0; k < AMBIENCE_CHANNELS; k++) {
    out[k] = Math.max(-1, Math.min(1, out[k]))
  }
  return out
}

/** 内联空间级使用的有状态环境渲染器；所有缓冲在构造/prepare 控制路径分配。 */
export class AmbienceRenderer {
  private readonly foa = new Float64Array(4)
  private readonly targets = new Float64Array(AMBIENCE_CHANNELS)
  private readonly current = new Float64Array(AMBIENCE_CHANNELS)
  private readonly delays: Int32Array
  private readonly lines: Float32Array[] = []
  private readonly positions = new Int32Array(AMBIENCE_CHANNELS)
  private readonly panL = new Float64Array(AMBIENCE_CHANNELS)
  private readonly panR = new Float64Array(AMBIENCE_CHANNELS)
  private readonly smoothCoef: number

  constructor(sampleRate: number) {
    this.delays = new Int32Array(AMBIENCE_CHANNELS)
    this.smoothCoef = Math.exp(-1 / (Math.max(1, sampleRate) * 0.02))
    for (let k = 0; k < AMBIENCE_CHANNELS; k++) {
      const delay = Math.max(1, ambienceDelaySamples(sampleRate, k))
      this.delays[k] = delay
      this.lines.push(new Float32Array(delay))
      const pan = (Math.sin((AMBIENCE_AZIMUTHS[k] * Math.PI) / 180) + 1) * 0.5
      this.panL[k] = Math.sqrt(1 - pan)
      this.panR[k] = Math.sqrt(pan)
    }
  }

  prepare(_maxBlockSize: number): void {
    // 延迟线容量只由采样率和固定最大延迟决定，构造时已完成分配。
  }

  processAdd(
    inL: Float32Array,
    inR: Float32Array,
    outL: Float32Array,
    outR: Float32Array,
    frameCount: number,
    amount: number,
  ): void {
    const mix = Math.min(1, Math.max(0, amount)) * 0.5
    if (mix <= 0 || frameCount <= 0) return
    foaAmbienceGains(inL, inR, frameCount, this.foa, this.targets)
    for (let i = 0; i < frameCount; i++) {
      let addL = 0
      let addR = 0
      for (let k = 0; k < AMBIENCE_CHANNELS; k++) {
        const line = this.lines[k]
        const pos = this.positions[k]
        const delayed = line[pos]
        line[pos] = k < 2 ? inR[i] : inL[i]
        this.positions[k] = pos + 1 >= this.delays[k] ? 0 : pos + 1
        const gain = this.targets[k] + this.smoothCoef * (this.current[k] - this.targets[k])
        this.current[k] = gain
        addL += delayed * gain * this.panL[k]
        addR += delayed * gain * this.panR[k]
      }
      outL[i] += addL * mix
      outR[i] += addR * mix
    }
  }

  reset(): void {
    this.foa.fill(0)
    this.targets.fill(0)
    this.current.fill(0)
    this.positions.fill(0)
    for (let k = 0; k < this.lines.length; k++) this.lines[k].fill(0)
  }
}
