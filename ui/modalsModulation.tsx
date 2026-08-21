/**
 * HyperSoundEngine v1 调音室 UI —— 调制类效果配置弹窗
 *
 * 延迟 / 合唱 / 镶边 / 移相 / 颤音 + 参数调制矩阵（LFO / Envelope Follower → 路由）。
 * 视觉与交互沿用弹窗规范（Modal + Slider + Toggle + Segmented）。
 */

import { Timer, Waves, Wind, Zap, Volume2, Radio, Plus, Trash2 } from 'lucide-react'
import type { HyperSoundEngineTheme } from './theme'
import { ActionButton, InfoLine, Modal, Segmented, Slider, Toggle } from './primitives'
import type { HyperSoundEngineParamsController } from './hooks'
import type { LfoShape } from '../src/types'

const LFO_SHAPES: { value: LfoShape; label: string }[] = [
  { value: 'sine', label: '正弦' },
  { value: 'triangle', label: '三角' },
  { value: 'square', label: '方波' },
  { value: 'saw', label: '锯齿' },
]

const pct = (v: number) => `${Math.round(v * 100)}%`

/* ─────────────────────────── 延迟 ─────────────────────────── */

export function DelayModal({ controller, theme, onClose }: { controller: HyperSoundEngineParamsController; theme: HyperSoundEngineTheme; onClose: () => void }) {
  const { params, patch } = controller
  const d = params.modEffects.delay
  const set = (next: typeof d) => patch({ modEffects: { ...params.modEffects, delay: next } })
  return (
    <Modal title="延迟" icon={<Timer className="w-4.5 h-4.5" />} onClose={onClose} theme={theme}>
      <p className={`${theme.textSecondary} text-xs leading-relaxed mb-4`}>环形延迟线 + 反馈 + 干湿混合，用于回声、空间纵深与节奏型重复。</p>
      <div className="flex items-center justify-between mb-4">
        <span className={`${theme.textPrimary} text-sm font-medium`}>启用 延迟</span>
        <Toggle checked={d.enabled} onChange={(v) => set({ ...d, enabled: v })} theme={theme} />
      </div>
      <Slider label="延迟时间" value={d.delayMs} min={0} max={2000} step={1} onChange={(v) => set({ ...d, delayMs: v })} display={`${d.delayMs.toFixed(0)} ms`} theme={theme} />
      <Slider label="反馈" value={d.feedback} min={0} max={0.9} step={0.01} onChange={(v) => set({ ...d, feedback: v })} display={pct(d.feedback)} theme={theme} />
      <Slider label="湿声混合" value={d.mix} min={0} max={1} step={0.01} onChange={(v) => set({ ...d, mix: v })} display={pct(d.mix)} theme={theme} />
      <InfoLine theme={theme}>反馈上限 0.9 防止无限累积；延迟上限 2s。</InfoLine>
    </Modal>
  )
}

/* ─────────────────────────── 合唱 ─────────────────────────── */

export function ChorusModal({ controller, theme, onClose }: { controller: HyperSoundEngineParamsController; theme: HyperSoundEngineTheme; onClose: () => void }) {
  const { params, patch } = controller
  const c = params.modEffects.chorus
  const set = (next: typeof c) => patch({ modEffects: { ...params.modEffects, chorus: next } })
  return (
    <Modal title="合唱" icon={<Waves className="w-4.5 h-4.5" />} onClose={onClose} theme={theme}>
      <p className={`${theme.textSecondary} text-xs leading-relaxed mb-4`}>多路 LFO 调制分数延迟，营造厚度与空间感，适合吉他、人声、合成器铺底。</p>
      <div className="flex items-center justify-between mb-4">
        <span className={`${theme.textPrimary} text-sm font-medium`}>启用 合唱</span>
        <Toggle checked={c.enabled} onChange={(v) => set({ ...c, enabled: v })} theme={theme} />
      </div>
      <Slider label="调制速率" value={c.rateHz} min={0.1} max={10} step={0.05} onChange={(v) => set({ ...c, rateHz: v })} display={`${c.rateHz.toFixed(2)} Hz`} theme={theme} />
      <Slider label="调制深度" value={c.depthMs} min={0} max={20} step={0.1} onChange={(v) => set({ ...c, depthMs: v })} display={`${c.depthMs.toFixed(1)} ms`} theme={theme} />
      <Slider label="湿声混合" value={c.mix} min={0} max={1} step={0.01} onChange={(v) => set({ ...c, mix: v })} display={pct(c.mix)} theme={theme} />
      <InfoLine theme={theme}>速率越快越"颤"，深度越大延迟摆动越宽。</InfoLine>
    </Modal>
  )
}

