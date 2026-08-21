/**
 * 构建核心与浏览器入口的 ESM 单文件包。
 *
 * 产物：
 *   dist/index.js   核心入口（纯 DSP，无浏览器依赖）
 *   dist/browser.js 浏览器宿主入口
 *   dist/worklet.js AudioWorklet 打包入口（ESM 形态，供上层 bundler 使用）
 *
 * 类型声明由 `tsc -p tsconfig.build.json --emitDeclarationOnly` 生成。
 */
import { build } from 'esbuild'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')

const common = {
  bundle: true,
  format: 'esm',
  platform: 'neutral',
  target: ['es2022'],
  // 可选增强依赖保持 external，由使用方按需安装；核心引擎不强制捆绑它们。
  external: ['meyda', 'signalsmith-stretch'],
  legalComments: 'external',
  logLevel: 'info',
}

await build({
  ...common,
  entryPoints: [path.join(root, 'src', 'index.ts')],
  outfile: path.join(root, 'dist', 'index.js'),
})

await build({
  ...common,
  entryPoints: [path.join(root, 'src', 'browser.ts')],
  outfile: path.join(root, 'dist', 'browser.js'),
})

await build({
  ...common,
  entryPoints: [path.join(root, 'src', 'worklet.ts')],
  outfile: path.join(root, 'dist', 'worklet.js'),
})

console.log('core bundles -> dist/index.js, dist/browser.js, dist/worklet.js')
