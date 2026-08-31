# HyperSoundEngine —— 接入其他软件指南

本文面向**各类接入方**：游戏引擎、DAW 插件、Web 应用、Node 服务、移动端桥接等。

---

## 1. 接入方式总览

| 场景 | 推荐入口 | 说明 |
|---|---|---|
| Node / 服务端离线处理 | `hypersoundengine` | 纯 DSP，无浏览器依赖 |
| Web 实时处理 | `hypersoundengine/browser` | 自动 worklet / ScriptProcessor |
| Electron / 自定义音频线程 | `hypersoundengine` | 自己把 `process` 接进音频回调 |
| AudioWorklet 处理器 | `hypersoundengine/worklet` + esbuild | 打包成 IIFE 后 `addModule` |
| React 调音室 UI | 仓库 `ui/` | 可选，不属于核心包 |

---

## 2. Node / 任意 JS 运行时接入

```ts
import { createEngine, createDefaultParams } from 'hypersoundengine'

const fs = 48000
const engine = createEngine(fs, 2)
engine.setParams(createDefaultParams(fs))

// 每块处理
function processBlock(inputL: Float32Array, inputR: Float32Array) {
  const outL = new Float32Array(inputL.length)
  const outR = new Float32Array(inputR.length)
  engine.process([inputL, inputR], [outL, outR])
  return { outL, outR }
}
```

要点：
- `process` 是同步、就地写入，适合放进任何音频回调；
- 不要在音频回调里调用 `setParams`，在控制线程调用。

---

## 3. Web 实时接入

```ts
import { createHyperSoundEngineHost } from 'hypersoundengine/browser'
import { createDefaultParams } from 'hypersoundengine'
```

```ts
const host = createHyperSoundEngineHost({
  mode: 'auto',
  workletUrl: '/worklet-bundle.js', // 可先不打包，auto 会回退 ScriptProcessor
})

async function start(ctx: AudioContext, masterGain: GainNode, analyser: AnalyserNode) {
  const params = createDefaultParams(ctx.sampleRate)
  await host.attach({ audioContext: ctx, masterGain, analyser }, params)
}

function update(params: HyperSoundEngineParams) {
  host.setParams(params)
}

function stop() {
  host.dispose()
}
```

---

## 4. 自定义音频宿主接入（游戏引擎 / 原生桥）

如果你的软件有自己的音频回调（例如 Unity、Unreal、C++/Rust FFI、Android Oboe），只需要把 PCM 数据转成 `Float32Array[]` 交给引擎：

```ts
// 伪代码：从你的音频回调取出左右声道
const inputL = new Float32Array(numSamples)
const inputR = new Float32Array(numSamples)
const outputL = new Float32Array(numSamples)
const outputR = new Float32Array(numSamples)

engine.process([inputL, inputR], [outputL, outputR])
// 把 outputL/outputR 交回你的音频输出
```

多通道支持：当前内核为单声道/立体声；如需 5.1/7.1，可在接入层自行拆分或扩展 `HyperSoundEngine`。

---

## 5. AudioWorklet 接入

1. 打包 worklet：

```bash
npm run build:worklet
# 产物 dist/worklet-bundle.js
```

2. 在 AudioContext 中加载：

```ts
await audioContext.audioWorklet.addModule('/path/to/dist/worklet-bundle.js')
```

3. 创建节点并传参：

```ts
const node = new AudioWorkletNode(audioContext, 'hypersoundengine', {
  outputChannelCount: [2],
  processorOptions: {
    inputChannelCount: 2,
    initialParams: params,
    requestId: 'initial-ready',
  },
})
node.port.onmessage = (e) => {
  if (e.data?.type === 'ready') {
    // 节点已用 initialParams 完成构造，可以接入音频图
  } else if (e.data?.type === 'stats') {
    // e.data.stats, e.data.analysis
  }
}
```

运行中的完整参数更新应调用 `HyperSoundEngineHost.setParams(params)`；Host 会预建新节点并在音频时间轴上交叉淡变。不要向 TS worklet 发送 `params` 消息。

---

## 6. 参数准备

推荐始终从 `createDefaultParams(sampleRate)` 派生，再覆盖需要的字段：

```ts
const p = createDefaultParams(48000)
p.eq.enabled = true
p.eq.simpleBands = [2, 0, 1, 0, 2]
p.reverb.enabled = true
p.reverb.mode = 'algorithmic'
p.limiter.thresholdDb = -1
engine.setParams(p)
```

完整字段见 [API.md](API.md)。

---

## 7. 延迟处理

- `engine.getLatencySamples()` 返回当前链延迟；
- 混响卷积模式与限幅器 lookahead 会贡献延迟；
- 需要延迟补偿的宿主应读取该值并做 PDC。

---

## 8. 离线导出

```ts
const engine = createEngine(48000, 2)
engine.setParams(params)

const BLOCK = 4096
// 逐块喂入解码后的 PCM，收集输出
for (let pos = 0; pos < totalFrames; pos += BLOCK) {
  const n = Math.min(BLOCK, totalFrames - pos)
  inL.set(srcL.subarray(pos, pos + n))
  inR.set(srcR.subarray(pos, pos + n))
  if (n < BLOCK) { inL.fill(0, n); inR.fill(0, n) }
  engine.process([inL, inR], [outL, outR])
  // 写入你的 WAV / 文件编码器
}
// 建议尾部再喂 1s 静音冲刷混响/限幅器内部延迟
```

---

## 9. 场景 / 分享串

```ts
import { SCENE_PRESETS, getSceneById, encodeShareCode, decodeShareCode } from 'hypersoundengine'

const scene = getSceneById('pop')
if (scene) engine.setParams(scene.params)

const code = encodeShareCode(engine.getParams?.() ?? createDefaultParams(48000))
```

---

## 10. 常见问题

| 问题 | 处理 |
|---|---|
| 输出全 0 | 确认输入长度 > 0，且 `outputs` 已分配足够长度 |
| 有爆音 | 参数变更不要在音频线程；确认 `outputs.length >= inputs.length` |
| worklet 404 | `workletUrl` 路径与打包产物不一致 |
| 想接 5.1/7.1 | 当前引擎为立体声核心，可在接入层多实例/拆分通道 |
| 需要实时变速变调 | 实时链建议使用外部 SoundTouch 节点（见 HyperSoundEngine 适配层），离线用 `HseStretch` |
