# HyperSoundEngine（独立音频引擎）

[![CI](https://github.com/IceFireIcer/HyperSoundEngine/actions/workflows/ci.yml/badge.svg)](https://github.com/IceFireIcer/HyperSoundEngine/actions/workflows/ci.yml)

HyperSoundEngine 是一个**不依赖任何特定宿主**的独立软件 DSP 音频效果引擎：

- 纯 TypeScript 实现，无运行时第三方依赖（可选增强依赖除外）；
- 同一份 DSP 内核同时用于 **实时播放** 与 **离线导出**；
- 核心零 DOM / AudioContext / React 依赖，可跑在 Node、浏览器、Electron、AudioWorklet；
- 浏览器宿主（`HyperSoundEngineHost`）与 WaveForge 适配层分离，其他软件可直接接入。

> 当前生成代号 **HyperSoundEngine v1**，稳定包版本 **1.1.0**；版本与命名规则见 [docs/VERSIONING.md](docs/VERSIONING.md)。
>
> **1.1.0 状态**：共享规格为 21 份（17 DSP + engine-chain + params + scenes + WAV），72 组音频冻结向量 / 144 文件，另有 3 个参数/场景结构化夹具与 1 个 standard WAV 共享夹具；Rust 对拍 72/72。WAV 编码保留默认 legacy 契约并新增显式 standard RIFF 模式，解码自动识别两者，WaveForge 导出固定使用 standard。Phase 3 已以 Rust 1–21 级完整链收口，服务进程也使用同一完整链；空间音频保持 `spatial.mode='off'` 契约。Phase 4 指标已达标，仅余 8h 真机压测；Phase 5 已完成 wasm 单 Biquad 最小试点，Rust `hrtf-core` 尚未启动。Windows 音频后端仅支持 WASAPI；项目不提供 MIDI 或 ASIO。

## 快速开始

```bash
npm install
npm test              # 全量测试
npm run build         # 产出 dist/（核心 ESM + 类型声明 + worklet 单文件包）
npm run benchmark     # 本地性能基准（48kHz/128 帧默认全链）
npm run benchmark:scenes  # 场景化基准（卷积/FDN 混响、DynamicEq）
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
│   ├── dsp/                  # 20+ 纯 DSP 模块（滤波/动态/混响/调制/变速/分析）
│   ├── engine/               # HyperSoundEngine 引擎总成、场景、分享串、工厂
│   ├── integration/          # 浏览器宿主 HyperSoundEngineHost
│   ├── worklet/              # AudioWorkletProcessor 源码
│   ├── analysis/             # 频谱分析、听力测试
│   ├── io/                   # WAV 编解码
│   ├── offline/              # 声源分离任务队列
│   └── spatial/              # 空间音频参考实现（解析 HRTF + 卷积后端 + 房间模拟）
├── adapters/
│   └── waveforge/            # ★ WaveForge 专属接线（独立于引擎核心）
├── ui/                       # 可选 React 调音室 UI（不参与核心构建）
├── docs/                     # 文档 + adr/（架构决策记录）
├── examples/                 # 独立接入示例
└── test/ + ui/uiSmoke.test.tsx

HyperSoundEngineRust/ —— **Rust 支线**（独立 Cargo workspace，见《原生化双支线与
Windows 音频接入规划书》）：hse-core（17 个 DSP 模块 + `EngineChainStage` 1–21 级主链，
`spatial.mode='off'` 契约）/ hse-parity（72/72）/ hse-wasapi / hse-service / hse-wasm
（单 Biquad 的 wasm32 最小试点）/ hse-napi（占位）；与 TS 支线零代码依赖。
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
- [算法参考文档](docs/ALGORITHMS.md)
- [版本策略与命名规范](docs/VERSIONING.md)
- [架构决策记录（ADR）](docs/adr/)
- [接入其他软件指南](docs/INTEGRATION.md)
- [WaveForge 适配说明](adapters/waveforge/README.md)
- [双支线原生化与 Windows 音频接入规划](原生化双支线与Windows音频接入规划书.md)（TS 支线 + Rust 支线路线图）

## 许可

核心代码自研；算法概念与公式来源见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。
可选依赖 signalsmith-stretch 为 MIT；引擎包零 LGPL 依赖。
