# specs/ —— HyperSoundEngine 双支线共享规格总纲

> **归属**：本目录由 **TS 支线（`src/`）与 Rust 支线（`HyperSoundEngineRust/`）共同所有**，
> 位于仓库根，不属于任何单一支线。术语基线见仓库根 `CONTEXT.md`；
> DSP/engine-chain 行为事实标准当前为 TS 支线源码，Rust-only renderer/ABI 的共享边界由
> `specs/spatial/renderer-abi.md` 与冻结夹具定义；本总纲与其下规格文档使用同一套领域语言。
>
> 当前基线：**25 份双支线共享规格（17 DSP + 4 engine + 1 I/O + 3 spatial）**，其中 18 份纳入音频对拍；
> 音频冻结向量 **72 组 / 144 文件**，另有参数/场景/分享串/default frameCount 结构化契约夹具 **4 个 JSON**、
> Phase 4 全链参数扫描 **40 case（1 个结构化 JSON）**、standard WAV 共享夹具 **1 个 JSON**、world-listener **14 个结构化 case**与 renderer/ABI
> **14 个结构化 case**；Rust `hse-parity` 综合门禁为音频 **72/72 PASS** + 空间 **28/28 PASS** + 参数扫描 **40/40 PASS**。

---

## 一、目的与两支线关系

HyperSoundEngine 按《原生化双支线与 Windows 音频接入规划书》Phase 0 执行**「规格先行双实现」**：

1. **规格先行**：每个 DSP 模块先在 `specs/` 落定行为规格（GWT 条款 + 测试向量），再谈实现；
2. **TS 支线（`src/`）是 DSP/engine-chain 行为事实标准**：Bootstrap 阶段的音频测试向量由导出工具驱动 TS 实现生成；
   Rust-only renderer/ABI 则由独立 TS 参考公式生成结构化夹具，再由 Rust 生产实现消费对拍，TS 不伪装为同算法生产实现；
3. **Rust 支线（`HyperSoundEngineRust/`）与 TS 支线功能对等**：以同一批冻结向量做对拍（Parity Run），
   相对容差 1e-6，跨实现不要求逐位一致；
4. **功能完成的定义 = 规格落定且两支线双双通过**（门禁规则见 §五）；
5. 两支线均须满足工程铁律：确定性（无随机/时钟/控制台输出）、稳态零分配、实时安全
   （详见 `CONTEXT.md`、`docs/ARCHITECTURE.md`）。

`specs/` 目录结构：

```text
specs/
├── README.md                        ← 本文件：规格书写总纲（两支线必读）
├── schema/
│   ├── vector-case.schema.json      ← DSP 音频向量的 draft-07 Schema
│   ├── frame-count.schema.json      ← 默认有效帧结构化夹具 Schema
│   ├── phase4-param-scan.schema.json ← Phase 4 全链参数扫描夹具 Schema
│   ├── world-listener.schema.json   ← world-listener 结构化夹具 Schema
│   └── spatial-renderer-abi.schema.json ← renderer/ABI 结构化夹具 Schema
└── dsp/
    ├── biquad.md                    ← 模块规格：biquad
    ├── limiter.md                   ← 模块规格：limiter
    ├── reverb-simple.md             ← 模块规格：reverb-simple
    ├── compressor.md                ← 模块规格：compressor
    ├── bass-enhancer.md             ← 模块规格：bass-enhancer
    ├── mid-side.md                  ← 模块规格：mid-side
    ├── eq-chain.md                  ← 模块规格：eq-chain
    ├── fdn-reverb.md                ← 模块规格：fdn-reverb
    ├── deesser.md                   ← 模块规格：deesser
    ├── loudness-comp.md             ← 模块规格：loudness-comp
    ├── dynamic-eq.md                ← 模块规格：dynamic-eq
    ├── mod-effects.md               ← 模块规格：mod-effects
    ├── fft.md                       ← 模块规格：fft（非流式变换驱动）
    ├── convolver.md                 ← 模块规格：convolver
    ├── modulation-matrix.md         ← 模块规格：modulation-matrix（控制率驱动）
    ├── hse-stretch.md               ← 模块规格：hse-stretch（块窗映射驱动）
    ├── lufs-meter.md                ← 模块规格：lufs-meter（计量型）
    └── vectors/                     ← 72 组冻结向量（.json + .f32，共 144 文件）
        ├── <dsp-module>.<case>.json / <dsp-module>.<case>.f32
        └── engine-chain.<case>.json / engine-chain.<case>.f32
└── engine/
    ├── chain.md                     ← 引擎层规格：1–21 级主链；spatial.mode='off'
    ├── param-scan.md                ← Phase 4 固定种子全链参数扫描
    ├── params.md                    ← 完整参数快照与默认值兼容契约
    ├── scenes.md                    ← 12 个内置场景与分享串契约
    └── vectors/                     ← 4 个结构化 JSON + 参数扫描 JSON 夹具
└── io/
    ├── wav.md                       ← legacy / standard WAV 双模式兼容契约
    └── vectors/wav-standard.json    ← 两支线共享的标准 RIFF 结构化夹具
└── spatial/
    ├── world-listener.md            ← 世界坐标到完整 listener 姿态方向契约
    ├── renderer-abi.md              ← nearest/time renderer 与 C ABI 成功路径契约
    ├── stage22.md                   ← Rust stage22 world/stage 参数投影与实时语义
    └── vectors/
        ├── world-listener.v1.json   ← 14 个结构化几何 case
        └── renderer-abi.v1.json     ← 14 个 grid/model/renderer 结构化 case
```

