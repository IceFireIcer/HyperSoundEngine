/**
 * HyperSoundEngine v1 调音室 UI —— 主面板（HyperSoundEngineMixingStudio）
 *
 * 布局与设计语言沿用调音室设计语言：
 * 玻璃拟态面板 + 顶部渐变高光 + 胶囊 Tab + 锚点弹出动画（CSS 版）。
 * 四个页签：音效场景 / 均衡器 / 调音器 / 分析。
 *
 * 融合接线（HyperSoundEngine 侧）：
 *   const bridge = createHyperSoundEngineUiBridge(engine, ctx.sampleRate)
 *   <HyperSoundEngineMixingStudio bridge={bridge} playerTheme={theme} onClose={...} ... />
 * 详见 docs/UI_GUIDE.md。
 */

import { useState } from 'react'
import { AudioLines, SlidersHorizontal, Sparkles, Activity, X } from 'lucide-react'
import type { HyperSoundEngineTheme } from './theme'
import { useHyperSoundEngineTheme } from './theme'
import type { HyperSoundEngineUiBridge } from './bridge'
import { useHyperSoundEngineParams } from './hooks'
import { EffectsPanel, type EffectUiKey } from './effectsPanel'
import { EqPanel } from './eqPanel'
import { SharePanel } from './sharePanel'
import { AnalysisPanel } from './analysisPanel'
import { SpatialModal } from './modalsSpatial'
import { DynamicsModal } from './modalsDynamics'
import { LoudnessModal } from './modalsLoudness'
import { ModulationModal } from './modalsModulation'

export interface HyperSoundEngineMixingStudioProps {
  bridge: HyperSoundEngineUiBridge
  onClose: () => void
  playerTheme: 'dark' | 'light'
  /** 打开按钮的锚点位置（弹窗从按钮侧弹出，CSS 动画实现） */
  anchorRect?: { x: number; y: number; width: number; height: number } | null
  /** 离线导出（融合侧实现）；缺省时按钮区显示占位提示 */
  exportWav?: (() => Promise<void>) | null
  exporting?: boolean
}

type Tab = 'effects' | 'eq' | 'tuner' | 'analyze'

const PANEL_KEYFRAMES = `
@keyframes hse-panel-backdrop { from { opacity: 0 } to { opacity: 1 } }
@keyframes hse-panel-in {
  from { opacity: 0; transform: translate(var(--fx, 0px), var(--fy, 0px)) scale(0.5); }
  to { opacity: 1; transform: translate(0, 0) scale(1); }
}
@keyframes hse-tab-fade { from { opacity: 0; transform: translateY(8px); } to { opacity: 1; transform: translateY(0); } }
`