/* ─────────────────────────── 镶边 ─────────────────────────── */

export function FlangerModal({ controller, theme, onClose }: { controller: HyperSoundEngineParamsController; theme: HyperSoundEngineTheme; onClose: () => void }) {
  const { params, patch } = controller
  const f = params.modEffects.flanger
  const set = (next: typeof f) => patch({ modEffects: { ...params.modEffects, flanger: next } })
  return (
    <Modal title="镶边" icon={<Wind className="w-4.5 h-4.5" />} onClose={onClose} theme={theme}>
      <p className={`${theme.textSecondary} text-xs leading-relaxed mb-4`}>短延迟 + LFO 调制 + 反馈，产生"喷气式"扫频梳状滤波，标志性金属扫频效果。</p>
      <div className="flex items-center justify-between mb-4">
        <span className={`${theme.textPrimary} text-sm font-medium`}>启用 镶边</span>
        <Toggle checked={f.enabled} onChange={(v) => set({ ...f, enabled: v })} theme={theme} />
      </div>
      <Slider label="调制速率" value={f.rateHz} min={0.05} max={10} step={0.05} onChange={(v) => set({ ...f, rateHz: v })} display={`${f.rateHz.toFixed(2)} Hz`} theme={theme} />
      <Slider label="调制深度" value={f.depthMs} min={0} max={10} step={0.05} onChange={(v) => set({ ...f, depthMs: v })} display={`${f.depthMs.toFixed(2)} ms`} theme={theme} />
      <Slider label="反馈" value={f.feedback} min={-0.9} max={0.9} step={0.01} onChange={(v) => set({ ...f, feedback: v })} display={pct(Math.abs(f.feedback)) + (f.feedback < 0 ? '（反相）' : '')} theme={theme} />
      <Slider label="湿声混合" value={f.mix} min={0} max={1} step={0.01} onChange={(v) => set({ ...f, mix: v })} display={pct(f.mix)} theme={theme} />
      <InfoLine theme={theme}>负反馈音色更空洞；速率与深度共同决定扫频速度。</InfoLine>
    </Modal>
  )
}

/* ─────────────────────────── 移相 ─────────────────────────── */

const PHASER_STAGES = [2, 4, 6, 8]

