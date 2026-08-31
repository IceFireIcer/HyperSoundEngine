# HyperSoundEngine（独立音频引擎）

[![CI](https://github.com/IceFireIcer/HyperSoundEngine/actions/workflows/ci.yml/badge.svg)](https://github.com/IceFireIcer/HyperSoundEngine/actions/workflows/ci.yml)

HyperSoundEngine 是一个**不依赖任何特定宿主**的独立软件 DSP 音频效果引擎：

- 纯 TypeScript 实现，无运行时第三方依赖（可选增强依赖除外）；
- 同一份 DSP 内核同时用于 **实时播放** 与 **离线导出**；
- 核心零 DOM / AudioContext / React 依赖，可跑在 Node、浏览器、Electron、AudioWorklet；
- 浏览器宿主（`HyperSoundEngineHost`）与 WaveForge 适配层分离，其他软件可直接接入。

> 当前生成代号 **HyperSoundEngine v1**，稳定包版本 **1.5.1**；版本与命名规则见 [docs/VERSIONING.md](docs/VERSIONING.md)。
>
> **1.5.1 状态**：共享规格为 25 份（17 DSP + 4 engine + 1 I/O + 3 spatial），72 组音频冻结向量 / 144 文件，另有 4 个引擎结构夹具、40 case 参数扫描、1 个 standard WAV 夹具、14 个 world-listener case 与 14 个 renderer/ABI case；综合 Rust 门禁为音频 72/72 + 空间 28/28，参数扫描结构摘要为 40/40。Rust 第 22 级已对等 `instant`/`headLocked`/`world`/`stage`，含完整 listener 姿态、轨迹与确定速度、遮挡、声源大小、stage seat/room/ambience 和稳定 slot；正式 wasm AudioWorklet 的 headless Chromium E2E 已纳入 CI，并用预解析合成 HRTF grid 覆盖成功的非 `off` stage 22 渲染。真实 SOFA 资产兼容性、Firefox E2E 与物理 multichannel 输出仍未自动验证；真实设备 shared/exclusive 端到端延迟与 CPU 仍待用户验收。Windows 音频后端仅支持 WASAPI；项目不提供 MIDI 或 ASIO。

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
import { createDefaultParams } from 'hypersoundengine'
import { createHyperSoundEngineHost } from 'hypersoundengine/browser'

const params = createDefaultParams(audioContext.sampleRate)
const host = createHyperSoundEngineHost({
  mode: 'auto',               // worklet 优先，失败回退 ScriptProcessor
  workletUrl: '/worklet-bundle.js',
})
await host.attach({ audioContext, masterGain, analyser }, params)

const nextParams = structuredClone(params)
nextParams.eq.simpleBands[0] = 3
await host.setParams(nextParams)
host.dispose()
```

## 接入其他项目

从 [接入指南](docs/INTEGRATION.md) 开始。该文档按使用场景明确区分：

- TypeScript core：Node/Electron 离线处理或自有音频回调；
- 浏览器 Host：TS AudioWorklet 或完整 Rust/WASM 1–22 级引擎；
- Rust `hse-service`：其他语言通过 localhost WebSocket JSON-RPC + 立体声 f32 PCM 接入，并输出到 WASAPI 设备；
- 空间 C ABI：仅供原生程序调用双耳 renderer，不等于完整 HSE API。

参考实现见 [`examples/node-offline.mjs`](examples/node-offline.mjs)、[`examples/browser-host.mjs`](examples/browser-host.mjs) 和可导入、无副作用的 [`examples/hse-service-client.mjs`](examples/hse-service-client.mjs)。完整 TS 类型参考见 [API 文档](docs/API.md)，服务 wire 契约见 [control-plane](specs/service/control-plane.md) 与 [push-stream](specs/service/push-stream.md)。

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
Windows 音频接入规划书》）：hse-core（17 个 DSP 模块 + `EngineChainStage` 1–22 级完整链，空间含
`instant`/`headLocked`/`world`/`stage`）/ hrtf-core（完整 renderer 能力）/ hse-parity（音频 72/72 + 空间 28/28）/ hse-wasapi / hse-service / hse-wasm
（完整 1–22 级 `HseEngine`、正式 Host 接入与空间 8 函数薄 ABI）/ hse-napi（占位）；与 TS 支线零代码依赖。
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
- [正式发布就绪度与各渠道阻断项](docs/RELEASE_READINESS.md)
- [版本策略与命名规范](docs/VERSIONING.md)
- [架构决策记录（ADR）](docs/adr/)
- [接入其他软件指南](docs/INTEGRATION.md)
- [WaveForge 适配说明](adapters/waveforge/README.md)
- [双支线原生化与 Windows 音频接入规划](原生化双支线与Windows音频接入规划书.md)（TS 支线 + Rust 支线路线图）

## 许可

本项目主体代码采用 [Creative Commons Attribution-NonCommercial-NoDerivatives 4.0 International](LICENSE)（**CC BY-NC-ND 4.0**）许可：使用和分享时必须署名，仅限非商业用途，不得分发修改后的版本。具体权利与限制以仓库 `LICENSE` 原文为准。

第三方依赖和算法来源继续适用各自的许可证，详见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。可选依赖 `signalsmith-stretch` 为 MIT；引擎包零 LGPL 依赖。
