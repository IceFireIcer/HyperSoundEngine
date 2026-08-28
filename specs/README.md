# specs/ —— HyperSoundEngine 双支线共享规格总纲

> **归属**：本目录由 **TS 支线（`src/`）与 Rust 支线（`HyperSoundEngineRust/`，规划中）共同所有**，
> 位于仓库根，不属于任何单一支线。术语基线见仓库根 `CONTEXT.md`；
> 行为事实标准当前为 TS 支线源码；本总纲与其下规格文档使用同一套领域语言。

---

## 一、目的与两支线关系

HyperSoundEngine 按《原生化双支线与 Windows 音频接入规划书》Phase 0 执行**「规格先行双实现」**：

1. **规格先行**：每个 DSP 模块先在 `specs/` 落定行为规格（GWT 条款 + 测试向量），再谈实现；
2. **TS 支线（`src/`）是行为事实标准**：Bootstrap 阶段的测试向量由导出工具驱动 TS 实现生成，
   冻结后升级为规格资产；
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
│   └── vector-case.schema.json      ← 测试向量 JSON 的 draft-07 Schema（唯一合法性判据）
└── dsp/
    ├── biquad.md                    ← 模块规格：biquad
    ├── limiter.md                   ← 模块规格：limiter
    ├── reverb-simple.md             ← 模块规格：reverb-simple
    └── vectors/                     ← 冻结测试向量（.json 元数据 + .f32 数据成对出现）
        ├── biquad.<case>.json / biquad.<case>.f32
        ├── limiter.<case>.json / limiter.<case>.f32
        └── reverb-simple.<case>.json / reverb-simple.<case>.f32
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
- `<module>` 为 kebab-case 模块 id，当前仅限：`biquad` / `limiter` / `reverb-simple`
  （后续模块命名约定见 §六）；
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
| TS 支线 | `npx vitest run test/spec-vectors.test.ts` | 遍历 `specs/dsp/vectors/` 全部夹具，按 §3.4 分块驱动 `src/` 实现，按 §3.5 判定 |
| Rust 支线 | `cargo test -p hse-parity` | 同一批夹具、同一公式，驱动 `HyperSoundEngineRust/` 实现 |

补充规则：

1. 只绿一边不算完成；任一支线红、夹具缺失或 .json 未通过 Schema 校验，均视同未完成；
2. 两支线加载器的切块方式、状态保持假设、容差公式必须与本契约逐字一致；加载器应在校验失败时
   报告具体向量文件名与首个超差样本位置；
3. 新增模块时先补齐模块规格与向量，再把该模块纳入两侧门禁范围。

---

## 六、模块 id 映射表

| 规格 id | TS 事实源码 | 参数快照字段来源 | Rust 目标（规划中） | 模块规格 |
|---|---|---|---|---|
| `biquad` | `src/dsp/biquad.ts` | `setParams(type, f0, q, gainDb)` 形参（立体声映射规则见模块规格 §五） | `HyperSoundEngineRust` 内对应模块 | [specs/dsp/biquad.md](dsp/biquad.md) |
| `limiter` | `src/dsp/Limiter.ts` | `LimiterSettings` 接口字段 | 同上 | [specs/dsp/limiter.md](dsp/limiter.md) |
| `reverb-simple` | `src/dsp/ReverbSimple.ts` | `ReverbSimpleParams` 接口字段 | 同上 | [specs/dsp/reverb-simple.md](dsp/reverb-simple.md) |

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
- 模块规格：[biquad](dsp/biquad.md) ｜ [limiter](dsp/limiter.md) ｜ [reverb-simple](dsp/reverb-simple.md)
- 服务层·控制面契约：[service/control-plane.md](service/control-plane.md)
- 服务层·推流协议设计：[service/push-stream.md](service/push-stream.md)