---

## 二、规格书写规范（GWT 模板）

所有模块规格（`specs/dsp/<id>.md`）的行为条款一律采用 **GWT（给定/当/则）** 三段式书写：

```markdown
### GWT-<MODULE>-<两位序号>：<一句话标题>
- **给定（Given）**：<初始条件——采样率、参数快照要点、模块内部状态假设>
- **当（When）**：<触发动作——送入何种输入、如何分块、调用了哪些方法>
- **则（Then）**：<可观测断言——输出性质/统计约束/误差界；精确数值一律引用冻结向量>
```

书写规则：

1. **一个条款只断言一件事**，且必须在两支线上可用同一程序机械判定（不允许"听起来像"的主观表述）；
2. 断言分两类：**定性断言**（直通、衰减收敛、有界无发散、逐位一致）与**定量断言**
   （必须指向冻结向量夹具 + §三容差公式，条款内不得内嵌具体参数数值与期望值）；
3. 禁止引入随机、时钟、控制台语义——核心算法确定性是全体条款的前提，不是断言对象；
4. **边界条件必须显式成款**：极值参数（clamp 生效）、静音输入、满幅输入、跨块状态连续性、reset 复现性；
5. 无法进入向量格式的行为（抛错路径、中途改参的管线清理等）单独标注「由单元测试覆盖」，不冒充向量条款；
6. 每份模块规格末尾必须有「向量用例」章节：声明以冻结夹具为准，并列出预期 case 覆盖面
   （覆盖面写"维度"，不写"数值"，避免与导出工具漂移）。

---

## 三、测试向量格式契约（全文）

> 本节是**两支线共享的唯一向量格式契约**。任何一方（含导出工具、TS 加载器、Rust 加载器）
> 不得私自扩展或收窄；修改本节属于破坏兼容契约，须走 MAJOR 流程（§四）。

### 3.1 路径规则

- 元数据：`specs/dsp/vectors/<module>.<case>.json`
- 数据：`specs/dsp/vectors/<module>.<case>.f32`（与 .json 同名、成对出现，缺一即无效）
- `<module>` 为 kebab-case 模块 id，当前合法值为：`biquad` / `limiter` / `reverb-simple` /
  `compressor` / `bass-enhancer` / `mid-side` / `eq-chain` / `fdn-reverb` / `deesser` /
  `loudness-comp` / `dynamic-eq` / `mod-effects` / `fft` / `convolver` /
  `modulation-matrix` / `hse-stretch` / `lufs-meter` / `engine-chain`；
- `<case>` 为小写字母数字与连字符组成的用例名（推荐 `case<N>` 编号形态，如 `case1`）；
- 文件编码：.json 为 UTF-8；.f32 为原始二进制。

### 3.2 JSON 字段表

