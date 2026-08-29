# 规格：deesser —— 动态齿音抑制（De-esser）

> **规格属性**：本文件是双支线共享规格。行为事实标准 = `src/dsp/Deesser.ts`；
> 参数字段名一律以该源码消费的 `DeesserSettings`（定义于 `src/types.ts`）为准，本规格不得
> 臆造字段。格式契约见 [`specs/README.md`](../README.md) §三，
> 向量合法性由 [`specs/schema/vector-case.schema.json`](../schema/vector-case.schema.json) 约束。

---

## 一、模块概述

- **定位**：链路级动态齿音抑制——侧链带通提取齿音频段 → 包络检测 → dB 域阈值压缩 →
  分带（推荐）或宽带增益施加。引擎链中位于 Pre-EQ 之后、Compressor 之前。
- **出处**：侧链带通 + 包络检测 + dB 域阈值压缩 + 分带/宽带施加的设计思路取自 Stanford
  EE264 数字音频处理课程公开的 de-esser 设计与项目《音频算法技术文档》§4；分带交叉采用
  Linkwitz-Riley 4 阶结构（每级 RBJ Q=0.7071 低通/高通，公式见 [biquad](biquad.md)）。
  本实现为全新自研 TypeScript 代码。
- **确定性**：同输入同参数必同输出；无随机、无 Date、无 console；process 内零分配
  （状态为包络标量 + 9 个 Biquad 实例的 TDF2 状态）。
- **采样率**：构造时固定；fs ≤ 0 或非有限值抛 `Error('invalid sample rate')`。

## 二、接口签名（事实标准摘录）

```ts
// src/types.ts
export interface DeesserSettings {
  enabled: boolean
  centerHz: number      // 侧链中心频率 Hz，默认 6000（4–8kHz 齿音频段）
  q: number             // 侧链带通 Q，默认 0.7
  thresholdDb: number   // 触发阈值 dB，默认 -30
  ratio: number         // 压缩比率，默认 8
  attackMs: number      // attack ms，默认 1
  releaseMs: number     // release ms，默认 80
  splitBand: boolean    // true=分带式（只压高频带，推荐）/ false=宽带式
  mix: number           // 效果混合 0..1
  sidechainEnabled?: boolean  // 是否使用外部 sidechain 检测（默认 false）
}

export class Deesser {
  constructor(fs: number)
  setParams(p: DeesserSettings): void
  // 就地处理立体声；传入 sideL/sideR 时以外部 sidechain 信号驱动检测
  processStereo(l: Float32Array, r: Float32Array, sideL?: Float32Array, sideR?: Float32Array): void
  reset(): void
}
```

## 三、参数表（向量 `params` 快照字段）

| 字段 | 类型 | 有效域（clamp 后生效值） | 默认值¹ | clamp / 边界行为 |
|---|---|---|---|---|
| `enabled` | boolean | true / false | 构造内置 true；引擎默认快照见 types | **false 时 `processStereo` 首行直接返回**：缓冲不被改写（逐位直通），所有滤波器/包络状态不推进 |
| `centerHz` | number（Hz） | `[100, fs × 0.45]` | 6000 | 越界双向钳制（上界随采样率变化） |
| `q` | number | `[0.1, 20]` | 0.7 | 越界双向钳制 |
| `thresholdDb` | number（dB） | `[−80, 0]` | −30 | 越界双向钳制 |
| `ratio` | number | `[1, 100]` | 8 | 越界双向钳制；`ratio = 1` 时压缩量恒为 0（invRatio=0，同 compressor GWT-CP-08 语义） |
| `attackMs` | number（ms） | 生效下限 0.05 ms | 1 | 经 `onePoleCoef` 换算：`coef = 1 − exp(−1 / ((max(ms, 0.05)/1000) × fs))` |
| `releaseMs` | number（ms） | 生效下限 1 ms | 80 | 经 `onePoleCoef(p.releaseMs, fs, 1)` 换算（release 的下限 floor 为 1 ms，**与 attack 的 0.05 ms 不同**） |
| `splitBand` | boolean | true / false | true | 分带式 / 宽带式施加（见 §4.4） |
| `mix` | number | `[0, 1]` | 1 | 越界双向钳制；干湿混合 `out = x + mix × (processed − x)` |
| `sidechainEnabled` | boolean | true / false | false | **模块本身不读取该字段**——它是引擎接线层的标志：置 true 且引擎 sidechain 激活时，调用方以四参形式提供外部检测信号（见 §4.5）；向量驱动器据其决定调用形态（见 §4.6） |

