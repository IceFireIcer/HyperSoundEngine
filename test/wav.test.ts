/**
 * WAV 编解码测试
 *
 * 覆盖：
 * - encode→decode 往返（16-bit PCM / 32-bit Float）
 * - 多通道（5.1 = 6 通道）
 * - 畸形输入（坏魔数 / 坏 chunk / 块不对齐 / 0 声道 / 不支持位深）全部抛错
 * - decode 结果可直接构造 HseAudioBus 并 processBus 无异常
 */

import { readFileSync } from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { describe, it, expect } from 'vitest'
import { encodeWav, decodeWav } from '../src/io/wav'
import { HseAudioBus } from '../src/dsp/HseAudioBus'
import { HyperSoundEngine, createDefaultParams } from '../src/index'

type WavStandardFixture = {
  encode: Array<{
    name: string
    sampleRate: number
    bitDepth: 16 | 32
    channels: string[][]
    wantHex: string
  }>
  decode: Array<{
    name: string
    inputHex: string
    want?: { sampleRate: number; bitDepth: 16 | 32; channels: string[][] }
    error?: string
  }>
}

const fixturePath = path.join(path.dirname(fileURLToPath(import.meta.url)), '..', 'specs', 'io', 'vectors', 'wav-standard.json')
const standardFixture = JSON.parse(readFileSync(fixturePath, 'utf8')) as WavStandardFixture

function hexToBytes(hex: string): Uint8Array {
  return Uint8Array.from(hex.match(/../g) ?? [], value => Number.parseInt(value, 16))
}

function bytesToHex(bytes: ArrayBuffer | Uint8Array): string {
  const view = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes)
  return Array.from(view, value => value.toString(16).padStart(2, '0')).join('')
}

function f32FromHex(hex: string): number {
  const bytes = hexToBytes(hex)
  return new DataView(bytes.buffer).getFloat32(0, false)
}

function f32Hex(value: number): string {
  const buffer = new ArrayBuffer(4)
  new DataView(buffer).setFloat32(0, value, false)
  return bytesToHex(buffer)
}

