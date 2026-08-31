#!/usr/bin/env node

const ALLOW_ENV = 'HSE_ALLOW_REAL_AUDIO'
const DEFAULTS = {
  url: 'ws://127.0.0.1:4780/',
  sampleRate: 48000,
  blockFrames: 128,
  frames: 48000,
  run: false,
  pretty: false,
}

function parseArgs(argv) {
  const config = { ...DEFAULTS }
  for (let index = 0; index < argv.length; index++) {
    const arg = argv[index]
    const value = () => {
      index++
      if (index >= argv.length) throw new Error(`${arg} 缺少取值`)
      return argv[index]
    }
    switch (arg) {
      case '--url': config.url = value(); break
      case '--rate': config.sampleRate = parsePositiveInt(value(), '--rate'); break
      case '--block': config.blockFrames = parsePositiveInt(value(), '--block'); break
      case '--frames': config.frames = parsePositiveInt(value(), '--frames'); break
      case '--run': config.run = true; break
      case '--pretty': config.pretty = true; break
      case '-h':
      case '--help': config.help = true; break
      default: throw new Error(`未知参数：${arg}`)
    }
  }
  if (config.frames < config.blockFrames) throw new Error('--frames 不得小于 --block')
  return config
}

function parsePositiveInt(text, name) {
  const value = Number(text)
  if (!Number.isSafeInteger(value) || value <= 0) throw new Error(`${name} 需要正整数`)
  return value
}

function report(config, status, extra = {}) {
  return {
    schemaVersion: 2,
    tool: 'phase4-dual-push',
    dryRun: !config.run || process.env[ALLOW_ENV] !== '1',
    status,
    gate: {
      runFlag: config.run,
      environment: ALLOW_ENV,
      environmentAllowed: process.env[ALLOW_ENV] === '1',
    },
    config: {
      url: config.url,
      sampleRate: config.sampleRate,
      blockSizeFrames: config.blockFrames,
      framesPerSession: config.frames,
      sessions: 2,
      websocketConnections: 2,
      totalFrames: config.frames * 2,
    },
    outputVerification: {
      status: 'external-output-required',
      expectedFrequenciesHz: [997, 1499],
      reason: '控制协议不回传渲染 PCM；频率和物理输出必须由外部录音或分析仪验证',
    },
    ...extra,
  }
}

function print(config, value) {
  process.stdout.write(`${JSON.stringify(value, null, config.pretty ? 2 : 0)}\n`)
}

function rpc(socket, id, method, params) {
  return new Promise((resolve, reject) => {
    const onMessage = (event) => {
      if (typeof event.data !== 'string') return
      const message = JSON.parse(event.data)
      if (message.id !== id) return
      socket.removeEventListener('message', onMessage)
      if (message.error) reject(new Error(`${message.error.code}: ${message.error.message}`))
      else resolve(message.result)
    }
    socket.addEventListener('message', onMessage)
    socket.send(JSON.stringify({ jsonrpc: '2.0', id, method, params }))
  })
}

async function connect(url) {
  const socket = new WebSocket(url)
  socket.binaryType = 'arraybuffer'
  await new Promise((resolve, reject) => {
    socket.addEventListener('open', resolve, { once: true })
    socket.addEventListener('error', () => reject(new Error(`连接 ${url} 失败`)), { once: true })
  })
  return socket
}

function makePcm(sessionIndex, sessionId, sequence, startFrame, frameCount, sampleRate) {
  const bytes = new Uint8Array(12 + frameCount * 8)
  const view = new DataView(bytes.buffer)
  const frequency = sessionIndex === 0 ? 997 : 1499
  const amplitude = sessionIndex === 0 ? 0.04 : 0.03
  view.setUint32(0, sessionId, true)
  view.setBigUint64(4, sequence, true)
  for (let frame = 0; frame < frameCount; frame++) {
    const sample = amplitude * Math.sin(2 * Math.PI * frequency * (startFrame + frame) / sampleRate)
    view.setFloat32(12 + frame * 8, sample, true)
    view.setFloat32(16 + frame * 8, sample, true)
  }
  return bytes
}