¹ 「默认值」为构造函数内置默认（构造快照 `{ enabled: true, centerHz: 6000, q: 0.7,
thresholdDb: -30, ratio: 8, attackMs: 1, releaseMs: 80, splitBand: true, mix: 1 }`）。
向量必须提供完整十字段快照（`sidechainEnabled` 为可选字段，但向量固定显式给出）。

## 四、处理语义

### 4.1 侧链检测信号（单声道和 → 带通）

逐样本（`enabled=false` 时整个循环跳过）：

```text
dl = sideL[i]（四参调用时）或 xl（两参调用时）
dr = sideR[i]（四参调用时）或 xr（两参调用时）
s  = bp(0.5 × (dl + dr))          # 单声道和 → RBJ 带通（常数 0dB 峰值增益型, centerHz/q）
```

- 检测信号是**单声道和的带通输出**（不是联合峰值、不是逐声道检测）；带通为 RBJ
  constant-peak 型（中心增益 0dB，公式见 [biquad](biquad.md)）；
- `bp` 系数在 `setParams` 时按钳制后的 centerHz/q 重算。

### 4.2 包络检测与增益计算（两支线必须逐字一致）

```text
p = s × s                                       # 平方包络
env += (p > env ? attackCoef : releaseCoef) × (p − env)   # 一阶 attack/release 分段跟随
levelDb = 10 × log10(env + 1e-12)               # 功率域 dB（+1e-12 静音地板防 log(0)）
over      = levelDb − thresholdDb
reduction = over > 0 ? over × (1 − 1/ratio) : 0
g         = 10^(−reduction / 20)
```

- `env` 为**单标量立体声联动**状态：左右声道共用同一检测/包络/增益；
- `attackCoef`/`releaseCoef` 在 `setParams` 时按 §三换算，处理循环内为常量。

### 4.3 分带交叉结构（splitBand=true）

- 交叉截止 `xo = clamp(centerHz × 0.6, 2500, fs × 0.45)`；
- 每通道 2 级 Q=0.7071 低通 + 2 级 Q=0.7071 高通（Linkwitz-Riley 4 阶；左右通道滤波器
  状态各自独立，共 8 个 Biquad 实例）；
- 施加：`processed = LP2(x) + g × HP2(x)`——低频带原样、高频带乘 g。g=1 时 LP2+HP2 为
  全通（幅度不变、相位旋转）。

### 4.4 增益施加与 mix

```text
splitBand=true ： processedL = lowL + g × highL ；processedR = lowR + g × highR
splitBand=false： processedL = xl × g ；processedR = xr × g        # 宽带：整体乘 g
out = x + mix × (processed − x)                                    # 就地写回
```

- 分带模式下**中低频带不受 g 影响**（齿音频段被精确压制）；宽带模式下**全频带同增益**。
- `mix=0` 时输出数值上回到输入（单元测试覆盖）。

### 4.5 sidechain 语义（外部检测驱动）

- `sidechainEnabled` 属于 `DeesserSettings`，但 **`Deesser` 类自身不读取它**；它由引擎接线层
  消费：`src/engine/HyperSoundEngine.ts` 在 sidechain 激活且该标志为 true 时，以
  `processStereo(L, R, sideL, sideR)` 四参形式提供外部检测信号；
- 模块行为：**提供 sideL/sideR 时，检测信号改用外部声道（dl = sideL[i]、dr = sideR[i]），
  音频路径仍处理 l/r 本体**；未提供时退化为内部单声道和检测（dl/dr 取 l/r 自身）；
- sidechain 内容不在模块内定义——它是调用方的接线决策。

### 4.6 向量驱动器语义（模块特有，两支线加载器必须一致实现）

**当向量 `params.sidechainEnabled === true` 时**，向量驱动器（导出工具与两支线门禁）按
compressor 规格 §4.5 同一规则构造 sidechain 通道——**单声道和（mono sum）派生**，取本块
就地处理前的原始输入：

```text
sideL[n] = sideR[n] = inL[n] + inR[n]     # n 为块内样本下标；加法以双精度执行后直接喂给模块
```

- 派生加法必须以**双精度（f64）**执行；sideL 与 sideR 内容相同（模块对 side 通道只读）；
- 快照时机：必须在就地处理**之前**完成派生；
- **当 `sidechainEnabled` 为 false 或缺省时**，以两参形式 `processStereo(l, r)` 调用（内部
  单声道和检测）。**本批冻结向量（case1–case4）全部为两参形态**；本规则为未来
  sidechainEnabled=true 向量的两支线一致实现契约。

### 4.7 状态集合与生命周期

内部状态：包络 `env`（标量）+ 1 个侧链带通 + 8 个分带交叉 Biquad 的 TDF2 状态。

