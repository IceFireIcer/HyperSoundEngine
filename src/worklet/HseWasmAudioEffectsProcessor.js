import { HseEngine, initSync } from './hse_wasm.js'

export const WASM_WORKLET_PROCESSOR_NAME = 'hypersoundengine-wasm'

function stringifyParams(params) {
  return JSON.stringify(params, (_key, value) => (
    ArrayBuffer.isView(value) && !(value instanceof DataView) ? Array.from(value) : value
  ))
}

function createEngine(maxFrames, params, hrtf) {
  const paramsJson = stringifyParams(params)
  if (hrtf instanceof ArrayBuffer) {
    return HseEngine.withSofaBytes(sampleRate, maxFrames, paramsJson, new Uint8Array(hrtf))
  }
  if (hrtf) {
    const azimuths = Float32Array.from(hrtf.azimuths ?? [])
    const elevations = Float32Array.from(hrtf.elevations ?? [])
    const left = hrtf.left instanceof Float32Array ? hrtf.left : Float32Array.from(hrtf.left ?? [])
    const right = hrtf.right instanceof Float32Array ? hrtf.right : Float32Array.from(hrtf.right ?? [])
    return HseEngine.withHrtfGrid(
      sampleRate,
      maxFrames,
      paramsJson,
      hrtf.sampleRate,
      azimuths,
      elevations,
      hrtf.hrirLength,
      left,
      right,
    )
  }
  return new HseEngine(sampleRate, maxFrames, paramsJson)
}

class HseWasmAudioEffectsProcessor extends AudioWorkletProcessor {
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
      if (data?.type === 'reset') this.reset()
    }

    const { requestId } = options.processorOptions ?? {}
    try {
      const { wasmModule, maxFrames = 128, params = {}, hrtf } = options.processorOptions ?? {}
      if (!(wasmModule instanceof WebAssembly.Module)) {
        throw new TypeError('processorOptions.wasmModule must be a compiled WebAssembly.Module')
      }
      const exports = initSync({ module: wasmModule })
      this.memory = exports.memory
      if (!(this.memory instanceof WebAssembly.Memory)) {
        throw new TypeError('hse wasm did not export memory')
      }
      this.engine = createEngine(maxFrames, params, hrtf)
      this.capacity = this.engine.capacity()
      this.refreshViews()
      this.port.postMessage({ type: 'ready', requestId })
    } catch (error) {
      this.engine = null
      this.postError('construct', 'initialization-failed', error, requestId)
    }
  }

  postError(phase, fallbackCode, error, requestId) {
    let code = fallbackCode
    let message = error instanceof Error ? error.message : String(error)
    if (phase !== 'process') {
      try {
        const structured = JSON.parse(message)
        if (typeof structured?.code === 'string') code = structured.code
        if (typeof structured?.message === 'string') message = structured.message
      } catch {
        // Non-Rust failures retain the worklet-level code and message.
      }
    }
    try {
      this.port.postMessage({ type: 'error', phase, code, message, ...(requestId ? { requestId } : {}) })
    } catch {
      // MessagePort failures must not escape into the render callback.
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
    this.sidechainLeft = new Float32Array(this.memory.buffer, this.engine.sidechain_left_ptr(), this.capacity)
    this.sidechainRight = new Float32Array(this.memory.buffer, this.engine.sidechain_right_ptr(), this.capacity)
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

registerProcessor(WASM_WORKLET_PROCESSOR_NAME, HseWasmAudioEffectsProcessor)
