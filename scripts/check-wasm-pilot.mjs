import { existsSync, readFileSync } from 'node:fs'
import { builtinModules } from 'node:module'
import path from 'node:path'
import process from 'node:process'
import { fileURLToPath } from 'node:url'
import { build } from 'esbuild'

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const pilotDir = path.join(root, 'HyperSoundEngineRust', 'web', 'wasm-pilot')
const pkgFlag = process.argv.indexOf('--pkg')
const pkgArgument = pkgFlag >= 0 ? process.argv[pkgFlag + 1] : undefined
if (pkgFlag >= 0 && !pkgArgument) throw new Error('--pkg requires a directory')

const pkgDir = path.resolve(pkgArgument ?? path.join(pilotDir, 'pkg'))
const gluePath = path.join(pkgDir, 'hse_wasm.js')
const wasmPath = path.join(pkgDir, 'hse_wasm_bg.wasm')
const browserEntries = [path.join(pilotDir, 'host.js'), path.join(pilotDir, 'worklet.js')]

for (const file of [gluePath, wasmPath, ...browserEntries]) {
  if (!existsSync(file)) throw new Error(`wasm pilot artifact is missing: ${file}`)
}

const wasm = readFileSync(wasmPath)
if (wasm.length < 8 || wasm.subarray(0, 4).toString('hex') !== '0061736d') {
  throw new Error(`invalid WebAssembly binary: ${wasmPath}`)
}

const requiredSpatialExports = [
  'spatial_abi_version',
  'spatial_load_hrtf',
  'spatial_hrir_length',
  'spatial_get_hrir',
  'spatial_render_objects',
  'spatial_set_room',
  'spatial_set_room_preset',
  'spatial_set_hrtf_interp_mode',
  'spatial_set_convolution_mode',
  'spatial_set_distance_model',
  'spatial_destroy',
  'spatial_last_error_code',
  'spatial_last_error_copy',
]
const wasmModule = new WebAssembly.Module(wasm)
const wasmExports = new Set(WebAssembly.Module.exports(wasmModule).map(({ name }) => name))
for (const name of requiredSpatialExports) {
  if (!wasmExports.has(name)) {
    throw new Error(`wasm binary is missing required spatial ABI export: ${name}`)
  }
}

const glue = readFileSync(gluePath, 'utf8')
const requiredEngineBindings = [
  /export class HseBiquad\b/,
  /export class HseEngine\b/,
  /\bcapacity\(\)/,
  /\bleft_ptr\(\)/,
  /\bright_ptr\(\)/,
  /\bsidechain_left_ptr\(\)/,
  /\bsidechain_right_ptr\(\)/,
  /\bwithSofaBytes\b/,
  /\bwithHrtfGrid\b/,
  /\bprocess\(frames\)/,
  /\breset\(\)/,
]
for (const binding of requiredEngineBindings) {
  if (!binding.test(glue)) {
    throw new Error(`wasm pilot generated glue is missing required binding: ${binding}`)
  }
}

const builtinNames = new Set(builtinModules.flatMap((name) => [name, `node:${name}`]))
const builtinImports = new Set()
const aliasGlue = {
  name: 'wasm-pilot-generated-glue',
  setup(buildApi) {
    buildApi.onResolve({ filter: /^\.\/pkg\/hse_wasm\.js$/ }, () => ({ path: gluePath }))
  },
}

for (const entryPoint of [gluePath, ...browserEntries]) {
  const result = await build({
    entryPoints: [entryPoint],
    bundle: true,
    format: 'esm',
    platform: 'browser',
    target: ['es2022'],
    write: false,
    metafile: true,
    logLevel: 'silent',
    plugins: [aliasGlue],
  })
  for (const input of Object.values(result.metafile.inputs)) {
    for (const imported of input.imports) {
      if (imported.path.startsWith('node:') || builtinNames.has(imported.path)) {
        builtinImports.add(imported.path)
      }
    }
  }
}

if (builtinImports.size > 0) {
  throw new Error(`browser bundle imports Node builtins: ${[...builtinImports].sort().join(', ')}`)
}

console.log(`wasm engine pilot static smoke passed (${wasm.length} wasm bytes; engine bindings, spatial ABI exports, and browser ESM bundles verified)`)
