# AGENTS.md — HyperSoundEngine 仓库指引

本目录（`HyperSoundEngine/`）是 DSP 音频引擎的**唯一工作目录**，git 管理；上层 `DSP-Design/` 只是容器，没有其他内容。若会话从上层目录打开，先 `cd HyperSoundEngine`。用户为中文使用者，文档与交付物使用中文；面向用户的研究类产出（.md）应有网络/GitHub 调研支撑。

## 目录总览

- `src/` — **TS 支线**引擎核心（纯 TS DSP + 引擎链 + 浏览器宿主），npm 包 `hypersoundengine`，兼作 golden 对拍参考
- `ui/` — 可选 React 调音室（不参与核心构建）
- `adapters/waveforge/` — WaveForge 专属接线（不入包）
- `test/` — vitest 测试
- `docs/` — 工程文档（API / ARCHITECTURE / INTEGRATION…）+ `docs/adr/`（架构决策记录：0001 独立进程形态 / 0002 双音频入口 / 0003 双支线原生化）
- `CONTEXT.md` — 领域术语表（ubiquitous language），改模型前先读
- `原生化双支线与Windows音频接入规划书.md` — 当前主线执行规划
- `空间音频实现规划书.md` — 空间音频规格输入（§3.2 契约、§八性能目标有效）
- `specs/` — 双支线共享规格 + 测试向量（建设中，见规划书 Phase 0）
- `HyperSoundEngineRust/` — **Rust 支线**（规划中）：全量原生重写，承接 Windows 引擎服务进程与性能目标
- `referencesDocs/` — 各模型调研参考（**独立 git 仓库**，已被 .gitignore 排除）
- `.scratch/`、`.hse-bench/` — 规划草稿与基准脚手架（gitignored）

## 常用命令（工作目录 = 本目录）

```bash
npm test                        # vitest 全量测试
npx vitest run test/xxx.test.ts # 单个测试文件
npm run typecheck               # 核心 tsc --noEmit
npm run typecheck:ui            # ui/ 的独立类型检查（tsconfig.ui.json）
npm run build                   # types + core(esbuild) + worklet 单文件包 → dist/
npm run benchmark               # 先 build 再跑 scripts/benchmark.mjs（48kHz/128 帧）
```

依赖未安装时先 `npm install`。平台为 Windows + Git Bash。

## 双支线铁律（ADR-0003）

- 两支线行为由 `specs/` 共享规格定义，**规格先行双实现**；对拍相对容差 1e-6，跨实现不要求逐位一致
- 兼容契约三层不得单方面破坏：`AudioEngine` 接口语义、参数模型/场景预设/分享串格式、引擎服务进程控制协议（WebSocket JSON-RPC）
- TS 支线与 Rust 支线各自内部保持确定性（无随机/时钟/控制台输出）与稳态零分配

## 架构铁律（改代码前必读 `docs/ARCHITECTURE.md`）

分层（自上而下，依赖只能向下）：

1. **宿主层** `src/browser.ts`、`src/integration/HyperSoundEngineHost.ts`、`src/worklet/`
2. **引擎核心** `src/engine/`（HyperSoundEngine 21 级处理链、ScenePresets、ShareCodec、工厂）
3. **DSP 内核** `src/dsp/`（fft/biquad/EqChain/Compressor/Convolver/Reverb 等）

- **核心零 DOM / AudioContext / React 依赖**，须能在 Node、浏览器、Electron、AudioWorklet 运行；`ui/` 与 `adapters/waveforge/` 不参与核心构建
- **实时安全**：音频回调内零分配、零锁、零系统调用；缓冲须先经 `prepare(maxBlockSize)` 预分配
- **确定性**：核心内禁用随机、Date、console；同输入同参数同输出
- **双路径一致**：实时播放与离线导出必须走同一个 `HyperSoundEngine.process`
- **对外唯一接缝是 `AudioEngine` 接口**；DSP 模块走 `StereoProcessor` 形态（setParams/processStereo/reset）；处理链阶段实现 `ProcessingStage`
- **参数快照语义**：`setParams` 整体替换，`getParams` 返回深拷贝
- 许可：核心 CC-BY-NC-ND-4.0；soundtouchjs 为 LGPL-2.1，只能"不修改源码 + 链接调用"（vendor/ 存原包副本）

## 测试与 UI 约定

- 测试在 `test/`（含 `audit-*` 链路审计、`performance-smoke` 性能冒烟）；UI 冒烟在 `ui/uiSmoke.test.tsx`
- vitest 全局 esbuild JSX=automatic；jsdom 由测试文件头 `@vitest-environment jsdom` 注释按文件启用，勿全局开启
- `hypersoundengine/worklet` 子路径不可在 Node 直接 import

## 空间音频工作约束（已被 ADR-0003 收编）

原《空间音频实现规划书.md》的 Rust HRTF 核方案**并入 Rust 支线**实现；TS 侧"兄弟 Worklet 节点"（`attachV3Engine.ts` 加 `syncSpatialChain`）方案作废。规划书中的契约函数（§3.2）、性能目标（§八：<5ms 渲染延迟、32-64 对象、<25% 单核、渲染循环零分配）与数值对拍要求（容差 1e-6）继续有效，作为 Rust 支线空间音频模块的规格输入。

## 改动前应读文档

- `docs/ARCHITECTURE.md` — 分层与关键设计决策
- `docs/API.md` / `INTEGRATION.md` — 对外接口与接入约定
- `src/dsp/API_SPEC.md` — DSP 模块规格
- `docs/GAP_ANALYSIS.md`、`docs/audit/` — 已知差距与审计结论
- `CHANGELOG.md` — 已完成功能清单（判断"是否已有"先查这里）
