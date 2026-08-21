/**
 * MIDI Learn 面板 —— 调音室 MIDI 页签
 *
 * 职责（PRD UI 消费层，非测试 seam）：
 * - 参数路径下拉（来自 AUTOMATABLE_PARAMS 白名单）+ CC 号输入 → 绑定；
 * - Learn 模式：进入后绑定流程提示；
 * - 绑定表展示（CC / 参数 / 范围 / 平滑 / 反向 / 删除）；
 * - 平滑/反向开关；
 * - 测试发送 CC（演示 sendMidi 通路）；
 * - 仅渲染冒烟覆盖（uiSmoke），不做深度 DOM 交互测试。
 */

import { useState } from 'react'
import { Radio, Plus, Trash2, Zap, Target } from 'lucide-react'
import type { HyperSoundEngineTheme } from './theme'
import type { HyperSoundEngineUiBridge } from './bridge'
import { GlassCard, SectionTitle, ActionButton, InfoLine, Segmented, Slider } from './primitives'
import { AUTOMATABLE_PARAMS, type AutomationTarget, type MidiBinding } from '../src/types'

function bindingLabel(b: MidiBinding): string {
  if (b.target.kind === 'builtin') return b.target.param
  const path = b.target.path
  const meta = AUTOMATABLE_PARAMS.find((m) => m.path === path)
  return meta ? meta.label : path
}

