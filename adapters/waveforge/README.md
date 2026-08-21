# WaveForge 适配层（独立于引擎核心）

本目录是 **WaveForge 专属接线代码**，不属于独立引擎包 `hypersoundengine`。

## 为什么单独放

- 引擎核心 `src/` 保持纯 TypeScript、零 WaveForge 依赖；
- WaveForge 侧只需要复制/引用本目录的 `attachV3Engine.ts` 即可完成 v3 接入；
- 其他软件接入时不要依赖本目录，直接使用 `src/index.ts`、`src/browser.ts` 或构建产物 `dist/`。

## 文件

| 文件 | 作用 |
|---|---|
| `attachV3Engine.ts` | 把 HyperSoundEngineHost 接进 WaveForge 音频图；参数持久化、UI 桥、SoundTouch 前置链、听力测试、离线 WAV 导出 |

## 依赖

- 引擎核心：本仓库 `src/` 或 `hypersoundengine` npm 包；
- `@soundtouchjs/audio-worklet`：WaveForge 项目已有依赖；
- React UI：本仓库 `ui/`（可选，若不需要调音室 UI 可去掉 UI 桥部分）。

## 使用

将 `attachV3Engine.ts` 复制到 WaveForge 源码目录，然后：

```ts
import { attachV3Engine, detachV3Engine, getV3Bridge } from './attachV3Engine'

await attachV3Engine({ audioContext, masterGain, analyser })
// 调音室渲染
const bridge = getV3Bridge()
bridge.setParams(nextParams)
// 切走
detachV3Engine()
```
