# AGENTS.md — HyperSoundEngine 仓库指引

本目录（`HyperSoundEngine/`）是 DSP 音频引擎的**唯一工作目录**，git 管理；上层 `DSP-Design/` 只是容器，没有其他内容。若会话从上层目录打开，先 `cd HyperSoundEngine`。用户为中文使用者，文档与交付物使用中文；面向用户的研究类产出（.md）应有网络/GitHub 调研支撑。

## 目录总览

- `src/` — **TS 支线**引擎核心（纯 TS DSP + 引擎链 + 浏览器宿主），npm 包 `hypersoundengine`，兼作 golden 对拍参考；`src/spatial/` 为空间音频参考实现（解析 HRTF + 分区卷积后端 + 房间模拟，引擎内联级，Rust hrtf-core 的对拍 ground truth）
- `ui/` — 可选 React 调音室（不参与核心构建）
- `adapters/waveforge/` — WaveForge 专属接线（不入包）
- `test/` — vitest 测试
- `docs/` — 工程文档（API / ARCHITECTURE / VERSIONING…）+ `docs/adr/`（架构决策记录：0001 独立进程形态 / 0002 双音频入口 / 0003 双支线原生化 / 0004 不支持 ASIO）
- `CONTEXT.md` — 领域术语表（ubiquitous language），改模型前先读
- `原生化双支线与Windows音频接入规划书.md` — 当前主线执行规划
- `空间音频实现规划书.md` — 空间音频规格输入（§3.2 契约、§八性能目标有效）
- `specs/` — 双支线共享规格 + 冻结测试向量（总纲 `specs/README.md`、**25 份共享规格：17 DSP + 4 engine + 1 I/O + 3 spatial**、**72 组音频冻结向量 / 144 文件**，另有 4 个引擎结构夹具、40 case 参数扫描、1 个 standard WAV 夹具、14 个 world-listener case 与 14 个 renderer/ABI case；Rust 综合门禁为音频 **72/72 PASS** + 空间 **28/28 PASS**，参数扫描结构摘要 **40/40 PASS**；engine-chain 音频向量仍固化 1–21 级并要求 `spatial.mode='off'`）；服务层规格 `specs/service/`：control-plane 控制面契约、push-stream 推流协议。**改动前先读 `specs/README.md`**；音频与空间基线分别由 `scripts/export-vectors.mjs`、`scripts/export-spatial-vectors.mjs` 幂等校验，既有期望不一致时拒写
- `HyperSoundEngineRust/` — **Rust 支线**：`hse-core` 已完成 17 个 DSP 模块与 `EngineChainStage` 1–22 级完整链，空间支持 `instant`/`headLocked`/`world`/`stage`；`hrtf-core` 已实现完整欧拉 world-listener、规则 grid、SOFA 解析、44.1/48/96 kHz 重采样、nearest/spherical、time/partitioned、距离/空气、Doppler、遮挡、声源大小、房间与稳定 slot；`hse-parity` 门禁为音频 **72/72 PASS** + 空间 **28/28 PASS**；`hse-wasapi`/`hse-service` 支持 shared/exclusive、事件等待、排队延迟统计与真机验收工具；`hse-wasm` 提供完整 1–22 级 `HseEngine`、正式 Host 可选接入及空间 8 函数 C ABI。真实设备 shared/exclusive 延迟/CPU、真实 SOFA 资产自动门禁、Firefox AudioWorklet E2E 与物理 multichannel 输出仍待完成。Windows 后端固定为 WASAPI，不提供 MIDI 或 ASIO

> 规则：README/AGENTS 等仓库文档只描述已跟踪文件，**不得引用 .gitignore 排除的路径**（本地参考资料、草稿目录等不入文档）。

## 远程与 CI

- 远程仓库：`https://github.com/IceFireIcer/HyperSoundEngine`（origin），工作分支 `main`
- push 到 `main` 触发 `.github/workflows/ci.yml` 的三个作业：`test` 执行许可、类型、frame-count、参数扫描、Vitest、契约导出与构建；`rust` 执行 rustfmt、Clippy、参数扫描、release 零分配、服务管线、benchmark 编译、wasm/Chromium E2E、workspace 测试与综合对拍；`rust-windows-silent` 执行 Windows 无设备 core/WASAPI/service/parity 门禁。三项全绿才算自动门禁通过
- 每日北京时间 17:00（cron 为 UTC 09:00）触发 `.github/workflows/nightly.yml`：质量门禁 → TS/Rust 构建 → 发 pre-release。tag 命名 `nightly-YYYYMMDD.当日构建次数`（重跑自动 +1）；**包版本号不随 nightly 递增**（版本规则见 docs/VERSIONING.md）；Rust 支线目录不存在时自动跳过

## 常用命令（工作目录 = 本目录）

