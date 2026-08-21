/**
 * HyperSoundEngine v1 调音室 UI —— 动态/调音类效果配置弹窗
 *
 * 动态压缩、齿音抑制、夜间模式、限幅器、智能均衡（IEQ）、变速变调、立体声宽度。
 * 视觉与交互沿用弹窗规范（glass 面板 + wf-glass-range + 胶囊开关）。
 */

import { Activity, Mic2, Moon, Shield, Sparkles, Music, Columns2 } from 'lucide-react'
import type { HyperSoundEngineTheme } from './theme'
import { InfoLine, Modal, Segmented, Slider, Toggle } from './primitives'
import type { HyperSoundEngineParamsController } from './hooks'

/* ─────────────────────────── 动态压缩 ─────────────────────────── */

export function CompressorModal({ controller, theme, onClose }: { controller: HyperSoundEngineParamsController; theme: HyperSoundEngineTheme; onClose: () => void }) {
  const { params, patch } = controller
  const c = params.compressor
  return (
    <Modal title="动态压缩" icon={<Activity className="w-4.5 h-4.5" />} onClose={onClose} theme={theme}>
      <p className={`${theme.textSecondary} text-xs leading-relaxed mb-4`}>软拐点压缩器压平音量起伏，让轻的部分更清晰、重的部分不爆。</p>
      <div className="flex items-center justify-between mb-4">
        <span className={`${theme.textPrimary} text-sm font-medium`}>启用 动态压缩</span>
        <Toggle checked={c.enabled} onChange={(v) => patch({ compressor: { ...c, enabled: v } })} theme={theme} />
      </div>
      <Slider label="阈值" value={c.thresholdDb} min={-60} max={0} step={1} onChange={(v) => patch({ compressor: { ...c, thresholdDb: v } })} display={`${c.thresholdDb}dB`} theme={theme} />
      <Slider label="比率" value={c.ratio} min={1} max={20} step={0.5} onChange={(v) => patch({ compressor: { ...c, ratio: v } })} display={`${c.ratio.toFixed(1)}:1`} theme={theme} />
      <Slider label="拐点宽度" value={c.kneeDb} min={0} max={20} step={0.5} onChange={(v) => patch({ compressor: { ...c, kneeDb: v } })} display={`${c.kneeDb.toFixed(1)}dB`} theme={theme} />
      <Slider label="起始时间" value={c.attackMs} min={0} max={100} step={1} onChange={(v) => patch({ compressor: { ...c, attackMs: v } })} display={`${c.attackMs}ms`} theme={theme} />
      <Slider label="释放时间" value={c.releaseMs} min={10} max={1000} step={10} onChange={(v) => patch({ compressor: { ...c, releaseMs: v } })} display={`${c.releaseMs}ms`} theme={theme} />
      <Slider label="补偿增益" value={c.makeupDb} min={0} max={12} step={0.5} onChange={(v) => patch({ compressor: { ...c, makeupDb: v } })} display={`+${c.makeupDb.toFixed(1)}dB`} theme={theme} />
      <Slider label="输出增益" value={c.outputGain} min={0} max={2} step={0.05} onChange={(v) => patch({ compressor: { ...c, outputGain: v } })} display={`${c.outputGain.toFixed(2)}x`} theme={theme} />
      <div className="flex items-center justify-between mb-2 mt-1">
        <span className={`${theme.textPrimary} text-sm font-medium`}>外部 Sidechain</span>
        <Toggle checked={!!c.sidechainEnabled} onChange={(v) => patch({ compressor: { ...c, sidechainEnabled: v } })} theme={theme} />
      </div>
      <InfoLine theme={theme}>开启后由 process() 第三参数传入的侧链信号驱动包络（如 kick 触发 ducking）；未传入侧链时回退到主信号检测。</InfoLine>
    </Modal>
  )
}

/* ─────────────────────────── 齿音抑制 ─────────────────────────── */

