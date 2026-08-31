# HANDOVER —— 双线进度交接

> 最后更新：2026-08-31（北京时间）
> 依据：《原生化双支线与Windows音频接入规划书》§五、当前代码、共享规格与本批自动门禁

## 一、当前版本与阶段

当前收尾版本为 **1.5.1**，npm 与 Cargo workspace 版本同步。

| 阶段 | 状态 | 当前结论 |
|---|---|---|
| Phase 0 规格基建 | 完成 | 25 份共享规格（17 DSP + 4 engine + 1 I/O + 3 spatial），72 组音频冻结向量 / 144 文件，另有 4 个引擎结构夹具、40 case 参数扫描、1 个 standard WAV 夹具、14 个 world-listener case 与 14 个 renderer/ABI case |
| Phase 1 Rust 核心骨架 | 完成 | `hse-core` Stage 抽象、对拍门禁与 criterion 基准 |
| Phase 2 服务进程 | 主体完成，出口待验收 | 服务、控制面、CLI、推流、独立捕获/渲染选路及 shared/exclusive 已实现；正式播放器/VB-CABLE 真机路径待用户验收 |
| Phase 3 双支线原生化 | 实现完成，出口待验收 | 17 个 DSP 模块及 Rust 1–21 级 `EngineChainStage` 全链对拍 **72/72 PASS**；双推流客户端 + 非零回环联合验收待完成 |
| Phase 4 性能冲刺 | 自动实现完成，真机待验收 | 参数扫描、release 零分配、服务基准、事件等待、排队延迟统计、shared/exclusive 与真机工具已落地；真实设备端到端延迟/CPU 待验收 |
| Phase 5 可选扩展 | 主体实现，出口仍有缺口 | 完整 1–22 级 wasm + 正式 Host 可选接入；Rust HRTF renderer、8 函数 ABI 与 stage 22 四模式已完成，空间门禁 **28/28 PASS** |

1.5.1 的状态结论只覆盖仓库内实现和自动门禁。真实设备 shared/exclusive 延迟与 CPU、真实 SOFA 资产兼容性、Firefox AudioWorklet E2E 以及物理 multichannel 输出均没有自动通过证据，不得写成已验收。

## 二、双支线现状

### TS 支线

- npm 包 `hypersoundengine`，22 级处理链；第 22 级为空间音频，默认 `spatial.mode='off'`。
- `src/spatial/` 提供 HRTF、卷积、房间与 multichannel 输入到双耳输出的参考实现；Host 支持 `inputChannelCount` 2/6/8，但物理输出仍固定 2 声道。
- engine-chain 音频向量只冻结第 1–21 级并强制 `spatial.mode='off'`。
- 正式 `HyperSoundEngineHost` 可选 `engineBackend='wasm'`；控制路径预建新 wasm 节点、等待 ready 后以零增益接入并预滚一个 128-frame render quantum，再交叉淡变替换。正式 bundle 已在 headless Chromium 中覆盖 ready、1–21 级处理、节点替换及合成 HRTF grid 的非 off stage 22；Firefox 与真实 SOFA 资产仍未纳入自动门禁。

### Rust 支线

- `hse-core`：17 个 DSP 模块与 `EngineChainStage` 1–22 级完整链；注入 HRTF grid 后支持 `instant`/`headLocked`/`world`/`stage`。
- `hrtf-core`：完整欧拉 world-listener、规则 HRTF grid、SOFA 解析、44.1/48/96 kHz Kaiser-windowed sinc 重采样、nearest/spherical、time/partitioned、距离/空气吸收、Doppler、遮挡、声源大小、房间与稳定 slot；prepare 后 render 路径零分配。
- `hse-parity`：音频 **72/72 PASS** + world-listener **14/14 PASS** + renderer/ABI **14/14 PASS**，空间合计 **28/28 PASS**。
- `hse-service` / `hse-wasapi`：WASAPI capture/loopback/render、shared/exclusive、事件等待、完整 1–22 级主链、控制面、推流、双环排队延迟统计与真机验收工具。
- `hse-wasm`：完整 1–22 级 `HseEngine`、独立 `HseBiquad` 试点与空间 8 函数 C ABI；正式 Host 通过完整引擎构造入口使用 stage 22，薄 ABI 保持独立边界。
- 七个 workspace 包 `hse-benches` / `hse-core` / `hrtf-core` / `hse-parity` / `hse-service` / `hse-wasapi` / `hse-wasm` 均为 1.5.1；`hse-napi` 仍为未入 workspace 的占位。

### Phase 4 自动门禁

