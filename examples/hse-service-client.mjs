// Node 22+ reference client for hse-service. Importing this file has no side effects.

export class HseRpcError extends Error {
  constructor(code, message) {
    super(`hse-service RPC ${code}: ${message}`)
    this.name = 'HseRpcError'
    this.code = code
  }
}

export function packPcmFrame(sessionId, sequence, interleavedStereo) {
  if (!Number.isInteger(sessionId) || sessionId <= 0 || sessionId > 0xffff_ffff) {
    throw new RangeError('sessionId must be a positive u32')
  }
  if (!(interleavedStereo instanceof Float32Array)) {
    throw new TypeError('interleavedStereo must be a Float32Array')
  }
  if (interleavedStereo.length === 0 || interleavedStereo.length % 2 !== 0) {
    throw new RangeError('PCM must contain complete interleaved stereo frames')
  }
  const payloadBytes = interleavedStereo.length * Float32Array.BYTES_PER_ELEMENT
  if (payloadBytes > 1_048_576) throw new RangeError('PCM payload exceeds 1 MiB')

  const buffer = new ArrayBuffer(12 + payloadBytes)
  const view = new DataView(buffer)
  view.setUint32(0, sessionId, true)
  view.setBigUint64(4, BigInt(sequence), true)
  for (let i = 0; i < interleavedStereo.length; i++) {
    view.setFloat32(12 + i * 4, interleavedStereo[i], true)
  }
  return buffer
}

export class HseServiceClient {
  constructor(socket, rpcTimeoutMs = 15_000) {
    this.socket = socket
    this.rpcTimeoutMs = rpcTimeoutMs
    this.nextId = 1
    this.pending = new Map()
    this.eventListeners = new Set()

    socket.addEventListener('message', (event) => {
      if (typeof event.data !== 'string') return
      let message
      try {
        message = JSON.parse(event.data)
      } catch {
        return
      }
      if (Object.hasOwn(message, 'id')) {
        const key = String(message.id)
        const pending = this.pending.get(key)
        if (!pending) return
        this.pending.delete(key)
        clearTimeout(pending.timeout)
        if (message.error) {
          pending.reject(new HseRpcError(message.error.code, message.error.message))
        } else {
          pending.resolve(message.result)
        }
        return
      }
      for (const listener of this.eventListeners) listener(message)
    })

    const rejectPending = () => {
      const error = new Error('hse-service WebSocket closed')
      for (const pending of this.pending.values()) {
        clearTimeout(pending.timeout)
        pending.reject(error)
      }
      this.pending.clear()
    }
    socket.addEventListener('close', rejectPending, { once: true })
  }

  static async connect(url = 'ws://127.0.0.1:4780/', { rpcTimeoutMs = 15_000 } = {}) {
    if (typeof WebSocket !== 'function') {
      throw new Error('Node 22+ or another runtime with global WebSocket is required')
    }
    const socket = new WebSocket(url)
    socket.binaryType = 'arraybuffer'
    await new Promise((resolve, reject) => {
      socket.addEventListener('open', resolve, { once: true })
      socket.addEventListener('error', () => reject(new Error(`cannot connect to ${url}`)), {
        once: true,
      })
    })
    return new HseServiceClient(socket, rpcTimeoutMs)
  }

  onEvent(listener) {
    this.eventListeners.add(listener)
    return () => this.eventListeners.delete(listener)
  }

  rpc(method, params = {}) {
    if (this.socket.readyState !== WebSocket.OPEN) {
      return Promise.reject(new Error('hse-service WebSocket is not open'))
    }
    const id = this.nextId++
    return new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        this.pending.delete(String(id))
        reject(new Error(`hse-service RPC timed out: ${method}`))
      }, this.rpcTimeoutMs)
      this.pending.set(String(id), { resolve, reject, timeout })
      try {
        this.socket.send(JSON.stringify({ jsonrpc: '2.0', id, method, params }))
      } catch (error) {
        this.pending.delete(String(id))
        clearTimeout(timeout)
        reject(error)
      }
    })
  }

  sendPcm(sessionId, sequence, interleavedStereo) {
    if (this.socket.readyState !== WebSocket.OPEN) {
      throw new Error('hse-service WebSocket is not open')
    }
    this.socket.send(packPcmFrame(sessionId, sequence, interleavedStereo))
  }

  async waitForSessionConsumed(sessionId, minimumConsumedFrames, maxPolls = 2_000) {
    for (let poll = 0; poll < maxPolls; poll++) {
      const state = await this.rpc('getState')
      const session = state.sessions?.find((entry) => entry.sessionId === sessionId)
      if (
        session &&
        session.queuedFrames === 0 &&
        session.consumedFrames >= minimumConsumedFrames
      ) {
        return { state, session }
      }
      await new Promise((resolve) => setTimeout(resolve, 2))
    }
    throw new Error(`session ${sessionId} was not consumed within the polling budget`)
  }

  close() {
    this.socket.close()
  }
}