export function DeesserModal({ controller, theme, onClose }: { controller: HyperSoundEngineParamsController; theme: HyperSoundEngineTheme; onClose: () => void }) {
  const { params, patch } = controller
  const d = params.deesser
  return (
    <Modal title="齿音抑制" icon={<Mic2 className="w-4.5 h-4.5" />} onClose={onClose} theme={theme}>
      <p className={`${theme.textSecondary} text-xs leading-relaxed mb-4`}>侧链带通检测 4-8kHz 齿音频段（s/z 音）并动态压低，消除刺耳齿音。</p>
      <div className="flex items-center justify-between mb-4">
        <span className={`${theme.textPrimary} text-sm font-medium`}>启用 齿音抑制</span>
        <Toggle checked={d.enabled} onChange={(v) => patch({ deesser: { ...d, enabled: v } })} theme={theme} />
      </div>
      <Segmented
        options={[
          { value: true as const, label: '分带式（推荐）' },
          { value: false as const, label: '宽带式' },
        ]}
        value={d.splitBand}
        onChange={(v) => patch({ deesser: { ...d, splitBand: v } })}
        theme={theme}
        small
      />
      <Slider label="中心频率" value={d.centerHz} min={2000} max={10000} step={100} onChange={(v) => patch({ deesser: { ...d, centerHz: v } })} display={`${d.centerHz >= 1000 ? (d.centerHz / 1000).toFixed(1) + 'k' : d.centerHz}Hz`} theme={theme} />
      <Slider label="触发阈值" value={d.thresholdDb} min={-50} max={0} step={1} onChange={(v) => patch({ deesser: { ...d, thresholdDb: v } })} display={`${d.thresholdDb}dB`} theme={theme} />
      <Slider label="压缩比率" value={d.ratio} min={1} max={20} step={0.5} onChange={(v) => patch({ deesser: { ...d, ratio: v } })} display={`${d.ratio.toFixed(1)}:1`} theme={theme} />
      <Slider label="起始时间" value={d.attackMs} min={0} max={10} step={0.1} onChange={(v) => patch({ deesser: { ...d, attackMs: v } })} display={`${d.attackMs.toFixed(1)}ms`} theme={theme} />
      <Slider label="释放时间" value={d.releaseMs} min={10} max={300} step={5} onChange={(v) => patch({ deesser: { ...d, releaseMs: v } })} display={`${d.releaseMs}ms`} theme={theme} />
      <Slider label="效果混合" value={d.mix} min={0} max={1} step={0.01} onChange={(v) => patch({ deesser: { ...d, mix: v } })} display={`${Math.round(d.mix * 100)}%`} theme={theme} />
      <div className="flex items-center justify-between mb-2 mt-1">
        <span className={`${theme.textPrimary} text-sm font-medium`}>外部 Sidechain</span>
        <Toggle checked={!!d.sidechainEnabled} onChange={(v) => patch({ deesser: { ...d, sidechainEnabled: v } })} theme={theme} />
      </div>
      <InfoLine theme={theme}>开启后由侧链信号检测齿音（如人声轨触发、压乐器轨）；未传入侧链时回退到主信号检测。</InfoLine>
    </Modal>
  )
}

/* ─────────────────────────── 夜间模式 ─────────────────────────── */

export function NightModeModal({ controller, theme, onClose }: { controller: HyperSoundEngineParamsController; theme: HyperSoundEngineTheme; onClose: () => void }) {
  const { params, patch } = controller
  const n = params.nightMode
  return (
    <Modal title="夜间模式" icon={<Moon className="w-4.5 h-4.5" />} onClose={onClose} theme={theme}>
      <p className={`${theme.textSecondary} text-xs leading-relaxed mb-4`}>动态压缩增强 + 6kHz 高频衰减：深夜低音量听感不刺耳、不吵人。</p>
      <div className="flex items-center justify-between mb-4">
        <span className={`${theme.textPrimary} text-sm font-medium`}>启用 夜间模式</span>
        <Toggle checked={n.enabled} onChange={(v) => patch({ nightMode: { ...n, enabled: v } })} theme={theme} />
      </div>
      <Slider label="强度" value={n.amount} min={0} max={10} step={1} onChange={(v) => patch({ nightMode: { ...n, amount: v } })} display={`${n.amount} 级`} theme={theme} />
      <InfoLine theme={theme}>强度为 0 时等效关闭；建议与音量自适应补偿同开，低音量听感更平衡。</InfoLine>
    </Modal>
  )
}

/* ─────────────────────────── 限幅器 ─────────────────────────── */

