/**
 * HyperSoundEngine v1 调音室 UI —— EQ 响应曲线编辑器（SVG）
 *
 * 对数频率轴（20Hz–20kHz）× 增益轴（-12..+12dB）折线图；
 * 控制点可拖拽（指针捕获），支持"拖动频点"交互。
 * 纯展示 + 受控回调，不依赖任何绘图库。
 */

import { useRef } from 'react'
import type { HyperSoundEngineTheme } from './theme'

export interface EqPoint {
  frequency: number
  gain: number
}

export interface EqCurveEditorProps {
  points: EqPoint[]
  theme: HyperSoundEngineTheme
  onChange?: (index: number, gain: number) => void
  /** 只读模式（锁定态/预览态） */
  readonly?: boolean
  /** 附加参考曲线（如 Q 补偿后的预估响应，仅展示） */
  reference?: EqPoint[]
  height?: number
  minDb?: number
  maxDb?: number
  fMin?: number
  fMax?: number
}

/** 对数频率 → 0..1 x 坐标 */
export function fToX(freq: number, fMin: number, fMax: number): number {
  return (Math.log(freq / fMin) / Math.log(fMax / fMin))
}

/** 增益 → 0..1 y 坐标（顶部为 +max） */
export function dbToY(gain: number, minDb: number, maxDb: number): number {
  return 1 - (gain - minDb) / (maxDb - minDb)
}

