/**
 * HyperSoundEngine v1 调音室 UI —— 公共出口
 *
 * 导出：主面板 HyperSoundEngineMixingStudio、引擎桥（createHyperSoundEngineUiBridge + 类型）、
 * 设计语言（useHyperSoundEngineTheme）、参数 hooks（useHyperSoundEngineParams/DeepPartial/deepMerge）。
 * 融合侧只 import 本文件即可。
 */

export { default as HyperSoundEngineMixingStudio } from './HyperSoundEngineMixingStudio'
export type { HyperSoundEngineMixingStudioProps } from './HyperSoundEngineMixingStudio'
export { createHyperSoundEngineUiBridge, MAX_MY_SCENES } from './bridge'
export type { HyperSoundEngineUiBridge, HyperSoundEngineHearingSession } from './bridge'
export { useHyperSoundEngineTheme } from './theme'
export type { HyperSoundEngineTheme } from './theme'
export { useHyperSoundEngineParams, deepMerge } from './hooks'
export type { DeepPartial, HyperSoundEngineParamsController } from './hooks'
export { autoBoostAtVolume, COMP_PRESETS, CUSTOM_BAND_FREQUENCIES } from './modalsLoudness'
export { REVERB_TYPES, HARMONIC_TYPES } from './modalsSpatial'
export { IEQ_CURVES } from './modalsDynamics'
export { EqCurveEditor } from './eqCurveEditor'
export type { EqPoint } from './eqCurveEditor'
