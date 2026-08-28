/**
 * HyperSoundEngine v1 调音室 UI —— 设计语言（Design Tokens）
 *
 * 调音室统一设计语言：
 *  - liquid glass 玻璃拟态：低不透明度面板 + 强毛玻璃 + 顶部渐变高光 + 内高光描边；
 *  - 全局主题色 accent（默认 #8b5cf6，跟随 accentColorChanged 事件实时联动）；
 *  - 三级文本层级 textPrimary / textSecondary / textTertiary（暗/亮双主题）；
 *  - wf-glass-range 滑块（白色圆点 thumb + accent 光晕）与渐变填充轨道。
 *
 * 使用：`const ui = useHyperSoundEngineTheme(playerTheme)` 后取 `ui.glassPanel` 等变量。
 * 本文件无第三方依赖（纯常量 + React hook），供 ui/ 下所有组件复用。
 */

import { useEffect, useState } from 'react'

export interface HyperSoundEngineTheme {
  dark: boolean
  accentColor: string
  /** 错误提示色（分享串导入失败等行内错误） */
  errorColor: string
  glassPanel: string
  glassPanelHighlight: string
  glassCard: string
  glassBorder: string
  glassBlur: string
  glassCardBlur: string
  textPrimary: string
  textSecondary: string
  textTertiary: string
  inputBg: string
  /** 渐变填充的 range 轨道背景 */
  sliderTrack: (value: number, min: number, max: number) => string
}

/** 跟随全局主题色（accentColorChanged 事件 + localStorage，与全局面板一致） */
function useAccentColor(): string {
  const [accentColor, setAccentColor] = useState(() => {
    try {
      const saved = localStorage.getItem('accentColor')
      return saved || '#8b5cf6'
    } catch {
      return '#8b5cf6'
    }
  })
  useEffect(() => {
    const handleAccentChange = (e: Event) => {
      const customEvent = e as CustomEvent
      if (customEvent.detail) setAccentColor(customEvent.detail)
    }
    window.addEventListener('accentColorChanged', handleAccentChange)
    return () => window.removeEventListener('accentColorChanged', handleAccentChange)
  }, [])
  return accentColor
}

/** 构造 UI 设计语言变量（每次渲染调用一次即可） */
export function useHyperSoundEngineTheme(playerTheme: 'dark' | 'light'): HyperSoundEngineTheme {
  const dark = playerTheme === 'dark'
  const accentColor = useAccentColor()
  return {
    dark,
    accentColor,
    errorColor: '#f87171',
    glassPanel: dark ? 'rgba(10, 12, 20, 0.38)' : 'rgba(255, 255, 255, 0.45)',
    glassPanelHighlight: dark
      ? 'linear-gradient(160deg, rgba(255,255,255,0.10) 0%, rgba(255,255,255,0.03) 45%, rgba(255,255,255,0.06) 100%)'
      : 'linear-gradient(160deg, rgba(255,255,255,0.85) 0%, rgba(255,255,255,0.35) 45%, rgba(255,255,255,0.55) 100%)',
    glassCard: dark
      ? 'linear-gradient(150deg, rgba(255,255,255,0.07) 0%, rgba(255,255,255,0.025) 100%)'
      : 'linear-gradient(150deg, rgba(255,255,255,0.55) 0%, rgba(255,255,255,0.30) 100%)',
    glassBorder: dark ? 'rgba(255,255,255,0.12)' : 'rgba(255,255,255,0.55)',
    glassBlur: 'blur(30px) saturate(185%)',
    glassCardBlur: 'blur(18px) saturate(160%)',
    textPrimary: dark ? 'text-white' : 'text-black',
    textSecondary: dark ? 'text-white/65' : 'text-black/65',
    textTertiary: dark ? 'text-white/40' : 'text-black/45',
    inputBg: dark ? 'rgba(255,255,255,0.06)' : 'rgba(255,255,255,0.5)',
    sliderTrack: (value: number, min: number, max: number) => {
      const ratio = Math.min(1, Math.max(0, (value - min) / (max - min)))
      const rest = dark ? 'rgba(255,255,255,0.16)' : 'rgba(0,0,0,0.14)'
      return `linear-gradient(to right, ${accentColor} 0%, ${accentColor} ${ratio * 100}%, ${rest} ${ratio * 100}%, ${rest} 100%)`
    },
  }
}
