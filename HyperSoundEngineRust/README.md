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
    ├── hse-core/            # ✅ 17 个 DSP 模块 + EngineChainStage 1–21 级主链
    ├── hrtf-core/           # ✅ world-listener f64 几何核；渲染器尚未实现
    ├── hse-parity/          # ✅ 音频 + 空间共享夹具门禁（二进制名 hse-parity）
    ├── hse-wasapi/          # ✅ WASAPI 共享模式渲染 + loopback 捕获
    ├── hse-service/         # ✅ 完整主链服务进程、JSON-RPC 控制面与推流入口
    ├── hse-wasm/            # ✅ 单 Biquad wasm32 最小试点
    └── hse-napi/            # ⏳ 可选 Node/Electron 进程内嵌入扩展占位
```

Windows 设备 I/O 固定由 `hse-wasapi` 承担，不规划其他 Windows 音频后端。根级 benches/
（criterion 基准矩阵）对应规划书 Phase 1。

## 构建与验证

在 `HyperSoundEngineRust/` 目录下：

```bash
cargo check                      # 全 workspace 类型/借用检查
cargo test                       # 单元测试（容差公式、f32 切分、用例解析、执行器、StageChain）
cargo run -p hse-parity          # 对拍音频 72 case + world-listener 12 case
cargo build --release            # nightly 工作流使用的构建方式
```

nightly 工作流的假设：检测到本目录即执行 `cargo build --release`，并把构建产物目录下
名为 `hse-*` 的可执行文件收进 nightly 产物——因此对拍二进制固定命名为 `hse-parity`。

## 对拍 harness（crates/hse-parity）

### 用法

```bash
cargo run -p hse-parity                        # 自动定位 specs/dsp/vectors
cargo run -p hse-parity -- <specs/dsp/vectors 目录>  # 显式音频向量目录；空间夹具从 specs 兄弟目录推导
```

自动定位顺序：先从编译期记录的 crate 路径（CARGO_MANIFEST_DIR）逐级向上查找
`specs/dsp/vectors`；找不到再从当前工作目录逐级向上查找。空间夹具固定从同一
`specs` 根下的 `spatial/vectors/world-listener.v1.json` 读取。

### 行为与退出码

| 场景 | 行为 | 退出码 |
|------|------|--------|
| 音频向量目录或 world-listener 夹具缺失/为空/无效 | 打印夹具缺陷并失败 | 1 |
| 音频 72 case 与空间 12 case 全部通过 | 分别打印 PASS 与最大误差汇总 | 0 |
| 任一用例失败 | 打印失配详情与首个失配字段/样本 | 1 |

### 与 specs/ 门禁的关系

- 音频向量由 TS 支线导出工具生成并冻结，按相对容差逐样本或 meter 读数对拍；
- `specs/spatial/vectors/world-listener.v1.json` 由 TS 与 `hrtf-core` 共同消费，按字段绝对容差对拍；
- 任一夹具缺失、无效或任一 case 失败，综合门禁退出码均为 1；
- 处理语义：输入按 blockSize 顺序分块调用模块（末块可短），状态跨块保持，
  期望输出为逐块输出按序拼接；
- `.f32` 布局为小端、非交错的四段：
  `[输入左 frames][输入右 frames][期望输出左 frames][期望输出右 frames]`；
- 用例 JSON 只提取已知标量键（schemaVersion/module/case/sampleRate/blockSize/
  channels/frames/tolerance），params 参数快照不解释其内容，未知字段一律容忍。

### 当前实现状态

`hse-core` 已覆盖 17 个 DSP 模块与 `EngineChainStage` 1–21 级，音频向量 72/72 PASS。
`hrtf-core` 已实现 world-listener position/yaw 几何核，空间结构化 case 12/12 PASS；
HRIR、插值、卷积、房间与 Rust 主链第 22 级尚未实现。

## 当前门禁对照

| 判据 | 归属 | 状态 |
|------|------|------|
| 七包 Rust workspace | 本目录 | ✅ 已交付 |
| 音频冻结向量 | hse-core + hse-parity | ✅ 72/72 |
| world-listener 结构化夹具 | hrtf-core + hse-parity | ✅ 12/12 |
| Rust HRTF 渲染 | hrtf-core | ⏳ 后续 Slice |

## 铁律提示（对新增代码生效）

- 核心算法确定性：禁随机、禁时钟、稳态零分配（hse-core 内禁日志输出）；
- 标识符/存储键/事件名禁止版本前缀字样（无前缀或 hse- 前缀）；
- 许可：workspace 统一 CC-BY-NC-ND-4.0；
- 文档只描述已跟踪文件，不引用版本控制排除的路径。
