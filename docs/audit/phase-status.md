# 阶段对照与全量验证记录（Phase 0–5）

> 日期：2026-08-30 · 依据：《原生化双支线与Windows音频接入规划书》§五、
> `CHANGELOG.md`、共享规格与本批门禁结果。

## 一、阶段实态

| 阶段 | 状态 | 证据与残留 |
|---|---|---|
| **Phase 0** 规格基建 | 完成 | 18 份规格（17 DSP + 1 engine-chain）；72 组冻结向量 / 144 文件；TS 导出器、Schema 与 Rust parity 门禁齐备 |
| **Phase 1** Rust 核心骨架 | 完成 | `hse-core` Stage 抽象、`hse-parity` 与 criterion 基准 |
| **Phase 2** 服务进程 | 完成 | `hse-wasapi` + `hse-service` + 控制面 + CLI + 推流；真机 GWT 14/14；8h 长跑归入 Phase 4 残留 |
| **Phase 3** 双支线原生化 | **完成** | 17 个 DSP 模块双绿；MIDI/WAV、ShareCodec Rust 解析、服务链完成；`EngineChainStage` 固化 1–21 级组装行为，`cargo run -q -p hse-parity` 为 **72/72 PASS** |
| **Phase 4** 性能冲刺 | 指标完成 | 基准矩阵与 SIMD 评估已留档；全链离线 0.546% realtime、默认链 0.546%、最重场景 10.7%，均达标；**仅余 8h 真机压测** |
| **Phase 5** 可选扩展 | 部分完成 | `hse-wasm` 单 Biquad + 独立 AudioWorklet 最小试点完成；ASIO 与 Rust `hrtf-core` 未启动 |

## 二、0.7.0 全链契约

Rust `EngineChainStage` 对齐 TS HyperSoundEngine 第 1–21 级：响度归一化、Surround3D、M/S、Pre-EQ、Deesser、Compressor、NightMode、五种调制效果、混响、BassEnhancer、LoudnessComp、IEQ、analysis、DynamicEq、LUFS、调制主增益与 Limiter。

第 22 级空间音频不在本批实现范围。`specs/engine/chain.md` 与 5 组 engine-chain 向量都要求 `spatial.mode='off'`；Rust 参数构造对非 off 值直接报错。因此“Phase 3 全链完成”准确含义是 **1–21 级全链完成，空间级以 off 契约封口**，不代表 Rust HRTF 已实现。

共享向量总计：原 17 个 DSP 模块 67 组，加 engine-chain 5 组，共 **72 组 / 144 文件**。

## 三、验证口径

| 门禁 | 0.7.0 口径 |
|---|---|
| npm 版本 | `package.json` 与 package lock 根包均为 0.7.0；锁文件无 Meyda 残留 |
| TS 全量测试 | **50 文件 / 670 用例** |
| 冻结向量 | 72 个 JSON + 72 个 f32 |
| Rust workspace | `hse-benches` / `hse-core` / `hse-parity` / `hse-service` / `hse-wasapi` / `hse-wasm` 共六个包，均解析为 0.7.0 |
| Rust 对拍 | `cargo run -q -p hse-parity`，**72/72 PASS** |
| wasm 试点 | `hse-wasm` native 单测、`hse-core`/`hse-wasm` wasm32 release 构建 |

`hse-wasm` 的公开边界只有 `HseBiquad`：构造时预分配左右 planar 缓冲，宿主以指针读写并调用 `process(frames)` 原位处理。它与现有 TS worklet 隔离，不包含 `EngineChainStage`，不能作为完整引擎 wasm 交付描述。

## 四、下一步

1. 在真实播放器路径执行 8h soak，验证 phase 持续 running、计数器单调、吞吐达标且 `xrunsOut == 0`。
2. ASIO 先完成许可决策，再决定是否实现后端。
3. Rust `hrtf-core` 另行立项，以 TS `src/spatial/` 和空间音频规格为输入；继续遵守渲染循环零分配、性能目标与 1e-6 对拍约束。
4. 若扩展 wasm，先定义完整引擎参数、内存所有权和 AudioWorklet 迁移契约，不从单 Biquad 试点直接推断生产可替换性。
