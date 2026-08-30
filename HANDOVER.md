# HANDOVER —— 双线进度交接

> 最后更新：2026-08-30（北京时间）
> 依据：《原生化双支线与Windows音频接入规划书》§五、`docs/audit/phase-status.md`、`CHANGELOG.md` 与本批验证

## 一、当前版本与阶段

当前收尾版本为 **1.2.0**，npm 与 Cargo workspace 版本同步。

| 阶段 | 状态 | 当前结论 |
|---|---|---|
| Phase 0 规格基建 | 完成 | 22 份共享规格（17 DSP + engine-chain + params + scenes + WAV + world-listener），72 组音频冻结向量 / 144 文件，另有 3 个参数/场景夹具、1 个 standard WAV 夹具与 12 个 world-listener case |
| Phase 1 Rust 核心骨架 | 完成 | `hse-core` Stage 抽象、对拍门禁与 criterion 基准 |
| Phase 2 服务进程 | 完成 | `hse-wasapi` + `hse-service` + WebSocket JSON-RPC + 推流协议，真机验收 14/14 |
| Phase 3 双支线原生化 | 完成 | 17 个 DSP 模块及 Rust 1–21 级 `EngineChainStage` 全链对拍，72/72 PASS |
| Phase 4 性能冲刺 | 指标完成 | 离线、默认链、最重场景指标均达标；仅余 8h 真机压测 |
| Phase 5 可选扩展 | 部分完成 | wasm32 单 Biquad 最小试点；`hrtf-core` world-listener 几何核与空间 12/12 对拍完成，HRIR/卷积/房间渲染未实现 |

1.2.0 新增 world-listener 共享规格与 Rust `hrtf-core` 几何核，修复 TS yaw 跨 ±180° 时的方位越界；综合 parity 保持音频 72/72，并新增空间结构化 case 12/12。

## 二、双支线现状

### TS 支线

- npm 包 `hypersoundengine`，22 级处理链；第 22 级为空间音频，默认 `spatial.mode='off'`。
- `src/spatial/` 是解析 HRTF、分区卷积与房间模拟的内联第 22 级参考实现；world-listener position/yaw 几何已进入共享契约。
- 全量测试口径以本批最终实测为准。
- engine-chain 向量只冻结第 1–21 级；所有用例强制 `spatial.mode='off'`，不把空间音频实现纳入本轮 Rust 全链完成声明。

### Rust 支线

- `hse-core`：17 个 DSP 模块，加 `EngineChainStage` 1–21 级主链；分析与 LUFS 级保持状态但不改写音频。
- `hse-parity`：综合门禁当前为音频 **72/72 PASS** + world-listener 空间 case **12/12 PASS**。
- `hse-service`：WASAPI 捕获/渲染、Rust 1–21 级完整 `EngineChainStage`、控制面、二进制 PCM 推流与背压。
- `hse-wasapi`：共享模式渲染与 loopback 捕获；真机出声测试需 `HSE_ALLOW_REAL_AUDIO=1`。
- `hse-wasm`：仅暴露单个 `HseBiquad`，通过预分配 planar 缓冲接入独立 AudioWorklet 示例；不替换 TS worklet，也不代表完整 Rust 全链 wasm 化。
- `hrtf-core`：已实现无分配的 f64 world-listener position/yaw 几何核；HRIR、卷积、房间与 renderer 尚未实现。
- 七个 workspace 包 `hse-benches` / `hse-core` / `hrtf-core` / `hse-parity` / `hse-service` / `hse-wasapi` / `hse-wasm` 均为 1.2.0。

### 共享规格

- 17 份 DSP 规格：biquad、limiter、reverb-simple、compressor、bass-enhancer、mid-side、eq-chain、fdn-reverb、deesser、loudness-comp、dynamic-eq、mod-effects、fft、convolver、modulation-matrix、hse-stretch、lufs-meter。
- 1 份引擎链规格、2 份引擎结构规格、1 份 WAV I/O 规格与 1 份 world-listener 空间规格。
- 72 组冻结音频向量，每组 `.json` + `.f32`，共 144 文件；另有 3 个引擎结构夹具、1 个 standard WAV 夹具与 12 个 world-listener case，既有期望值不得修改。

## 三、性能与剩余项

Phase 4 已达指标：全链离线 0.546% realtime，约为 TS 基线 9.7–10.2 倍；默认链 CPU 0.546%；最重场景 10.7%。Convolver SIMD 按现有评估缓办。

剩余工作：

1. **8h 真机压测**：正式播放器路径验证零 xrun，这是 Phase 4 唯一残留。
2. **Rust `hrtf-core` renderer**：world-listener 几何核已完成；继续实现 HRIR、插值、卷积与房间，并遵守实时零分配与 1e-6 对拍约束。
3. **完整 wasm 引擎链**：当前只有最小 Biquad 试点；扩大边界需另行立项，不能把试点描述为生产替换。
4. `hse-napi` 仍为占位；Rust 服务进程接入文档与双份 integration 文档仍有整理空间。

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
cargo run -q -p hse-parity        # 音频 72/72 + world-listener 12/12
cargo build -p hse-core --target wasm32-unknown-unknown --release --locked
cargo build -p hse-wasm --target wasm32-unknown-unknown --release --locked

# 8h 真机压测
node scripts/soak-8h.mjs --no-session --duration 28800
```

关键纪律：新增冻结向量/结构化 case 必须与两侧驱动实现同时进入同一批次；真机出声测试必须显式设置 `HSE_ALLOW_REAL_AUDIO=1`；Rust HRTF renderer 未实现前，`EngineChainStage` 始终拒绝非 `off` 的 spatial 模式。

## 五、文档索引

- 阶段状态：`docs/audit/phase-status.md`
- Engine chain 规格：`specs/engine/chain.md`
- Phase 4 基准：`docs/audit/phase4-bench-matrix.md`
- Phase 4 SIMD：`docs/audit/phase4-simd-eval.md`
- wasm 试点审计：`docs/audit/wasm-pilot.md`
- Phase 2 真机验收：`docs/audit/service-phase2-acceptance.md`