export function PhaserModal({ controller, theme, onClose }: { controller: HyperSoundEngineParamsController; theme: HyperSoundEngineTheme; onClose: () => void }) {
  const { params, patch } = controller
  const p = params.modEffects.phaser
  const set = (next: typeof p) => patch({ modEffects: { ...params.modEffects, phaser: next } })
  return (
    <Modal title="移相" icon={<Zap className="w-4.5 h-4.5" />} onClose={onClose} theme={theme}>
      <p className={`${theme.textSecondary} text-xs leading-relaxed mb-4`}>多级一阶全通滤波器 + LFO 调制中心频率 + 反馈，产生柔和的相位扫频，比镶边更平滑。</p>
      <div className="flex items-center justify-between mb-4">
        <span className={`${theme.textPrimary} text-sm font-medium`}>启用 移相</span>
        <Toggle checked={p.enabled} onChange={(v) => set({ ...p, enabled: v })} theme={theme} />
      </div>
      <Slider label="调制速率" value={p.rateHz} min={0.05} max={10} step={0.05} onChange={(v) => set({ ...p, rateHz: v })} display={`${p.rateHz.toFixed(2)} Hz`} theme={theme} />
      <Slider label="调制深度" value={p.depth} min={0} max={1} step={0.01} onChange={(v) => set({ ...p, depth: v })} display={pct(p.depth)} theme={theme} />
      <Slider label="反馈" value={p.feedback} min={0} max={0.9} step={0.01} onChange={(v) => set({ ...p, feedback: v })} display={pct(p.feedback)} theme={theme} />
      <Slider label="湿声混合" value={p.mix} min={0} max={1} step={0.01} onChange={(v) => set({ ...p, mix: v })} display={pct(p.mix)} theme={theme} />
      <div className={`${theme.textSecondary} text-xs mb-1.5`}>全通级数</div>
      <div className="grid grid-cols-4 gap-1.5 mb-2">
        {PHASER_STAGES.map((s) => {
          const active = p.stages === s
          return (
            <button key={s} type="button" onClick={() => set({ ...p, stages: s })}
              className={`py-1.5 rounded-lg text-xs font-medium transition-colors ${active ? '' : theme.dark ? 'bg-white/10' : 'bg-black/8'}`}
              style={active ? { backgroundColor: theme.accentColor, color: '#fff' } : undefined}>{s} 级</button>
          )
        })}
      </div>
      <InfoLine theme={theme}>级数越多凹陷越深、音色越"旋"；常见 4 级。</InfoLine>
    </Modal>
  )
}

/* ─────────────────────────── 颤音 ─────────────────────────── */

export function TremoloModal({ controller, theme, onClose }: { controller: HyperSoundEngineParamsController; theme: HyperSoundEngineTheme; onClose: () => void }) {
  const { params, patch } = controller
  const t = params.modEffects.tremolo
  const set = (next: typeof t) => patch({ modEffects: { ...params.modEffects, tremolo: next } })
  return (
    <Modal title="颤音" icon={<Volume2 className="w-4.5 h-4.5" />} onClose={onClose} theme={theme}>
      <p className={`${theme.textSecondary} text-xs leading-relaxed mb-4`}>LFO 调制信号幅度，产生周期性音量起伏，常用于吉他复古颤音与氛围铺底。</p>
      <div className="flex items-center justify-between mb-4">
        <span className={`${theme.textPrimary} text-sm font-medium`}>启用 颤音</span>
        <Toggle checked={t.enabled} onChange={(v) => set({ ...t, enabled: v })} theme={theme} />
      </div>
      <Slider label="调制速率" value={t.rateHz} min={0.1} max={20} step={0.05} onChange={(v) => set({ ...t, rateHz: v })} display={`${t.rateHz.toFixed(2)} Hz`} theme={theme} />
      <Slider label="调制深度" value={t.depth} min={0} max={1} step={0.01} onChange={(v) => set({ ...t, depth: v })} display={pct(t.depth)} theme={theme} />
      <Slider label="湿声混合" value={t.mix} min={0} max={1} step={0.01} onChange={(v) => set({ ...t, mix: v })} display={pct(t.mix)} theme={theme} />
      <InfoLine theme={theme}>深度 100% 时音量可完全切断；混合控制干湿比例。</InfoLine>
    </Modal>
  )
}

/* ─────────────────────────── 参数调制矩阵 ─────────────────────────── */

const MOD_SOURCES: { value: 'lfo' | 'envelope'; label: string }[] = [
  { value: 'lfo', label: 'LFO' },
  { value: 'envelope', label: '包络' },
]
const MOD_TARGETS: { value: 'masterGain' | 'stereoWidth'; label: string }[] = [
  { value: 'masterGain', label: '主增益' },
  { value: 'stereoWidth', label: '立体声宽度' },
]

