/**
 * WAV 编解码 —— legacy 兼容与标准 RIFF/WAVE 多通道文件 I/O
 *
 * 用途：
 * - 核心包内置音频文件读写（WaveForge 适配层复用本模块，不再内嵌实现）；
 * - 解码结果为非交错 Float32Array[]，可直接构造 HseAudioBus 进入 processBus；
 * - 编码支持 16-bit PCM（消费级）与 32-bit Float（专业级）。
 *
 * 约定（与引擎一致）：
 * - 纯 TS、零依赖、确定性（无随机/Date）；
 * - 解码对畸形输入一律抛错（防注入语义，与 ShareCodec 白名单一致），不静默返回；
 * - 块对齐校验：data 长度必须能被 blockAlign 整除。
 */

export interface WavDecodeResult {
  sampleRate: number
  channels: Float32Array[]
  bitDepth: 16 | 32
}

export type WavContainerFormat = 'legacy' | 'standard'

export interface WavEncodeOptions {
  bitDepth?: 16 | 32
  format?: WavContainerFormat
}

const RIFF_MAGIC = 0x52494646 // 'RIFF'
const WAVE_MAGIC = 0x57415645 // 'WAVE'
const FMT_CHUNK = 0x666d7420 // 'fmt '
const DATA_CHUNK = 0x64617461 // 'data'

function readU32(view: DataView, off: number, littleEndian: boolean): number {
  return view.getUint32(off, littleEndian)
}

function writeU32(view: DataView, off: number, val: number, littleEndian = false): void {
  view.setUint32(off, val >>> 0, littleEndian)
}

function writeU16(view: DataView, off: number, val: number, littleEndian = false): void {
  view.setUint16(off, val & 0xffff, littleEndian)
}

function hasPlausibleFmt(view: DataView, length: number, littleEndian: boolean): boolean {
  let off = 12
  while (off + 8 <= length) {
    const id = readU32(view, off, false)
    const size = readU32(view, off + 4, littleEndian)
    const body = off + 8
    if (size > length - body) return false
    if (id === FMT_CHUNK) {
      if (size < 16 || body + 16 > length) return false
      const formatTag = view.getUint16(body, littleEndian)
      const bits = view.getUint16(body + 14, littleEndian)
      return (formatTag === 1 && bits === 16) || (formatTag === 3 && bits === 32)
    }
    off = body + size + (size % 2)
  }
  return false
}

/**
 * 编码多通道音频为 WAV 文件。
 * @param channels 非交错 Float32Array[]（声道数 ≥1，各通道等长）
 * @param sampleRate 采样率 Hz
 * @param opts.bitDepth 16=PCM（默认）/ 32=Float
 * @param opts.format legacy=历史兼容格式（默认）/ standard=标准 RIFF 小端格式
 * @returns WAV 文件 ArrayBuffer
 */
