# 规格：mod-effects —— 调制类效果组（Delay → Chorus → Flanger → Phaser → Tremolo 级联）

> **规格属性**：本文件是双支线共享规格。行为事实标准 = `src/dsp/ModEffects.ts`
> （`DelayEffect` / `ChorusEffect` / `FlangerEffect` / `PhaserEffect` / `TremoloEffect`
> 五个类）；参数字段名一律以 `src/types.ts` 消费的 `DelaySettings` / `ChorusSettings` /
> `FlangerSettings` / `PhaserSettings` / `TremoloSettings`（合称 `ModEffectsSettings`）
> 为准，本规格不得臆造字段。格式契约见 [`specs/README.md`](../README.md) §三，
> 向量合法性由 [`specs/schema/vector-case.schema.json`](../schema/vector-case.schema.json) 约束。

---

## 一、模块概述

- **定位**：向量模块 id `mod-effects` 覆盖**整组五效果按引擎接线顺序的级联**：
  `Delay → Chorus → Flanger → Phaser → Tremolo`（`src/engine/HyperSoundEngine.ts`
  buildStages 顺序）。每一级是一个独立 `StereoProcessor`（就地处理）；级联由
  「链路驱动器」（引擎 stage 序列 / 向量加载器）完成，本规格同时固化级联语义与
  五个效果各自的 DSP 语义。
- **出处**：全部为自研实现——Delay：环形延迟线 + 反馈 + 干湿混合；Chorus / Flanger：
  LFO 调制分数延迟（线性插值读出）；Phaser：多级一阶全通 + LFO 调制中心频率 + 输出反馈；
  Tremolo：LFO 幅度调制。
- **确定性**：同输入同参数必同输出；无随机、无 Date、无 console；process 内零分配
  （缓冲在构造期预分配）。
- **采样率**：各效果构造时固定；fs ≤ 0 抛 `Error('invalid sample rate')`。

## 二、接口签名（事实标准摘录）

```ts
// src/types.ts
export interface DelaySettings   { enabled: boolean; delayMs: number; feedback: number; mix: number }
export interface ChorusSettings  { enabled: boolean; rateHz: number; depthMs: number; mix: number }
export interface FlangerSettings { enabled: boolean; rateHz: number; depthMs: number; feedback: number; mix: number }
export interface PhaserSettings  {
  enabled: boolean; rateHz: number
  depth: number        // 0..1 调制深度
  feedback: number; mix: number
  stages: number       // 全通级数（建议 2/4/6/8）
}
export interface TremoloSettings { enabled: boolean; rateHz: number; depth: number; mix: number }
export interface ModEffectsSettings { delay: DelaySettings; chorus: ChorusSettings; flanger: FlangerSettings; phaser: PhaserSettings; tremolo: TremoloSettings }

// src/dsp/ModEffects.ts
export class DelayEffect   { constructor(fs: number); setParams(p: DelaySettings): void;   processStereo(l, r): void; reset(): void }
export class ChorusEffect  { constructor(fs: number); setParams(p: ChorusSettings): void;  processStereo(l, r): void; reset(): void }
export class FlangerEffect { constructor(fs: number); setParams(p: FlangerSettings): void; processStereo(l, r): void; reset(): void }
export class PhaserEffect  { constructor(fs: number); setParams(p: PhaserSettings): void;  processStereo(l, r): void; reset(): void }
export class TremoloEffect { constructor(fs: number); setParams(p: TremoloSettings): void; processStereo(l, r): void; reset(): void }
```

## 三、参数表（向量 `params` 快照字段）

向量 `params` 为 `ModEffectsSettings` 形状——**五个子对象全部完整给出**（含 `enabled`），
不依赖缺省；采样率 fs 不进 params，取自向量顶层 `sampleRate` 字段。

