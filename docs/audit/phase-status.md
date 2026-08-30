# 阶段对照与全量验证记录（Phase 0–5）

> 日期：2026-08-31 · 依据：《原生化双支线与Windows音频接入规划书》§五、
> `CHANGELOG.md`、共享规格与本批门禁结果。

## 一、阶段实态

| 阶段 | 状态 | 证据与残留 |
|---|---|---|
| **Phase 0** 规格基建 | 完成 | 22 份共享规格（17 DSP + engine-chain + params + scenes + WAV + world-listener）；72 组音频冻结向量 / 144 文件、3 个引擎结构夹具、1 个 standard WAV 夹具与 12 个 world-listener case；TS/Rust 门禁齐备 |
| **Phase 1** Rust 核心骨架 | 完成 | `hse-core` Stage 抽象、`hse-parity` 与 criterion 基准 |
| **Phase 2** 服务进程 | 主体完成，出口待验收 | `hse-wasapi` + `hse-service` + 控制面 + CLI + 推流已实现；独立捕获/渲染选路本批落地，VB-CABLE 与正式播放器全链真机路径待验收 |
| **Phase 3** 双支线原生化 | 实现完成，出口待验收 | 17 个 DSP 模块、WAV、ShareCodec、推流协议与 `EngineChainStage` 1–21 级已完成，音频门禁 **72/72 PASS**；双推流客户端 + 非零回环联合验收待完成 |
| **Phase 4** 性能冲刺 | 部分完成 | 离线吞吐、默认链与最重场景 CPU 已达标，稳态零分配门禁已落地；WASAPI 端到端延迟与全链随机参数扫描尚未验证 |
| **Phase 5** 可选扩展 | 部分完成 | `hse-wasm` 单 Biquad 试点；Rust `hrtf-core` world-listener 几何核与空间 12/12 对拍完成，HRIR/卷积/房间渲染未实现 |

## 二、1.3.0 全链、WAV 与 Spatial Slice 1 契约

Rust `EngineChainStage` 对齐 TS HyperSoundEngine 第 1–21 级：响度归一化、Surround3D、M/S、Pre-EQ、Deesser、Compressor、NightMode、五种调制效果、混响、BassEnhancer、LoudnessComp、IEQ、analysis、DynamicEq、LUFS、调制主增益与 Limiter。

WAV I/O 在不改变 1.0.0 legacy 字节契约的前提下新增 standard 小端 RIFF 编码与自动双模式解码；standard 共享夹具由 TS/Rust 共同消费，WaveForge 离线导出使用 standard。

第 22 级空间音频仍不在 Rust 主链内。`hrtf-core` 本批只实现 world-listener position/yaw 几何，并以 12 个结构化 case 与 TS 双绿；HRIR、卷积、房间和 renderer 尚未实现。`specs/engine/chain.md` 与 5 组 engine-chain 向量继续要求 `spatial.mode='off'`。

共享向量总计：原 17 个 DSP 模块 67 组，加 engine-chain 5 组，共 **72 组 / 144 文件**。

## 三、验证口径

| 门禁 | 1.3.0 口径 |
|---|---|
| npm 版本 | `package.json` 与 package lock 根包均为 1.3.0；锁文件无 Meyda 残留 |
| TS 全量测试 | 52 文件 / 682 passed / 4 skipped |
| 冻结音频向量 | 72 个 JSON + 72 个 f32 |
| Spatial fixture | world-listener 12/12 PASS |
| Rust workspace | 七个包，均解析为 1.3.0 |
| Rust 对拍 | `cargo run -q -p hse-parity`：音频 **72/72 PASS** + 空间 **12/12 PASS** |
| wasm 试点 | `hse-wasm` native 单测、`hse-core`/`hse-wasm` wasm32 release 构建 |

`hse-wasm` 的公开边界只有 `HseBiquad`：构造时预分配左右 planar 缓冲，宿主以指针读写并调用 `process(frames)` 原位处理。它与现有 TS worklet 隔离，不包含 `EngineChainStage`，不能作为完整引擎 wasm 交付描述。

## 四、下一步

1. 完成独立捕获/渲染选路的远端 CI 与 VB-CABLE/正式播放器真机验收，并测量 WASAPI 端到端延迟。
2. 扩展 Rust `hrtf-core`：下一步实现 HRIR/插值/卷积 renderer，并继续遵守渲染循环零分配、性能目标与 1e-6 对拍约束。
3. 若扩展 wasm，先定义完整引擎参数、内存所有权和 AudioWorklet 迁移契约，不从单 Biquad 试点直接推断生产可替换性。
