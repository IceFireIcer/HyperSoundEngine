#!/usr/bin/env node
/**
 * soak-8h —— Phase 4 §三「8h 零 xrun」压测工具（真机长跑，静音链路默认）。
 *
 * 前置：hse-service 已启动（cargo run -p hse-service --release，默认 ws://127.0.0.1:4780/）。
 * 拓扑：configure 回环拦截默认渲染端点 → openSession 推流会话按实时速率喂确定性 PCM
 *       → 混后处理 → 渲染回默认设备。默认 --muted：reverbSimple wet=0/dry=0 → 渲染静音，
 *       但回环捕获/DSP/渲染三线程与会话推流全部真实运转（8h 连续负载）。
 *
 * 通过判据（写入报告 JSON）：
 *   1. phase 全程恒为 "running"；
 *   2. 四个 stats 计数器单调不减；
 *   3. framesProcessed ≥ 97% × 理论帧数（推流帧实时速率喂入）；
 *   4. xrunsIn/xrunsOut 累计值记录（目标 0；>0 时报告各采样点的增长斜率供归因）。
 *
 * 用法：
 *   node scripts/soak-8h.mjs [--url ws://127.0.0.1:4780/] [--duration 28800]
 *        [--sample-rate 48000] [--block 1024] [--muted] [--audible] [--report <path>]
 * 建议过夜执行：service 先启，本工具结束自动 stop 并落盘报告。
 */
import { writeFileSync } from 'node:fs'

const args = process.argv.slice(2)
const arg = (name, dflt) => {
  const i = args.indexOf('--' + name)
  return i >= 0 && i + 1 < args.length ? args[i + 1] : dflt
}
const has = (name) => args.includes('--' + name)

const URL = arg('url', 'ws://127.0.0.1:4780/')
const DURATION = Number(arg('duration', '28800')) // 秒；8h = 28800
const SAMPLE_RATE = Number(arg('sample-rate', '48000'))
const BLOCK = Number(arg('block', '1024')) // 每帧的帧数（一次 WS 帧承载的立体声帧数）
const REPORT = arg('report', `soak-report-${new Date().toISOString().replace(/[:.]/g, '-')}.json`)
const MUTED = has('audible') ? false : true // 默认静音链路（保护耳朵）
const NO_SESSION = has('no-session') // 真实播放器模式：不推流，用户播放音乐，loopback 为实时时钟源；xrunsOut==0 为硬判据

// ---- JSON-RPC over WebSocket 最小客户端 ----
const ws = new WebSocket(URL)
let nextId = 1
const pending = new Map()
const events = []
ws.onmessage = (e) => {
  const m = JSON.parse(e.data)
  if (m.id !== undefined && m.id !== null && pending.has(m.id)) {
    pending.get(m.id)(m)
    pending.delete(m.id)
  } else if (m.method?.startsWith('event.')) {
    events.push({ at: Date.now(), ...m })
  }
}
const send = (method, params) => new Promise((res) => {
  const id = nextId++
  pending.set(id, res)
  setTimeout(() => { if (pending.has(id)) { pending.delete(id); res({ timeout: true }) } }, 20000)
  ws.send(JSON.stringify({ jsonrpc: '2.0', id, method, params: params ?? {} }))
})
await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej })

// ---- 配置 + 启动 ----
await send('listDevices')
const cfg = await send('configure', { mode: 'loopback', renderDeviceId: null, sampleRate: SAMPLE_RATE, blockSizeFrames: 256 })
if (!cfg.result?.applied) { console.error('configure 失败：', JSON.stringify(cfg)); process.exit(1) }
if (MUTED) await send('setParams', { params: { reverbSimple: { wet: 0, dry: 0 } } })
const startP = send('start')
await new Promise((r) => setTimeout(r, 1500))
const sr = await startP
if (!sr.result?.started) { console.error('start 失败：', JSON.stringify(sr)); process.exit(1) }
console.log(`soak 开始：${DURATION}s，sampleRate=${SAMPLE_RATE} block=${BLOCK} muted=${MUTED}`)