| 子对象 | 字段 | 类型 | 有效域（clamp 后生效值） | 默认值 | clamp / 边界行为 |
|---|---|---|---|---|---|
| `delay` | `enabled` | boolean | — | — | **效果类自身不消费该字段**：由链路驱动器门控（引擎 `active()` / 向量加载器跳过该级，见 §4.1） |
| | `delayMs` | number（ms） | `[0, 2000]` | 0 | 越界双向钳制；`delaySamples = clamp(delayMs, 0, 2000) / 1000 × fs`（f64，逐样本常量） |
| | `feedback` | number | `[0, 0.98]` | 0.3 | 越界双向钳制；上界 0.98 保证反馈环稳定 |
| | `mix` | number | `[0, 1]` | 0.3 | 越界双向钳制 |
| `chorus` | `enabled` | boolean | — | — | 同上，驱动器门控 |
| | `rateHz` | number（Hz） | `[0.01, 20]` | 1 | 越界双向钳制 |
| | `depthMs` | number（ms） | `[0, 50]` | 0 | 越界双向钳制；**基础延迟固定 20 ms（非参数，setCommon(20, …)）** |
| | `mix` | number | `[0, 1]` | 0.4 | 越界双向钳制；**反馈恒为 0（不可配）** |
| `flanger` | `enabled` | boolean | — | — | 同上，驱动器门控 |
| | `rateHz` | number（Hz） | `[0.01, 20]` | 1 | 越界双向钳制 |
| | `depthMs` | number（ms） | `[0, 50]` | 0 | 越界双向钳制；**基础延迟固定 1 ms（非参数，setCommon(1, …)）** |
| | `feedback` | number | `[0, 0.98]` | 0.4 | 越界双向钳制 |
| | `mix` | number | `[0, 1]` | 0.5 | 越界双向钳制 |
| `phaser` | `enabled` | boolean | — | — | 同上，驱动器门控 |
| | `rateHz` | number（Hz） | `[0.01, 20]` | 0.5 | 越界双向钳制 |
| | `depth` | number | `[0, 1]` | 0.5 | 越界双向钳制 |
| | `feedback` | number | `[0, 0.98]` | 0.4 | 越界双向钳制 |
| | `mix` | number | `[0, 1]` | 0.5 | 越界双向钳制 |
| | `stages` | number（整数语义） | `[2, 8]` | 4 | `max(2, min(8, round(stages)))`；语义见 §4.5（各级并行处理同一输入、仅末级输出被采用） |
| `tremolo` | `enabled` | boolean | — | — | 同上，驱动器门控 |
| | `rateHz` | number（Hz） | `[0.01, 30]` | 5 | 越界双向钳制（**上界 30 与 chorus/flanger 的 20 不同**） |
| | `depth` | number | `[0, 1]` | 0.5 | 越界双向钳制 |
| | `mix` | number | `[0, 1]` | 1 | 越界双向钳制 |

补充说明：

- 各效果默认值为类字段内置初值；向量固定完整快照，不依赖缺省；
- `delayMs/depthMs` 等 ms→样本换算在 `setParams` 时一次性完成，处理循环内为常量。

## 四、处理语义

### 4.1 级联结构与门控（模块特有——两支线驱动器必须逐字一致）

```text
级联顺序（引擎接线顺序，固定）：Delay → Chorus → Flanger → Phaser → Tremolo
每级：enabled == true 时对本块缓冲就地处理；enabled == false 时整级跳过（逐位旁路）
```

- **禁用级逐位旁路**：缓冲不被触碰、该级状态不推进——全五级禁用时输出与输入
  **逐位一致**（最强链路锚点，实证）；
- **setParams 无条件调用**（引擎接线事实）：引擎对五个效果**无论 enabled 与否**都调用
  `setParams(子对象)`（`enabled` 字段被效果类自身忽略）；向量驱动器同样对五效果无条件
  setParams，再按 `enabled` 决定是否参与链路；
- 引擎在各自 `enabled` false→true 迁移时调用对应效果的 `reset()`（生命周期事实；
  向量为全新零状态实例，与首次 enable 等价）。

### 4.2 环形延迟线读取（Delay / Chorus / Flanger 共用，两支线必须逐字一致）

```text
readDelay(buf, pos, d):
  d   = clamp(d, 0, size − 1)                  # size = 环形缓冲长度
  i0  = floor(d); frac = d − i0
  idx0 = (pos − i0 + size) % size
  idx1 = (idx0 − 1 + size) % size
  return buf[idx0] × (1 − frac) + buf[idx1] × frac
writeDelay(buf, pos, value): buf[pos] = value    # 读取发生在写入之前
```

- **d < 1 的退化读取区（行为事实）**：`d ∈ [0,1)` 时 `idx0 = pos`，读到的是**上次访问
  该槽位的内容（整环回绕前值，即 size 样本之前）**，而非「0 样本前」——等效于把
  调制负半周钳到「整环延迟」。该读取对 d 连续（frac→0 时回到 buf[idx0]），无数值跳变，
  但与理想分数延迟在 d<1 区间不同——**两支线必须按上式逐字复刻**（实证：chorus
  depthMs > 20 或 flanger depthMs > ~1 时调制负半周进入该区，输出有限且确定）；
- 缓冲长度：Delay = `ceil(fs×2)+1`（最大 2 s）；Chorus = `ceil(fs×0.1)+2`；
  Flanger = `ceil(fs×0.05)+2`。