describe('WAV 编解码', () => {
  it('standard 共享夹具的编码字节与解码结果一致', () => {
    for (const fixture of standardFixture.encode) {
      const channels = fixture.channels.map(channel => Float32Array.from(channel.map(f32FromHex)))
      const encoded = encodeWav(channels, fixture.sampleRate, {
        bitDepth: fixture.bitDepth,
        format: 'standard',
      })
      expect(bytesToHex(encoded), fixture.name).toBe(fixture.wantHex)
    }

    for (const fixture of standardFixture.decode) {
      const input = hexToBytes(fixture.inputHex)
      if (fixture.error) {
        expect(() => decodeWav(input), fixture.name).toThrow(fixture.error)
        continue
      }
      const decoded = decodeWav(input)
      expect(decoded.sampleRate, fixture.name).toBe(fixture.want?.sampleRate)
      expect(decoded.bitDepth, fixture.name).toBe(fixture.want?.bitDepth)
      expect(decoded.channels.map(channel => Array.from(channel, f32Hex)), fixture.name)
        .toEqual(fixture.want?.channels)
    }
  })

  it('standard 模式写入小端 RIFF 头且可自动解码，默认仍为 legacy', () => {
    const samples = [new Float32Array([0.25, -0.25])]
    const legacy = new Uint8Array(encodeWav(samples, 44100))
    const standard = new Uint8Array(encodeWav(samples, 44100, { format: 'standard' }))

    expect(Array.from(legacy.slice(24, 28))).toEqual([0x00, 0x00, 0xac, 0x44])
    expect(Array.from(standard.slice(24, 28))).toEqual([0x44, 0xac, 0x00, 0x00])
    expect(Array.from(standard.slice(40, 44))).toEqual([0x04, 0x00, 0x00, 0x00])

    const decoded = decodeWav(standard)
    expect(decoded.sampleRate).toBe(44100)
    expect(decoded.bitDepth).toBe(16)
    expect(decoded.channels[0]).toEqual(new Float32Array([
      8192 / 32767,
      -8192 / 32767,
    ]))
  })

  it('standard 总长字段字节回文时仍能自动识别', () => {
    const samples = [new Float32Array(32878)]
    const encoded = encodeWav(samples, 48000, { format: 'standard' })
    expect(Array.from(new Uint8Array(encoded, 4, 4))).toEqual([0x00, 0x01, 0x01, 0x00])
    expect(decodeWav(encoded).channels[0]).toHaveLength(32878)
  })

  it('standard 模式严格拒绝截断 data，legacy 继续保留截断容忍', () => {
    const standard = new Uint8Array(encodeWav([new Float32Array([0.5, -0.5])], 48000, { format: 'standard' }))
    const truncated = standard.slice(0, standard.length - 1)
    expect(() => decodeWav(truncated)).toThrow('decodeWav: RIFF size does not match file length')

    const legacy = new Uint8Array(encodeWav([new Float32Array([0.5, -0.5])], 48000))
    new DataView(legacy.buffer).setUint32(40, 16, false)
    expect(decodeWav(legacy).channels[0]).toHaveLength(2)
  })

  it('standard 编码拒绝不可表示的采样率和声道数', () => {
    const mono = [new Float32Array(0)]
    expect(() => encodeWav(mono, 0.5, { format: 'standard' })).toThrow('encodeWav: invalid sampleRate')
    expect(() => encodeWav(mono, 1.5, { format: 'standard' })).toThrow('encodeWav: invalid sampleRate')
    expect(() => encodeWav(mono, 0x1_0000_0000, { format: 'standard' })).toThrow('encodeWav: invalid sampleRate')
    expect(() => encodeWav(Array(0x10000).fill(new Float32Array(0)), 48000, { format: 'standard' }))
      .toThrow('encodeWav: channel count exceeds WAV limit')
    expect(() => encodeWav(Array(0x8000).fill(new Float32Array(0)), 48000, { format: 'standard' }))
      .toThrow('encodeWav: blockAlign exceeds WAV limit')
    expect(() => encodeWav(mono, 0xffff_ffff, { format: 'standard' }))
      .toThrow('encodeWav: byteRate exceeds WAV limit')
    const oversized = { length: 0x8000_0000 } as Float32Array
    expect(() => encodeWav([oversized], 48000, { format: 'standard' }))
      .toThrow('encodeWav: data size exceeds RIFF limit')
  })

  it('standard PCM16 最小码解码为 -1.0', () => {
    const standard = new Uint8Array(encodeWav([new Float32Array([0])], 48000, { format: 'standard' }))
    standard[44] = 0x00
    standard[45] = 0x80
    expect(decodeWav(standard).channels[0][0]).toBe(-1)
  })

  it('32-bit Float 往返：单声道/立体声电平与长度一致', () => {
    const fs = 48000
    const mono = [new Float32Array([0, 0.5, -0.5, 0.25, -0.25, 0, 1, -1])]
    const buf = encodeWav(mono, fs, { bitDepth: 32 })
    const res = decodeWav(buf)
    expect(res.bitDepth).toBe(32)
    expect(res.sampleRate).toBe(fs)
    expect(res.channels.length).toBe(1)
    expect(res.channels[0].length).toBe(8)
    for (let i = 0; i < 8; i++) {
      expect(res.channels[0][i]).toBeCloseTo(mono[0][i], 6)
    }

    const stereo = [new Float32Array(64).fill(0.3), new Float32Array(64).fill(-0.4)]
    const res2 = decodeWav(encodeWav(stereo, fs, { bitDepth: 32 }))
    expect(res2.channels.length).toBe(2)
    expect(res2.channels[0][0]).toBeCloseTo(0.3, 6)
    expect(res2.channels[1][0]).toBeCloseTo(-0.4, 6)
  })

  it('16-bit PCM 往返：量化误差在容忍范围内', () => {
    const fs = 44100
    const ch = new Float32Array(256)
    for (let i = 0; i < 256; i++) ch[i] = Math.sin(i * 0.1) * 0.8
    const res = decodeWav(encodeWav([ch], fs, { bitDepth: 16 }))
    expect(res.bitDepth).toBe(16)
    expect(res.channels[0].length).toBe(256)
    // 16-bit 量化步长 ~3.05e-5，半步误差 ≈ 1.5e-5
    for (let i = 0; i < 256; i++) {
      expect(Math.abs(res.channels[0][i] - ch[i])).toBeLessThan(1.7e-5)
    }
  })

  it('16-bit 钳制超量程样本到 [-1, 1]', () => {
    const ch = new Float32Array([2.0, -2.0, 1.5])
    const res = decodeWav(encodeWav([ch], 48000, { bitDepth: 16 }))
    expect(res.channels[0][0]).toBeCloseTo(1, 5)
    expect(res.channels[0][1]).toBeCloseTo(-1, 5)
    expect(res.channels[0][2]).toBeCloseTo(1, 5)
  })

  it('多通道（5.1 = 6 通道）编码解码正确', () => {
    const fs = 48000
    const n = 128
    const channels: Float32Array[] = []
    for (let c = 0; c < 6; c++) channels.push(new Float32Array(n).fill((c + 1) * 0.1))
    const res = decodeWav(encodeWav(channels, fs, { bitDepth: 32 }))
    expect(res.channels.length).toBe(6)
    for (let c = 0; c < 6; c++) {
      expect(res.channels[c][0]).toBeCloseTo((c + 1) * 0.1, 6)
      expect(res.channels[c].length).toBe(n)
    }
  })

  it('畸形输入：坏 RIFF/WAVE 魔数抛错', () => {
    expect(() => decodeWav(new ArrayBuffer(44))).toThrow(/bad RIFF magic/)
    const bad = new Uint8Array(48)
    const dv = new DataView(bad.buffer)
    dv.setUint32(0, 0x58464552, false) // 'XFER' 不是 RIFF
    dv.setUint32(8, 0x57415645, false) // WAVE
    expect(() => decodeWav(bad)).toThrow(/bad RIFF magic/)
  })

  it('畸形输入：缺失 fmt / data chunk 抛错', () => {
    const bad = new Uint8Array(48)
    const dv = new DataView(bad.buffer)
    dv.setUint32(0, 0x52494646, false) // RIFF
    dv.setUint32(8, 0x57415645, false) // WAVE
    // 没有任何 chunk
    expect(() => decodeWav(bad)).toThrow(/missing fmt chunk/)
  })

  it('畸形输入：0 声道抛错', () => {
    const bad = new Uint8Array(48)
    const dv = new DataView(bad.buffer)
    dv.setUint32(0, 0x52494646, false) // RIFF
    dv.setUint32(8, 0x57415645, false) // WAVE
    dv.setUint32(12, 0x666d7420, false) // 'fmt '
    dv.setUint32(16, 16, false)
    dv.setUint16(20, 1, false) // PCM
    dv.setUint16(22, 0, false) // 0 声道
    dv.setUint32(24, 48000, false)
    dv.setUint16(34, 16, false)
    dv.setUint32(36, 0x64617461, false) // data
    dv.setUint32(40, 0, false)
    expect(() => decodeWav(bad)).toThrow(/channel count must be >= 1/)
  })

  it('decode 结果可直接构造 HseAudioBus 并 processBus 无异常', () => {
    const fs = 48000
    const engine = new HyperSoundEngine(fs, 2)
    const params = createDefaultParams(fs)
    params.limiter.enabled = false
    engine.setParams(params)
    const n = 256
    const src = [new Float32Array(n).fill(0.2), new Float32Array(n).fill(0.2)]
    const buf = encodeWav(src, fs, { bitDepth: 32 })
    const res = decodeWav(buf)
    const input = new HseAudioBus(res.channels)
    const output = HseAudioBus.create(2, n)
    engine.processBus(input, output)
    expect(output.getChannel(0)[0]).toBeCloseTo(0.2, 5)
  })

  it('默认 bitDepth 为 16-bit PCM', () => {
    const buf = encodeWav([new Float32Array(8).fill(0.5)], 48000)
    const res = decodeWav(buf)
    expect(res.bitDepth).toBe(16)
  })
})
