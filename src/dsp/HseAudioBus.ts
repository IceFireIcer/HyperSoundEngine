/**
 * HseAudioBus —— 多通道非交错音频缓冲抽象
 *
 * 出处/许可：自研容器抽象（引擎多通道入口 processBus 的配套数据结构），
 * 无第三方代码。
 *
 * 用途：
 * - 统一表达单声道/立体声/多通道音频块（5.1 / 7.1 / 任意 N 通道）；
 * - 提供通道级工具：创建、交错/解交错、拷贝、增益、混音、子集提取、单声道下混；
 * - 引擎核心 DSP 为立体声：`processBus` 提供 downmix（下混立体声）与
 *   perChannelPair（逐立体声对独立处理）两种多通道路由。
 *
 * 约定：
 * - channels 为非交错 Float32Array 数组；
 * - 所有通道长度应一致（frameCount 取第一通道长度）；
 * - 工具方法均为确定性实现，process 内零分配由调用方保证（本类自身不持有状态）。
 */

export class HseAudioBus {
  readonly channels: Float32Array[]

  constructor(channels: Float32Array[]) {
    if (channels.length === 0) throw new Error('HseAudioBus: at least one channel required')
    this.channels = channels
  }

  get channelCount(): number {
    return this.channels.length
  }

  get frameCount(): number {
    return this.channels[0].length
  }

  getChannel(index: number): Float32Array {
    const ch = this.channels[index]
    if (!ch) throw new Error(`HseAudioBus: channel ${index} out of range`)
    return ch
  }

  /** 从 Float32Array[] 创建 HseAudioBus（直接引用，不拷贝） */
  static from(inputs: Float32Array[]): HseAudioBus {
    return new HseAudioBus(inputs)
  }

  /** 创建 N 通道 × frameCount 帧的零填充 HseAudioBus */
  static create(channelCount: number, frameCount: number): HseAudioBus {
    if (!Number.isInteger(channelCount) || channelCount < 1) {
      throw new Error('HseAudioBus: channelCount must be a positive integer')
    }
    if (!Number.isInteger(frameCount) || frameCount < 0) {
      throw new Error('HseAudioBus: frameCount must be a non-negative integer')
    }
    const channels: Float32Array[] = []
    for (let c = 0; c < channelCount; c++) channels.push(new Float32Array(frameCount))
    return new HseAudioBus(channels)
  }

  /** 由交错缓冲创建（length 必须为 channelCount 的整数倍；拷贝） */
  static fromInterleaved(interleaved: Float32Array, channelCount: number): HseAudioBus {
    if (!Number.isInteger(channelCount) || channelCount < 1) {
      throw new Error('HseAudioBus: channelCount must be a positive integer')
    }
    if (interleaved.length % channelCount !== 0) {
      throw new Error('HseAudioBus: interleaved length must be a multiple of channelCount')
    }
    const frames = interleaved.length / channelCount
    const channels: Float32Array[] = []
    for (let c = 0; c < channelCount; c++) {
      const ch = new Float32Array(frames)
      for (let i = 0; i < frames; i++) ch[i] = interleaved[i * channelCount + c]
      channels.push(ch)
    }
    return new HseAudioBus(channels)
  }

  /** 输出交错缓冲（长度 = frameCount × channelCount；新分配） */
  toInterleaved(): Float32Array {
    const n = this.frameCount
    const cc = this.channelCount
    const out = new Float32Array(n * cc)
    for (let c = 0; c < cc; c++) {
      const ch = this.channels[c]
      for (let i = 0; i < n; i++) out[i * cc + c] = ch[i]
    }
    return out
  }

  /** 拷贝本 bus 到 target（取 min(channelCount, frameCount)；越界通道忽略） */
  copyTo(target: HseAudioBus): void {
    const cc = Math.min(this.channelCount, target.channelCount)
    const n = Math.min(this.frameCount, target.frameCount)
    for (let c = 0; c < cc; c++) target.channels[c].set(this.channels[c].subarray(0, n))
  }

  /** 全部通道填充同一值 */
  fill(value: number): void {
    for (const ch of this.channels) ch.fill(value)
  }

  /** 全部通道乘以线性增益（就地） */
  applyGain(gain: number): void {
    for (const ch of this.channels) {
      for (let i = 0; i < ch.length; i++) ch[i] *= gain
    }
  }

  /** 将 other 各通道乘 gain 后混入本 bus（就地累加；取 min 通道数/帧数） */
  mixFrom(other: HseAudioBus, gain = 1): void {
    const cc = Math.min(this.channelCount, other.channelCount)
    const n = Math.min(this.frameCount, other.frameCount)
    for (let c = 0; c < cc; c++) {
      const a = this.channels[c]
      const b = other.channels[c]
      for (let i = 0; i < n; i++) a[i] += b[i] * gain
    }
  }

  /** 提取通道子集为新 HseAudioBus（直接引用原通道，不拷贝） */
  extract(channelIndices: number[]): HseAudioBus {
    const picked: Float32Array[] = []
    for (const idx of channelIndices) picked.push(this.getChannel(idx))
    return new HseAudioBus(picked)
  }

  /** 下混为单声道（全部通道平均；新分配） */
  downmixToMono(): Float32Array {
    const n = this.frameCount
    const out = new Float32Array(n)
    const cc = this.channelCount
    for (let c = 0; c < cc; c++) {
      const ch = this.channels[c]
      for (let i = 0; i < n; i++) out[i] += ch[i]
    }
    if (cc > 1) {
      const inv = 1 / cc
      for (let i = 0; i < n; i++) out[i] *= inv
    }
    return out
  }

  /** 下混为立体声（>2 声道时取前两声道；单声道复制到双声道） */
  downmixToStereo(): { l: Float32Array; r: Float32Array } {
    const n = this.frameCount
    const l = new Float32Array(n)
    const r = new Float32Array(n)
    const first = this.channels[0]
    const second = this.channels[1] ?? first
    l.set(first)
    r.set(second)
    return { l, r }
  }

  /** 将立体声结果写回本 bus：不足 2 声道只写第一声道，超过 2 声道复制到其余声道 */
  writeStereo(l: Float32Array, r: Float32Array): void {
    const n = Math.min(this.frameCount, l.length, r.length)
    const first = this.channels[0]
    for (let i = 0; i < n; i++) first[i] = l[i]
    if (this.channels.length >= 2) {
      const second = this.channels[1]
      for (let i = 0; i < n; i++) second[i] = r[i]
    }
    for (let c = 2; c < this.channels.length; c++) {
      const ch = this.channels[c]
      for (let i = 0; i < n; i++) ch[i] = l[i]
    }
  }
}
