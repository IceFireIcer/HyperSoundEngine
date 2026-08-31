import { createServer } from 'node:http'
import { access, readFile } from 'node:fs/promises'
import path from 'node:path'
import process from 'node:process'
import { fileURLToPath } from 'node:url'
import { chromium } from 'playwright-core'

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const wasmFlag = process.argv.indexOf('--wasm')
if (wasmFlag >= 0 && !process.argv[wasmFlag + 1]) throw new Error('--wasm requires a file path')
const wasmPath = path.resolve(
  wasmFlag >= 0 && process.argv[wasmFlag + 1]
    ? process.argv[wasmFlag + 1]
    : path.join(root, 'dist', 'hse_wasm_bg.wasm'),
)

const requiredFiles = [
  path.join(root, 'dist', 'browser.js'),
  path.join(root, 'dist', 'index.js'),
  path.join(root, 'dist', 'wasm-worklet-bundle.js'),
  path.join(root, 'specs', 'engine', 'vectors', 'default-params.48000.json'),
  wasmPath,
]
await Promise.all(requiredFiles.map((file) => access(file)))

const sinkWorklet = `
class HseE2eSink extends AudioWorkletProcessor {
  constructor() {
    super()
    this.pendingBarrier = null
    this.port.onmessage = ({ data }) => {
      if (data?.type === 'barrier') this.pendingBarrier = data.id
    }
    this.port.postMessage({
      capabilities: {
        TextEncoder: typeof TextEncoder,
        TextDecoder: typeof TextDecoder,
        FinalizationRegistry: typeof FinalizationRegistry,
        WebAssembly: typeof WebAssembly,
        Symbol: typeof Symbol,
      },
    })
  }

  process(inputs, outputs) {
    const input = inputs[0]
    const output = outputs[0]
    const left = input?.[0]
    const right = input?.[1] ?? left
    if (left) {
      this.port.postMessage({ type: 'audio', left: Array.from(left), right: Array.from(right) })
    }
    if (this.pendingBarrier !== null) {
      this.port.postMessage({ type: 'barrier', id: this.pendingBarrier })
      this.pendingBarrier = null
    }
    for (let channel = 0; channel < output.length; channel++) {
      const source = input?.[channel] ?? input?.[0]
      if (source) output[channel].set(source)
      else output[channel].fill(0)
    }
    return true
  }
}
registerProcessor('hse-e2e-sink', HseE2eSink)
`

const contentTypes = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.json': 'application/json; charset=utf-8',
  '.wasm': 'application/wasm',
}

const server = createServer(async (request, response) => {
  try {
    const pathname = new URL(request.url ?? '/', 'http://127.0.0.1').pathname
    if (pathname === '/') {
      response.writeHead(200, { 'content-type': contentTypes['.html'] })
      response.end('<!doctype html><meta charset="utf-8"><title>HSE wasm worklet E2E</title>')
      return
    }
    if (pathname === '/favicon.ico') {
      response.writeHead(204).end()
      return
    }
    if (pathname === '/e2e-sink.js') {
      response.writeHead(200, { 'content-type': contentTypes['.js'] })
      response.end(sinkWorklet)
      return
    }
    const routes = new Map([
      ['/browser.js', path.join(root, 'dist', 'browser.js')],
      ['/index.js', path.join(root, 'dist', 'index.js')],
      ['/wasm-worklet-bundle.js', path.join(root, 'dist', 'wasm-worklet-bundle.js')],
      ['/hse_wasm_bg.wasm', wasmPath],
      ['/default-params.json', path.join(root, 'specs', 'engine', 'vectors', 'default-params.48000.json')],
    ])
    const file = routes.get(pathname)
    if (!file) {
      response.writeHead(404).end('not found')
      return
    }
    response.writeHead(200, {
      'content-type': contentTypes[path.extname(file)] ?? 'application/octet-stream',
      'cache-control': 'no-store',
    })
    response.end(await readFile(file))
  } catch (error) {
    response.writeHead(500).end(error instanceof Error ? error.message : String(error))
  }
})

await new Promise((resolve, reject) => {
  server.once('error', reject)
  server.listen(0, '127.0.0.1', resolve)
})
const address = server.address()
if (!address || typeof address === 'string') throw new Error('E2E server did not expose a TCP port')
const baseUrl = `http://127.0.0.1:${address.port}`

const candidates = [
  process.env.HSE_CHROMIUM_EXECUTABLE,
  process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH,
  process.platform === 'win32' ? 'C:/Program Files/Google/Chrome/Application/chrome.exe' : undefined,
  process.platform === 'win32' ? 'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe' : undefined,
  '/usr/bin/google-chrome',
  '/usr/bin/google-chrome-stable',
  '/usr/bin/chromium',
  '/usr/bin/chromium-browser',
].filter(Boolean)