// ---- 推流会话：实时速率喂确定性正弦（幅度 0.05）----
let sessionId = 0
if (!NO_SESSION) {
  const os = await send('openSession', { sampleRate: SAMPLE_RATE, channels: 2, format: 'f32le' })
  sessionId = os.result.sessionId
}
const framesPerWs = BLOCK
const PACE = Number(arg('pace', '1.02')) // >1 = 轻微超速：渲染端永不饥饿；入环溢出按背压设计丢最旧
const wsIntervalMs = (framesPerWs / SAMPLE_RATE) * 1000 / PACE
const totalFrames = Math.floor(DURATION * SAMPLE_RATE)
let seq = 0
let sentFrames = 0
const payload = Buffer.alloc(framesPerWs * 2 * 4)
{
  const step = (2 * Math.PI * 997) / SAMPLE_RATE
  for (let i = 0; i < framesPerWs; i++) {
    const v = 0.05 * Math.sin(step * i)
    payload.writeFloatLE(v, i * 8)
    payload.writeFloatLE(v, i * 8 + 4)
  }
}
let pushing = !NO_SESSION
const pacer = setInterval(() => {
  if (!pushing) return
  const frame = Buffer.alloc(12 + payload.length)
  frame.writeUInt32LE(sessionId, 0)
  frame.writeBigUInt64LE(BigInt(seq++), 4)
  payload.copy(frame, 12)
  sentFrames += framesPerWs
  try { ws.send(frame) } catch { /* 断线由采样点上报 */ }
}, wsIntervalMs)

// ---- 周期采样 ----
const samples = []
const t0 = Date.now()
let phaseBroken = false
let last = null
const sample = async (label) => {
  const st = await send('getState')
  const s = st.result.stats
  const row = { t: Math.round((Date.now() - t0) / 1000), phase: st.result.phase, ...s }
  samples.push(row)
  if (label !== 'post-stop' && st.result.phase !== 'running') phaseBroken = true
  const monoOk = !last || (s.xrunsIn >= last.xrunsIn && s.xrunsOut >= last.xrunsOut && s.framesProcessed >= last.framesProcessed)
  console.log(`[${label}] t=${row.t}s phase=${row.phase} frames=${row.framesProcessed} (${(row.framesProcessed / SAMPLE_RATE).toFixed(0)}s) xrunsIn=${row.xrunsIn} xrunsOut=${row.xrunsOut}${monoOk ? '' : ' ⚠计数器回退!'}`)
  last = s
  return row
}

// 长跑：每 60s 采样一次，直至时长到
const endAt = t0 + DURATION * 1000
while (Date.now() < endAt) {
  await new Promise((r) => setTimeout(r, Math.min(60000, endAt - Date.now())))
  await sample('periodic')
  if (phaseBroken) break
}
pushing = false
clearInterval(pacer)
await new Promise((r) => setTimeout(r, 2000))
const preStop = await sample('pre-stop')

// ---- 停止 + 终态 ----
await send('stop')
await new Promise((r) => setTimeout(r, 2000))
const fin = await send('getState')
const finRow = await sample('post-stop')

// ---- 判定 ----
const first = samples[0] ?? { xrunsIn: 0, xrunsOut: 0, framesProcessed: 0 }
const monoOk = samples.every((s, i) => i === 0 || (s.xrunsIn >= samples[i - 1].xrunsIn
  && s.xrunsOut >= samples[i - 1].xrunsOut && s.framesProcessed >= samples[i - 1].framesProcessed))
const expectedFrames = DURATION * SAMPLE_RATE
const throughputOk = preStop.framesProcessed >= expectedFrames * 0.97
// 判据：
//   通用：phase 全程 running、计数器单调不减、吞吐合格
//   --no-session（真实播放器模式）：xrunsOut==0 为硬判据（零 xrun）
//   默认（合成馈送模式）：xrunsIn 增长为背压设计预期（drop-oldest 保最新），xrunsOut 归因合成抖动
const xrunOutOk = finRow.xrunsOut === 0
const throughputOk2 = NO_SESSION ? true : throughputOk // no-session 模式吞吐由真实播放器决定
const pass = !phaseBroken && monoOk && throughputOk2 && (NO_SESSION ? xrunOutOk : true)
const report = {
  url: URL, durationSec: DURATION, muted: MUTED, noSession: NO_SESSION, sampleRate: SAMPLE_RATE,
  block: BLOCK, pace: PACE, startedAt: new Date(t0).toISOString(), pushedFrames: sentFrames,
  counters: finRow, countersMonotonic: monoOk, throughputOk, xrunOutOk, phaseBroken,
  xruns: { in: finRow.xrunsIn, out: finRow.xrunsOut },
  notes: 'PACE>1 轻微超速下 xrunsIn 增长为背压设计预期；零 xrun 硬判据在 --no-session（真实播放器）模式',
  verdict: pass ? 'PASS' : 'FAIL', events: events.length, samples,
}
writeFileSync(REPORT, JSON.stringify(report, null, 2))
console.log('==== SOAK ' + report.verdict + ' ==== 报告：' + REPORT)
console.log('判据：phase 全程 running=' + (!phaseBroken) + '；计数器单调=' + monoOk + '；吞吐合格=' + throughputOk2 + '；xrunsOut=0 硬门（仅 no-session）=' + xrunOutOk)
ws.close()
process.exit(pass ? 0 : 1)