| 字段 | 类型 | 约束 | 必填 | 说明 |
|---|---|---|---|---|
| `schemaVersion` | number | 恒为 `1` | 是 | 向量格式版本号（当前唯一合法值 1） |
| `module` | string | kebab-case 模块 id | 是 | 必须与文件名中的 `<module>` 一致 |
| `case` | string | 小写字母数字与连字符 | 是 | 必须与文件名中的 `<case>` 一致；示例 `"case1"` |
| `sampleRate` | number | > 0 | 是 | 构造模块实例所用采样率 fs，默认 48000 |
| `blockSize` | number（整数） | ≥ 1 | 是 | 分块大小（每块每声道样本数） |
| `channels` | number | 恒为 `2` | 是 | 固定立体声 |
| `frames` | number（整数） | ≥ 1 | 是 | 每声道帧数 |
| `params` | object | — | 是 | 模块 `setParams` 接受的参数快照，**字段名以 TS 源码为准**（各模块规格的参数表给出固定字段集） |
| `tolerance` | object | 见 §3.5 | 是 | 固定形态 `{kind:"relative", value:1e-6, floor:1e-9}` |
| `moduleKind` | string | `"stream"` 或 `"meter"` | 否 | 缺省视为 `"stream"`；`"meter"` = 计量型模块（processStereo 就地分析、无音频输出，见 §3.3 计量型布局） |
| `readings` | object | 读数名 → `{want, tol}`；want 为有限 number 或哨兵字符串 `"NaN"/"+Infinity"/"-Infinity"`；tol ≥ 0 | 否 | **计量型专用**：标量读数期望值与绝对容差（`\|got−want\| ≤ tol`；哨兵走等值判定）。有 readings ⇒ 必为 meter（draft-07 if/then 双向绑定） |
| `notes` | string | — | 否 | 人类可读备注 |

全部向量 JSON 必须通过 `specs/schema/vector-case.schema.json`（draft-07）校验；
其中 `tolerance.value` 以 enum 固定，当前唯一合法值为 `1e-6`。
`module`/`case` 字段值与文件名的一致性无法用 JSON Schema 表达，由加载器启动时自查。

### 3.3 f32 布局（小端、非交错 planar）

```text
字节偏移  0                4·frames           8·frames            12·frames           16·frames
        ┌───────────────────┬───────────────────┬───────────────────┬───────────────────┐
        │   输入 · 左声道     │    输入 · 右声道   │  期望输出 · 左声道  │  期望输出 · 右声道  │
        │ frames × float32LE │ frames × float32LE │ frames × float32LE │ frames × float32LE │
        └───────────────────┴───────────────────┴───────────────────┴───────────────────┘
文件总长 = 16 × frames 字节（4 段 × frames 样本 × 4 字节）
```

**计量型（moduleKind="meter"）布局**：无期望输出段，.f32 收窄为两段输入：

```text
文件总长 = 8 × frames 字节（inL + inR 两段）
```

计量型模块的判定对象不是音频输出，而是 `readings` 标量（见 §3.2/§3.5）。

读法：依次读入 `inL`、`inR`、`wantL`、`wantR` 四个长度为 `frames` 的 float32 数组。

### 3.4 分块处理语义

1. 以 `sampleRate` 构造模块实例，以 `params` 调用一次 `setParams`（实例为全新零初始状态，
   不额外调用 `reset`）；
2. 将 `inL`/`inR` 按 `blockSize` 自头至尾顺序切块（**末块允许短于 blockSize**），
   逐块调用模块 `processStereo(l, r)`（就地处理）；
3. **模块内部状态跨块保持**——分块处理与一次性整块处理必须产出一致结果；
4. 期望输出 = 各块输出按原顺序逐样本拼接，写入 `wantL`/`wantR`；
5. `gotL`/`gotR` 为被测实现按同样流程产出的实际输出，交由 §3.5 判定。

> 模块特有语义（如 biquad 为单声道核的立体声映射）在各模块规格中定义：
> 见 `specs/dsp/biquad.md` §五。

### 3.5 容差判定公式（两支线统一）

对每个输出样本（左右声道分别逐样本判定）：

```text
|got − want| ≤ value × max(|want|, floor)
```

- `value = 1e-6`（相对容差，当前唯一合法值）；
- `floor = 1e-9`（绝对下限，防止 want≈0 时判据失效）；
- 任一样本超差即整条向量判红；两支线加载器必须使用同一公式与同一常量。

---

## 四、冻结规则

1. **Bootstrap 来源**：向量由导出工具驱动 **TS 支线实现**生成（TS 行为是当前唯一事实标准）；
   导出过程完全确定（无随机、无时钟），同环境重跑逐位一致；