```bash
npm test                        # vitest 全量测试
npx vitest run test/xxx.test.ts # 单个测试文件
npm run typecheck               # 核心 tsc --noEmit
npm run typecheck:ui            # ui/ 的独立类型检查（tsconfig.ui.json）
npm run build                   # types + core(esbuild) + worklet 单文件包 → dist/
npm run benchmark               # 先 build 再跑 scripts/benchmark.mjs（48kHz/128 帧，默认链）
npm run benchmark:scenes        # 场景化基准（卷积/FDN 混响、DynamicEq）
node scripts/export-vectors.mjs # 导出/校验冻结对拍向量（幂等；不一致拒写，防单方面改基线）
cd HyperSoundEngineRust && cargo test            # Rust 支线单元测试（workspace 全成员）
cd HyperSoundEngineRust && cargo run -q -p hse-parity  # 综合门禁：音频 72/72 + 空间 28/28
cd HyperSoundEngineRust && cargo bench           # criterion 基准（biquad/limiter/reverb + 块长矩阵）
cd HyperSoundEngineRust && cargo test -p hse-service  # 引擎服务单测+集成（fake 后端，无需真实音频设备）
cd HyperSoundEngineRust && cargo run -p hse-service   # 引擎服务（ws://127.0.0.1:4780）；hse-cli 调参见 crates/hse-service/README.md
```

依赖未安装时先 `npm install`。平台为 Windows + Git Bash。

## 双支线铁律（ADR-0003）

- 两支线行为由 `specs/` 共享规格定义，**规格先行双实现**；对拍相对容差 1e-6，跨实现不要求逐位一致
- 兼容契约三层不得单方面破坏：`AudioEngine` 接口语义、参数模型/场景预设/分享串格式、引擎服务进程控制协议（WebSocket JSON-RPC）
- TS 支线与 Rust 支线各自内部保持确定性（无随机/时钟/控制台输出）与稳态零分配

## 版本与命名铁律（docs/VERSIONING.md）

- 生成代号 **HyperSoundEngine vN ↔ semver MAJOR N**；当前线 HyperSoundEngine v1（包版本 0.x 为其预发布）。旧 WaveForge v1/v2/v3 引擎谱系已废止，不得再引入其描述。
- MAJOR=破坏兼容契约三层；MINOR=新功能/新向量；PATCH=行为不变修复。bump 与 CHANGELOG 更新由实施变更的会话按 `docs/VERSIONING.md` 规则自动完成，不需用户手动管理。
- 标识符/存储键/事件名/CSS 动画/worklet URL 一律无版本前缀或 `hse-` 前缀；禁止 `v1/v2/v3` 字样（第三方名称如 GPLv3、Freeverb3 除外）。
- 已冻结对拍向量的期望值永不修改；行为变更=新增向量或走 MAJOR。
- 固定时长测试已删除且不得恢复；异步/实时流程以事件、帧数、块序号或显式超时上限收敛，不以“运行 N 秒”作为成功条件。

## 架构铁律（改代码前必读 `docs/ARCHITECTURE.md`）

分层（自上而下，依赖只能向下）：

1. **宿主层** `src/browser.ts`、`src/integration/HyperSoundEngineHost.ts`、`src/worklet/`
2. **引擎核心** `src/engine/`（HyperSoundEngine 22 级处理链、ScenePresets、ShareCodec、工厂）
3. **DSP 内核** `src/dsp/`（fft/biquad/EqChain/Compressor/Convolver/Reverb 等）

- **核心零 DOM / AudioContext / React 依赖**，须能在 Node、浏览器、Electron、AudioWorklet 运行；`ui/` 与 `adapters/waveforge/` 不参与核心构建
- **实时安全**：音频回调内零分配、零锁、零系统调用；缓冲须先经 `prepare(maxBlockSize)` 预分配
- **确定性**：核心内禁用随机、Date、console；同输入同参数同输出
- **双路径一致**：实时播放与离线导出必须走同一个 `HyperSoundEngine.process`
- **对外唯一接缝是 `AudioEngine` 接口**；DSP 模块走 `StereoProcessor` 形态（setParams/processStereo/reset）；处理链阶段实现 `ProcessingStage`
- **参数快照语义**：`setParams` 整体替换，`getParams` 返回深拷贝
- 许可：核心 CC-BY-NC-ND-4.0；引擎包零 LGPL 依赖（WaveForge 适配层的 @soundtouchjs/audio-worklet 为宿主侧 LGPL-2.1 依赖，不属引擎包）

## 测试与 UI 约定

- 测试在 `test/`（含 `audit-*` 链路审计、`performance-smoke` 性能冒烟）；UI 冒烟在 `ui/uiSmoke.test.tsx`
- vitest 全局 esbuild JSX=automatic；jsdom 由测试文件头 `@vitest-environment jsdom` 注释按文件启用，勿全局开启
- `hypersoundengine/worklet` 子路径不可在 Node 直接 import

## 空间音频工作约束（已被 ADR-0003 收编）

原《空间音频实现规划书.md》的 Rust HRTF 核方案**并入 Rust 支线**实现；TS 侧"兄弟 Worklet 节点"（`attachEngine.ts` 加 `syncSpatialChain`）方案作废。规划书中的契约函数（§3.2）、性能目标（§八：<5ms 渲染延迟、32-64 对象、<25% 单核、渲染循环零分配）与数值对拍要求（容差 1e-6）继续有效，作为 Rust 支线空间音频模块的规格输入。

## 改动前应读文档

- `docs/ARCHITECTURE.md` — 分层与关键设计决策
- `docs/API.md` / `INTEGRATION.md` — 对外接口与接入约定
- `src/dsp/API_SPEC.md` — DSP 模块规格
- `docs/GAP_ANALYSIS.md`、`docs/audit/` — 已知差距与审计结论
- `CHANGELOG.md` — 已完成功能清单（判断"是否已有"先查这里）