export function MidiPanel({ bridge, theme }: { bridge: HyperSoundEngineUiBridge; theme: HyperSoundEngineTheme }) {
  const midi = bridge.midi
  const [bindings, setBindings] = useState(() => (midi ? midi.getBindings() : []))
  const [selectedPath, setSelectedPath] = useState(AUTOMATABLE_PARAMS[3].path) // compressor.thresholdDb
  const [cc, setCc] = useState(7)
  const [eventType, setEventType] = useState<'cc' | 'note'>('cc')
  const [smoothMs, setSmoothMs] = useState(20)
  const [invert, setInvert] = useState(false)
  const [learnMode, setLearnMode] = useState(false)
  const [dropped, setDropped] = useState(0)

  const refresh = () => { if (midi) setBindings([...midi.getBindings()]) }

  const doBind = () => {
    if (!midi) return
    const meta = AUTOMATABLE_PARAMS.find((m) => m.path === selectedPath)
    if (!meta) return
    const target: AutomationTarget = (meta.path === 'masterGain' || meta.path === 'stereoWidth')
      ? { kind: 'builtin', param: meta.path as 'masterGain' | 'stereoWidth' }
      : { kind: 'path', path: meta.path }
    midi.learn(cc, target, { eventType, min: meta.min, max: meta.max, smoothMs, invert })
    refresh()
    setLearnMode(false)
  }

  const doUnlearn = (b: MidiBinding) => {
    if (!midi) return
    midi.unlearn(b.cc, { eventType: 'cc' })
    midi.unlearn(b.cc, { eventType: 'note' })
    refresh()
  }

  const sendTestCc = (value: number) => {
    if (!midi) return
    midi.sendMidi([{ type: 'cc', channel: 0, cc, value }])
    setDropped(midi.getDroppedCount())
  }

  return (
    <div className="space-y-4">
      {!midi && (
        <GlassCard theme={theme} className="p-4">
          <InfoLine theme={theme}>当前后端不支持 MIDI 接口（需 HyperSoundEngine 引擎）。</InfoLine>
        </GlassCard>
      )}

      <SectionTitle icon={<Radio className="w-4 h-4" />} theme={theme} hint="把外部控制器（CC/Note）绑定到引擎参数">
        MIDI Learn
      </SectionTitle>

      <GlassCard theme={theme} className="p-4 space-y-3">
        <div className="flex items-center gap-2">
          <Target className={'w-4 h-4 ' + theme.textSecondary} />
          <select
            value={selectedPath}
            onChange={(e) => setSelectedPath(e.target.value)}
            className={'flex-1 px-3 py-2 rounded-xl text-sm ' + (theme.dark ? 'bg-white/10 text-white' : 'bg-black/5 text-gray-900')}
            style={{ border: `1px solid ${theme.glassBorder}` }}
          >
            {AUTOMATABLE_PARAMS.map((m) => (
              <option key={m.path} value={m.path}>{m.label}</option>
            ))}
          </select>
        </div>

        <div className="flex items-center gap-3">
          <span className={'text-xs ' + theme.textSecondary}>事件类型</span>
          <Segmented
            options={[{ label: 'CC', value: 'cc' }, { label: 'Note', value: 'note' }]}
            value={eventType}
            onChange={(v) => setEventType(v as 'cc' | 'note')}
            theme={theme}
            small
          />
          <span className={'text-xs ' + theme.textSecondary}>{eventType === 'cc' ? 'CC 号' : 'Note 号'}</span>
          <input
            type="number"
            min={0}
            max={127}
            value={cc}
            onChange={(e) => setCc(Math.max(0, Math.min(127, Number(e.target.value) | 0)))}
            className={'w-20 px-2 py-1 rounded-lg text-sm ' + (theme.dark ? 'bg-white/10 text-white' : 'bg-black/5 text-gray-900')}
            style={{ border: `1px solid ${theme.glassBorder}` }}
          />
        </div>

        <Slider label="平滑时间" value={smoothMs} min={0} max={500} step={5} onChange={setSmoothMs}
          display={`${smoothMs} ms`} theme={theme} />
        <div className="flex items-center gap-3">
          <Segmented
            options={[{ label: '正向', value: 'fwd' }, { label: '反向', value: 'inv' }]}
            value={invert ? 'inv' : 'fwd'}
            onChange={(v) => setInvert(v === 'inv')}
            theme={theme}
            small
          />
          <ActionButton onClick={doBind} theme={theme} disabled={!midi} title="绑定">
            <Plus className="w-4 h-4" /> 绑定
          </ActionButton>
          <ActionButton onClick={() => setLearnMode((v) => !v)} theme={theme} ghost title="Learn 模式">
            <Zap className="w-4 h-4" /> {learnMode ? '学习中…' : 'Learn'}
          </ActionButton>
        </div>
        {learnMode && (
          <InfoLine theme={theme}>Learn 模式：选择参数与 CC 后点击"绑定"即完成（无硬件时用此流程）。</InfoLine>
        )}
      </GlassCard>

      <SectionTitle icon={<Radio className="w-4 h-4" />} theme={theme}>绑定表</SectionTitle>
      <GlassCard theme={theme} className="p-3">
        {bindings.length === 0 ? (
          <InfoLine theme={theme}>暂无绑定。</InfoLine>
        ) : (
          <div className="space-y-2">
            {bindings.map((b, i) => (
              <div key={i} className="flex items-center gap-2 text-xs px-2 py-1.5 rounded-lg"
                style={{ background: theme.dark ? 'rgba(255,255,255,0.04)' : 'rgba(0,0,0,0.03)' }}>
                <span className="font-mono w-14">{b.cc}</span>
                <span className="flex-1 truncate">{bindingLabel(b)}</span>
                <span className={theme.textSecondary}>[{b.min.toFixed(1)}~{b.max.toFixed(1)}]</span>
                <span className={theme.textSecondary}>{b.smoothMs}ms</span>
                {b.invert && <span className={theme.textSecondary}>rev</span>}
                <button type="button" onClick={() => doUnlearn(b)} className={'p-1 rounded ' + (theme.dark ? 'hover:bg-white/10' : 'hover:bg-black/10')}>
                  <Trash2 className="w-3.5 h-3.5" />
                </button>
              </div>
            ))}
          </div>
        )}
      </GlassCard>

      <SectionTitle icon={<Zap className="w-4 h-4" />} theme={theme}>测试发送</SectionTitle>
      <GlassCard theme={theme} className="p-3 space-y-2">
        <div className="flex items-center gap-2">
          <span className={'text-xs ' + theme.textSecondary}>向 CC {cc} 发送：</span>
          {[0, 32, 64, 96, 127].map((v) => (
            <ActionButton key={v} onClick={() => sendTestCc(v)} theme={theme} ghost disabled={!midi} title={`value ${v}`}>
              {v}
            </ActionButton>
          ))}
        </div>
        {dropped > 0 && <InfoLine theme={theme}>队列溢出丢弃计数：{dropped}</InfoLine>}
      </GlassCard>
    </div>
  )
}
