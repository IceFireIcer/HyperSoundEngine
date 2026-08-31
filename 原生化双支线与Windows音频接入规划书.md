# 原生化双支线与 Windows 音频接入规划书

- 日期：2026-08-22
- 状态：已定稿（grilling 会话产出，决策记录见 `docs/adr/0001`~`0004`，术语见 `CONTEXT.md`）
- 范围：① 性能优化（激进原生化路线）② Windows 音频 API 接入（仅 WASAPI）

---

## 一、决策基线（grilling 会话全部结论）

| # | 决策点 | 结论 | 记录 |
|---|--------|------|------|
| 1 | 产品形态 | 独立引擎；播放器只把音频送进来处理；不绑定 Electron | ADR-0001 |
| 2 | 交付形态 | **引擎服务进程**（独立常驻进程，独占音频设备；接入方含原生应用） | ADR-0001 |
| 3 | 音频入口 | **双入口**：回环拦截（WASAPI loopback / 虚拟缆，零集成）+ 推流协议（WebSocket PCM） | ADR-0002 |
| 4 | 性能路线 | **激进原生化**：全部 TS 实现用原生语言重写 | ADR-0003 |
| 5 | 原生形态 | **双支线并存**（TS 支线 + Rust 支线），语言选 **Rust**，对外接口保持开放 | ADR-0003 |
| 6 | 开发主线 | **规格先行双实现**：共享规格 + 测试向量定义行为，两支线各自实现双绿才算完成 | ADR-0003 |

关键修订：ADR-0003 落地后，引擎服务进程为**纯 Rust 二进制**（不再经 Node 中介）；napi-rs 扩展保留为 Node/Electron 接入方的可选进程内路径。

## 二、目标架构

### 2.1 Rust 支线工程结构（仓库根下 `HyperSoundEngineRust/`，与 `src/` 同级；Cargo workspace）

```
HyperSoundEngineRust/
├── Cargo.toml              # workspace
├── crates/
│   ├── hse-core/           # DSP 模块 + 引擎链（纯库；process 稳态零分配、无时钟/随机）
│   ├── hse-parity/         # 对拍 harness：读 specs/ 向量 → 跑 hse-core → 比对（dev-only）
│   ├── hse-wasapi/         # WASAPI 后端：shared/exclusive capture/render + shared render loopback
│   ├── hse-napi/           # napi-rs 扩展（可选：Node/Electron 进程内嵌入）
│   └── hse-service/        # 引擎服务进程（bin）：线程编排 + WebSocket 控制面
└── benches/                # 基准矩阵（criterion）
```

规格向量放仓库根 `specs/`（两支线共同所有，见 §四）。

### 2.2 引擎服务进程内部（Windows）

```
                     ┌────────────────────────────────────────────┐
  接入方（任何语言）  │            hse-service（纯 Rust bin）        │
  ──WebSocket JSON-RPC──▶ 控制面线程（枚举设备/配置/参数/状态）      │
                     │                                            │
  播放器A ──推流 PCM──▶ 会话混合(混后处理) ─┐                      │
  播放器B（输出选虚拟缆）                   ├─▶ rtrb 输入环        │
  指定渲染设备 ←─WASAPI loopback 捕获────────┤    │                  │
  虚拟缆 CABLE Output ←─capture 直捕─────────┘    │                  │
                     │                    DSP 线程：hse-core      │
                     │                    process（全原生零GC）    │
                     │                        │                  │
                     │              rtrb 渲染环 → WASAPI 渲染线程 ─▶ 真实设备
                     └────────────────────────────────────────────┘
```

- **实时路径全原生**：捕获线程 → 环形缓冲 → DSP 线程 → 环形缓冲 → 渲染线程，JS/Node 不在音频路径上；
- **环形缓冲用 `rtrb`**（realtime-safe 单生产者单消费者，无锁无分配）；
- **控制面与数据面分离**：控制面 WebSocket JSON-RPC（localhost），可随时重连；数据面异常不影响已建立的音频流；
- **多会话 = 混后处理**：先求和再进链，与回环拦截语义一致（ADR-0002）。

### 2.3 虚拟缆用法（回环拦截的单播放器隔离）

