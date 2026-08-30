# HANDOVER —— 双线进度交接

> 最后更新：2026-08-30（北京时间）
> 依据：《原生化双支线与Windows音频接入规划书》§五、`docs/audit/phase-status.md`、`CHANGELOG.md` 与本批验证

## 一、当前版本与阶段

当前收尾版本为 **0.7.0**，npm 与 Cargo workspace 版本同步。

| 阶段 | 状态 | 当前结论 |
|---|---|---|
| Phase 0 规格基建 | 完成 | 18 份共享规格（17 DSP + 1 engine-chain），72 组冻结向量 / 144 文件 |
| Phase 1 Rust 核心骨架 | 完成 | `hse-core` Stage 抽象、对拍门禁与 criterion 基准 |
| Phase 2 服务进程 | 完成 | `hse-wasapi` + `hse-service` + WebSocket JSON-RPC + 推流协议，真机验收 14/14 |
| Phase 3 双支线原生化 | 完成 | 17 个 DSP 模块及 Rust 1–21 级 `EngineChainStage` 全链对拍，72/72 PASS |
| Phase 4 性能冲刺 | 指标完成 | 离线、默认链、最重场景指标均达标；仅余 8h 真机压测 |
| Phase 5 可选扩展 | 部分完成 | wasm32 单 Biquad 最小试点完成；ASIO 与 Rust `hrtf-core` 未启动 |

本次最新未提交批次包含：0.7.0 版本同步、5 组 engine-chain 冻结向量及 Rust 1–21 级全链实现、`hse-wasm` 最小试点、相关 CI/测试与文档收尾。工作树包含该批次改动，交接时不得假定工作树干净，也不得以旧提交号替代当前未提交内容。

## 二、双支线现状

### TS 支线

- npm 包 `hypersoundengine`，22 级处理链；第 22 级为空间音频，默认 `spatial.mode='off'`。
- `src/spatial/` 是解析 HRTF、分区卷积与房间模拟的参考实现，也是未来 Rust `hrtf-core` 的对拍事实来源。
- 当前全量测试口径：**50 文件 / 670 用例**。
- engine-chain 向量只冻结第 1–21 级；所有用例强制 `spatial.mode='off'`，不把空间音频实现纳入本轮 Rust 全链完成声明。

### Rust 支线

- `hse-core`：17 个 DSP 模块，加 `EngineChainStage` 1–21 级主链；分析与 LUFS 级保持状态但不改写音频。
- `hse-parity`：读取同一批冻结夹具，当前门禁 **72/72 PASS**。
- `hse-service`：WASAPI 捕获/渲染、全模块服务链、控制面、二进制 PCM 推流与背压。
- `hse-wasapi`：共享模式渲染与 loopback 捕获；真机出声测试需 `HSE_ALLOW_REAL_AUDIO=1`。
- `hse-wasm`：仅暴露单个 `HseBiquad`，通过预分配 planar 缓冲接入独立 AudioWorklet 示例；不替换 TS worklet，也不代表完整 Rust 全链 wasm 化。
- 六个 workspace 包 `hse-benches` / `hse-core` / `hse-parity` / `hse-service` / `hse-wasapi` / `hse-wasm` 均为 0.7.0。

### 共享规格

- 17 份 DSP 规格：biquad、limiter、reverb-simple、compressor、bass-enhancer、mid-side、eq-chain、fdn-reverb、deesser、loudness-comp、dynamic-eq、mod-effects、fft、convolver、modulation-matrix、hse-stretch、lufs-meter。
- 1 份引擎链规格：`specs/engine/chain.md`。
- 72 组冻结向量，每组 `.json` + `.f32`，共 144 文件；既有期望值不得修改。

## 三、性能与剩余项

Phase 4 已达指标：全链离线 0.546% realtime，约为 TS 基线 9.7–10.2 倍；默认链 CPU 0.546%；最重场景 10.7%。Convolver SIMD 按现有评估缓办。

剩余工作：

1. **8h 真机压测**：正式播放器路径验证零 xrun，这是 Phase 4 唯一残留。
2. **ASIO**：尚未启动，先处理 GPLv3 与 Steinberg 专有许可决策。
3. **Rust `hrtf-core`**：尚未启动；必须遵守空间音频规格、实时零分配与 1e-6 对拍约束。
4. **完整 wasm 引擎链**：当前只有最小 Biquad 试点；扩大边界需另行立项，不能把试点描述为生产替换。
5. `hse-napi` 仍为占位；Rust 服务进程接入文档与双份 integration 文档仍有整理空间。

## 四、常用门禁

```bash
# TS 支线
npm run typecheck
npm run typecheck:ui
npm test
npm run build
node scripts/export-vectors.mjs

# Rust 支线
cd HyperSoundEngineRust
cargo check --workspace
cargo test --workspace --locked
cargo run -q -p hse-parity        # 72/72
cargo build -p hse-core --target wasm32-unknown-unknown --release --locked
cargo build -p hse-wasm --target wasm32-unknown-unknown --release --locked

# 8h 真机压测
node scripts/soak-8h.mjs --no-session --duration 28800
```

关键纪律：新增冻结向量必须与两侧驱动实现同时进入同一批次；真机出声测试必须显式设置 `HSE_ALLOW_REAL_AUDIO=1`；空间音频未实现前，Rust engine-chain 始终拒绝非 `off` 的 spatial 模式。

## 五、文档索引

- 阶段状态：`docs/audit/phase-status.md`
- Engine chain 规格：`specs/engine/chain.md`
- Phase 4 基准：`docs/audit/phase4-bench-matrix.md`
- Phase 4 SIMD：`docs/audit/phase4-simd-eval.md`
- wasm 试点审计：`docs/audit/wasm-pilot.md`
- Phase 2 真机验收：`docs/audit/service-phase2-acceptance.md`