export default function HyperSoundEngineMixingStudio({ bridge, onClose, playerTheme, anchorRect, exportWav = null, exporting = false }: HyperSoundEngineMixingStudioProps) {
  const theme = useHyperSoundEngineTheme(playerTheme)
  const controller = useHyperSoundEngineParams(bridge)
  const [activeTab, setActiveTab] = useState<Tab>('effects')
  const [effectModal, setEffectModal] = useState<EffectUiKey | null>(null)

  // 锚点弹出偏移（相对视口中心）
  const cx = typeof window !== 'undefined' ? window.innerWidth / 2 : 0
  const cy = typeof window !== 'undefined' ? window.innerHeight / 2 : 0
  const fx = anchorRect ? anchorRect.x - cx : 0
  const fy = anchorRect ? anchorRect.y - cy : 0

  const closeModal = () => setEffectModal(null)

  return (
    <>
      <style>{PANEL_KEYFRAMES}</style>
      {/* 遮罩 */}
      <div
        className="fixed inset-0 z-[70] flex items-center justify-center p-4 sm:p-8"
        style={{
          backgroundColor: theme.dark ? 'rgba(0,0,0,0.35)' : 'rgba(0,0,0,0.18)',
          backdropFilter: 'blur(6px) saturate(140%)',
          WebkitBackdropFilter: 'blur(6px) saturate(140%)',
          animation: 'hse-panel-backdrop 0.18s ease-out',
        }}
        onClick={onClose}
      >
        {/* 面板 */}
        <div
          onClick={(e) => e.stopPropagation()}
          className="w-full max-w-3xl max-h-[88vh] flex flex-col overflow-hidden rounded-3xl shadow-2xl"
          style={{
            background: theme.glassPanel,
            backdropFilter: theme.glassBlur,
            WebkitBackdropFilter: theme.glassBlur,
            border: `1px solid ${theme.glassBorder}`,
            boxShadow: '0 24px 64px rgba(0,0,0,0.35), inset 0 1px 0 rgba(255,255,255,0.2)',
            animation: 'hse-panel-in 0.26s cubic-bezier(0.2, 0.9, 0.3, 1.15)',
            ['--fx' as string]: `${fx}px`,
            ['--fy' as string]: `${fy}px`,
          }}
        >
          {/* 面板顶部渐变高光 */}
          <div className="pointer-events-none absolute inset-x-0 top-0 h-24" style={{ background: theme.glassPanelHighlight, borderRadius: '1.5rem 1.5rem 0 0' }} />

          {/* 头部 */}
          <div className="relative flex items-center justify-between px-5 py-4" style={{ borderBottom: `1px solid ${theme.glassBorder}` }}>
            <div className="flex items-center gap-2.5">
              <div className="w-9 h-9 rounded-xl flex items-center justify-center"
                style={{ backgroundColor: `${theme.accentColor}2e`, border: `1px solid ${theme.accentColor}55`, boxShadow: `0 4px 14px ${theme.accentColor}33` }}>
                <AudioLines className="w-4.5 h-4.5" style={{ color: theme.accentColor }} />
              </div>
              <div>
                <h2 className={`text-lg font-semibold ${theme.textPrimary}`}>调音室</h2>
                <div className={`${theme.textTertiary} text-[11px] -mt-0.5`}>场景方案 · 16 模块 DSP · 均衡器 · 分享串 · 分析</div>
              </div>
            </div>
            <div className="flex items-center gap-2">
              <button type="button" onClick={onClose} aria-label="关闭调音室"
                className={`p-2 rounded-full transition-colors ${theme.dark ? 'hover:bg-white/15' : 'hover:bg-black/10'}`}>
                <X className={`w-5 h-5 ${theme.textSecondary}`} />
              </button>
            </div>
          </div>

          {/* Tab 栏 */}
          <div className="relative flex px-3 pt-2 gap-1" style={{ borderBottom: `1px solid ${theme.glassBorder}` }}>
            {([
              { key: 'effects' as Tab, label: '音效场景', icon: Sparkles },
              { key: 'eq' as Tab, label: '均衡器', icon: SlidersHorizontal },
              { key: 'tuner' as Tab, label: '调音器', icon: AudioLines },
              { key: 'analyze' as Tab, label: '分析', icon: Activity },
            ]).map((tab) => {
              const active = activeTab === tab.key
              return (
                <button key={tab.key} type="button" onClick={() => setActiveTab(tab.key)}
                  className={`px-4 py-2.5 flex items-center justify-center gap-1.5 text-sm rounded-t-xl transition-all ${active ? theme.textPrimary + ' font-medium' : theme.textSecondary}`}
                  style={active ? {
                    background: theme.dark ? 'rgba(255,255,255,0.09)' : 'rgba(255,255,255,0.5)',
                    border: `1px solid ${theme.glassBorder}`,
                    borderBottom: 'none',
                    color: theme.accentColor,
                    boxShadow: 'inset 0 1px 0 rgba(255,255,255,0.15)',
                  } : undefined}>
                  <tab.icon className="w-4 h-4" />
                  {tab.label}
                  {active && <span className="w-1.5 h-1.5 rounded-full" style={{ backgroundColor: theme.accentColor }} />}
                </button>
              )
            })}
          </div>

          {/* 内容 */}
          <div className="relative p-4 sm:p-5 overflow-y-auto" style={{ height: 'calc(88vh - 140px)' }}>
            <div key={activeTab} style={{ animation: 'hse-tab-fade 0.2s ease-out' }}>
              {activeTab === 'effects' && (
                <EffectsPanel controller={controller} bridge={bridge} theme={theme} onOpenEffect={setEffectModal} />
              )}
              {activeTab === 'eq' && (
                <EqPanel controller={controller} theme={theme} />
              )}
              {activeTab === 'tuner' && (
                <SharePanel controller={controller} bridge={bridge} theme={theme} exportWav={exportWav} exporting={exporting} />
              )}
              {activeTab === 'analyze' && (
                <AnalysisPanel bridge={bridge} theme={theme} controller={controller} />
              )}
            </div>
          </div>
        </div>
      </div>

      {/* 效果配置弹窗（独立层级，避免冒泡关闭整个调音室） */}
      {effectModal && (() => {
        const key = effectModal
        if (key === 'reverb' || key === 'surround3d' || key === 'bassEnhancer') {
          return <SpatialModal effectKey={key} key={key} controller={controller} theme={theme} onClose={closeModal} />
        }
        if (key === 'loudnessCompensation' || key === 'loudnessNormalization') {
          return <LoudnessModal effectKey={key} key={key} controller={controller} theme={theme} onClose={closeModal} />
        }
        if (key === 'delay' || key === 'chorus' || key === 'flanger' || key === 'phaser' || key === 'tremolo' || key === 'modulation') {
          return <ModulationModal effectKey={key} key={key} controller={controller} theme={theme} onClose={closeModal} />
        }
        return <DynamicsModal effectKey={key} key={key} controller={controller} theme={theme} onClose={closeModal} />
      })()}
    </>
  )
}