### 4.3 DelayEffect

```text
逐样本：wet = readDelay(buf, pos, delaySamples)
        writeDelay(buf, pos, x + wet × feedback)
        out = x × (1 − mix) + wet × mix
        pos = (pos + 1) % size
```

- 纯逐样本递推（环形缓冲 + pos）：**分块与整块逐位一致**（实证，跨块状态连续性成立）；
- 反馈写入发生在干湿读出之后（反馈环含一步延迟）。

### 4.4 Chorus / Flanger（共享 ModulatedDelay 核）

```text
setCommon(baseMs, depthMs, rateHz):
  baseDelay    = clamp(baseMs, 0, 100) / 1000 × fs     # chorus 固定传 20，flanger 固定传 1
  depthSamples = clamp(depthMs, 0, 50) / 1000 × fs
  rateHz       = clamp(rateHz, 0.01, 20)

逐样本（processCore）：
  mod  = sin(2π × phase)                                # 双极性 LFO
  d    = baseDelay + depthSamples × mod                 # 可为负 → readDelay 下界钳制（§4.2）
  wet  = readDelay(buf, pos, d)
  writeDelay(buf, pos, x + wet × feedback)              # chorus 的 feedback 恒 0
  out  = x × (1 − mix) + wet × mix
块末：phase = (phase + rateHz × n / fs) % 1             # n = 本块样本数
```

- **LFO 相位按整块步进（模块特有行为事实）**：`lfoValue()` 在循环内读取的 `phase`
  在块内不变——调制量为**块级常量**，块间按 `rateHz × n / fs` 累进。因此 chorus/flanger
  的输出**依赖驱动分块 blockSize**（实证：chunk=333 vs 整块最大差 0.6–0.9 量级），
  两支线必须按冻结向量的同一 blockSize 回放；
- chorus 与 flanger 的差异：基础延迟（20 ms vs 1 ms）、反馈（恒 0 vs 可配）、
  缓冲长度与默认 mix。

### 4.5 PhaserEffect（逐样本 LFO + 全通组 + 输出反馈）

```text
逐样本：
  lfo = 0.5 + 0.5 × sin(2π × phase)                     # 0..1 单极性
  fc  = 200 + 1800 × (0.2 + 0.8 × lfo × depth)          # 中心频率 200..2000 Hz
  a   = (1 − tan(π × fc / fs)) / (1 + tan(π × fc / fs)) # 一阶全通系数
  inL = xL + feedback × lastOutL                        # 上一样本输出的单样本反馈（逐声道）
  inR = xR + feedback × lastOutR
  对 s = 0..stages−1：yL = allpass(inL, stateL, s×2, a)；yR = allpass(inR, stateR, s×2, a)
  lastOutL = yL；lastOutR = yR
  out = x × (1 − mix) + y × mix
  phase = (phase + rateHz / fs) % 1                     # 逐样本步进

allpass(x, state, base, a):
  x1 = state[base]; y1 = state[base + 1]
  y  = −a × x + x1 + a × y1
  state[base] = x；state[base + 1] = y
  return y
```

- **并级结构（行为事实，两支线必须逐字复刻）**：各级全通**并行处理同一输入 `in`**
  （循环体内以 `in` 而非上一级输出喂入），**仅末级的输出被采用**为 `y`——这不是级联。
  实证：stages=7 与 stages=8 输出逐位一致（末级状态槽独立演化、其余级输出被丢弃）；
  `stages` 通过改变「哪一级是末级」影响输出；
- **逐样本 LFO**：与 chorus/flanger 的块级步进不同，phaser 的 phase 逐样本累进，
  输出**不依赖驱动分块**（实证：分块 vs 整块逐位一致）；
- 反馈基准是**最终全通输出**（mix 之前），单样本延迟，逐声道独立（lastOutL/lastOutR）。

### 4.6 TremoloEffect（逐样本 LFO 幅度调制）

```text
逐样本：
  g   = 1 − depth × (0.5 + 0.5 × sin(2π × phase))       # 1−depth .. 1
  out = x × (1 − mix + mix × g)
  phase = (phase + rateHz / fs) % 1                     # 逐样本步进
```

- `mix = 0` 时乘数为精确 1.0，输出与输入逐位一致（实证）；
- 逐样本 LFO：分块与整块逐位一致（实证）。

### 4.7 状态集合与生命周期

- Delay：环形缓冲 ×2 + `pos`；Chorus / Flanger：环形缓冲 ×2 + `pos` + `phase`；
  Phaser：每通道 `stages×2` 状态 + `phase` + `lastOutL/R`；Tremolo：`phase`；
