/**
 * HyperSoundEngine v1 调音室 UI —— 调音器页（分享串 / WAV 导出 / 引擎信息）
 *
 * 分享串：完整参数快照（HSE2 紧凑格式：Crockford Base32 分组 + 仅存与默认参数的差异项；
 * FNV-1a 校验 + 白名单解码），兼容导入旧版全量串。WAV 导出与引擎信息（latency/采样率）
 * 供融合侧接线。
 */

import { useEffect, useState } from 'react'
import { Copy, ClipboardPaste, FileAudio, Cpu, Info, RefreshCw, Check, AlertCircle } from 'lucide-react'
import type { HyperSoundEngineTheme } from './theme'
import type { HyperSoundEngineUiBridge } from './bridge'
import { ActionButton, GlassCard, InfoLine, SectionTitle } from './primitives'
import type { HyperSoundEngineParamsController } from './hooks'

const CODE_FONT = 'ui-monospace, SFMono-Regular, Menlo, Consolas, monospace'

export interface SharePanelProps {
  controller: HyperSoundEngineParamsController
  bridge: HyperSoundEngineUiBridge
  theme: HyperSoundEngineTheme
  /** 离线导出（融合侧实现：解码 → HyperSoundEngine.process → WAV） */
  exportWav?: (() => Promise<void>) | null
  /** 导出进行中状态由父级管理 */
  exporting?: boolean
}

