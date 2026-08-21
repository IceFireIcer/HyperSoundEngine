/**
 * 构建 AudioWorklet 单文件包（IIFE）。
 *
 * 产物：dist/worklet-bundle.js
 * 用途：`audioWorklet.addModule('/path/to/worklet-bundle.js')`
 *
 * 说明：
 * - AudioWorkletGlobalScope 不支持 ESM import，因此必须打包为 IIFE；
 * - 入口 src/worklet.ts 只导出处理器类与注册名，打包器会把 HyperSoundEngine 与全部 DSP 内联。
 */
import { build } from 'esbuild'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')

await build({
  entryPoints: [path.join(root, 'src', 'worklet.ts')],
  bundle: true,
  format: 'iife',
  platform: 'browser',
  target: ['chrome110', 'firefox110', 'safari16'],
  outfile: path.join(root, 'dist', 'worklet-bundle.js'),
  legalComments: 'external',
  logLevel: 'info',
})

console.log('worklet bundle -> dist/worklet-bundle.js')
