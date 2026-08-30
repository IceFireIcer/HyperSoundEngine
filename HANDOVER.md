# HANDOVER —— 双线进度交接

> 最后更新：2026-08-31（北京时间）
> 依据：《原生化双支线与Windows音频接入规划书》§五、`docs/audit/phase-status.md`、`CHANGELOG.md` 与本批验证

## 一、当前版本与阶段

当前收尾版本为 **1.3.0**，npm 与 Cargo workspace 版本同步。

| 阶段 | 状态 | 当前结论 |
|---|---|---|
| Phase 0 规格基建 | 完成 | 22 份共享规格（17 DSP + engine-chain + params + scenes + WAV + world-listener），72 组音频冻结向量 / 144 文件，另有 3 个参数/场景夹具、1 个 standard WAV 夹具与 12 个 world-listener case |
| Phase 1 Rust 核心骨架 | 完成 | `hse-core` Stage 抽象、对拍门禁与 criterion 基准 |
| Phase 2 服务进程 | 主体完成，出口待验收 | 服务、控制面、CLI 与 1–21 级链已实现；独立捕获/渲染选路本批落地，VB-CABLE 与正式播放器真机路径待验收 |
| Phase 3 双支线原生化 | 实现完成，出口待验收 | 17 个 DSP 模块及 Rust 1–21 级 `EngineChainStage` 全链对拍 72/72 PASS；双推流客户端 + 非零回环联合验收待完成 |
| Phase 4 性能冲刺 | 部分完成 | 离线吞吐、默认链与最重场景 CPU 已达标，稳态零分配门禁已落地；WASAPI 端到端延迟与全链随机参数扫描尚未验证 |
| Phase 5 可选扩展 | 部分完成 | wasm32 单 Biquad 最小试点；`hrtf-core` world-listener 几何核与空间 12/12 对拍完成，HRIR/卷积/房间渲染未实现 |

1.3.0 在 1.2.1 的 push-stream 跨平台时序修复基础上，新增 Windows 捕获/渲染独立选路：兼容旧 loopback 配置，同时支持 capture 端点直捕与独立输出设备，并在启动时拒绝两端采样率不一致。Windows 本地完整 `push_stream` 套件压力循环 200/200、`hse-service` 全量测试、Rust workspace 与综合 parity（音频 72/72 + 空间 12/12）均通过；远端 CI 待本批推送后确认。

### 远端 CI 状态

GitHub Actions 最新已完成 run `33318570393`（提交 `bf7e556`）：

- `test`（Linux TS）：通过，包含 typecheck、UI typecheck、全量测试、engine contract、spatial world-listener contract 与 build。
- `rust-windows-silent`：失败于同一 push-stream 双会话时序假阴性。
- `rust`（Linux）：失败于 `hse-service/tests/push_stream.rs` 的双会话时序假阴性。
- 本批已修复该测试并在 Windows 本地完成 200 次完整测试文件压力循环；修复和独立设备选路尚未推送，远端状态仍以旧 run 为准。

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
- `hse-wasapi`：共享模式渲染、渲染端点 loopback 捕获与 capture 端点直捕；服务可独立选择捕获源和最终输出，真机出声测试需 `HSE_ALLOW_REAL_AUDIO=1`。
- `hse-wasm`：仅暴露单个 `HseBiquad`，通过预分配 planar 缓冲接入独立 AudioWorklet 示例；不替换 TS worklet，也不代表完整 Rust 全链 wasm 化。
- `hrtf-core`：已实现无分配的 f64 world-listener position/yaw 几何核；HRIR、卷积、房间与 renderer 尚未实现。
- 七个 workspace 包 `hse-benches` / `hse-core` / `hrtf-core` / `hse-parity` / `hse-service` / `hse-wasapi` / `hse-wasm` 均为 1.3.0。

### 共享规格

- 17 份 DSP 规格：biquad、limiter、reverb-simple、compressor、bass-enhancer、mid-side、eq-chain、fdn-reverb、deesser、loudness-comp、dynamic-eq、mod-effects、fft、convolver、modulation-matrix、hse-stretch、lufs-meter。
- 1 份引擎链规格、2 份引擎结构规格、1 份 WAV I/O 规格与 1 份 world-listener 空间规格。
- 72 组冻结音频向量，每组 `.json` + `.f32`，共 144 文件；另有 3 个引擎结构夹具、1 个 standard WAV 夹具与 12 个 world-listener case，既有期望值不得修改。

## 三、未完成工作与建议顺序