function sendSession(socket, sessionIndex, sessionId, config) {
  let sent = 0
  let sequence = 0n
  while (sent < config.frames) {
    const count = Math.min(config.blockFrames, config.frames - sent)
    socket.send(makePcm(sessionIndex, sessionId, sequence, sent, count, config.sampleRate))
    sent += count
    sequence++
  }
  return {
    sessionId,
    connection: sessionIndex + 1,
    frequencyHz: sessionIndex === 0 ? 997 : 1499,
    framesSent: sent,
    blocksSent: Number(sequence),
  }
}

function sessionDiagnostics(state, sessionId) {
  return state.sessions?.find((session) => session.sessionId === sessionId)
}

async function waitUntilConsumed(socket, firstRequestId, sessionIds, config) {
  const maxPolls = Math.ceil(config.frames / config.blockFrames) * 8 + 32
  for (let poll = 0; poll < maxPolls; poll++) {
    const state = await rpc(socket, firstRequestId + poll, 'getState', {})
    const sessions = sessionIds.map((sessionId) => sessionDiagnostics(state, sessionId))
    if (sessions.every((session) => session
      && session.ingestedFrames === config.frames
      && session.consumedFrames >= config.frames
      && session.queuedFrames === 0)) {
      return { polls: poll + 1, state, sessions }
    }
  }
  throw new Error('固定轮询预算内未观察到两条会话分别完成消费')
}

function assertZeroXruns(state) {
  const xrunsIn = Number(state.stats?.xrunsIn ?? -1)
  const xrunsOut = Number(state.stats?.xrunsOut ?? -1)
  if (xrunsIn !== 0 || xrunsOut !== 0) {
    throw new Error(`验收要求 xrun 为 0，实际 xrunsIn=${xrunsIn}, xrunsOut=${xrunsOut}`)
  }
  return { xrunsIn, xrunsOut }
}

async function run(config) {
  const sockets = await Promise.all([connect(config.url), connect(config.url)])
  try {
    const params = { sampleRate: config.sampleRate, channels: 2, format: 'f32le' }
    const opened = await Promise.all(sockets.map((socket, index) => rpc(socket, 1, 'openSession', params)
      .then((result) => ({ ...result, connection: index + 1 }))))
    const sessionIds = opened.map((entry) => entry.sessionId)
    if (new Set(sessionIds).size !== 2) throw new Error('两条独立连接必须获得不同 sessionId')

    const before = await rpc(sockets[0], 2, 'getState', {})
    if (before.phase !== 'running') throw new Error(`服务必须为 running，当前为 ${before.phase}`)
    assertZeroXruns(before)

    const sent = sockets.map((socket, index) => sendSession(socket, index, sessionIds[index], config))
    const consumed = await waitUntilConsumed(sockets[0], 100, sessionIds, config)
    const xruns = assertZeroXruns(consumed.state)
    if (consumed.state.phase !== 'running') throw new Error(`消费后服务必须保持 running，当前为 ${consumed.state.phase}`)

    await Promise.all(sockets.map((socket, index) => rpc(socket, 3, 'closeSession', { sessionId: sessionIds[index] })))
    return report(config, 'pass', {
      connections: opened,
      sessions: sent,
      consumption: { polls: consumed.polls, sessions: consumed.sessions },
      xruns,
    })
  } finally {
    for (const socket of sockets) socket.close()
  }
}

const usage = `phase4-dual-push.mjs - 固定帧双连接推流验收客户端（默认 dry-run）

用法：
  node scripts/phase4-dual-push.mjs [--url ws://127.0.0.1:4780/] [--rate 48000]
       [--block 128] [--frames 48000] [--pretty] [--run]

--run 还必须显式设置 HSE_ALLOW_REAL_AUDIO=1。脚本使用两条独立 WebSocket，按固定帧数和消费计数结束，不启动服务。`

let config
try {
  config = parseArgs(process.argv.slice(2))
  if (config.help) {
    process.stdout.write(`${usage}\n`)
    process.exit(0)
  }
  if (!config.run) {
    print(config, report(config, 'dry-run-ready'))
    process.exit(0)
  }
  if (process.env[ALLOW_ENV] !== '1') {
    print(config, report(config, 'real-audio-gate-required'))
    process.exit(3)
  }
  if (typeof WebSocket !== 'function') throw new Error('当前 Node 运行时不提供全局 WebSocket；请使用 Node 22+')
  print(config, await run(config))
} catch (error) {
  const fallback = config ?? DEFAULTS
  print(fallback, report(fallback, 'error', { error: error instanceof Error ? error.message : String(error) }))
  process.exit(2)
}