- `setParams` **保留包络与滤波器状态**（参数即时生效、不清历史，避免改参爆音）——
  仅重算系数（含带通、8 个交叉滤波器与 attack/release 系数）；
- `enabled = false` 时逐样本循环整体跳过：缓冲不被改写（逐位直通），**所有状态不推进**；
- `reset()` 将 `env` 归零并 reset 全部 9 个 Biquad。

### 4.8 实证行为记录（导出工具冻结前逐项验证）

1. **enabled=false → 输出与输入逐位一致**（实证：提前返回，缓冲零改写）。
2. **阈下 g=1 的分带输出是全通重构，非逐位恒等**（实证）：env 低于阈值时 reduction=0、g=1，
   输出 = LP2(x)+HP2(x)——幅度不变（实测全序列 RMS 变化 <0.001dB）但相位经 LR-4 旋转，
   逐样本差异可达 1e−2 量级。故"不衰减"的向量语义是**幅度不变**，不是逐位一致。
3. **分带 vs 宽带成对可区分**（实证）：同一激励下，分带模式低频带成分保持（200Hz 正弦
   RMS 变化 −0.00dB）而齿音频段衰减；宽带模式全频带同增益衰减（200Hz 成分与齿音成分
   衰减量一致，均为 g）。齿音衰减量与 g 由冻结向量界定。
4. **跨块状态连续性逐位成立**（实证：env 与全部滤波器均为逐样本递推，分块 vs 整块逐位一致）。
5. 检测信号为**单声道和的带通**（实证语义，非联合峰值）：单侧声道的齿音能量足以驱动两侧
   共同的 g。

## 五、GWT 行为条款

> 定量断言一律以 `specs/dsp/vectors/deesser.*.json` 冻结夹具 + 容差公式判定（README §3.5），
> 条款内不内嵌具体参数数值与期望值。

### GWT-DE-01：禁用即直通（逐位锚点）
- **给定**：enabled=false 的任意参数组合（其余参数取激进值随载荷固化）
- **当**：送入任意信号（含齿音频带能量）
- **则**：输出与输入**逐位一致**（左右声道皆然），全部内部状态不推进

### GWT-DE-02：合成齿音分带衰减
- **给定**：enabled=true、splitBand=true、阈值/比率使齿音频带稳态包络高于阈值
- **当**：送入 4–8kHz 频带限定的确定性合成齿音（固定频点正弦族 + 固定种子 LCG 相位，
  非 Math.random）长序列，伴低频正弦成分
- **则**：齿音成分进入稳态衰减（衰减深度以冻结向量界定）；低频带成分幅度保持（实证
  ≈0dB 变化，仅 LR-4 全通相位旋转）；全程无 NaN / Infinity

### GWT-DE-03：splitBand 成对对照
- **给定**：两条参数仅 `splitBand` 不同的成对向量（其余字段与输入完全相同）
- **当**：同一确定性激励分别处理
- **则**：分带式只压高频带（低频带保持），宽带式全频带同增益衰减——低频成分的输出在两式间
  显著可区分（驱动器漏做交叉/误用宽带路径必然超差）

### GWT-DE-04：阈下不衰减（幅度不变，非逐位）
- **给定**：enabled=true、输入电平使包络稳态低于阈值（reduction 恒 0、g 恒 1）
- **当**：送入低电平齿音 + 低频成分长序列
- **则**：输出幅度与输入一致（RMS 变化 ≈0），但分带模式下输出为 LR-4 全通重构——
  **相位旋转，非逐位一致**（实证，见 §4.8.2）；期望波形以冻结向量界定

### GWT-DE-05：极值参数钳制
- **给定**：centerHz 越上界（按 fs×0.45 生效）、q 越下界（0.1 生效）、attackMs/releaseMs
  取 0（分别按 0.05 ms / 1 ms 下限生效）、thresholdDb/ratio/mix 越界组合
- **当**：常规激励
- **则**：clamp 按生效值精确等效，全程无数值事故；钳制语义随向量载荷固化

### GWT-DE-06：ratio=1 恒不压缩（由单元测试覆盖，不入向量）
- **给定**：ratio=1（含越下界取值）、任意阈值与输入电平
- **当**：任意激励
- **则**：invRatio=0 → reduction 恒为 0、g 恒为 1，输出仅为分带交叉或宽带直通形态

### GWT-DE-07：跨块状态连续性（逐位一致）
- **给定**：任意参数与输入序列
- **当**：分别按 blockSize=k 分块处理与一次性整块处理（末块可短）
- **则**：两种方式输出**逐位一致**（包络与滤波器均为纯逐样本递推，切块不改变运算序列；实证）

