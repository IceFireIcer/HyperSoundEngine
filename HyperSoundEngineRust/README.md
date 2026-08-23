# HyperSoundEngineRust —— Rust 支线

HyperSoundEngine 的原生化重写支线（见 docs/adr/0003-dual-track-native-rewrite.md 与
《原生化双支线与Windows音频接入规划书》§2.1）：全量重写 DSP 内核、引擎链与宿主逻辑，
承接 Windows 引擎服务进程与性能目标。两支线共同的行为基准是仓库根 `specs/` 的
共享规格 + 测试向量，TS 支线（仓库根 `src/`）是行为事实标准——规格先行双实现，
双双对拍通过才算完成。

## 目录结构

```text
HyperSoundEngineRust/
├── Cargo.toml               # Cargo workspace 根（resolver = "2"，edition 2021）
├── README.md                # 本文档
└── crates/
    ├── hse-core/            # ✅ 骨架已建：Stage 抽象 + 引擎链雏形（纯库；真实 DSP 自下一阶段落地）
    ├── hse-parity/          # ✅ 已建：对拍 harness（开发专用 bin，二进制名 hse-parity）
    ├── hse-wasapi/          # ⏳ 占位说明：WASAPI 后端（渲染 + loopback/虚拟缆捕获），Phase 2+
    ├── hse-napi/            # ⏳ 占位说明：Node/Electron 进程内嵌入扩展（napi-rs），Phase 2+ 按需
    └── hse-service/         # ⏳ 占位说明：引擎服务进程 bin（线程编排 + 控制面），Phase 2+ 按需
```

规划中还有 hse-asio（feature flag，许可决策后启动）与根级 benches/（criterion 基准矩阵），
分别对应规划书 Phase 5 与 Phase 1，届时再加入工程。

## 构建与验证

在 `HyperSoundEngineRust/` 目录下：

```bash
cargo check                      # 全 workspace 类型/借用检查
cargo test                       # 单元测试（容差公式、f32 切分、用例解析、执行器、StageChain）
cargo run -p hse-parity          # 空跑 / 对拍 specs/dsp/vectors（见下节）
cargo build --release            # nightly 工作流使用的构建方式
```

nightly 工作流的假设：检测到本目录即执行 `cargo build --release`，并把构建产物目录下
名为 `hse-*` 的可执行文件收进 nightly 产物——因此对拍二进制固定命名为 `hse-parity`。

## 对拍 harness（crates/hse-parity）

### 用法

```bash
cargo run -p hse-parity                        # 自动定位 specs/dsp/vectors
cargo run -p hse-parity -- <specs 向量目录>     # 或显式指定向量目录
```

自动定位顺序：先从编译期记录的 crate 路径（CARGO_MANIFEST_DIR）逐级向上查找
`specs/dsp/vectors`；找不到再从当前工作目录逐级向上查找。

### 行为与退出码

| 场景 | 行为 | 退出码 |
|------|------|--------|
| 向量目录不存在，或没有任何 *.json 用例 | 打印友好提示后结束（Phase 0 允许空跑框架） | 0 |
| 全部用例通过 | 打印每个用例的 PASS 与最大误差汇总 | 0 |
| 任一用例失败 | 打印失配详情（失配样本数、首个失配样本定位）或夹具缺陷原因 | 1 |

### 与 specs/ 门禁的关系

- 向量由 TS 支线导出工具生成并**冻结**，归 `specs/` 所有，任何一方不得单方面修改；
- 同一份向量同时喂 TS 侧 vitest 测试与本 harness，**双绿才算实现完成**（CI 门禁的 Rust 半边）；
- 容差口径两支线统一：对每个样本 `|got - want| <= value * max(|want|, floor)`，
  非有限值一律判失配；
- 处理语义：输入按 blockSize 顺序分块调用模块（末块可短），状态跨块保持，
  期望输出为逐块输出按序拼接；
- `.f32` 布局为小端、非交错的四段：
  `[输入左 frames][输入右 frames][期望输出左 frames][期望输出右 frames]`；
- 用例 JSON 只提取已知标量键（schemaVersion/module/case/sampleRate/blockSize/
  channels/frames/tolerance），params 参数快照不解释其内容，未知字段一律容忍。

### 当前实现状态

被测对象是**直通假实现**（输出=输入）。因此一旦向量存在，成片 FAIL 属预期——
这正是 Phase 0 出口判据要求的"harness 能跑通一个假实现"；待 hse-core 真实模块
按规格逐个落地后，同一命令应当转绿。

## Phase 0 出口判据对照（规划书 §五）

| 判据 | 归属 | 状态 |
|------|------|------|
| Rust workspace 骨架 | 本目录 | ✅ 已交付 |
| hse-parity harness（读向量 / 比对 / 容差） | 本目录 crates/hse-parity | ✅ 已交付，含单元测试 |
| harness 能跑通一个假实现 | 本目录 | ✅ 直通假实现 + 空跑兜底 |
| 试点 biquad / limiter / reverb-simple 规格与 TS 绿 | specs/ 与 TS 支线侧 | 由并行工作流推进 |

## 铁律提示（对新增代码生效）

- 核心算法确定性：禁随机、禁时钟、稳态零分配（hse-core 内禁日志输出）；
- 标识符/存储键/事件名禁止版本前缀字样（无前缀或 hse- 前缀）；
- 许可：workspace 统一 CC-BY-NC-ND-4.0；
- 文档只描述已跟踪文件，不引用版本控制排除的路径。
