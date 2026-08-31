import { existsSync, readFileSync } from 'node:fs'
import path from 'node:path'
import process from 'node:process'
import { fileURLToPath } from 'node:url'
import { build } from 'esbuild'

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const pkgFlag = process.argv.indexOf('--pkg')
const pkgArgument = pkgFlag >= 0 ? process.argv[pkgFlag + 1] : undefined
if (!pkgArgument) throw new Error('build:wasm-worklet requires --pkg <wasm-bindgen web output directory>')

const pkgDir = path.resolve(pkgArgument)
const gluePath = path.join(pkgDir, 'hse_wasm.js')
const wasmPath = path.join(pkgDir, 'hse_wasm_bg.wasm')
const textCodecPolyfill = readFileSync(
  path.join(root, 'src', 'worklet', 'text-codec-polyfill.js'),
  'utf8',
)
for (const file of [gluePath, wasmPath]) {
  if (!existsSync(file)) throw new Error(`wasm worklet artifact is missing: ${file}`)
}
const wasm = readFileSync(wasmPath)
if (wasm.length < 8 || wasm.subarray(0, 4).toString('hex') !== '0061736d') {
  throw new Error(`invalid WebAssembly binary: ${wasmPath}`)
}
const glue = readFileSync(gluePath, 'utf8')
for (const binding of [
  /export class HseEngine\b/,
  /\bwithSofaBytes\(/,
  /\bwithHrtfGrid\(/,
  /\bcapacity\(\)/,
  /\bleft_ptr\(\)/,
  /\bright_ptr\(\)/,
  /\bsidechain_left_ptr\(\)/,
  /\bsidechain_right_ptr\(\)/,
  /\bprocess\(frames\)/,
  /\breset\(\)/,
]) {
  if (!binding.test(glue)) throw new Error(`wasm glue is missing required HseEngine binding: ${binding}`)
}

await build({
  entryPoints: [path.join(root, 'src', 'worklet', 'HseWasmAudioEffectsProcessor.js')],
  bundle: true,
  format: 'iife',
  platform: 'browser',
  target: ['chrome110', 'firefox110', 'safari16'],
  define: { 'import.meta.url': '"about:blank"' },
  banner: { js: textCodecPolyfill },
  outfile: path.join(root, 'dist', 'wasm-worklet-bundle.js'),
  legalComments: 'external',
  logLevel: 'info',
  plugins: [{
    name: 'hse-wasm-bindgen-glue',
    setup(buildApi) {
      buildApi.onResolve({ filter: /^\.\/hse_wasm\.js$/ }, () => ({ path: gluePath }))
    },
  }],
})

console.log(`wasm worklet bundle -> dist/wasm-worklet-bundle.js (${wasm.length} wasm bytes checked)`)