2. **落库即冻结**：`.json` 与 `.f32` 同时进入 `specs/dsp/vectors/` 即成为**冻结基线**，
   所有权归规格（本目录），不再从属于任何支线；
3. **禁止单方面修改**：任何支线不得改写已冻结向量的输入或期望值；对拍失败时先怀疑实现，
   再走下述修订流程。**期望值永不修改**（仓库铁律）；
4. **修订流程（唯一合法路径）**：提出向量替换提案 → 双支线共同确认 → 整体替换（旧文件删除、
   新文件落库）→ 按下方分级记录到 `CHANGELOG.md`；
5. **变更分级**：
   - 破坏既有向量兼容的行为变更 → **MAJOR**；
   - 仅新增向量（不动任何旧向量）→ **MINOR**；
   - 不影响任何向量结果的修复 → **PATCH**；
6. 版本管理细则遵循 `docs/VERSIONING.md`；标识符/存储键/事件名一律无版本前缀或 `hse-` 前缀。

---

## 五、门禁规则

**实现完成 = 规格落定 + 两支线双绿**，缺一不可：

| 支线 | 门禁命令 | 判定内容 |
|---|---|---|
| TS 支线 | `npm run verify:specs` | 遍历 72 组 DSP 音频向量、40 组 Phase 4 全链参数扫描与三份 spatial 共享契约，按各自契约判定 |
| Rust 支线 | `cd HyperSoundEngineRust && cargo run -q -p hse-parity` | 驱动 `hse-core` 音频模块/主链及参数扫描，以及 `hrtf-core` world-listener 与 renderer；音频 72/72、空间 28/28、参数扫描 40/40 均通过才算绿 |

补充规则：

1. 只绿一边不算完成；任一支线红、夹具缺失或 .json 未通过 Schema 校验，均视同未完成；
2. 两支线加载器的切块方式、状态保持假设、容差公式必须与本契约逐字一致；加载器应在校验失败时
   报告具体向量文件名与首个超差样本位置；
3. 新增模块时先补齐模块规格与向量，再把该模块纳入两侧门禁范围。

---

## 六、模块 id 映射表

| 规格 id | 层级 | TS 事实源码 | Rust 对拍实现 | 模块规格 |
|---|---|---|---|---|
| `biquad` | DSP | `src/dsp/biquad.ts` | `hse-core::biquad` | [biquad.md](dsp/biquad.md) |
| `limiter` | DSP | `src/dsp/Limiter.ts` | `hse-core::limiter` | [limiter.md](dsp/limiter.md) |
| `reverb-simple` | DSP | `src/dsp/ReverbSimple.ts` | `hse-core::reverb_simple` | [reverb-simple.md](dsp/reverb-simple.md) |
| `compressor` | DSP | `src/dsp/Compressor.ts` | `hse-core::compressor` | [compressor.md](dsp/compressor.md) |
| `bass-enhancer` | DSP | `src/dsp/BassEnhancer.ts` | `hse-core::bass_enhancer` | [bass-enhancer.md](dsp/bass-enhancer.md) |
| `mid-side` | DSP | `src/dsp/MidSide.ts` | `hse-core::mid_side` | [mid-side.md](dsp/mid-side.md) |
| `eq-chain` | DSP | `src/dsp/EqChain.ts` | `hse-core::eq_chain` | [eq-chain.md](dsp/eq-chain.md) |
| `fdn-reverb` | DSP | `src/dsp/FdnReverb.ts` | `hse-core::fdn_reverb` | [fdn-reverb.md](dsp/fdn-reverb.md) |
| `deesser` | DSP | `src/dsp/Deesser.ts` | `hse-core::deesser` | [deesser.md](dsp/deesser.md) |
| `loudness-comp` | DSP | `src/dsp/LoudnessComp.ts` | `hse-core::loudness_comp` | [loudness-comp.md](dsp/loudness-comp.md) |
| `dynamic-eq` | DSP | `src/dsp/DynamicEq.ts` | `hse-core::dynamic_eq` | [dynamic-eq.md](dsp/dynamic-eq.md) |
| `mod-effects` | DSP | `src/dsp/ModEffects.ts` | `hse-core::mod_effects` | [mod-effects.md](dsp/mod-effects.md) |
| `fft` | DSP | `src/dsp/fft.ts` | `hse-core::fft` | [fft.md](dsp/fft.md) |
| `convolver` | DSP | `src/dsp/Convolver.ts` | `hse-core::convolver` | [convolver.md](dsp/convolver.md) |
| `modulation-matrix` | DSP | `src/dsp/modulation.ts` | `hse-core::modulation_matrix` | [modulation-matrix.md](dsp/modulation-matrix.md) |
| `hse-stretch` | DSP | `src/dsp/HseStretch.ts` | `hse-core::hse_stretch` | [hse-stretch.md](dsp/hse-stretch.md) |
| `lufs-meter` | DSP meter | `src/dsp/LufsMeter.ts` | `hse-core::lufs_meter` | [lufs-meter.md](dsp/lufs-meter.md) |
| `engine-chain` | Engine | `src/engine/HyperSoundEngine.ts` 第 1–21 级 | `hse-core::engine_chain::EngineChainStage` | [engine/chain.md](engine/chain.md) |