- `setParams` **保留全部状态**（仅钳制参数/换算常量）；
- `reset()` 清零缓冲与相位（各效果各自实现，见源码）；
- 链路级：禁用级状态完全不推进（§4.1）。

### 4.8 实证行为记录（导出工具冻结前逐项验证）

1. **全五级禁用 → 输出与输入逐位一致**（链路驱动器跳过语义，实证）；
2. **delay / phaser / tremolo 分块与整块逐位一致**（纯逐样本递推，实证）；
3. **chorus / flanger 输出依赖驱动分块**（LFO 块级步进，实证 chunk=333 vs 整块最大差
   0.6–0.9 量级）——两支线必须按冻结向量同一 blockSize 回放；
4. **phaser stages=7 与 stages=8 输出逐位一致**（并级结构、仅末级输出被采用，实证）；
5. **flanger depthMs 足够大时调制负半周进入 readDelay 的 d<1 退化读取区**（§4.2，
   实证连续、有限、确定）；
6. **全部效果的极值参数钳制与「直接按生效值配置」逐位一致**（delay/chorus/flanger/
   phaser/tremolo 全维度，含 stages 上下界与 round 语义，实证）；
7. **tremolo mix=0 → 逐位恒等**；chorus 反馈恒 0（写入项 wet×0 精确为零）。

## 五、GWT 行为条款

> 定量断言一律以 `specs/dsp/vectors/mod-effects.<case>.json` 冻结夹具 + 容差公式判定
> （README §3.5），条款内不内嵌具体参数数值与期望值。

### GWT-ME-01：全禁用逐位锚点
- **给定**：五效果 enabled 全为 false（其余参数取激进值随载荷固化，五级 setParams
  仍被无条件调用）
- **当**：送入任意信号
- **则**：输出与输入**逐位一致**（左右声道皆然），全部五级状态不推进——链路驱动器
  跳过语义的最强锚点

### GWT-ME-02：Delay 单独激活（反馈回声族）
- **给定**：仅 delay enabled（delayMs/feedback/mix 生效），其余四级禁用
- **当**：前段有声、后段静音的确定性激励长序列
- **则**：输出呈现延迟 `delaySamples` 的回声族（反馈逐代衰减、环稳定不发散），
  干湿按 mix 混合；回声时间/衰减轨迹整段冻结，任何环形读写序或换算偏差必然超差

### GWT-ME-03：Chorus + Flanger 组合（块级步进 LFO）
- **给定**：仅 chorus 与 flanger enabled（其余三级禁用）；flanger 深度取值使调制负半周
  进入 readDelay 的 d<1 退化读取区（§4.2）
- **当**：常规确定性激励按冻结向量 blockSize 分块处理
- **则**：输出为两级调制延迟的级联结果，全程无 NaN / Infinity、有界；**输出依赖
  blockSize**（LFO 块级步进），两支线按同一块长回放判定；d<1 区的读取语义任何偏差
  （如按「0 样本前」理想化）必然超差

### GWT-ME-04：Phaser + Tremolo 组合（stages 变体）
- **给定**：仅 phaser 与 tremolo enabled；phaser stages 取非默认值（覆盖 round/钳制域内
  变体）、tremolo mix=1
- **当**：常规确定性激励长序列（多采样率覆盖）
- **则**：输出为并级全通调制（含单样本输出反馈）与幅度调制的级联结果；并级结构、
  逐样本 LFO、反馈基准（mix 前的最终全通输出）任何偏差必然超差

### GWT-ME-05：极值参数钳制（随载荷固化）
- **给定**：各效果参数双向越界组合（delayMs/feedback/mix/rateHz/depthMs/depth/stages），
  生效值按 §三钳制域；越界值随禁用级载荷与启用级参数固化
- **当**：常规激励
- **则**：clamp 按生效值精确等效（实证：与直接按边界值配置输出逐位一致），全程无数值事故

### GWT-ME-06：级联顺序与隔离（由向量组整体覆盖）
- **给定**：四个向量分别覆盖「全禁用 / delay 单独 / chorus+flanger / phaser+tremolo」，
  每个效果恰在一个 case 中处于启用状态
- **当**：同一驱动语义（引擎级联顺序、禁用级跳过）逐块处理
- **则**：任一效果的位置放错（级联顺序偏离引擎接线）或禁用级未跳过，都会在至少一个
  case 中产生可测差异