- 播放器输出选 **CABLE Input**，引擎直接捕获 **CABLE Output**（虚拟缆是真实捕获端点，无需 loopback）；
- 拦截真实设备混音时才用 WASAPI loopback；
- VB-CABLE 官方许可**允许随应用免费/商业分发并嵌入安装包**（[VB-Audio Licensing](https://vb-audio.com/Services/licensing.htm)）；免费版仅一条缆，多条需捐赠解锁——规划按"引导安装/可选捆绑"处理。

## 三、性能验收目标

| 指标 | 目标 | 口径 |
|------|------|------|
| 实时链 CPU（默认全链） | ≤ 5% 单核 | 48kHz / 128 帧 / 立体声，中端桌面 CPU（Ryzen 5 / i5 级） |
| 实时链 CPU（最重场景） | ≤ 25% 单核 | IR≈4s 卷积混响 + 全链开启（对齐空间音频规划 §八） |
| 端到端延迟（共享模式） | ≤ 30ms | 回环捕获→处理→渲染；独占模式目标 ≤10ms |
| 离线吞吐 | ≥ 3× TS 支线基线 | 同硬件、同测试素材、同参数 |
| 对拍 | 相对容差 1e-6 | 全模块 + 全链，随机参数扫描（ADR-0003） |
| 稳态安全 | 音频回调零分配 | release 构建以分配计数器机械验证 |
| 确定性 | 同输入同参数同输出 | 两支线各自内部维持（无随机/时钟/分配） |

## 四、规格基建（Phase 0 核心产物）

```
specs/
├── README.md               # 规格书写规范与向量格式
├── dsp/
│   ├── biquad.md           # 给定/当/则 + 边界条件 + 参数域
│   ├── fft.md
│   ├── convolver.md        # 含分区语义、延迟语义（partitionSize）
│   ├── limiter.md …        # 每个 DSP 模块一份
│   └── vectors/
│       ├── biquad.case1.json    # 用例描述（参数引用 + 数据文件 + 容差）
│       └── biquad.case1.f32     # 输入/期望输出二进制夹具
├── engine/
│   ├── chain.md            # 21 级链顺序、旁路、sidechain、调制矩阵语义
│   ├── params.md           # 参数模型 = HyperSoundEngineParams 兼容契约
│   └── scenes.md           # 场景预设/分享串格式
└── service/
    ├── control-plane.md    # WebSocket JSON-RPC 方法表
    └── push-stream.md      # 推流协议（帧格式/会话/背压）
```

- **向量格式**：JSON 用例（参数、采样率、块长、引用数据文件、容差）+ 裸 `f32` 二进制（输入/期望输出，非交错）；
- **Bootstrap**：TS 支线是行为事实标准，先写导出工具从 TS 现行为生成初始向量，**冻结后归规格所有**，两支线此后都不得单方面改向量；
- **门禁**：向量对拍进入两边 CI——TS 侧 vitest 跑同一向量，Rust 侧 `hse-parity` 跑同一向量，双绿才是"实现完成"。

## 五、阶段计划

### 当前执行状态（2026-08-31）

| 阶段 | 状态 | 当前边界 |
|---|---|---|
| Phase 0 | 完成 | 25 份共享规格（17 DSP + 4 engine + 1 I/O + 3 spatial），音频 72/72、空间 28/28 与参数扫描结构摘要 40/40 综合门禁已建立 |
| Phase 1 | 完成 | Rust Stage 生命周期、试点实现、对拍与 criterion 基准已落地 |
| Phase 2 | 主体完成，出口待验收 | 服务、控制面、CLI、1–22 级链及独立捕获/输出选路已实现；仍需 VB-CABLE/正式播放器真机链路与端到端延迟 |
| Phase 3 | 实现完成，出口待验收 | 17 个 DSP 模块、WAV、ShareCodec、推流协议及 1–21 级全链已双绿；双独立推流客户端工具已落地，仍需非零真实回环联合验收 |
| Phase 4 | 自动实现完成，真机待验收 | release 零分配、参数扫描、服务/空间基准、事件等待、排队统计、shared/exclusive 与验收工具已完成；真实完整服务链延迟与目标机 CPU 待验收 |
| Phase 5 | 主体实现，出口仍有缺口 | 完整 1–22 级 wasm/Host、Rust SOFA/HRTF renderer、四模式 stage22、8 函数 ABI 和 Chromium E2E 已完成；真实 SOFA 自动门禁与 Firefox E2E 未完成，物理 multichannel 输出尚未实现 |

当前工作树版本为 `1.5.1`。综合门禁口径为音频 72/72、空间 28/28、固定种子参数扫描结构摘要 40/40；旧音频向量继续承担逐样本 `1e-6` 对拍。详细证据与持续更新口径见 `docs/audit/phase-status.md`；下列条目保留原始阶段目标与出口判据。

### Phase 0：规格基建（1-2 周）
1. `specs/` 目录与书写规范；向量 JSON schema；
2. TS 侧向量导出工具（engine 参数 → 输入 PCM → TS 输出 → 冻结夹具）；
3. 试点 3 个模块规格：`biquad`、`Limiter`、`ReverbSimple`（选小选稳）；
4. Rust workspace 骨架 + `hse-parity` harness（读向量/比对/容差）；
5. CI：TS 向量测试绿（Rust 侧先空跑框架）。
- **出口判据**：3 个试点模块"规格 + TS 绿"完成，Rust 对拍 harness 能跑通一个假实现。

### Phase 1：Rust 核心骨架（2-3 周）
1. `hse-core`：stage 抽象（对齐 `ProcessingStage` 语义）、参数模型（serde，JSON 与 `HyperSoundEngineParams` 互通）、`prepare()/process()/reset()` 生命周期；
2. 试点 3 模块 Rust 实现，对拍 1e-6 双绿；
3. `benches/` 基准雏形（criterion，对齐 TS benchmark 场景）。
- **出口判据**：试点模块 TS/Rust 双绿；基准能出数。

### Phase 2：Windows 服务进程 v1——回环拦截端到端（3-4 周）
1. `hse-wasapi`：事件驱动共享模式渲染 + render 端点 loopback 捕获 + capture 端点直捕（`wasapi` crate）；`rtrb` 双环；
2. `hse-service`：线程编排、设备枚举、WebSocket JSON-RPC 控制面（方法：listDevices/configure/setParams/getStats/getState）；
3. 回环拦截全链路：真实设备 loopback → 全链 → 渲染；虚拟缆直捕路径；
4. CLI 调参工具（或最小 WebSocket 客户端）+ xrun 计数上报；
5. 安装引导：检测/引导 VB-CABLE 安装（可选捆绑，官方许可允许分发）。
- **出口判据**：任意播放器（浏览器、foobar2000 等）输出到指定设备 → 经引擎全链 → 真实设备出声；控制面可热改参数。

### Phase 3：模块对拍推进 + 推流协议（4-6 周）
1. 21 级链逐模块规格化 + Rust 实现 + 双绿（Convolver 分区卷积、FFT 基-4、FdnReverb、DynamicEq、ModEffects、Compressor/Deesser sidechain、调制矩阵、LoudnessComp/LufsMeter…）；
2. WAV I/O；
3. 推流协议：会话管理（open/close）、二进制 PCM 帧（会话 id + 序号 + f32 载荷）、背压策略（丢弃旧块 + xrun 上报）、混后处理；
4. `ShareCodec`/场景预设格式兼容（旧分享串在 Rust 支线可解析）。
- **出口判据**：全链双绿；两个推流客户端 + 一路回环同时运行正确混音。

### Phase 4：性能冲刺（3-4 周）
1. 基准矩阵：模块×参数域×块长（128/256/512）×IR 长度；
2. SIMD：`wide`/`std::simd` 评估（Convolver/FFT/Limiter 热点），auto-vectorization 审计（`cargo asm`）；
3. 针对性优化 + 与 TS 基线对比报告（验证 §三目标达成）；
4. 稳态零分配验证。
- **出口判据**：§三全部指标实测达标并留档。

### Phase 5：可选扩展（按需启动）
- **wasm32 目标**：hse-core 编译 wasm 供浏览器 worklet 逐步替换 TS 核心（同代码近零成本，属可选演进，非本规划承诺）；
- **空间音频**：按《空间音频实现规划书》§3.2 契约函数与 §八性能目标在 Rust 支线实现（TS 侧兄弟节点方案已作废，ADR-0003）。

## 六、技术选型依据（调研）

| 选型 | 依据 |
|------|------|
| Rust（否决 C） | 空间音频规划已引 rustup/cargo/wasm-pack；ADR-0001 已定 napi-rs；内存安全 |
| Windows 音频后端 | 仅使用 [`wasapi`](https://docs.rs/wasapi) crate；回环捕获、虚拟缆直捕与渲染统一走 WASAPI |
| cpal | 不采用；跨平台音频后端不在当前项目范围 |
| `rtrb` 环形缓冲 | realtime-safe SPSC，音频线程标准做法 |
| VB-CABLE 分发 | 官方允许免费/商业应用分发与嵌入安装包（[VB-Audio Licensing](https://vb-audio.com/Services/licensing.htm)；商用确据可邮件确认） |
| WASAPI loopback 已知坑 | 毛刺/间断问题有社区记录（[Microsoft Learn](https://learn.microsoft.com/en-gb/answers/questions/1188388/persistent-audio-discontinuity-in-wasapi-loopback-capture)）——缓冲策略与 xrun 上报列为 Phase 2 验收项 |

## 七、风险与缓解

| 风险 | 等级 | 缓解 |
|------|------|------|
| 双实现三倍工件成本（规格+两实现） | 高（已知情接受） | 严格"规格先行"；小功能允许向量冻结滞后一拍但不得跳过 |
| 两支线行为漂移 | 高 | CI 双绿门禁；向量归规格所有，单方面不可改 |
| WASAPI loopback 毛刺 | 中 | 事件驱动 + 冗余缓冲 + xrun 计数上报；虚拟缆走直捕绕开 loopback |
| 全链重写周期长、中间态不可用 | 中 | Phase 2 先端到端跑通小链（试点模块），后续模块逐个替换进链 |
| Windows 专注导致跨平台债务 | 低（当前定位 Windows） | hse-core 保持平台无关；平台代码隔离在 hse-wasapi |

## 八、与既有规划的关系

- 《空间音频实现规划书.md》：Rust HRTF 核并入 Rust 支线（Phase 5），其 §3.2 契约与 §八目标继续有效；TS 兄弟节点方案作废（ADR-0003）；
- `.scratch/alg-optimization/PLAN.md`：已完成（TS 支线内优化），其结论构成 Rust 移植的算法蓝本（分区卷积、基-4 FFT、FDN、DynamicEq）；
- TS 支线（本仓库 `src/`）：继续作为浏览器宿主与 Node 离线路径维护，新功能按"规格先行双实现"同步落两支线。
