# HyperSoundEngineRust —— Rust 支线

HyperSoundEngine 的原生化重写支线（见 `docs/adr/0003-dual-track-native-rewrite.md` 与
《原生化双支线与Windows音频接入规划书》§2.1）：承接 DSP 内核、引擎链、Windows 服务与
Rust 空间渲染。两支线共同边界由仓库根 `specs/` 定义；不得把只在 Rust 行为测试覆盖的能力
写成跨语言数值对拍。

## 目录结构

```text
HyperSoundEngineRust/
├── Cargo.toml               # Cargo workspace 根（resolver = "2"，edition 2021）
├── benches/                 # criterion DSP、参数域与服务纯内存路径基准
└── crates/
    ├── hse-core/            # 17 DSP + EngineChainStage 1–22（含四种 spatial 模式）
    ├── hrtf-core/           # world-listener、SOFA/grid、插值、卷积、距离/效果/房间 renderer
    ├── hse-parity/          # 音频 72/72 + 空间 28/28 综合门禁
    ├── hse-wasapi/          # WASAPI shared/exclusive、render loopback、capture 直捕
    ├── hse-service/         # 主链服务、控制面、推流、统计与真机验收工具
    ├── hse-wasm/            # 完整 1–22 级 wasm、Biquad 试点与空间薄 ABI
    └── hse-napi/            # 可选 Node/Electron 进程内扩展占位，未入 workspace
```

Windows 设备 I/O 固定由 `hse-wasapi` 承担，不规划 MIDI、ASIO 或其他 Windows 音频后端。

## 构建与验证

在 `HyperSoundEngineRust/` 目录下：

```bash
cargo check --workspace
cargo test --workspace --locked
cargo run -q -p hse-parity              # 音频 72/72 + 空间 28/28
cargo bench --workspace --no-run --locked
cargo build -p hse-wasm --target wasm32-unknown-unknown --release --locked
```

综合门禁自动定位 `specs/dsp/vectors`，并从同一 `specs` 根加载
`spatial/vectors/world-listener.v1.json` 与 `renderer-abi.v1.json`。任一向量/夹具缺失、无效或
case 失败均返回非零退出码。

## 当前实现状态

### hse-core 与 hse-wasm

`hse-core` 已覆盖 17 个 DSP 模块与 `EngineChainStage` 1–22 级，音频冻结向量 **72/72 PASS**。
第 22 级可在控制路径注入 HRTF grid 后处理 `instant`、`headLocked`、`world` 与 `stage`；world 消费完整 listener 姿态、sources/trajectories/playhead/occlusion 与相邻快照确定速度，stage 对齐 preset/seat/roomSize/reverbAmount/customSources，ambience 同级叠加。`hse-service` 通过 idle-only `loadHrtf` 预载 grid，并让 `start`/运行态 `setParams` 在块边界换链。音频冻结向量仍要求 `spatial.mode='off'`，保持旧 72 组逐位结果。

`hse-wasm::HseEngine` 的默认构造保持完整 1–21 级、`spatial.mode='off'` 兼容路径；`withSofaBytes` 与 `withHrtfGrid` 在 worklet 构造控制阶段建立完整 1–22 级链，非 off 且无 HRTF 明确失败。引擎使用预分配 planar 主输入/sidechain 缓冲，render 不解析 HRTF。正式 `HyperSoundEngineHost` 可选 wasm backend，在主线程缓存 wasm module 与 SOFA bytes/预解析 grid，参数更新通过复用资源预建新节点、等待 ready 后交叉淡变替换。CI 以 headless Chromium 和无设备音频目的节点覆盖正式 bundle 的 ready、spatial off 1–21 级非静音处理、失败静音与参数节点替换淡变；Firefox 尚未纳入自动门禁。

### hrtf-core 与空间 ABI

`hrtf-core` 已实现：

- world-listener 完整 position/yaw/pitch/roll 几何与规则 HRTF grid；
- `sofar`（`default-features=false`）SOFA 控制路径解析；
- 44.1/48/96 kHz 间确定性 129-tap Kaiser-windowed sinc HRIR 重采样；
- nearest 与 spherical L=3 插值；
- time 与 64/128 样本 partitioned 卷积；
- inverse/linear/exponential 距离、空气吸收、Doppler、遮挡、声源大小与房间模型；
- 按稳定 slot 保持对象卷积/滤波状态，prepare 后 render 路径零分配。

`hse-wasm::spatial_abi` 精确提供规划中的 8 个 C 风格函数，并提供 destroy/reset/error 等辅助符号。
共享空间门禁为 world-listener **14/14** + renderer/ABI **14/14**，合计 **28/28 PASS**。
renderer 跨语言数值夹具只覆盖 nearest/time/单声源/room off；spherical、非零 room 与 partitioned
扩展行为由 Rust 测试覆盖。真实 SOFA 文件尚未进入自动门禁，不能据合成夹具宣称真实资产已验收。

### Phase 4 服务与性能

Phase 4 自动实现已完成：固定 LCG 全链参数扫描（40 个共享 TS/Rust 摘要对拍 case）、完整卷积 release 的 alloc/realloc/dealloc 零分配、
服务纯内存路径 criterion、WASAPI event readiness、双环 current/high-water 与排队帧延迟分位数、
shared/exclusive，以及 `hse-real-audio-check` 真机工具均已落地。

自动门禁不证明真实设备结果。shared/exclusive 端到端延迟、xrun 与整进程 CPU 必须由用户在真实设备
分别验收；WASAPI loopback 不支持 exclusive。固定时长测试已删除且不得恢复，测试以事件、固定帧数、
块序号或显式超时上限收敛。

## 当前门禁对照

| 判据 | 状态 |
|---|---|
| 七包 Rust workspace 版本 | 1.5.0 |
| 音频冻结向量 | **72/72 PASS** |
| Phase 4 全链参数扫描 | **40/40 PASS**（固定 LCG + 结构摘要） |
| world-listener | **14/14 PASS** |
| renderer/ABI | **14/14 PASS** |
| 空间合计 | **28/28 PASS** |
| 完整 1–22 级 wasm | 已实现；Chromium AudioWorklet E2E 已纳入 CI，Firefox 待覆盖 |
| Rust spatial stage 22 | `instant`/`headLocked`/`world`/`stage` 完整参数投影已实现 |
| 真实 SOFA 自动门禁 | 未完成 |
| 物理 multichannel 输出 | 未实现，当前最终输出为立体声双耳 |
| 真实设备延迟/CPU | 工具已就绪，待用户验收 |

## 铁律提示

- 核心算法确定性：禁随机、禁时钟；prepare 后实时路径零分配、零锁、零系统调用；
- 既有冻结期望不得修改，新增夹具必须同步规格与门禁；
- 固定时长测试不得恢复；
- 未实际运行的真机、真实 SOFA 或真浏览器路径不得写成通过；
- workspace 统一使用 `CC-BY-NC-ND-4.0`。
