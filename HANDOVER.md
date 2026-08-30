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

1.2.0 新增 world-listener 共享规格与 Rust `hrtf-core` 几何核，修复 TS yaw 跨 ±180° 时的方位越界；综合 parity 保持音频 72/72，并新增空间结构化 case 12/12。对应功能基线提交为 `c7569fb`，已推送至 `origin/main`。

### 远端 CI 状态

GitHub Actions run `33317111024` 已完成：

- `test`（Linux TS）：通过，包含 typecheck、UI typecheck、全量测试、engine contract、spatial world-listener contract 与 build。
- `rust-windows-silent`：通过，包含 Windows `hse-core`、`hse-service` 与综合 parity。
- `rust`（Linux）：失败。失败点仅为 `hse-service/tests/push_stream.rs` 的 `混后处理_双会话求和进渲染_无串扰_无丢失`，断言“受控速率下不应发生丢弃”；此前格式、Clippy、全部 benchmark 编译和 wasm32 构建均已通过，但后续 parity 步骤因该测试失败被跳过。
- 当前远端主线因此**不是全绿**；Linux push-stream 时序稳定性需要修复后重新推送验证。

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

## 三、未完成工作与建议顺序

1. **修复 Linux CI 的 push-stream 时序失败**：`hse-service/tests/push_stream.rs` 中双会话混后处理测试在 Linux 调度下偶发记录丢弃；需要把等待条件覆盖到两个会话的末帧均被消费，再重跑远端 CI。
2. **TS spatial 实时安全 Slice 2**：尚未提交。需要把 `prepare(maxBlockSize)` 贯穿 `SpatialBackend`、`TsConvolverBackend`、`Convolver`、`TimeConvolver` 与 `RoomSim`，显式传递有效 `frameCount`，移除热路径 closure/subarray，并把 IR 构建移到控制路径。此前实验性改动已撤回，当前仓库没有半成品。
3. **Rust `hrtf-core` renderer**：world-listener 几何核已完成；尚需 HRIR 数据、插值、双耳卷积、距离/空气吸收、房间与零分配 renderer，再接入 Rust 主链第 22 级。
4. **TS ambience 主链接线**：FOA 数学与增益计算已有，但 `ambience.enabled/amount` 仍未进入 `HyperSoundEngine` 的实际音频处理路径。
5. **空间多声道端到端接线**：`TsConvolverBackend.processMulti()` 已有叶子实现和单测，但 `HyperSoundEngine`、AudioWorklet 与 Host 仍固定立体声，5.1/7.1 空间输入尚不可从产品路径使用。
6. **完整 wasm 引擎链**：当前 `hse-wasm` 只有单 Biquad 试点，尚未承载 1–21 级主链或 Rust 空间渲染。
7. **`hse-napi`**：仍为占位，尚无可供 Node/Electron 进程内加载的原生 crate。
8. **外部模型注入式 2-stem ONNX 分离**：Rust 实现及端到端接入尚未完成。
9. **真机验收**：正式播放器/VB-CABLE 拓扑、8 小时 `xrunsOut == 0` soak 与 WASAPI 端到端延迟仍未完成。按用户要求，不得自动运行长时间可听测试。

## 四、已知未修复问题

- **远端 Linux CI 非全绿**：run `33317111024` 的 Rust workspace 测试因 push-stream 双会话时序断言失败；Windows Rust 和 TS job 已通过。
- **空间实时安全尚未兑现完整承诺**：TS spatial 在 `prepare()` 后仍可能首块扩容、创建 `subarray`/closure，并在 IR 淡出完成时于渲染路径调用 `loadIR()`；AudioWorklet 回调本身也仍会创建输入/输出数组及统计消息对象。
- **空间公开参数有未接线项**：`ambience.enabled/amount` 当前不改变主链输出；listener 的 pitch/roll 未参与坐标变换；世界模式多普勒速度仍固定为零；`stage.roomSize` 未真正改变 RoomSim 房间尺寸。
- **距离模型边界**：linear 模型在 `distance < refDistance` 时可能产生大于 1 的增益，尚未修复。
- **原生空间能力不完整**：Rust `EngineChainStage` 仍拒绝非 `off` 的 spatial 参数，不能将 12/12 几何对拍描述为完整 Rust HRTF。
- **外部发布边界**：代码已推送至 GitHub `main`，但没有证据表明 npm/crates.io 已发布对应包；其他项目可使用 Git 依赖、源码构建或本地包，不能假定注册表已有 1.2.0。
- **许可边界**：仓库采用 `CC-BY-NC-ND-4.0`；商业集成、修改和再分发前必须单独确认许可是否满足目标项目。

## 五、当前可用接入边界

- **Node / 浏览器 / Electron 的 TS 核心**：可用；通过 `hypersoundengine`、`hypersoundengine/browser`、`hypersoundengine/worklet` 接入。TS 第 22 级空间参考实现可用，但上述实时安全与部分参数接线问题仍存在。
- **Windows 独立服务**：可用；`hse-service` 提供 localhost WebSocket JSON-RPC、二进制立体声 `f32le` 推流、WASAPI loopback 捕获与共享模式渲染。服务链当前为 Rust 第 1–21 级，不含 spatial。
- **Rust crate 源码集成**：`hse-core` 可作为源码/path dependency 使用；对外稳定的 C ABI 和 N-API 尚未提供。
- **浏览器 Rust/WASM**：仅单 Biquad 试点，不可作为完整引擎替换。
- **明确不支持**：MIDI 与 ASIO。

## 六、性能现状

Phase 4 已达指标：全链离线 0.546% realtime，约为 TS 基线 9.7–10.2 倍；默认链 CPU 0.546%；最重场景 10.7%。Convolver SIMD 按现有评估缓办。这些是离线 DSP 数据，不包含完整 WASAPI 调度成本，不能替代端到端延迟与长稳测试。

## 七、常用门禁

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

## 八、文档索引

- 阶段状态：`docs/audit/phase-status.md`
- Engine chain 规格：`specs/engine/chain.md`
- Phase 4 基准：`docs/audit/phase4-bench-matrix.md`
- Phase 4 SIMD：`docs/audit/phase4-simd-eval.md`
- wasm 试点审计：`docs/audit/wasm-pilot.md`
- Phase 2 真机验收：`docs/audit/service-phase2-acceptance.md`
