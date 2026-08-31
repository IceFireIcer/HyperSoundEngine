/**
 * 浏览器实时接入示例（伪代码 / 可直接在 Vite 等打包器中运行）
 *
 * 运行前先构建：
 *   npm run build
 *
 * 注意：本文件包含 DOM API，只能在浏览器环境运行。
 */

import { createHyperSoundEngineHost } from '../dist/browser.js'
import { createDefaultParams } from '../dist/index.js'

/**
 * @param {AudioContext} audioContext
 * @param {GainNode} masterGain
 * @param {AnalyserNode} analyser
 */
export async function startEngine(audioContext, masterGain, analyser) {
  const host = createHyperSoundEngineHost({
    mode: 'auto',
    workletUrl: '/hse-worklet-bundle.js', // 未打包时会自动回退 ScriptProcessor
  })

  const params = createDefaultParams(audioContext.sampleRate)
  await host.attach({ audioContext, masterGain, analyser }, params)

  return {
    host,
    async update(nextParams) {
      await host.setParams(nextParams)
    },
    stop() {
      host.dispose()
    },
  }
}
