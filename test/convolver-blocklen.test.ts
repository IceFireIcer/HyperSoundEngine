/**
 * convolver-blocklen.test.ts —— Convolver 任意块长回归（验收审计发现：B>L 时湿路损坏）
 * 修复：逐样本放行（completedBlocks·L 记账 + totalOut 位置）+ 突发动态扩容。
 * 验证：B=128/512/1024/4096 下流式湿路与双精度直接卷积一致（延迟 L，无丢块/发散）。
 */
import { describe, it, expect } from 'vitest'
import { Convolver } from '../src/dsp/Convolver'

const FS = 48000
const zeros = (n: number) => new Float32Array(n)
const sine = (n: number, f: number, a: number, fs: number) => {
  const x = new Float32Array(n)
  for (let i = 0; i < n; i++) x[i] = a * Math.sin((2 * Math.PI * f * i) / fs)
  return x
}
/** 参考：直接线性卷积（双精度） */
function directConv(x: Float32Array, ir: Float32Array): Float32Array {
  const y = new Float32Array(x.length + ir.length - 1)
  for (let i = 0; i < x.length; i++) for (let j = 0; j < ir.length; j++) y[i + j] += x[i] * ir[j]
  return y
}

describe('Convolver 任意块长（B ≤ 或 > 分区长 512）', () => {
  for (const B of [128, 512, 1024, 4096]) {
    it('B=' + B + '：流式湿路与直接卷积一致（延迟 L，零 NaN，无丢块/发散）', () => {
      const L = 512
      const cv = new Convolver(FS, { partitionSize: L, dePeriodize: false })
      const M = 800 // IR 长于分区（P=2）
      const ir = new Float32Array(M)
      for (let i = 0; i < M; i++) ir[i] = Math.exp(-i / 200) * 0.5
      cv.loadIR(ir, 'exp')
      cv.setMix(1)
      const n = FS // 1s
      const x = sine(n, 440, 0.5, FS)
      const l = new Float32Array(n + L)
      l.set(x)
      const r = zeros(n + L)
      for (let off = 0; off < n + L; off += B) cv.processStereo(l.subarray(off, off + B), r.subarray(off, off + B))
      const ref = directConv(x, ir)
      let maxErr = 0
      let nan = 0
      for (let i = L; i < n; i++) {
        if (!Number.isFinite(l[i])) nan++
        const expected = i - L < ref.length ? ref[i - L] : 0
        maxErr = Math.max(maxErr, Math.abs(l[i] - expected))
      }
      expect(nan).toBe(0)
      expect(maxErr).toBeLessThan(1e-3)
    })
  }
})

// ---------------------------------------------------------------------------
// 非均匀分区（模块 9 升级）：短分区 256 / 长分区 2048，任意块长回归
// IR 长度 > 短区段（100ms@48k=4800），长分区参与（Pl≥1）
// ---------------------------------------------------------------------------
describe('Convolver 非均匀分区任意块长（短 256 / 长 2048）', () => {
  for (const B of [128, 512, 1024, 4096]) {
    it('B=' + B + '：非均匀分区流式湿路与直接卷积一致（延迟 Ls，零 NaN，无丢块/发散）', () => {
      const Ls = 256
      const cv = new Convolver(FS, { partitionSize: Ls, longPartitionSize: 2048, dePeriodize: false })
      const M = 8000 // 0.167s：longStart=4864 < M → Pl=2 长分区参与
      const ir = new Float32Array(M)
      for (let i = 0; i < M; i++) ir[i] = Math.exp(-i / 1800) * Math.sin(i / 47) * 0.35
      cv.loadIR(ir, 'nonuniform')
      cv.setMix(1)
      const n = 12000 // 0.25s（缩小规模以控制直接卷积耗时）
      const x = sine(n, 440, 0.5, FS)
      const l = new Float32Array(n + Ls)
      l.set(x)
      const r = zeros(n + Ls)
      for (let off = 0; off < n + Ls; off += B) cv.processStereo(l.subarray(off, off + B), r.subarray(off, off + B))
      const ref = directConv(x, ir)
      let maxErr = 0
      let nan = 0
      for (let i = Ls; i < n; i++) {
        if (!Number.isFinite(l[i])) nan++
        const expected = i - Ls < ref.length ? ref[i - Ls] : 0
        maxErr = Math.max(maxErr, Math.abs(l[i] - expected))
      }
      expect(nan).toBe(0)
      expect(maxErr).toBeLessThan(1e-3)
    })
  }
})
