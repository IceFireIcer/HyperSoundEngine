/**
 * HyperSoundEngine v1 独立音频引擎 —— 浏览器宿主入口
 *
 * 本入口依赖浏览器 AudioContext / AudioWorklet / ScriptProcessor 形态（鸭子类型），
 * 但不依赖任何特定宿主、不依赖 React。适合直接接入任意 Web 应用：
 *
 *   import { createHyperSoundEngineHost } from 'hypersoundengine/browser'
 *   const host = createHyperSoundEngineHost({ mode: 'auto', workletUrl: '/hse-worklet.js' })
 *   await host.attach({ audioContext, masterGain, analyser }, params)
 */

export { HyperSoundEngineHost, createHyperSoundEngineHost } from './integration/HyperSoundEngineHost'
export type {
  HyperSoundEngineHostHandle,
  HyperSoundEngineHostMode,
  HyperSoundEngineBackend,
  HyperSoundEngineHostOptions,
  HyperSoundEngineAudioContextLike,
  HyperSoundEngineAudioNodeLike,
} from './integration/HyperSoundEngineHost'