export function EqCurveEditor({ points, theme, onChange, readonly, reference, height = 170, minDb = -12, maxDb = 12, fMin = 20, fMax = 20000 }: EqCurveEditorProps) {
  const W = 420
  const H = height
  const padL = 10
  const padR = 10
  const padT = 10
  const padB = 18
  const iw = W - padL - padR
  const ih = H - padT - padB
  const svgRef = useRef<SVGSVGElement>(null)

  const x = (f: number) => padL + fToX(f, fMin, fMax) * iw
  const y = (g: number) => padT + dbToY(g, minDb, maxDb) * ih

  const linePath = (pts: EqPoint[]) =>
    pts.map((p, i) => `${i === 0 ? 'M' : 'L'}${x(p.frequency).toFixed(2)},${y(p.gain).toFixed(2)}`).join(' ')

  /** 命中检测：频率最近的拖拽点 */
  const hitIndex = (clientX: number, clientY: number): number | null => {
    const rect = svgRef.current?.getBoundingClientRect()
    if (!rect) return null
    const px = ((clientX - rect.left) / rect.width) * W
    const py = ((clientY - rect.top) / rect.height) * H
    // 找最近的频点（x 距离 < 18px 才命中）
    let best = -1
    let bestD = 18
    points.forEach((p, i) => {
      const d = Math.hypot(x(p.frequency) - px, y(p.gain) - py)
      if (d < bestD) { bestD = d; best = i }
    })
    return best >= 0 ? best : null
  }

  const handlePointerDown = (e: React.PointerEvent<SVGSVGElement>) => {
    if (readonly || !onChange) return
    const idx = hitIndex(e.clientX, e.clientY)
    if (idx === null) return
    e.currentTarget.setPointerCapture(e.pointerId)
    const rect = e.currentTarget.getBoundingClientRect()
    const py = ((e.clientY - rect.top) / rect.height) * H
    applyGain(idx, py, rect.height)
  }

  const handlePointerMove = (e: React.PointerEvent<SVGSVGElement>) => {
    if (readonly || !onChange || e.buttons !== 1) return
    const rect = e.currentTarget.getBoundingClientRect()
    const py = ((e.clientY - rect.top) / rect.height) * H
    const idx = hitIndex(e.clientX, e.clientY)
    if (idx !== null) applyGain(idx, py, rect.height)
  }

  const applyGain = (idx: number, py: number, svgHeight: number) => {
    if (!onChange) return
    const gain = minDb + (1 - (py - padT) / ih) * (maxDb - minDb)
    onChange(idx, Math.round(Math.min(maxDb, Math.max(minDb, gain)) * 10) / 10)
  }

  // 频率网格刻度（100Hz / 1kHz / 10kHz 标签）
  const gridFreqs = [50, 100, 200, 500, 1000, 2000, 5000, 10000, 20000]
  const zeroY = y(0)

  return (
    <svg
      ref={svgRef}
      viewBox={`0 0 ${W} ${H}`}
      className="w-full touch-none select-none"
      style={{ cursor: readonly ? 'default' : 'grab' }}
      onPointerDown={handlePointerDown}
      onPointerMove={handlePointerMove}
      onPointerUp={(e) => { if (e.currentTarget.hasPointerCapture(e.pointerId)) e.currentTarget.releasePointerCapture(e.pointerId) }}
    >
      {/* 网格 */}
      {gridFreqs.map((f) => (
        <line key={f} x1={x(f)} y1={padT} x2={x(f)} y2={padT + ih} stroke={theme.dark ? 'rgba(255,255,255,0.07)' : 'rgba(0,0,0,0.07)'} strokeWidth={1} />
      ))}
      {/* 0dB 中线 */}
      <line x1={padL} y1={zeroY} x2={padL + iw} y2={zeroY} stroke={theme.dark ? 'rgba(255,255,255,0.18)' : 'rgba(0,0,0,0.18)'} strokeWidth={1} strokeDasharray="4 3" />

      {/* 参考曲线（Q 补偿预估等） */}
      {reference && reference.length > 0 && (
        <path d={linePath(reference)} fill="none" stroke={theme.dark ? 'rgba(255,255,255,0.35)' : 'rgba(0,0,0,0.3)'} strokeWidth={1.2} strokeDasharray="3 3" />
      )}

      {/* 主曲线 */}
      <path d={linePath(points)} fill="none" stroke={theme.accentColor} strokeWidth={2} strokeLinejoin="round" />
      {/* 填充 */}
      <path d={`${linePath(points)} L${x(points[points.length - 1]?.frequency ?? 20000)},${zeroY} L${x(points[0]?.frequency ?? 20)},${zeroY} Z`}
        fill={theme.accentColor} opacity={0.12} />

      {/* 控制点 */}
      {points.map((p, i) => (
        <g key={`${p.frequency}-${i}`}>
          <circle cx={x(p.frequency)} cy={y(p.gain)} r={8} fill="transparent" style={{ cursor: readonly ? 'default' : 'pointer' }} />
          <circle cx={x(p.frequency)} cy={y(p.gain)} r={4.5} fill={theme.dark ? '#0b0d14' : '#ffffff'} stroke={theme.accentColor} strokeWidth={2}
            style={{ cursor: readonly ? 'default' : 'pointer', filter: `drop-shadow(0 0 4px ${theme.accentColor}88)` }} />
        </g>
      ))}

      {/* 频率标签 */}
      {[100, 1000, 10000].map((f) => (
        <text key={f} x={x(f)} y={H - 5} textAnchor="middle" fontSize={9}
          fill={theme.dark ? 'rgba(255,255,255,0.4)' : 'rgba(0,0,0,0.45)'}>{f >= 1000 ? `${f / 1000}k` : f}Hz</text>
      ))}
      <text x={padL} y={H - 5} textAnchor="start" fontSize={9} fill={theme.dark ? 'rgba(255,255,255,0.4)' : 'rgba(0,0,0,0.45)'}>20</text>
      <text x={padL + iw} y={H - 5} textAnchor="end" fontSize={9} fill={theme.dark ? 'rgba(255,255,255,0.4)' : 'rgba(0,0,0,0.45)'}>20k</text>

      {/* 增益刻度提示 */}
      <text x={padL} y={y(maxDb) + 9} fontSize={9} fill={theme.dark ? 'rgba(255,255,255,0.35)' : 'rgba(0,0,0,0.4)'}>+{maxDb}</text>
      <text x={padL} y={y(minDb) + 9} fontSize={9} fill={theme.dark ? 'rgba(255,255,255,0.35)' : 'rgba(0,0,0,0.4)'}>{minDb}</text>
    </svg>
  )
}
