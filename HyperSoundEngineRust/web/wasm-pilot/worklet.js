import initSync, { HseEngine } from './pkg/hse_wasm.js'

const PROCESSOR_NAME = 'hypersoundengine-wasm-pilot'

class HyperSoundEngineWasmPilotProcessor extends AudioWorkletProcessor {
  constructor(options) {
    super()
    this.engine = null
    this.memory = null
    this.capacity = 0
    this.left = null
    this.right = null
    this.sidechainLeft = null
    this.sidechainRight = null

    this.port.onmessage = ({ data }) => {
      if (data?.type === 'configure') {
        this.configure(data)
      } else if (data?.type === 'reset') {
        this.reset()
      }
    }

    try {
      const { wasmModule, maxFrames = 128, params = {} } = options.processorOptions ?? {}
      if (!(wasmModule instanceof WebAssembly.Module)) {
        throw new TypeError('processorOptions.wasmModule must be a compiled WebAssembly.Module')
      }
      const exports = initSync({ module: wasmModule })
      this.memory = exports.memory
      if (!(this.memory instanceof WebAssembly.Memory)) {
        throw new TypeError('wasm pilot did not export memory')
      }

      this.engine = new HseEngine(sampleRate, maxFrames, JSON.stringify(params))
      this.capacity = this.engine.capacity()
      this.refreshViews()
    } catch (error) {
      this.engine = null
      this.postError('construct', 'initialization-failed', error)
    }
  }

  postError(phase, fallbackCode, error, requestId) {
    let code = fallbackCode
    let message = error instanceof Error ? error.message : String(error)
    try {
      const structured = JSON.parse(message)
      if (typeof structured?.code === 'string') code = structured.code
      if (typeof structured?.message === 'string') message = structured.message
    } catch {
      // Non-Rust errors retain the worklet-level fallback code and message.
    }
    try {
      this.port.postMessage({ type: 'error', phase, code, message, ...(requestId ? { requestId } : {}) })
    } catch {
      // A closed/broken MessagePort must not escape into the render callback.
    }
  }

  configure({ params = {}, requestId }) {
    if (!requestId) {
      this.postError('configure', 'missing-request-id', 'configure requestId is required')
      return
    }
    if (!this.engine) {
      this.postError('configure', 'engine-unavailable', 'wasm pilot engine is unavailable', requestId)
      return
    }
    try {
      this.engine.configure(JSON.stringify(params))
      this.port.postMessage({ type: 'configured', requestId })
    } catch (error) {
      this.postError('configure', 'configure-failed', error, requestId)
    }
  }

  reset() {
    if (!this.engine) return
    try {
      this.engine.reset()
    } catch (error) {
      this.postError('reset', 'reset-failed', error)
    }
  }

  refreshViews() {
    this.left = new Float32Array(this.memory.buffer, this.engine.left_ptr(), this.capacity)
    this.right = new Float32Array(this.memory.buffer, this.engine.right_ptr(), this.capacity)
    this.sidechainLeft = new Float32Array(
      this.memory.buffer,
      this.engine.sidechain_left_ptr(),
      this.capacity,
    )
    this.sidechainRight = new Float32Array(
      this.memory.buffer,
      this.engine.sidechain_right_ptr(),
      this.capacity,
    )
  }

  process(inputs, outputs) {
    const output = outputs[0]
    if (!output || output.length === 0) return true
    if (!this.engine) {
      output.forEach((channel) => channel.fill(0))
      return true
    }

    try {
      const frames = output[0].length
      if (frames > this.capacity) {
        throw new RangeError(`render quantum ${frames} exceeds capacity ${this.capacity}`)
      }
      if (this.left.buffer !== this.memory.buffer) this.refreshViews()

      const input = inputs[0]
      const inputLeft = input?.[0]
      const inputRight = input?.[1] ?? inputLeft
      const sidechain = inputs[1]
      const sidechainLeft = sidechain?.[0]
      const sidechainRight = sidechain?.[1] ?? sidechainLeft
      for (let i = 0; i < frames; i++) {
        this.left[i] = inputLeft ? inputLeft[i] : 0
        this.right[i] = inputRight ? inputRight[i] : 0
        this.sidechainLeft[i] = sidechainLeft ? sidechainLeft[i] : this.left[i]
        this.sidechainRight[i] = sidechainRight ? sidechainRight[i] : this.right[i]
      }

      this.engine.process(frames)
      const outputLeft = output[0]
      const outputRight = output[1]
      for (let i = 0; i < frames; i++) {
        outputLeft[i] = this.left[i]
        if (outputRight) outputRight[i] = this.right[i]
      }
    } catch (error) {
      output.forEach((channel) => channel.fill(0))
      this.engine = null
      this.postError('process', 'processing-failed', error)
    }
    return true
  }
}

registerProcessor(PROCESSOR_NAME, HyperSoundEngineWasmPilotProcessor)