export function ModulationMatrixModal({ controller, theme, onClose }: { controller: HyperSoundEngineParamsController; theme: HyperSoundEngineTheme; onClose: () => void }) {
  const { params, patch } = controller
  const m = params.modulation
  const set = (next: typeof m) => patch({ modulation: next })
  const routes = m.routes
  const updateRoute = (i: number, p: Partial<typeof routes[number]>) =>
    set({ ...m, routes: routes.map((r, idx) => (idx === i ? { ...r, ...p } : { ...r })) })
  const addRoute = () =>
    set({ ...m, routes: [...routes, { source: 'lfo', target: 'masterGain', amount: 0.3, offset: 0 }] })
  const removeRoute = (i: number) => set({ ...m, routes: routes.filter((_, idx) => idx !== i) })

  return (
    <Modal title="参数调制矩阵" icon={<Radio className="w-4.5 h-4.5" />} onClose={onClose} theme={theme}>
      <p className={`${theme.textSecondary} text-xs leading-relaxed mb-4`}>LFO / 包络跟随器作为调制源，按路由叠加到主增益或立体声宽度，实现自动化的呼吸、摆动与动态控制。</p>
      <div className="flex items-center justify-between mb-4">
        <span className={`${theme.textPrimary} text-sm font-medium`}>启用 调制矩阵</span>
        <Toggle checked={m.enabled} onChange={(v) => set({ ...m, enabled: v })} theme={theme} />
      </div>

      {/* LFO */}
      <div className={`rounded-xl p-3 mb-3 ${theme.dark ? 'bg-white/5' : 'bg-black/4'}`} style={{ border: `1px solid ${theme.glassBorder}` }}>
        <div className={`${theme.textPrimary} text-xs font-medium mb-2`}>LFO 调制源</div>
        <div className="flex items-center justify-between mb-2">
          <span className={`${theme.textSecondary} text-xs`}>启用 LFO</span>
          <Toggle checked={m.lfo.enabled} onChange={(v) => set({ ...m, lfo: { ...m.lfo, enabled: v } })} theme={theme} />
        </div>
        <div className={`${theme.textSecondary} text-xs mb-1.5`}>波形</div>
        <Segmented options={LFO_SHAPES} value={m.lfo.shape} onChange={(v) => set({ ...m, lfo: { ...m.lfo, shape: v as LfoShape } })} theme={theme} />
        <Slider label="速率" value={m.lfo.rateHz} min={0.05} max={20} step={0.05} onChange={(v) => set({ ...m, lfo: { ...m.lfo, rateHz: v } })} display={`${m.lfo.rateHz.toFixed(2)} Hz`} theme={theme} />
        <Slider label="深度" value={m.lfo.depth} min={0} max={1} step={0.01} onChange={(v) => set({ ...m, lfo: { ...m.lfo, depth: v } })} display={pct(m.lfo.depth)} theme={theme} />
      </div>

      {/* 包络跟随器 */}
      <div className={`rounded-xl p-3 mb-3 ${theme.dark ? 'bg-white/5' : 'bg-black/4'}`} style={{ border: `1px solid ${theme.glassBorder}` }}>
        <div className={`${theme.textPrimary} text-xs font-medium mb-2`}>包络跟随器</div>
        <div className="flex items-center justify-between mb-2">
          <span className={`${theme.textSecondary} text-xs`}>启用 包络</span>
          <Toggle checked={m.envelope.enabled} onChange={(v) => set({ ...m, envelope: { ...m.envelope, enabled: v } })} theme={theme} />
        </div>
        <Slider label="起控" value={m.envelope.attackMs} min={0.5} max={500} step={0.5} onChange={(v) => set({ ...m, envelope: { ...m.envelope, attackMs: v } })} display={`${m.envelope.attackMs.toFixed(1)} ms`} theme={theme} />
        <Slider label="释放" value={m.envelope.releaseMs} min={10} max={3000} step={5} onChange={(v) => set({ ...m, envelope: { ...m.envelope, releaseMs: v } })} display={`${m.envelope.releaseMs.toFixed(0)} ms`} theme={theme} />
        <Slider label="强度" value={m.envelope.amount} min={0} max={1} step={0.01} onChange={(v) => set({ ...m, envelope: { ...m.envelope, amount: v } })} display={pct(m.envelope.amount)} theme={theme} />
      </div>

      {/* 路由列表 */}
      <div className={`${theme.textPrimary} text-xs font-medium mb-2`}>调制路由（{routes.length}）</div>
      {routes.length === 0 && (
        <div className={`${theme.textTertiary} text-xs mb-2`}>暂无路由，点击下方添加。</div>
      )}
      <div className="space-y-2 mb-3">
        {routes.map((r, i) => (
          <div key={i} className={`rounded-lg p-2.5 ${theme.dark ? 'bg-white/5' : 'bg-black/4'}`} style={{ border: `1px solid ${theme.glassBorder}` }}>
            <div className="flex items-center gap-2 mb-2">
              <div className="flex-1">
                <Segmented options={MOD_SOURCES} value={r.source} onChange={(v) => updateRoute(i, { source: v as 'lfo' | 'envelope' })} theme={theme} />
              </div>
              <span className={`${theme.textTertiary} text-xs`}>→</span>
              <div className="flex-1">
                <Segmented options={MOD_TARGETS} value={r.target} onChange={(v) => updateRoute(i, { target: v as 'masterGain' | 'stereoWidth' })} theme={theme} />
              </div>
              <button type="button" onClick={() => removeRoute(i)} className={`shrink-0 p-1 rounded-md ${theme.dark ? 'hover:bg-white/10' : 'hover:bg-black/8'} ${theme.textTertiary}`} title="删除路由">
                <Trash2 className="w-3.5 h-3.5" />
              </button>
            </div>
            <Slider label="调制量" value={r.amount} min={-1} max={1} step={0.01} onChange={(v) => updateRoute(i, { amount: v })} display={`${r.amount > 0 ? '+' : ''}${(r.amount * 100).toFixed(0)}%`} theme={theme} />
            <Slider label="偏移" value={r.offset ?? 0} min={-2} max={2} step={0.01} onChange={(v) => updateRoute(i, { offset: v })} display={`${(r.offset ?? 0) > 0 ? '+' : ''}${((r.offset ?? 0) * 100).toFixed(0)}%`} theme={theme} />
          </div>
        ))}
      </div>
      <ActionButton onClick={addRoute} theme={theme}>
        <Plus className="w-4 h-4" /> 添加路由
      </ActionButton>
      <InfoLine theme={theme}>主增益路由使音量随调制源呼吸；立体声宽度路由让声场周期性开合。</InfoLine>
    </Modal>
  )
}

/* 聚合导出 */
export function ModulationModal({ effectKey: key, controller, theme, onClose }: { effectKey: 'delay' | 'chorus' | 'flanger' | 'phaser' | 'tremolo' | 'modulation'; controller: HyperSoundEngineParamsController; theme: HyperSoundEngineTheme; onClose: () => void }) {
  if (key === 'delay') return <DelayModal controller={controller} theme={theme} onClose={onClose} />
  if (key === 'chorus') return <ChorusModal controller={controller} theme={theme} onClose={onClose} />
  if (key === 'flanger') return <FlangerModal controller={controller} theme={theme} onClose={onClose} />
  if (key === 'phaser') return <PhaserModal controller={controller} theme={theme} onClose={onClose} />
  if (key === 'tremolo') return <TremoloModal controller={controller} theme={theme} onClose={onClose} />
  return <ModulationMatrixModal controller={controller} theme={theme} onClose={onClose} />
}
