import { spawnSync } from 'node:child_process'
import { existsSync } from 'node:fs'
import path from 'node:path'
import process from 'node:process'
import { fileURLToPath } from 'node:url'

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const header = path.join(root, 'HyperSoundEngineRust', 'crates', 'hse-wasm', 'include', 'hypersoundengine_spatial.h')
if (!existsSync(header)) throw new Error(`missing spatial ABI header: ${header}`)

const source = `#include "${header.replaceAll('\\', '/')}"
_Static_assert(HSE_SPATIAL_ABI_VERSION == 1u, "unexpected spatial ABI version");
int main(void) {
  uint32_t (*version_fn)(void) = spatial_abi_version;
  int32_t (*destroy_fn)(uint32_t) = spatial_destroy;
  return version_fn == 0 || destroy_fn == 0;
}
`

const candidates = process.platform === 'win32' ? ['cc', 'clang', 'gcc'] : ['cc', 'clang', 'gcc']
let compiler
for (const candidate of candidates) {
  const probe = spawnSync(candidate, ['--version'], { stdio: 'ignore' })
  if (!probe.error && probe.status === 0) {
    compiler = candidate
    break
  }
}
if (!compiler) throw new Error('no C compiler found for spatial ABI header smoke')

const result = spawnSync(compiler, ['-std=c11', '-fsyntax-only', '-x', 'c', '-'], {
  input: source,
  encoding: 'utf8',
})
if (result.status !== 0) {
  throw new Error(`spatial ABI header smoke failed with ${compiler}:\n${result.stderr || result.stdout}`)
}
console.log(`spatial ABI header smoke passed (${compiler}, ABI v1)`)
