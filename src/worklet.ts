/**
 * HyperSoundEngine v1 独立音频引擎 —— AudioWorklet 处理器打包入口
 *
 * 警告：本文件**不能**在普通 Node / 主线程直接 import（AudioWorkletProcessor
 * 仅存在于 AudioWorkletGlobalScope）。它只应作为 esbuild / vite 的打包入口，
 * 产出单文件 IIFE 后通过 `audioWorklet.addModule(url)` 加载。
 *
 * 打包示例：
 *   npx esbuild src/worklet.ts --bundle --format=iife --outfile=dist/worklet-bundle.js
 */

export {
  HseAudioEffectsProcessor,
  WORKLET_PROCESSOR_NAME,
} from './worklet/HseAudioEffectsProcessor'
