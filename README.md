# HyperSoundEngine（独立音频引擎）

HyperSoundEngine 是一个**不依赖任何特定宿主**的独立软件 DSP 音频效果引擎：

- 纯 TypeScript 实现，无运行时第三方依赖（可选增强依赖除外）；
- 同一份 DSP 内核同时用于 **实时播放** 与 **离线导出**；
- 核心零 DOM / AudioContext / React 依赖，可跑在 Node、浏览器、Electron、AudioWorklet；
- 浏览器宿主（`HyperSoundEngineHost`）与 WaveForge 适配层分离，其他软件可直接接入。

## 快速开始

```bash
cd HyperSoundEngine
npm install
npm test          # 全量测试
npm run build     # 产出 dist/（核心 ESM + 类型声明 + worklet 单文件包）
npm run benchmark # 本地性能基准（48kHz/128 帧默认全链）
```

### Node / 任意 JS 运行时（纯离线处理）

```ts
import { createEngine, createDefaultParams } from 'hypersoundengine'

const fs = 48000
const engine = createEngine(fs, 2)
const params = createDefaultParams(fs)
engine.setParams(params)

const inL = new Float32Array(4800) // 0.1s 输入
const inR = new Float32Array(4800)
const outL = new Float32Array(4800)
const outR = new Float32Array(4800)
engine.process([inL, inR], [outL, outR])
```

### 浏览器实时接入

```ts
import { createHyperSoundEngineHost } from 'hypersoundengine/browser'

const host = createHyperSoundEngineHost({
  mode: 'auto',               // worklet 优先，失败回退 ScriptProcessor
  workletUrl: '/worklet-bundle.js',
})
await host.attach({ audioContext, masterGain, analyser }, params)
host.setParams(nextParams)
host.dispose()
```

## 目录结构

```
HyperSoundEngine/
├── src/
│   ├── index.ts              # 核心入口（纯 DSP 引擎，无浏览器/UI 依赖）
│   ├── browser.ts            # 浏览器宿主入口（HyperSoundEngineHost）
│   ├── worklet.ts            # AudioWorklet 打包入口
│   ├── interfaces.ts         # 对外统一接口（AudioEngine / StereoProcessor）
│   ├── types.ts              # 参数模型与默认值
│   ├── dsp/                  # 16 个纯 DSP 模块
│   ├── engine/               # HyperSoundEngine 引擎总成、场景、分享串、工厂
│   ├── integration/          # 浏览器宿主 HyperSoundEngineHost
│   ├── worklet/              # AudioWorkletProcessor 源码
│   ├── analysis/             # 频谱分析、听力测试
│   └── offline/              # 声源分离任务队列
├── adapters/
│   └── waveforge/            # ★ WaveForge 专属接线（独立于引擎核心）
├── ui/                       # 可选 React 调音室 UI（不参与核心构建）
├── docs/
│   ├── API.md                # 对外接口文档
│   ├── ARCHITECTURE.md       # 架构说明
│   └── INTEGRATION.md        # 接入其他软件指南
├── examples/                 # 独立接入示例
├── test/ + ui/uiSmoke.test.tsx
└── vendor/soundtouchjs/      # LGPL-2.1 原包副本（可选变速变调路径）
```

## 子路径导入

| 导入路径 | 内容 |
|---|---|
| `hypersoundengine` | 核心引擎（HyperSoundEngine、DSP、场景、分享串、分析、离线） |
| `hypersoundengine/browser` | 浏览器宿主 HyperSoundEngineHost / createHyperSoundEngineHost |
| `hypersoundengine/worklet` | AudioWorklet 处理器打包入口（不可在 Node 直接 import） |

## 文档

- [接口文档](docs/API.md)
- [架构说明](docs/ARCHITECTURE.md)
- [接入其他软件指南](docs/INTEGRATION.md)
- [WaveForge 适配说明](adapters/waveforge/README.md)

## 许可

核心代码自研；算法概念与公式来源见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。
可选依赖 soundtouchjs 为 LGPL-2.1，以“不修改源码、链接调用”方式使用。