### GWT-ME-07：chorus/flanger 的 blockSize 敏感性（行为事实）
- **给定**：chorus 或 flanger enabled 的任意参数
- **当**：按冻结向量 blockSize 分块回放
- **则**：两支线一致（对拍判定）；改变块长会改变 LFO 步进粒度从而改变输出（实证），
  本组对 chorus/flanger 不主张与块长无关，只主张同一块长下逐块回放可复现

### GWT-ME-08：delay/phaser/tremolo 跨块状态连续性（逐位一致，由单元测试覆盖，不入向量）
- **给定**：三者各自 enabled 的任意参数与输入序列
- **当**：分别按 blockSize=k 分块处理与一次性整块处理（末块可短）
- **则**：两种方式输出**逐位一致**（纯逐样本递推，实证）

### GWT-ME-09：tremolo mix=0 恒等（由单元测试覆盖，不入向量）
- **给定**：tremolo enabled、mix=0、任意 rateHz/depth
- **当**：任意激励
- **则**：输出与输入逐位一致

### GWT-ME-10：静音输入（由单元测试覆盖，不入向量）
- **给定**：任意效果组合 enabled、零状态实例
- **当**：送入全零输入长序列
- **则**：输出全零（Delay 反馈环、Phaser 反馈、各 LFO 相位均无数值泄漏）

### GWT-ME-11：reset 后行为可复现（由单元测试覆盖，不入向量）
- **给定**：已处理过信号的各效果实例
- **当**：reset() 后重放同一输入
- **则**：输出与首次从零状态处理的输出完全一致

### GWT-ME-12：抛错路径（由单元测试覆盖，不入向量）
- **给定**：fs ≤ 0
- **当**：构造任一效果
- **则**：抛 `Error('invalid sample rate')`

## 六、向量用例

**本模块全部定量行为以 `specs/dsp/vectors/mod-effects.<case>.json` + 同名 `.f32` 冻结
夹具为准**；本章仅列预期 case 覆盖面（维度清单，不含具体参数数值）。格式契约见
[`specs/README.md`](../README.md) §三；JSON 合法性由 Schema 校验。

预期覆盖面（case1–case4）：

1. **全禁用恒等锚点**：五效果 enabled 全 false、其余参数取激进值（含 stages 越上界钳制）
   随载荷固化，输出与输入逐位一致；输入为正弦叠加 + 固定种子 LCG 噪声（GWT-ME-01）；
2. **Delay 单独**：仅 delay enabled（短延迟 + 中高反馈 + 干湿混合），burst→静音激励使
   反馈回声族与衰减尾清晰；其余四级禁用（GWT-ME-02）；
3. **Chorus + Flanger 组合**：两者 enabled（chorus 深度取常规域、flanger 深度使负半周
   进入 d<1 退化读取区）；顶层 blockSize 取非平凡值并固定（GWT-ME-03/07）；
4. **Phaser + Tremolo 组合**：两者 enabled（phaser stages 非默认变体、tremolo mix=1）；
   多采样率 44100 覆盖 phaser 全通系数与各相位步进随 fs 变化（GWT-ME-04）。

采样率维度：case4 取 44100，其余 48000。帧数对顶层 blockSize 非整除（含末块短块；
chorus/flanger 所在 case 的 blockSize 为行为参数，两支线必须同值回放）。

.f32 数据按 [README §3.3](../README.md) 四段 planar 小端布局存放；分块驱动方式按 §3.4，
且驱动器必须遵守 §4.1 的级联顺序、禁用级跳过与五级无条件 setParams 语义。

## 七、关联文件

- 总纲契约：[`specs/README.md`](../README.md)
- Schema：[`specs/schema/vector-case.schema.json`](../schema/vector-case.schema.json)
- 行为事实标准：`src/dsp/ModEffects.ts`；参数类型：`src/types.ts`
  （DelaySettings / ChorusSettings / FlangerSettings / PhaserSettings / TremoloSettings /
  ModEffectsSettings）
- 引擎接线（级联顺序与门控事实）：`src/engine/HyperSoundEngine.ts`（buildStages 中
  delay → chorus → flanger → phaser → tremolo 五 stage；各 `active()` 读快照；
  false→true 迁移 reset）
- 参考单元测试：`test/modulation.test.ts`
- 兄弟规格：[biquad](biquad.md) ｜ [limiter](limiter.md) ｜ [compressor](compressor.md) ｜
  [bass-enhancer](bass-enhancer.md) ｜ [mid-side](mid-side.md) ｜ [deesser](deesser.md) ｜
  [eq-chain](eq-chain.md) ｜ [dynamic-eq](dynamic-eq.md) ｜ [loudness-comp](loudness-comp.md)
