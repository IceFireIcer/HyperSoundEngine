// Independent Phase 5 pilot. Generate pkg/ with:
// wasm-bindgen ../../target/wasm32-unknown-unknown/release/hse_wasm.wasm --target web --out-dir web/wasm-pilot/pkg
import init from './pkg/hse_wasm.js'

export const WASM_PILOT_PROCESSOR_NAME = 'hypersoundengine-wasm-pilot'

const DEFAULT_CONFIGURE_TIMEOUT_MS = 2000
const requestStates = new WeakMap()
let nextRequestId = 1

function createPilotError(data, fallbackMessage) {
  const error = new Error(data?.message ?? fallbackMessage)
  error.name = 'WasmPilotError'
  if (data?.code) error.code = data.code
  if (data?.phase) error.phase = data.phase
  if (data?.requestId) error.requestId = data.requestId
  return error
}

function rejectPending(state, error) {
  for (const pending of state.pending.values()) {
    clearTimeout(pending.timeoutId)
    pending.reject(error)
  }
  state.pending.clear()
}

function stateFor(node) {
  if (!node?.port || typeof node.port.postMessage !== 'function') {
    throw new TypeError('node.port must be an AudioWorklet MessagePort')
  }

  let state = requestStates.get(node)
  if (state) return state

  state = { pending: new Map() }
  requestStates.set(node, state)

  node.port.addEventListener('message', ({ data }) => {
    if (data?.type === 'configured' && data.requestId) {
      const pending = state.pending.get(data.requestId)
      if (!pending) return
      clearTimeout(pending.timeoutId)
      state.pending.delete(data.requestId)
      pending.resolve()
      return
    }

    if (data?.type === 'error') {
      const error = createPilotError(data, 'wasm pilot reported an error')
      const pending = data.requestId && state.pending.get(data.requestId)
      if (pending) {
        clearTimeout(pending.timeoutId)
        state.pending.delete(data.requestId)
        pending.reject(error)
      } else if (data.phase === 'construct' || data.phase === 'process') {
        rejectPending(state, error)
      }
    }
  })
  node.port.addEventListener('messageerror', () => {
    rejectPending(state, createPilotError(
      { phase: 'port', code: 'message-deserialization-failed' },
      'wasm pilot port could not deserialize a message',
    ))
  })
  node.addEventListener?.('processorerror', () => {
    rejectPending(state, createPilotError(
      { phase: 'port', code: 'processor-error' },
      'wasm pilot AudioWorklet processor failed',
    ))
  })
  node.port.start?.()
  return state
}

export async function createWasmPilotNode(context, options = {}) {
  const {
    workletUrl = new URL('./worklet.js', import.meta.url),
    wasmUrl = new URL('./pkg/hse_wasm_bg.wasm', import.meta.url),
    maxFrames = 128,
    type = 'peaking',
    f0 = 1000,
    q = 1,
    gainDb = 0,
  } = options

  const response = await fetch(wasmUrl)
  if (!response.ok) throw new Error(`Failed to fetch wasm pilot: ${response.status}`)
  const wasmBytes = await response.arrayBuffer()
  const wasmModule = await WebAssembly.compile(wasmBytes)

  // Initialize on the main thread as a packaging smoke check. The worklet gets its own instance.
  const exports = await init({ module_or_path: wasmModule })
  if (!(exports.memory instanceof WebAssembly.Memory)) throw new Error('wasm pilot did not export memory')

  await context.audioWorklet.addModule(workletUrl)
  const node = new AudioWorkletNode(context, WASM_PILOT_PROCESSOR_NAME, {
    numberOfInputs: 1,
    numberOfOutputs: 1,
    outputChannelCount: [2],
    processorOptions: {
      wasmModule,
      maxFrames,
      params: { type, f0, q, gainDb },
    },
  })
  stateFor(node)
  return node
}

export function configureWasmPilot(node, params, options = {}) {
  const { timeoutMs = DEFAULT_CONFIGURE_TIMEOUT_MS } = options
  if (!Number.isFinite(timeoutMs) || timeoutMs < 0) {
    return Promise.reject(new RangeError('timeoutMs must be a non-negative finite number'))
  }

  let state
  try {
    state = stateFor(node)
  } catch (error) {
    return Promise.reject(error)
  }

  const requestId = `configure-${nextRequestId++}`
  return new Promise((resolve, reject) => {
    const timeoutId = setTimeout(() => {
      state.pending.delete(requestId)
      reject(createPilotError(
        { phase: 'configure', code: 'timeout', requestId },
        `wasm pilot configure timed out after ${timeoutMs}ms`,
      ))
    }, timeoutMs)
    state.pending.set(requestId, { resolve, reject, timeoutId })

    try {
      node.port.postMessage({ type: 'configure', requestId, params })
    } catch (error) {
      clearTimeout(timeoutId)
      state.pending.delete(requestId)
      reject(createPilotError(
        { phase: 'port', code: 'post-message-failed', requestId, message: error?.message },
        'wasm pilot configure message could not be sent',
      ))
    }
  })
}

export function resetWasmPilot(node) {
  node.port.postMessage({ type: 'reset' })
}