1. **推送并确认远端 CI**：push-stream 时序假阴性与独立设备选路已在本地修复；推送后确认 Linux `rust`、Windows `rust-windows-silent` 与 TS `test` 三项 job 全绿。
2. **Phase 2 真机出口验收**：使用正式播放器与 VB-CABLE 验证 capture 端点直捕 → 1–21 级主链 → 独立物理输出，并测量 WASAPI 端到端延迟。按用户要求，不得自动运行可听测试。
3. **TS spatial 实时安全 Slice 2**：尚未提交。需要把 `prepare(maxBlockSize)` 贯穿 `SpatialBackend`、`TsConvolverBackend`、`Convolver`、`TimeConvolver` 与 `RoomSim`，显式传递有效 `frameCount`，移除热路径 closure/subarray，并把 IR 构建移到控制路径。
4. **Rust `hrtf-core` renderer**：world-listener 几何核已完成；尚需 HRIR 数据、插值、双耳卷积、距离/空气吸收、房间与零分配 renderer，再接入 Rust 主链第 22 级。
5. **TS ambience 主链接线**：FOA 数学与增益计算已有，但 `ambience.enabled/amount` 仍未进入 `HyperSoundEngine` 的实际音频处理路径。
6. **空间多声道端到端接线**：`TsConvolverBackend.processMulti()` 已有叶子实现和单测，但 `HyperSoundEngine`、AudioWorklet 与 Host 仍固定立体声，5.1/7.1 空间输入尚不可从产品路径使用。
7. **完整 wasm 引擎链**：当前 `hse-wasm` 只有单 Biquad 试点，尚未承载 1–21 级主链或 Rust 空间渲染。
8. **`hse-napi`**：仍为占位，尚无可供 Node/Electron 进程内加载的原生 crate。
9. **外部模型注入式 2-stem ONNX 分离**：Rust 实现及端到端接入尚未完成。

## 四、已知未修复问题

- **远端 CI 待复验**：run `33318570393` 的旧提交在 Linux/Windows Rust job 均因 push-stream 双会话测试时序假阴性失败；本批修复已在本地完成完整 `push_stream` 套件 200 次压力循环、Rust workspace 与综合 parity，尚待推送复验。
- **空间实时安全尚未兑现完整承诺**：TS spatial 在 `prepare()` 后仍可能首块扩容、创建 `subarray`/closure，并在 IR 淡出完成时于渲染路径调用 `loadIR()`；AudioWorklet 回调本身也仍会创建输入/输出数组及统计消息对象。
- **空间公开参数有未接线项**：`ambience.enabled/amount` 当前不改变主链输出；listener 的 pitch/roll 未参与坐标变换；世界模式多普勒速度仍固定为零；`stage.roomSize` 未真正改变 RoomSim 房间尺寸。
- **距离模型边界**：linear 模型在 `distance < refDistance` 时可能产生大于 1 的增益，尚未修复。
- **原生空间能力不完整**：Rust `EngineChainStage` 仍拒绝非 `off` 的 spatial 参数，不能将 12/12 几何对拍描述为完整 Rust HRTF。
- **外部发布边界**：本批 1.3.0 尚未推送，且没有证据表明 npm/crates.io 已发布对应包；其他项目可使用当前源码或本地包，不能假定注册表已有 1.3.0。
- **许可边界**：仓库采用 `CC-BY-NC-ND-4.0`；商业集成、修改和再分发前必须单独确认许可是否满足目标项目。

## 五、当前可用接入边界

- **Node / 浏览器 / Electron 的 TS 核心**：可用；通过 `hypersoundengine`、`hypersoundengine/browser`、`hypersoundengine/worklet` 接入。TS 第 22 级空间参考实现可用，但上述实时安全与部分参数接线问题仍存在。
- **Windows 独立服务**：可用；`hse-service` 提供 localhost WebSocket JSON-RPC、二进制立体声 `f32le` 推流、WASAPI loopback 捕获与共享模式渲染。服务链当前为 Rust 第 1–21 级，不含 spatial。
- **Rust crate 源码集成**：`hse-core` 可作为源码/path dependency 使用；对外稳定的 C ABI 和 N-API 尚未提供。
- **浏览器 Rust/WASM**：仅单 Biquad 试点，不可作为完整引擎替换。
- **明确不支持**：MIDI 与 ASIO。

## 六、性能现状

Phase 4 已测的三项离线 DSP 性能均达标：全链离线 0.546% realtime，约为 TS 基线 9.7–10.2 倍；默认链 CPU 0.546%；最重场景 10.7%。Convolver SIMD 按现有评估缓办。这些数据不包含完整 WASAPI 调度成本，不能证明端到端延迟目标已经完成。

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
```

关键纪律：新增冻结向量/结构化 case 必须与两侧驱动实现同时进入同一批次；真机出声测试必须显式设置 `HSE_ALLOW_REAL_AUDIO=1`；Rust HRTF renderer 未实现前，`EngineChainStage` 始终拒绝非 `off` 的 spatial 模式。

## 八、文档索引

- 阶段状态：`docs/audit/phase-status.md`
- Engine chain 规格：`specs/engine/chain.md`
- Phase 4 基准：`docs/audit/phase4-bench-matrix.md`
- Phase 4 SIMD：`docs/audit/phase4-simd-eval.md`
- wasm 试点审计：`docs/audit/wasm-pilot.md`
- Phase 2 真机验收：`docs/audit/service-phase2-acceptance.md`
