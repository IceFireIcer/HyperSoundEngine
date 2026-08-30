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

console.log(`wasm pilot static smoke passed (${wasm.length} wasm bytes; browser ESM bundles parsed)`)