let executablePath
for (const candidate of candidates) {
  try {
    await access(candidate)
    executablePath = candidate
    break
  } catch {
    // Try the next known browser location.
  }
}
if (!executablePath) {
  await new Promise((resolve) => server.close(resolve))
  throw new Error('Chromium executable not found; set HSE_CHROMIUM_EXECUTABLE')
}

let browser
try {
  browser = await chromium.launch({
    executablePath,
    headless: true,
    args: ['--autoplay-policy=no-user-gesture-required'],
  })
  const page = await browser.newPage()
  const consoleErrors = []
  page.on('console', (message) => {
    if (message.type() === 'error') consoleErrors.push(message.text())
  })
  page.on('pageerror', (error) => consoleErrors.push(error.message))
  await page.goto(baseUrl)

  const results = await page.evaluate(async ({ baseUrl }) => {
    const timeout = (promise, label, ms = 5000) => Promise.race([
      promise,
      new Promise((_, reject) => setTimeout(() => reject(new Error(`${label} timed out after ${ms}ms`)), ms)),
    ])

    const makeContext = async () => {
      const context = new AudioContext({ sampleRate: 48000, latencyHint: 'interactive' })
      if (!context.audioWorklet) throw new Error('AudioWorklet is unavailable in this Chromium build')
      await Promise.all([
        context.audioWorklet.addModule(`${baseUrl}/wasm-worklet-bundle.js`),
        context.audioWorklet.addModule(`${baseUrl}/e2e-sink.js`),
      ])
      const probe = new AudioWorkletNode(context, 'hse-e2e-sink')
      const capabilities = await timeout(new Promise((resolve) => {
        probe.port.onmessage = ({ data }) => resolve(data.capabilities)
      }), 'AudioWorklet capability probe')
      probe.disconnect()
      if (capabilities.TextEncoder !== 'function' || capabilities.TextDecoder !== 'function') {
        throw new Error(`AudioWorklet text codecs are unavailable: ${JSON.stringify(capabilities)}`)
      }
      await context.resume()
      return context
    }

    const compileWasm = async () => {
      const response = await fetch(`${baseUrl}/hse_wasm_bg.wasm`)
      if (!response.ok) throw new Error(`wasm fetch failed: ${response.status}`)
      return WebAssembly.compile(await response.arrayBuffer())
    }

    const makeSource = (context, leftValue = 0.3, rightValue = -0.1) => {
      const buffer = context.createBuffer(2, 128, context.sampleRate)
      buffer.getChannelData(0).fill(leftValue)
      buffer.getChannelData(1).fill(rightValue)
      const source = context.createBufferSource()
      source.buffer = buffer
      source.loop = true
      return source
    }

    const attachCollector = (context, upstream) => {
      const sink = new AudioWorkletNode(context, 'hse-e2e-sink', {
        numberOfInputs: 1,
        numberOfOutputs: 1,
        outputChannelCount: [2],
      })
      const destination = context.createMediaStreamDestination()
      const blocks = []
      let waiter = null
      sink.port.onmessage = ({ data }) => {
        if (data?.type === 'barrier' && waiter?.barrierId === data.id) {
          const resolve = waiter.resolve
          waiter = null
          resolve(blocks.slice())
          return
        }
        if (data?.type !== 'audio') return
        blocks.push(data)
        if (waiter && waiter.count !== undefined && blocks.length >= waiter.count) {
          const resolve = waiter.resolve
          waiter = null
          resolve(blocks.slice())
        }
      }
      upstream.connect(sink)
      sink.connect(destination)
      return {
        blocks,
        clear() { blocks.length = 0 },
        waitFor(count) {
          if (blocks.length >= count) return Promise.resolve(blocks.slice())
          return timeout(new Promise((resolve) => { waiter = { count, resolve } }), `collect ${count} audio blocks`)
        },
        async barrier(id) {
          await timeout(new Promise((resolve) => {
            waiter = { barrierId: id, resolve }
            sink.port.postMessage({ type: 'barrier', id })
          }), `audio barrier ${id}`)
        },
      }
    }

    const stats = (blocks) => {
      let energy = 0
      let count = 0
      let maxChannelDelta = 0
      let maxAbs = 0
      const blockMeans = []
      for (const block of blocks) {
        let leftMean = 0
        let rightMean = 0
        for (let i = 0; i < block.left.length; i++) {
          const left = block.left[i]
          const right = block.right[i]
          if (!Number.isFinite(left) || !Number.isFinite(right)) throw new Error('audio output contains non-finite samples')
          energy += left * left + right * right
          count += 2
          maxAbs = Math.max(maxAbs, Math.abs(left), Math.abs(right))
          maxChannelDelta = Math.max(maxChannelDelta, Math.abs(left - right))
          leftMean += left
          rightMean += right
        }
        blockMeans.push({
          left: leftMean / block.left.length,
          right: rightMean / block.right.length,
        })
      }
      return { rms: Math.sqrt(energy / Math.max(1, count)), maxAbs, maxChannelDelta, blockMeans }
    }

    const createFormalNode = async (context, wasmModule, params, requestId, hrtf) => {
      const node = new AudioWorkletNode(context, 'hypersoundengine-wasm', {
        numberOfInputs: 2,
        numberOfOutputs: 1,
        outputChannelCount: [2],
        processorOptions: { wasmModule, maxFrames: 128, params, requestId, hrtf },
      })
      const messages = []
      const firstMessage = timeout(new Promise((resolve) => {
        node.port.onmessage = ({ data }) => {
          messages.push(data)
          resolve(data)
        }
      }), `${requestId} worklet response`)
      node.port.start()
      return { node, messages, firstMessage }
    }

    const tests = []
    const wasmModule = await compileWasm()
    const syntheticHrtf = {
      sampleRate: 48000,
      azimuths: [-30, 30],
      elevations: [0],
      hrirLength: 2,
      left: new Float32Array([1, 0, 0.2, 0]),
      right: new Float32Array([0.1, 0, 0.8, 0]),
    }

    {
      const context = await makeContext()
      try {
        const params = {
          spatial: { mode: 'off' },
          eq: { enabled: false },
          limiter: { enabled: false },
          stereoWidth: 0,
        }
        const formal = await createFormalNode(context, wasmModule, params, 'e2e-ready')
        const response = await formal.firstMessage
        if (response.type !== 'ready' || response.requestId !== 'e2e-ready') {
          throw new Error(`unexpected ready response: ${JSON.stringify(response)}`)
        }
        const collector = attachCollector(context, formal.node)
        const source = makeSource(context)
        source.connect(formal.node)
        source.start()
        const output = stats((await collector.waitFor(12)).slice(-6))
        if (output.rms <= 0.02) throw new Error(`spatial-off 1-21 chain was silent (rms=${output.rms})`)
        if (output.maxChannelDelta > 0.002) {
          throw new Error(`stage 3 stereo-width processing was not applied (delta=${output.maxChannelDelta})`)
        }
        tests.push({ name: 'ready and spatial-off stages 1-21 produce audio', ...output })
        source.stop()
      } finally {
        await context.close()
      }
    }

    {
      const context = await makeContext()
      try {
        const off = await createFormalNode(
          context,
          wasmModule,
          { spatial: { mode: 'off' }, eq: { enabled: false }, limiter: { enabled: false } },
          'e2e-spatial-off-reference',
        )
        const spatial = await createFormalNode(
          context,
          wasmModule,
          {
            spatial: {
              mode: 'instant',
              masterGain: 1,
              instant: { spreadDeg: 60, amount: 1, room: 'studio', roomAmount: 0, multichannelAuto: false },
              convolution: 'time',
              hrtfInterp: 'nearest',
            },
            eq: { enabled: false },
            limiter: { enabled: false },
          },
          'e2e-stage22-ready',
          syntheticHrtf,
        )
        const [offResponse, spatialResponse] = await Promise.all([off.firstMessage, spatial.firstMessage])
        if (
          offResponse.type !== 'ready'
          || offResponse.requestId !== 'e2e-spatial-off-reference'
          || spatialResponse.type !== 'ready'
          || spatialResponse.requestId !== 'e2e-stage22-ready'
        ) {
          throw new Error(`unexpected stage22 ready responses: ${JSON.stringify({ offResponse, spatialResponse })}`)
        }

        const offCollector = attachCollector(context, off.node)
        const spatialCollector = attachCollector(context, spatial.node)
        const offSource = makeSource(context, 0.25, 0.25)
        const spatialSource = makeSource(context, 0.25, 0.25)
        offSource.connect(off.node)
        spatialSource.connect(spatial.node)
        offSource.start()
        spatialSource.start()
        const [offBlocks, spatialBlocks] = await Promise.all([
          offCollector.waitFor(12),
          spatialCollector.waitFor(12),
        ])
        const offOutput = stats(offBlocks.slice(-6))
        const output = stats(spatialBlocks.slice(-6))
        if (output.rms <= 0.02) throw new Error(`stage22 instant render was silent (rms=${output.rms})`)
        if (output.maxChannelDelta <= 0.02) {
          throw new Error(`stage22 instant render was not binaurally asymmetric (delta=${output.maxChannelDelta})`)
        }
        const rmsDelta = Math.abs(output.rms - offOutput.rms)
        if (rmsDelta <= 0.01) {
          throw new Error(`stage22 instant render matched spatial-off output (rms delta=${rmsDelta})`)
        }
        tests.push({
          name: 'ready and synthetic-grid stage22 instant produces asymmetric audio',
          ...output,
          offRms: offOutput.rms,
        })
        offSource.stop()
        spatialSource.stop()
      } finally {
        await context.close()
      }
    }

    {
      const context = await makeContext()
      try {
        const formal = await createFormalNode(
          context,
          wasmModule,
          { spatial: { mode: 'instant' } },
          'e2e-construct-failure',
        )
        const response = await formal.firstMessage
        if (response.type !== 'error' || response.phase !== 'construct') {
          throw new Error(`expected construct error, received ${JSON.stringify(response)}`)
        }
        const collector = attachCollector(context, formal.node)
        const source = makeSource(context)
        source.connect(formal.node)
        source.start()
        const output = stats((await collector.waitFor(8)).slice(-4))
        if (output.maxAbs !== 0) throw new Error(`failed constructor emitted audio (peak=${output.maxAbs})`)
        tests.push({ name: 'constructor failure remains silent', code: response.code, maxAbs: output.maxAbs })
        source.stop()
      } finally {
        await context.close()
      }
    }

    {
      const [{ createHyperSoundEngineHost }, { createDefaultParams }, fixture] = await Promise.all([
        import(`${baseUrl}/browser.js`),
        import(`${baseUrl}/index.js`),
        fetch(`${baseUrl}/default-params.json`).then((response) => response.json()),
      ])
      const context = await makeContext()
      try {
        const initial = createDefaultParams(context.sampleRate)
        Object.assign(initial, fixture.params)
        initial.sampleRate = context.sampleRate
        initial.eq.enabled = false
        initial.limiter.enabled = false
        initial.spatial.mode = 'off'
        initial.stereoWidth = 1
        const masterGain = context.createGain()
        const analyser = context.createGain()
        const collector = attachCollector(context, analyser)
        const source = makeSource(context)
        source.connect(masterGain)
        source.start()
        const host = createHyperSoundEngineHost({
          mode: 'worklet',
          engineBackend: 'wasm',
          wasmWorkletUrl: `${baseUrl}/wasm-worklet-bundle.js`,
          wasmUrl: `${baseUrl}/hse_wasm_bg.wasm`,
          workletCrossfadeMs: 40,
          wasmRequestTimeoutMs: 5000,
        })
        await host.attach({ audioContext: context, masterGain, analyser }, initial)
        await collector.waitFor(8)
        await collector.barrier('before-crossfade')
        collector.clear()
        const oldNode = host.getAudioNode()
        const replacement = structuredClone(initial)
        replacement.stereoWidth = 0
        await host.setParams(replacement)
        await collector.barrier('after-crossfade')
        const blocks = collector.blocks.slice()
        const output = stats(blocks)
        if (host.getAudioNode() === oldNode) throw new Error('setParams did not replace the AudioWorkletNode')
        if (output.rms <= 0.02) throw new Error(`crossfade output was silent (rms=${output.rms})`)
        const transition = output.blockMeans.some(({ left, right }) => {
          const delta = Math.abs(left - right)
          return delta > 0.025 && delta < 0.35
        })
        if (!transition) throw new Error('crossfade did not expose an intermediate old/new node mix')
        const quietestBlock = Math.min(...output.blockMeans.map(({ left, right }) => Math.hypot(left, right)))
        if (quietestBlock <= 0.04) throw new Error(`crossfade introduced a dropout (${quietestBlock})`)
        tests.push({
          name: 'parameter update replaces node with audible crossfade',
          rms: output.rms,
          quietestBlock,
          observedTransition: transition,
        })
        host.dispose()
        source.stop()
      } finally {
        await context.close()
      }
    }

    return { userAgent: navigator.userAgent, tests }
  }, { baseUrl })

  if (consoleErrors.length > 0) {
    throw new Error(`browser console errors:\n${consoleErrors.join('\n')}`)
  }
  console.log(`wasm AudioWorklet E2E passed in ${results.userAgent}`)
  for (const result of results.tests) console.log(`  PASS ${result.name}`)
} finally {
  await browser?.close()
  await new Promise((resolve) => server.close(resolve))
}