export function encodeWav(channels: Float32Array[], sampleRate: number, opts?: WavEncodeOptions): ArrayBuffer {
  if (!channels || channels.length < 1) throw new Error('encodeWav: at least one channel required')
  const cc = channels.length
  const frames = channels[0].length
  for (let c = 1; c < cc; c++) {
    if (channels[c].length !== frames) throw new Error('encodeWav: all channels must have equal length')
  }
  if (!Number.isFinite(sampleRate) || sampleRate <= 0) throw new Error('encodeWav: invalid sampleRate')
  const bitDepth = opts?.bitDepth ?? 16
  if (bitDepth !== 16 && bitDepth !== 32) throw new Error('encodeWav: bitDepth must be 16 or 32')
  const format = opts?.format ?? 'legacy'
  if (format !== 'legacy' && format !== 'standard') throw new Error('encodeWav: format must be legacy or standard')
  const littleEndian = format === 'standard'
  if (littleEndian && (!Number.isInteger(sampleRate) || sampleRate > 0xffff_ffff)) {
    throw new Error('encodeWav: invalid sampleRate')
  }
  if (littleEndian && cc > 0xffff) throw new Error('encodeWav: channel count exceeds WAV limit')
  const formatTag = bitDepth === 16 ? 1 : 3 // PCM=1, IEEE float=3
  const bytesPerSample = bitDepth / 8
  const blockAlign = cc * bytesPerSample
  if (littleEndian && blockAlign > 0xffff) throw new Error('encodeWav: blockAlign exceeds WAV limit')
  if (littleEndian && sampleRate * blockAlign > 0xffff_ffff) throw new Error('encodeWav: byteRate exceeds WAV limit')
  const dataSize = frames * blockAlign
  const bufferSize = 44 + dataSize // 12(RIFF) + 24(fmt) + 8(data header) + data
  if (littleEndian && (dataSize > 0xffff_ffff || bufferSize - 8 > 0xffff_ffff)) {
    throw new Error('encodeWav: data size exceeds RIFF limit')
  }

  const buf = new ArrayBuffer(bufferSize)
  const view = new DataView(buf)
  const u8 = new Uint8Array(buf)

  // RIFF header
  writeU32(view, 0, RIFF_MAGIC)
  writeU32(view, 4, bufferSize - 8, littleEndian) // chunk size
  writeU32(view, 8, WAVE_MAGIC)

  // fmt chunk
  writeU32(view, 12, FMT_CHUNK)
  writeU32(view, 16, 16, littleEndian) // fmt chunk size
  writeU16(view, 20, formatTag, littleEndian)
  writeU16(view, 22, cc, littleEndian)
  writeU32(view, 24, sampleRate, littleEndian)
  writeU32(view, 28, sampleRate * blockAlign, littleEndian) // byte rate
  writeU16(view, 32, blockAlign, littleEndian)
  writeU16(view, 34, bitDepth, littleEndian)

  // data chunk
  writeU32(view, 36, DATA_CHUNK)
  writeU32(view, 40, dataSize, littleEndian)

  // 交错写入样本
  let off = 44
  if (bitDepth === 16) {
    for (let i = 0; i < frames; i++) {
      for (let c = 0; c < cc; c++) {
        let s = channels[c][i]
        // 钳制到 [-1, 1] 再量化（deterministic, 无抖动）
        if (s > 1) s = 1
        else if (s < -1) s = -1
        const v = Math.round(s * 32767)
        // 小端有符号 16-bit
        u8[off++] = v & 0xff
        u8[off++] = (v >> 8) & 0xff
      }
    }
  } else {
    // 32-bit float，小端
    for (let i = 0; i < frames; i++) {
      for (let c = 0; c < cc; c++) {
        view.setFloat32(off, channels[c][i], true)
        off += 4
      }
    }
  }

  return buf
}

/**
 * 解码 WAV 文件为非交错 Float32Array[]。
 * @param buffer ArrayBuffer 或 Uint8Array（自动识别 legacy / standard）
 * @returns { sampleRate, channels, bitDepth }
 * @throws 畸形输入（坏魔数 / 坏 chunk / 块不对齐 / 0 声道 / 不支持位深）一律抛错
 */
