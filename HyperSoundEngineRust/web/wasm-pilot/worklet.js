import initSync, { HseBiquad } from './pkg/hse_wasm.js'

const PROCESSOR_NAME = 'hypersoundengine-wasm-pilot'

class HyperSoundEngineWasmPilotProcessor extends AudioWorkletProcessor {
  constructor(options) {
    super()
    this.engine = null
    this.memory = null
    this.capacity = 0
    this.left = null
    this.right = null

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

      this.engine = new HseBiquad(
        sampleRate,
        params.type ?? 'peaking',
        params.f0 ?? 1000,
        params.q ?? 1,
        params.gainDb ?? 0,
        maxFrames,
      )
      this.capacity = this.engine.capacity()
      this.refreshViews()
    } catch (error) {
      this.engine = null
      this.postError('construct', 'initialization-failed', error)
    }
  }

  postError(phase, code, error, requestId) {
    const message = error instanceof Error ? error.message : String(error)
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
      this.engine.configure(params.type, params.f0, params.q, params.gainDb)
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
      for (let i = 0; i < frames; i++) {
        this.left[i] = inputLeft ? inputLeft[i] : 0
        this.right[i] = inputRight ? inputRight[i] : 0
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