export function LimiterModal({ controller, theme, onClose }: { controller: HyperSoundEngineParamsController; theme: HyperSoundEngineTheme; onClose: () => void }) {
  const { params, patch } = controller
  const l = params.limiter
  return (
    <Modal title="限幅器" icon={<Shield className="w-4.5 h-4.5" />} onClose={onClose} theme={theme}>
      <p className={`${theme.textSecondary} text-xs leading-relaxed mb-4`}>前瞻式限幅保护输出：默认 -1dBFS 阈值，杜绝削波；真峰值检测为 4× 过采样。</p>
      <div className="flex items-center justify-between mb-4">
        <span className={`${theme.textPrimary} text-sm font-medium`}>启用 限幅器</span>
        <Toggle checked={l.enabled} onChange={(v) => patch({ limiter: { ...l, enabled: v } })} theme={theme} />
      </div>
      <Slider label="阈值" value={l.thresholdDb} min={-12} max={0} step={0.1} onChange={(v) => patch({ limiter: { ...l, thresholdDb: v } })} display={`${l.thresholdDb.toFixed(1)}dBFS`} theme={theme} />
      <Slider label="前瞻时间" value={l.lookaheadMs} min={0} max={20} step={0.5} onChange={(v) => patch({ limiter: { ...l, lookaheadMs: v } })} display={`${l.lookaheadMs.toFixed(1)}ms`} theme={theme} />
      <Slider label="起始时间" value={l.attackMs} min={0} max={5} step={0.1} onChange={(v) => patch({ limiter: { ...l, attackMs: v } })} display={`${l.attackMs.toFixed(1)}ms`} theme={theme} />
      <Slider label="释放时间" value={l.releaseMs} min={20} max={500} step={10} onChange={(v) => patch({ limiter: { ...l, releaseMs: v } })} display={`${l.releaseMs}ms`} theme={theme} />
      <div className="flex items-center justify-between mb-2">
        <span className={`${theme.textSecondary} text-xs`}>真峰值检测（4× 过采样）</span>
        <Toggle checked={l.truePeak} onChange={(v) => patch({ limiter: { ...l, truePeak: v } })} theme={theme} />
      </div>
      <InfoLine theme={theme}>前瞻时间贡献引擎延迟（见分析页 latency 读数）。</InfoLine>
    </Modal>
  )
}

/* ─────────────────────────── 智能均衡 IEQ ─────────────────────────── */

export const IEQ_CURVES: { value: 'flat' | 'warm' | 'bright' | 'vocal'; label: string; hint: string }[] = [
  { value: 'flat', label: '平坦', hint: '中性直白，还原混音' },
  { value: 'warm', label: '温暖', hint: '中低频略厚' },
  { value: 'bright', label: '通透', hint: '高频更亮' },
  { value: 'vocal', label: '人声', hint: '突出人声频段' },
]

export function IeqModal({ controller, theme, onClose }: { controller: HyperSoundEngineParamsController; theme: HyperSoundEngineTheme; onClose: () => void }) {
  const { params, patch } = controller
  const ieq = params.ieq
  return (
    <Modal title="智能均衡" icon={<Sparkles className="w-4.5 h-4.5" />} onClose={onClose} theme={theme}>
      <p className={`${theme.textSecondary} text-xs leading-relaxed mb-4`}>实时分析频谱并与目标曲线对比，慢速平滑自动修正频响，不会跟随音乐抽吸。</p>
      <div className="flex items-center justify-between mb-4">
        <span className={`${theme.textPrimary} text-sm font-medium`}>启用 智能均衡</span>
        <Toggle checked={ieq.enabled} onChange={(v) => patch({ ieq: { ...ieq, enabled: v } })} theme={theme} />
      </div>
      <div className={`${theme.textSecondary} text-xs mb-1.5`}>目标曲线</div>
      <div className="grid grid-cols-4 gap-1.5 mb-3">
        {IEQ_CURVES.map((cv) => {
          const active = ieq.targetCurve === cv.value
          return (
            <button key={cv.value} type="button" title={cv.hint} onClick={() => patch({ ieq: { ...ieq, targetCurve: cv.value } })}
              className="py-1.5 rounded-lg text-[11px] transition-all"
              style={active ? { backgroundColor: theme.accentColor, color: '#fff', boxShadow: `0 0 10px ${theme.accentColor}55` } : { background: theme.dark ? 'rgba(255,255,255,0.08)' : 'rgba(0,0,0,0.06)', color: theme.textSecondary }}>
              {cv.label}
            </button>
          )
        })}
      </div>
      <Slider label="修正强度" value={ieq.strength} min={0} max={1} step={0.01} onChange={(v) => patch({ ieq: { ...ieq, strength: v } })} display={`${Math.round(ieq.strength * 100)}%`} theme={theme} />
      <Slider label="平滑时间" value={ieq.timeConstantSec} min={0.5} max={10} step={0.1} onChange={(v) => patch({ ieq: { ...ieq, timeConstantSec: v } })} display={`${ieq.timeConstantSec.toFixed(1)}s`} theme={theme} />
      <InfoLine theme={theme}>强度 0% = 只分析不修正；建议 3s 平滑（默认）防抽吸。</InfoLine>
    </Modal>
  )
}

