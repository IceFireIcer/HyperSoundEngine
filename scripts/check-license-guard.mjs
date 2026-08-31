import { readFileSync } from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const packageJson = JSON.parse(readFileSync(path.join(root, 'package.json'), 'utf8'))
const cargoToml = readFileSync(path.join(root, 'HyperSoundEngineRust', 'crates', 'hrtf-core', 'Cargo.toml'), 'utf8')
const cargoLock = readFileSync(path.join(root, 'HyperSoundEngineRust', 'Cargo.lock'), 'utf8')
const notices = readFileSync(path.join(root, 'THIRD_PARTY_NOTICES.md'), 'utf8')

if (!packageJson.devDependencies?.['playwright-core']) throw new Error('playwright-core dependency missing')
if (!cargoToml.includes('rustfft')) throw new Error('rustfft dependency missing')
if (!cargoToml.includes('sofar')) throw new Error('sofar dependency missing')
for (const name of ['playwright-core', 'rustfft', 'sofar']) {
  if (!notices.includes(`\`${name}\``)) throw new Error(`THIRD_PARTY_NOTICES missing ${name}`)
}

const forbidden = ['sofa-reader', 'libsoxr', 'rubber-band', 'essentia']
for (const name of forbidden) {
  if (cargoLock.includes(`name = "${name}"`)) throw new Error(`forbidden dependency in Cargo.lock: ${name}`)
}
console.log('direct dependency license guard passed (Apache/MIT set; no known GPL/AGPL additions)')