export function decodeWav(buffer: ArrayBuffer | Uint8Array): WavDecodeResult {
  const u8 = buffer instanceof Uint8Array ? buffer : new Uint8Array(buffer)
  if (u8.length < 44) throw new Error('decodeWav: file too short (<44 bytes)')
  const view = new DataView(u8.buffer, u8.byteOffset, u8.byteLength)

  // RIFF / WAVE 魔数校验
  if (readU32(view, 0, false) !== RIFF_MAGIC) throw new Error('decodeWav: bad RIFF magic')
  if (readU32(view, 8, false) !== WAVE_MAGIC) throw new Error('decodeWav: bad WAVE magic')
  const declaredSize = u8.length - 8
  const legacyRiffSize = readU32(view, 4, false)
  const standardRiffSize = readU32(view, 4, true)
  const legacySizeMatches = legacyRiffSize === declaredSize
  const standardSizeMatches = standardRiffSize === declaredSize
  const legacyPlausible = hasPlausibleFmt(view, u8.length, false)
  const standardPlausible = hasPlausibleFmt(view, u8.length, true)
  const littleEndian = standardSizeMatches
    ? !legacySizeMatches || standardPlausible && !legacyPlausible
    : !legacySizeMatches && standardPlausible && !legacyPlausible
  if (littleEndian && standardRiffSize !== declaredSize) {
    throw new Error('decodeWav: RIFF size does not match file length')
  }

  // 扫描 chunk：fmt 与 data（跳过未知 chunk，如 LIST/INFO）
  let off = 12
  let formatTag = 0
  let channels = 0
  let sampleRate = 0
  let byteRate = 0
  let headerBlockAlign = 0
  let bitsPerSample = 0
  let dataOff = -1
  let dataLen = 0

  while (off + 8 <= u8.length) {
    const id = readU32(view, off, false)
    const size = readU32(view, off + 4, littleEndian)
    const body = off + 8
    if (id === FMT_CHUNK) {
      if (size < 16) throw new Error('decodeWav: fmt chunk too small')
      formatTag = view.getUint16(body, littleEndian)
      channels = view.getUint16(body + 2, littleEndian)
      sampleRate = view.getUint32(body + 4, littleEndian)
      byteRate = view.getUint32(body + 8, littleEndian)
      headerBlockAlign = view.getUint16(body + 12, littleEndian)
      bitsPerSample = view.getUint16(body + 14, littleEndian)
    } else if (id === DATA_CHUNK) {
      dataOff = body
      dataLen = size
      break // data 之后通常无其他需要解析的 chunk
    }
    // chunk 对齐到偶数
    off = body + size + (size % 2)
  }

  if (formatTag === 0) throw new Error('decodeWav: missing fmt chunk')
  if (dataOff < 0) throw new Error('decodeWav: missing data chunk')
  if (channels < 1) throw new Error('decodeWav: channel count must be >= 1')
  if (bitsPerSample !== 16 && bitsPerSample !== 32) throw new Error(`decodeWav: unsupported bit depth ${bitsPerSample}`)
  if (formatTag === 1 && bitsPerSample !== 16) throw new Error('decodeWav: PCM format requires 16-bit')
  if (formatTag === 3 && bitsPerSample !== 32) throw new Error('decodeWav: float format requires 32-bit')
  if (formatTag !== 1 && formatTag !== 3) throw new Error(`decodeWav: unsupported format tag ${formatTag}`)

  const bytesPerSample = bitsPerSample / 8
  const blockAlign = channels * bytesPerSample
  if (littleEndian) {
    if (sampleRate === 0) throw new Error('decodeWav: invalid sampleRate')
    if (headerBlockAlign !== blockAlign) throw new Error('decodeWav: blockAlign does not match format')
    if (byteRate !== sampleRate * blockAlign) throw new Error('decodeWav: byteRate does not match format')
    if (dataOff + dataLen !== u8.length) throw new Error('decodeWav: data chunk size does not match file length')
  }
  if (dataLen % blockAlign !== 0) throw new Error('decodeWav: data length not aligned to block size')

  // 实际可用样本帧数（dataLen 可能超过实际 buffer，取较小者）
  const available = Math.min(dataLen, u8.length - dataOff)
  const frames = Math.floor(available / blockAlign)

  const out: Float32Array[] = []
  for (let c = 0; c < channels; c++) out.push(new Float32Array(frames))

  if (bitsPerSample === 16) {
    let p = dataOff
    for (let i = 0; i < frames; i++) {
      for (let c = 0; c < channels; c++) {
        const v = view.getInt16(p, true)
        out[c][i] = littleEndian && v === -32768 ? -1 : v / 32767
        p += 2
      }
    }
  } else {
    let p = dataOff
    for (let i = 0; i < frames; i++) {
      for (let c = 0; c < channels; c++) {
        out[c][i] = view.getFloat32(p, true)
        p += 4
      }
    }
  }

  return { sampleRate, channels: out, bitDepth: bitsPerSample as 16 | 32 }
}