- 固定 LCG 参数扫描覆盖 44.1/48/96 kHz、63/128/257/512 块长、8 个种子及边界，共 40 个合法全链快照。
- release 零分配门禁覆盖默认链、代表性全开链和 4 秒卷积 IR 的完整 release 调度，统计 alloc/realloc/dealloc 均为零。
- `bench_service_path` 覆盖 deinterleave、三会话 mix、DSP、interleave、双 rtrb 与串联块纯内存路径；已纳入 benchmark 编译门禁，不伪造未实际运行的机器性能数字。
- 捕获改为 WASAPI event readiness；控制面暴露双环 current/high-water、`blockSequence` 与确定性 `latencyFrames` p50/p95/max。该值只是服务排队帧估算，不是设备端到端延迟。
- `hse-real-audio-check` 支持 dry-run、显式真实音频门控、shared/exclusive、脉冲相关延迟、xrun 与整进程 CPU 报告。
- 固定时长测试已删除且不得恢复；流程以事件、固定帧数、块序号或显式超时上限结束。

### 共享规格

- 25 份共享规格：17 DSP + 4 engine + 1 I/O + 3 spatial。
- 72 组冻结音频向量，每组 `.json` + `.f32`，共 144 文件；另有 4 个引擎结构夹具、40 case 参数扫描与 1 个 standard WAV 夹具。
- 空间夹具为 world-listener 14 case + renderer/ABI 14 case。renderer 跨语言数值夹具只覆盖 nearest/time/单声源/room off；spherical、非零 room、partitioned 与扩展效果由 Rust 测试覆盖，不冒充 TS/Rust 同算法对拍。

## 三、未完成工作与建议顺序

1. **真实设备 Phase 4 验收**：分别测 shared/exclusive 端到端延迟、xrun 和整进程 CPU；loopback 不支持 exclusive，共享结果不得替代独占结果。真实开流必须由用户显式设置 `HSE_ALLOW_REAL_AUDIO=1` 并传 `--run`。
2. **真实 SOFA 自动门禁**：引入许可明确、可再分发的真实 SOFA 资产，覆盖解析、44.1/48/96 kHz 重采样及 renderer；当前合成 grid/夹具不能证明真实资产兼容性。
3. **Firefox AudioWorklet E2E**：当前 Chromium 已自动覆盖，Firefox 尚未纳入门禁。
4. **物理 multichannel 输出**：当前 2/6/8 是输入能力，最终仍输出立体声双耳；没有 5.1/7.1 物理设备输出。
5. **Phase 3 联合出口验收**：两个推流客户端与一路非零真实 capture/loopback 同时运行，验证混合、背压与 xrun。
6. **`hse-napi` 与外部模型注入式 2-stem ONNX**：仍未实现。

## 四、当前可用边界与限制

- **TS 核心**：Node / 浏览器 / Electron 可用；TS 空间参考实现可用，物理输出固定立体声。
- **Windows 独立服务**：localhost JSON-RPC、二进制立体声推流、capture/loopback、独立 render、shared/exclusive 与 Rust stage 22 四模式可用；物理输出固定双耳立体声。
- **浏览器 Rust/WASM**：完整 1–22 级主链已有正式 Host 可选接入，Chromium E2E 已覆盖；Firefox 尚未自动门禁。
- **Rust 空间**：renderer 与 world/stage 参数投影已实现；真实 SOFA 自动门禁未完成。
- **外部发布**：源码版本为 1.5.1；没有证据表明 npm/crates.io 已发布对应包，不能假定注册表可用。
- **明确不支持**：MIDI 与 ASIO。

## 五、常用门禁

```bash
# TS 支线
npm run typecheck
npm run typecheck:ui
npm test
npm run build
node scripts/export-vectors.mjs
node scripts/export-spatial-vectors.mjs

# Rust 支线
cd HyperSoundEngineRust
cargo check --workspace
cargo test --workspace --locked
cargo run -q -p hse-parity        # 音频 72/72 + 空间 28/28
cargo bench --workspace --no-run --locked
cargo build -p hse-wasm --target wasm32-unknown-unknown --release --locked
```

关键纪律：既有冻结期望不得修改；新增向量/结构化 case 必须同步更新规格与两侧门禁；固定时长测试不得恢复；真实音频必须显式门控；任何未实际运行的真机或浏览器路径都不得写成通过。

## 六、文档索引

- 正式发布就绪度：`docs/RELEASE_READINESS.md`（1.5.1 可作为明确限制的源码型 GitHub Release；npm、Windows 二进制与 crates.io 暂不发布）
- 阶段状态：`docs/audit/phase-status.md`
- 共享规格总纲：`specs/README.md`
- 空间 ABI：`docs/SPATIAL_ABI.md`、`specs/spatial/renderer-abi.md`
- Phase 4 自动门禁：`docs/audit/phase4-automatic-gates.md`
- Phase 4 基准：`docs/audit/phase4-bench-matrix.md`
- Phase 4 真机验收：`docs/audit/phase4-real-audio-acceptance.md`