### GWT-DE-08：外部 sidechain 驱动（向量驱动器契约）
- **给定**：sidechainEnabled=true 的向量（**本批冻结向量不含此形态**）
- **当**：按 §4.6 单声道和规则派生 sidechain 并四参调用
- **则**：检测由派生信号驱动、音频路径仍处理输入本体；两支线加载器必须按同一 §4.6 规则实现，
  未来新增该形态向量时自动纳入对拍

### GWT-DE-09：mix 干湿边界（由单元测试覆盖，不入向量）
- **给定**：mix=0、enabled=true、任意处理参数
- **当**：任意激励
- **则**：输出与输入一致（`out = x + 0 × (processed − x)`）

### GWT-DE-10：静音输入静音输出（由单元测试覆盖，不入向量）
- **给定**：enabled=true、任意合法参数
- **当**：送入全零输入长序列
- **则**：输出全零；env 沿 release 衰减趋 0（levelDb 地板由 `env + 1e−12` 保证，不产生 NaN）

### GWT-DE-11：reset 后行为可复现（由单元测试覆盖，不入向量）
- **给定**：已处理过信号的实例
- **当**：reset() 后重放同一输入
- **则**：输出与首次从零状态处理的输出完全一致（env 与 9 个 Biquad 状态全部清零）

### GWT-DE-12：抛错路径（由单元测试覆盖，不入向量）
- **给定**：fs ≤ 0 或非有限
- **当**：构造 Deesser
- **则**：抛 `Error('invalid sample rate')`

## 六、向量用例

**本模块全部定量行为以 `specs/dsp/vectors/deesser.<case>.json` + 同名 `.f32` 冻结夹具为准**；
本章仅列预期 case 覆盖面（维度清单，不含具体参数数值）。格式契约见
[`specs/README.md`](../README.md) §三；JSON 合法性由 Schema 校验。

预期覆盖面（case1–case4）：

1. **禁用恒等锚点**：enabled=false，输出与输入逐位一致；输入含合成齿音与低频成分
   （GWT-DE-01）；
2. **分带重衰减**：enabled=true、splitBand=true，左声道为持续合成齿音（4–8kHz 固定频点
   正弦族 + 固定种子 LCG 相位）、右声道为低频正弦——齿音衰减、低频保持（GWT-DE-02）；
3. **宽带对照**：与 2 完全成对（参数与输入全同、仅 splitBand=false），低频成分同被衰减
   （GWT-DE-03）；
4. **阈下不衰减 + 钳制极值**：低电平输入使包络低于阈值（g 恒 1、幅度不变、相位旋转），
   同时 centerHz/q/attackMs/releaseMs 多项越界钳制入载荷；多采样率 44100 覆盖
   （attack/release 系数与 centerHz 上界随 fs 变化）（GWT-DE-04/05）。

维度说明：sidechainEnabled=true 形态本批不含（§4.6 规则为未来向量契约）；ratio=1、mix=0、
静音输入、reset 复现由单元测试覆盖。采样率维度 case4 取 44100，其余 48000。帧数对
blockSize 非整除（含末块短块）。

.f32 数据按 [README §3.3](../README.md) 四段 planar 小端布局存放；分块驱动方式按 §3.4
（本模块原生 `processStereo` 就地语义）；sidechain 派生规则（若未来使用）按 §4.6。

## 七、关联文件

- 总纲契约：[`specs/README.md`](../README.md)
- Schema：[`specs/schema/vector-case.schema.json`](../schema/vector-case.schema.json)
- 行为事实标准：`src/dsp/Deesser.ts`；参数类型：`src/types.ts`（DeesserSettings）；
  TS 实现契约：`src/dsp/API_SPEC.md` 模块 5
- 基本单元规格：[biquad](biquad.md)（带通 / LR-4 交叉用的低通高通公式与 TDF2 递推）
- sidechain 驱动器规则先例：[compressor](compressor.md) §4.5（同构单声道和派生）
- 引擎接线（sidechain 消费方）：`src/engine/HyperSoundEngine.ts`（deesser 阶段按
  `sidechainEnabled` 决定四参/两参调用）
- 参考单元测试：`test/deesser.test.ts`
- 兄弟规格：[biquad](biquad.md) ｜ [limiter](limiter.md) ｜ [compressor](compressor.md) ｜
  [bass-enhancer](bass-enhancer.md) ｜ [mid-side](mid-side.md) ｜ [eq-chain](eq-chain.md) ｜
  [reverb-simple](reverb-simple.md) ｜ [fdn-reverb](fdn-reverb.md) ｜ [loudness-comp](loudness-comp.md)