/* ─────────────────────────── 变速变调 ─────────────────────────── */

export function PitchModal({ controller, theme, onClose }: { controller: HyperSoundEngineParamsController; theme: HyperSoundEngineTheme; onClose: () => void }) {
  const { params, patch } = controller
  const pitch = params.pitch
  return (
    <Modal title="变速变调" icon={<Music className="w-4.5 h-4.5" />} onClose={onClose} theme={theme}>
      <p className={`${theme.textSecondary} text-xs leading-relaxed mb-4`}>独立变调与变速（相位声码器自研实现；融合侧可选 signalsmith WASM 路径）。</p>
      <div className="flex items-center justify-between mb-4">
        <span className={`${theme.textPrimary} text-sm font-medium`}>启用 变速变调</span>
        <Toggle checked={pitch.enabled} onChange={(v) => patch({ pitch: { ...pitch, enabled: v } })} theme={theme} />
      </div>
      <Slider label="变调" value={pitch.semitones} min={-10} max={10} step={0.5} onChange={(v) => patch({ pitch: { ...pitch, semitones: v } })} display={`${pitch.semitones > 0 ? '+' : ''}${pitch.semitones} 半音`} theme={theme} />
      <Slider label="倍速" value={pitch.rate} min={0.25} max={3} step={0.05} onChange={(v) => patch({ pitch: { ...pitch, rate: v } })} display={`${pitch.rate.toFixed(2)}x`} theme={theme} />
      <InfoLine theme={theme}>变调与变速互相独立；倍速 ≠ 播放器变速（仅在启用时作用于音色）。</InfoLine>
    </Modal>
  )
}

/* ─────────────────────────── 立体声宽度 ─────────────────────────── */

export function StereoWidthModal({ controller, theme, onClose }: { controller: HyperSoundEngineParamsController; theme: HyperSoundEngineTheme; onClose: () => void }) {
  const { params, patch } = controller
  const p = params
  const vb = p.pitch.voiceBalance
  return (
    <Modal title="立体声宽度" icon={<Columns2 className="w-4.5 h-4.5" />} onClose={onClose} theme={theme}>
      <p className={`${theme.textSecondary} text-xs leading-relaxed mb-4`}>基于中/侧声道分离：宽度控制声场开合，人声比例在伴奏与纯人声之间滑动（卡拉OK级）。</p>
      <Slider label="立体声宽度" value={p.stereoWidth} min={0} max={2} step={0.05} onChange={(v) => patch({ stereoWidth: v })} display={`${p.stereoWidth.toFixed(2)}x`} theme={theme} />
      <Slider label="人声 ↔ 伴奏" value={vb} min={-1} max={1} step={0.05}
        onChange={(v) => patch({ pitch: { ...p.pitch, voiceBalance: v } })}
        display={vb === 0 ? '原声' : vb > 0 ? `人声 +${Math.round(vb * 100)}%` : `伴奏 +${Math.round(-vb * 100)}%`} theme={theme} />
      <InfoLine theme={theme}>宽度 1.0 = 原始；0 = 单声道；2 = 极宽。人声比例同时影响居中低频。</InfoLine>
    </Modal>
  )
}

/* 聚合导出：动态/调音类弹窗按 key 分发 */
export function DynamicsModal({ effectKey: key, controller, theme, onClose }: { effectKey: 'compressor' | 'deesser' | 'nightMode' | 'limiter' | 'ieq' | 'pitch' | 'stereoWidth'; controller: HyperSoundEngineParamsController; theme: HyperSoundEngineTheme; onClose: () => void }) {
  if (key === 'compressor') return <CompressorModal controller={controller} theme={theme} onClose={onClose} />
  if (key === 'deesser') return <DeesserModal controller={controller} theme={theme} onClose={onClose} />
  if (key === 'nightMode') return <NightModeModal controller={controller} theme={theme} onClose={onClose} />
  if (key === 'limiter') return <LimiterModal controller={controller} theme={theme} onClose={onClose} />
  if (key === 'ieq') return <IeqModal controller={controller} theme={theme} onClose={onClose} />
  if (key === 'pitch') return <PitchModal controller={controller} theme={theme} onClose={onClose} />
  return <StereoWidthModal controller={controller} theme={theme} onClose={onClose} />
}