export function SharePanel({ controller, bridge, theme, exportWav, exporting }: SharePanelProps) {
  const { params, replace } = controller
  const [shareText, setShareText] = useState('')
  const [importText, setImportText] = useState('')
  const [importError, setImportError] = useState('')
  const [copied, setCopied] = useState(false)

  // 参数变化即刷新分享串（v2 紧凑格式生成成本极低，免去手动点生成的遗忘成本）
  useEffect(() => {
    try {
      setShareText(bridge.encodeShare(params))
    } catch {
      /* 生成失败保持上一份串（toast 由显式动作负责，避免渲染期噪声） */
    }
  }, [bridge, params])

  const handleRegenerate = () => {
    try {
      setShareText(bridge.encodeShare(params))
    } catch (e) {
      window.dispatchEvent(new CustomEvent('showToast', { detail: { message: '生成分享串失败：' + (e instanceof Error ? e.message : '未知错误'), type: 'error' } }))
    }
  }

  const handleCopyShare = async () => {
    try {
      await navigator.clipboard.writeText(shareText)
      setCopied(true)
      window.dispatchEvent(new CustomEvent('showToast', { detail: { message: '分享串已复制到剪贴板', type: 'info' } }))
      setTimeout(() => setCopied(false), 1500)
    } catch {
      window.dispatchEvent(new CustomEvent('showToast', { detail: { message: '复制失败：请手动选中分享串复制', type: 'error' } }))
    }
  }

  const handleImportShare = () => {
    const text = importText.trim()
    if (!text) return
    try {
      const decoded = bridge.decodeShare(text)
      replace(decoded)
      setImportText('')
      setImportError('')
      window.dispatchEvent(new CustomEvent('showToast', { detail: { message: '分享串已导入并应用', type: 'info' } }))
    } catch (e) {
      const msg = e instanceof Error ? e.message : '分享串无效'
      setImportError(msg)
      window.dispatchEvent(new CustomEvent('showToast', { detail: { message: '导入失败：' + msg, type: 'error' } }))
    }
  }

  const stats = bridge.getStats()

  return (
    <div className="space-y-3">
      {/* 分享串 */}
      <GlassCard theme={theme}>
        <SectionTitle icon={<Copy className="w-4 h-4" style={{ color: theme.accentColor }} />} theme={theme}
          hint="HSE2 紧凑格式：只存与默认不同的项 + 校验和，跨设备导入安全">
          分享串
        </SectionTitle>
        <div className="flex gap-2 mb-2">
          <ActionButton onClick={handleRegenerate} theme={theme}>
            <RefreshCw className="w-3.5 h-3.5" /> 生成分享串
          </ActionButton>
          {shareText && (
            <ActionButton onClick={() => void handleCopyShare()} theme={theme} ghost>
              {copied ? <Check className="w-3.5 h-3.5" /> : <Copy className="w-3.5 h-3.5" />} {copied ? '已复制' : '复制'}
            </ActionButton>
          )}
        </div>
        {shareText && (
          <>
            <textarea readOnly value={shareText} onFocus={(e) => e.currentTarget.select()}
              className={`w-full h-24 overflow-auto px-3 py-2 rounded-lg text-xs outline-none mb-2 break-all ${theme.textPrimary}`}
              style={{ background: theme.inputBg, border: `1px solid ${theme.glassBorder}`, fontFamily: CODE_FONT, lineHeight: 1.6 }} />
            <div className={`flex justify-between text-[10px] mb-3 ${theme.textTertiary}`}>
              <span>{shareText.length} 字符 · 仅存与默认不同的项</span>
              <span>含校验和，篡改/截断会被拒绝</span>
            </div>
          </>
        )}
        <div className="flex gap-2">
          <textarea value={importText} onChange={(e) => { setImportText(e.target.value); setImportError('') }}
            placeholder="粘贴分享串（新格式 HSE2-… 或旧版串均可）"
            className={`flex-1 h-16 px-3 py-2 rounded-lg text-xs outline-none ${theme.textPrimary}`}
            style={{ background: theme.inputBg, border: `1px solid ${importError ? theme.errorColor ?? '#f87171' : theme.glassBorder}`, fontFamily: CODE_FONT }} />
          <ActionButton onClick={handleImportShare} theme={theme}>
            <ClipboardPaste className="w-4 h-4" /> 导入
          </ActionButton>
        </div>
        {importError && (
          <div className="flex items-center gap-1 mt-1 text-xs" style={{ color: theme.errorColor ?? '#f87171' }}>
            <AlertCircle className="w-3 h-3 shrink-0" /> {importError}
          </div>
        )}
        <InfoLine theme={theme}>
          <Info className="w-3 h-3 shrink-0" /> 新旧格式互相兼容：旧版分享串仍可导入；卷积 IR 以名称引用，不随串传输。
        </InfoLine>
      </GlassCard>

      {/* WAV 导出 */}
      <GlassCard theme={theme}>
        <SectionTitle icon={<FileAudio className="w-4 h-4" style={{ color: theme.accentColor }} />} theme={theme}>
          导出处理后的音乐
        </SectionTitle>
        <div className={`${theme.textSecondary} text-xs mb-3`}>把当前参数离线渲染成 WAV 文件下载（个人处理用途，涉及版权曲目请勿分发）；离线与实时共用同一内核，逐样本一致。</div>
        {exportWav ? (
          <button type="button" onClick={() => void exportWav()} disabled={exporting}
            className="w-full py-2.5 rounded-lg text-sm font-medium text-white disabled:opacity-40 transition-all hover:brightness-110 active:scale-[0.98] flex items-center justify-center gap-2"
            style={{ backgroundColor: theme.accentColor, boxShadow: `0 6px 18px ${theme.accentColor}44` }}>
            <FileAudio className="w-4 h-4" />
            {exporting ? '导出中…' : '导出 WAV'}
          </button>
        ) : (
          <div className={`${theme.textTertiary} text-xs`}>融合侧接入离线导出后显示此按钮（见 UI_GUIDE）。</div>
        )}
      </GlassCard>

      {/* 引擎信息 */}
      <GlassCard theme={theme}>
        <SectionTitle icon={<Cpu className="w-4 h-4" style={{ color: theme.accentColor }} />} theme={theme}>引擎信息</SectionTitle>
        <div className="grid grid-cols-2 gap-2 text-xs">
          <div className="flex items-center justify-between px-3 py-2 rounded-lg" style={{ background: theme.inputBg, border: `1px solid ${theme.glassBorder}` }}>
            <span className={`${theme.textSecondary}`}>采样率</span>
            <span className={`${theme.textPrimary} font-medium`}>{bridge.getSampleRate()} Hz</span>
          </div>
          <div className="flex items-center justify-between px-3 py-2 rounded-lg" style={{ background: theme.inputBg, border: `1px solid ${theme.glassBorder}` }}>
            <span className={`${theme.textSecondary}`}>引擎延迟</span>
            <span className={`${theme.textPrimary} font-medium`}>{(bridge.getLatencySamples() / bridge.getSampleRate() * 1000).toFixed(1)} ms</span>
          </div>
          <div className="flex items-center justify-between px-3 py-2 rounded-lg" style={{ background: theme.inputBg, border: `1px solid ${theme.glassBorder}` }}>
            <span className={`${theme.textSecondary}`}>整合响度</span>
            <span className={`${theme.textPrimary} font-medium`}>{Number.isFinite(stats.lufsIntegrated) ? stats.lufsIntegrated.toFixed(1) : '—'} LUFS</span>
          </div>
          <div className="flex items-center justify-between px-3 py-2 rounded-lg" style={{ background: theme.inputBg, border: `1px solid ${theme.glassBorder}` }}>
            <span className={`${theme.textSecondary}`}>限幅衰减</span>
            <span className={`${theme.textPrimary} font-medium`}>{stats.limiterReductionDb.toFixed(1)} dB</span>
          </div>
        </div>
        <InfoLine theme={theme}><Info className="w-3 h-3 shrink-0" /> 响度/限幅读数实时更新，详细分析见「分析」页。</InfoLine>
      </GlassCard>
    </div>
  )
}