---

## 七、后续模块命名约定

1. 规格 id 一律 **kebab-case**，与 TS 源文件名一一对应：
   `EqChain.ts → eq-chain`、`MidSide.ts → mid-side`、`Convolver.ts → convolver`、
   `Compressor.ts → compressor`、`ReverbSimple.ts → reverb-simple`（既有示例）；
2. 新模块落地顺序（三件套齐备才算该模块规格落定）：
   ① 写 `specs/dsp/<id>.md`（含 GWT 条款与向量覆盖面）→
   ② 导出工具生成并冻结 `specs/dsp/vectors/<id>.*` →
   ③ 该 id 才允许进入两支线实现与门禁范围；
3. 标识符、存储键、事件名禁止携带版本前缀字样；需要消歧时用无前缀命名或 `hse-` 前缀；
4. 文档只引用仓库已跟踪路径，不引用任何被 `.gitignore` 排除的路径。

---

## 附：关联文件索引

- 领域术语：[`CONTEXT.md`](../CONTEXT.md)
- 立体声处理器通用契约：`src/interfaces.ts`（`StereoProcessor`：`setParams`/`processStereo`/`reset`）
- DSP 实现契约（TS 侧）：`src/dsp/API_SPEC.md`
- 向量 Schema：[specs/schema/vector-case.schema.json](schema/vector-case.schema.json)
- 模块规格：[biquad](dsp/biquad.md) ｜ [limiter](dsp/limiter.md) ｜ [reverb-simple](dsp/reverb-simple.md) ｜ [compressor](dsp/compressor.md) ｜ [bass-enhancer](dsp/bass-enhancer.md) ｜ [mid-side](dsp/mid-side.md) ｜ [eq-chain](dsp/eq-chain.md) ｜ [fdn-reverb](dsp/fdn-reverb.md) ｜ [deesser](dsp/deesser.md) ｜ [loudness-comp](dsp/loudness-comp.md) ｜ [dynamic-eq](dsp/dynamic-eq.md) ｜ [mod-effects](dsp/mod-effects.md) ｜ [fft](dsp/fft.md) ｜ [convolver](dsp/convolver.md) ｜ [modulation-matrix](dsp/modulation-matrix.md) ｜ [hse-stretch](dsp/hse-stretch.md) ｜ [lufs-meter](dsp/lufs-meter.md)
- 引擎规格：[engine-chain](engine/chain.md)（第 1–21 级；`spatial.mode='off'`）｜[param-scan](engine/param-scan.md)（Phase 4 固定种子 40 case）｜[params](engine/params.md)（完整参数快照）｜[scenes](engine/scenes.md)（12 个内置场景与分享串）
- 空间规格：[world-listener](spatial/world-listener.md)（14 个完整姿态几何 case）｜[renderer-abi](spatial/renderer-abi.md)（14 个 nearest/model/time renderer case；spherical 与非零 room 不做伪对拍）｜[stage22](spatial/stage22.md)（Rust world/stage 参数投影与实时语义）
- 空间 Schema：[world-listener](schema/world-listener.schema.json)｜[renderer-abi](schema/spatial-renderer-abi.schema.json)
- 服务层·控制面契约：[service/control-plane.md](service/control-plane.md)
- 服务层·推流协议设计：[service/push-stream.md](service/push-stream.md)