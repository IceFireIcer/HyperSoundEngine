# 规格：loudness-comp —— 等响度补偿（ISO 226 简化近似 + biquad 拟合）

> **规格属性**：本文件是双支线共享规格。行为事实标准 = `src/dsp/LoudnessComp.ts`；
> 参数字段名一律以该源码（`LoudnessCompParams`）为准，本规格不得臆造字段。
> 格式契约见 [`specs/README.md`](../README.md) §三，
> 向量合法性由 [`specs/schema/vector-case.schema.json`](../schema/vector-case.schema.json) 约束。

---

## 一、模块概述

- **定位**：链路级等响度补偿——按 ISO 226 等响度曲线的简化 1/3 倍频程近似，将音量相关
  （auto）、场景预设（preset）或用户自定义（custom）目标曲线拟合为至多 6 段 RBJ
  shelf/peaking biquad 级联，并做逐块增益平滑。引擎链中位于 BassEnhancer 之后、IEQ 之前
  （引擎参数 `loudnessCompensation` 组）。
- **出处**：等响度曲线概念源自 ISO 226:2003（人耳频率灵敏度随音量变化）；本实现使用简化的
  1/3 倍频程近似增益表（技术文档 §6：低频 0–12dB、高频 0–6dB，随 volumePercent 线性），
  采用 RBJ Audio EQ Cookbook（公开公式）的 shelf/peaking biquad 拟合。表数据为
  「ISO 226 简化近似」（低频系数 0.35、高频系数 0.15 → 归一化 w(100Hz)=1.0、
  w(10kHz)≈0.43）。本实现为自研 TypeScript 代码。
- **确定性**：同输入同参数同块长必同输出；无随机、无 Date、无 console；process 内零分配
  （6 段 biquad 的系数与 TDF2 状态均预分配）。
- **采样率**：构造时固定；fs ≤ 0 或非有限值抛 `Error('invalid sample rate')`。
- **重要边界（行为事实）**：**本模块没有 `enabled` 字段**——`enabled` 属于引擎层
  `LoudnessCompSettings`，由引擎阶段门控消费（`HyperSoundEngine.ts`：阶段 `active()` 判断
  enabled、重新启用时调用 `reset()`）；模块自身恒处理。向量 `params` 不得包含 `enabled`。

## 二、接口签名（事实标准摘录）

```ts
export interface LoudnessCompParams {
  volumePercent: number                                    // 系统音量 0..100（auto 模式输入）
  maxBoostDb: number                                       // 最大提升 dB，默认 12
  preset: string                                           // preset 模式预设 id（§3.2 六条）
  bands: { frequency: number; gain: number }[]             // custom 模式目标曲线控制点
  mode: CompensationMode                                   // 'auto' | 'preset' | 'custom'（src/types.ts）
  smoothingSeconds: number                                 // 增益平滑时间常数 s，默认 0.2
}

export class LoudnessComp {
  constructor(fs: number)
  setParams(p: LoudnessCompParams): void
  processStereo(l: Float32Array, r: Float32Array): void    // 就地处理立体声
  reset(): void
}
```

## 三、参数表（向量 `params` 快照字段）

向量 JSON 的 `params` 对象固定使用以下六字段；采样率 fs 不进 params，取自向量顶层
`sampleRate` 字段：

| 字段 | 类型 | 有效域（clamp 后生效值） | 默认值¹ | clamp / 边界行为 |
|---|---|---|---|---|
| `volumePercent` | number | `[0, 100]` | 100 | 双向钳制；仅 auto 模式消费（preset/custom 忽略，但仍随载荷固化） |
| `maxBoostDb` | number（dB） | `[0, 24]` | 12 | 双向钳制；仅 auto 模式消费 |
| `preset` | string | flat / bass / vocal / warm / bright / night | 'flat' | 非字符串回退 'flat'；未知 id 回退 flat 曲线（空表 → 全 0 目标，防御逻辑，向量不得依赖非法 id）；仅 preset 模式消费 |
| `bands` | `{frequency, gain}[]` | 见 §4.1.3 custom 分组规则 | [] | 非数组按 [] 处理；仅在 custom 模式消费（frequency/gain 的钳制见 §4.1.3） |
| `mode` | string（三种枚举） | auto / preset / custom | auto | 枚举外值回退 'auto'（防御逻辑，向量不得依赖非法枚举） |
| `smoothingSeconds` | number（s） | `[0.01, 10]` | 0.2 | 双向钳制；逐块平滑时间常数（见 §4.3——**输出依赖 blockSize 的根源**） |

¹ 「默认值」为类字段初始值（`setParams` 必须收到完整快照）；向量必须提供完整六字段。

## 四、处理语义

### 4.1 目标曲线计算（`setParams` 时执行一次，两支线必须逐字一致）

`setParams` 计算目标段数组（至多 6 槽；每槽为 (增益 dB, 频率 Hz, 类型)，类型
0=low shelf / 1=high shelf / 2=peaking），不改动滤波器当前状态：

#### 4.1.1 auto 模式（音量线性等响度）

```text
v = volumePercent / 100
table(f) = maxBoostDb × (1 − v) × w(f)     # 1/3 倍频程中心频点表（20Hz..20kHz 共 31 点）
w(f)：  f ≤ 100        → 1.0
        100 < f < 250  → 1 − (log10(f) − log10(100)) / (log10(250) − log10(100))   # 对数线性 1→0
        250 ≤ f < 2000 → 0
        2000 ≤ f < 10000 → (0.15/0.35) × (log10(f) − log10(2000)) / (log10(10000) − log10(2000))
        f ≥ 10000      → 0.15/0.35（≈0.4286）
```

- 固定取 table(100Hz) 为低频 shelf 目标、table(10kHz) 为高频 shelf 目标；
  中频 peaking 从候选频点 **[315, 630, 1000, 1600, 2500, 4000, 6300]** 中取 |table(f)| ≥ 0.25
  者，按 |增益| 降序（同值按频率升序）取前 4，再按频率升序排列；
- volumePercent=100（v=1）时全部目标为 0：**所有槽目标增益为 0 → 恒等链**
  （见 §4.5 恒等锚点实证）；
- 提升单调性：目标增益 = maxBoostDb × (1−v) × w(f)，volumePercent 越低低频/高频 shelf
  目标越大（线性）。

#### 4.1.2 preset 模式（固定场景曲线）

- 六条预设曲线（控制点 频率→dB，行为标准的一部分，**原样固化**）：

```text
flat  : （空表 → 全 0 目标）
bass  : 63→6, 100→5, 160→4, 250→2.5, 400→1.5, 630→0.5, 1000→0, 2000→0, 4000→−0.5, 8000→−1, 12000→−1.5
vocal : 100→0, 200→0.5, 400→1.5, 800→2.5, 1000→3, 2000→3.5, 3000→3, 5000→2, 8000→1, 12000→0.5
warm  : 63→2, 100→2.5, 200→3, 400→2.5, 800→1.5, 1600→0.5, 3000→0, 6000→−1, 10000→−1.5, 16000→−2
bright: 63→0, 200→0, 500→0.5, 1000→1, 2000→1.5, 4000→2.5, 6300→3, 10000→3, 16000→2.5
night : 63→4, 100→3.5, 200→2.5, 400→1.5, 800→0.5, 1600→0, 3000→−1, 6000→−2, 10000→−2.5, 16000→−3
```

- 31 个 1/3 倍频程中心频点按 **对数线性插值**（控制点先按频率升序排序；频带外取端点值）
  求得 table(f)，其后与 auto 共用同一拟合流程（§4.1.1 后半段：shelf 取点 + peaking 候选）。

#### 4.1.3 custom 模式（用户曲线分组）

```text
low  = bands 中 frequency ≤ 250 的项；lowGain  = 其 gain（各 clamp ±24）的算术平均（无项则 0）
high = bands 中 frequency ≥ 6000 的项；highGain = 同上
mid  = 其余项（250 < frequency < 6000）
```

- |lowGain| ≥ 0.25 → low shelf 目标 (lowGain, 120Hz)；|highGain| ≥ 0.25 → high shelf 目标
  (highGain, 12kHz)；
- mid 项先钳制（frequency → [20, 20000]，gain → ±24），丢弃 |gain| < 0.25 的项，按 |gain|
  降序（同值按频率升序）取前 4，再按频率升序排列为 peaking 段；
- **custom 模式不消费 volumePercent / maxBoostDb / preset**（但三字段仍在参数快照中）。

### 4.2 biquad 拟合（RBJ 公式，见 [biquad](biquad.md)）

- low shelf 120Hz / high shelf 12kHz：RBJ shelf，斜率固定 S=1（α = sin(w0)/2 × √2，
  语义上不含 Q）；
- peaking：RBJ peaking，Q=1.0；
- 频率越界防御：设计频率钳制到 [1, fs × 0.45]（防极点出圆 NaN）；
- 6 槽级联；目标增益为 0 的槽**保持恒等系数 {b0=1, b1=0, b2=0, a1=0, a2=0}**（构造初值），
  TDF2 状态恒为 0。

### 4.3 逐块增益平滑（**输出依赖 blockSize——模块特有行为事实**）

每次 `processStereo(l, r)` 开头（B = 本块帧数 = min(l.length, r.length)）：

```text
alpha = 1 − exp(−B / (smoothingSeconds × fs))
对每槽 i：若 currentGains[i] ≠ targetGains[i]：
    g = currentGains[i] + alpha × (targetGains[i] − currentGains[i])
    若 |g − targetGains[i]| < 1e−9 → g = targetGains[i]      # 收敛钉扎
    currentGains[i] = g ；按 (g, targetFreqs[i], targetTypes[i]) 重算该槽系数（左右共用系数）
随后逐样本滤波
```

- **块长敏感性（实证）**：alpha 随块长变化 → 目标≠当前的过渡期内输出依赖 blockSize。
  因此对爬升型向量而言 blockSize 是行为参数，两支线对拍必须按向量固定的 blockSize 回放；
  目标恒为 0（恒等型）时无重算、输出与 blockSize 无关（分块 vs 整块逐位一致，实证）；
- 首次处理从 currentGains=0（构造初值）出发向目标爬升；稳态段增益趋近目标（1e−9 钉扎）。

### 4.4 立体声映射与滤波次序

- 6 段 TDF2 级联；**左右声道各自独立状态**（bq / bqR 两组 TDF2 状态变量），**系数共享**
  （recomputeCoeffs 同时写左右两组系数）——避免"一链两声道"时另一声道污染滤波器状态；
- 逐样本处理：每样本先整条 6 段跑 L、再整条 6 段跑 R（两组状态互不相交，处理次序不影响
  数值结果，无跨声道耦合）；
- 滤波本身为纯逐样本递推，块内分块不改变滤波运算序列；块级效应仅来自 §4.3 的平滑 alpha。

### 4.5 实证行为记录（导出工具冻结前逐项验证）

1. **auto 且 volumePercent=100 → 输出与输入逐位一致**（实证）：全部目标为 0 → 无槽重算 →
   恒等系数 + 零状态 → `y = x`（所用确定性输入不含 −0）。这是本模块的逐位锚点形态
   （**不是**"近似恒等"）。
2. **auto 目标段实例（volumePercent=20、maxBoostDb=12，fs=48k）**：5 个活动段——
   low shelf +9.6dB@120Hz、high shelf ≈+4.114dB@12kHz、peaking +0.570@2500 / +1.772@4000 /
   +2.933@6300（与 §4.1.1 公式解析值一致，实证核对）。
3. **custom 分组/钳制/丢弃实例**：bands 含 {60,+18}、{30,+200→钳 +24}、{300,+0.1}、
   {1000,−6}、{4000,+9}、{20000,+5} 时：low 组均值 (18+24)/2=+21 → shelf@120；
   |0.1|<0.25 项丢弃；peaking −6@1k、+9@4k；high 组 +5 → shelf@12k（实证核对）。
4. **preset 'night' 为 6 段满配**（MAX_BANDS）：shelf +3.5@120、−2.5@12k + peaking
   +1.845@315 / +0.845@630 / −1.415@4000 / −2.048@6300（正负增益混合，实证核对）。
5. **reset 不复现首次爬升（实证）**：`reset()` 将 currentGains 直接钉到 targetGains 并重算
   系数 → reset 后重放与首次处理（从 0 爬升）**不同**（实测差异显著）。这与多数模块的
   "reset 后复现"语义相反：本模块 reset 的语义是**跳过平滑直接到位**。引擎在阶段重新启用时
   调用 reset 正是消费该语义。平滑爬升路径的复现性由「同参数同块长重放一致」表述。
6. **块长敏感性（实证）**：爬升型参数下 blockSize 384/512/整块三者输出互不相同（差异
   1e−2..1e−1 量级）；恒等型（目标全 0）下分块与整块逐位一致。

## 五、GWT 行为条款

> 定量断言一律以 `specs/dsp/vectors/loudness-comp.*.json` 冻结夹具 + 容差公式判定
> （README §3.5），条款内不内嵌具体参数数值与期望值。

### GWT-LC-01：auto 满音量恒等（逐位锚点）
- **给定**：mode=auto、volumePercent=100（任意 maxBoostDb/smoothingSeconds）
- **当**：送入任意允许输入（多频正弦叠加、确定性伪噪声）
- **则**：全部目标增益为 0，级联为恒等系统，输出与输入**逐位一致**（左右声道皆然）——
  最强跨实现精度锚点，捕获任何目标计算/拟合/平滑路径的偏差

### GWT-LC-02：auto 低音量提升（shelf + peaking 拟合）
- **给定**：mode=auto、volumePercent < 100（低音量）、maxBoostDb 在域内
- **当**：送入覆盖低/中/高频的确定性激励长序列（帧数足够使增益爬升进入稳态段）
- **则**：低频 shelf 与高频 shelf 目标增益为正（= maxBoostDb×(1−v)×w(f)），中频参考区
  （1kHz 附近）目标为 0；输出呈现频段相关提升，精确波形以冻结向量界定（含首段爬升轨迹）

### GWT-LC-03：volumePercent 提升单调性
- **给定**：除 volumePercent 外相同的 auto 参数（如 100 与 20 两条成对向量）
- **当**：同一确定性激励分别处理
- **则**：目标 shelf 增益随 volumePercent 降低而线性增大（§4.1.1 公式）；两向量输出显著
  可区分；全单调性由单元测试覆盖（100/20 两点随向量固化）

### GWT-LC-04：preset 曲线拟合（6 段满配 + 正负增益混合）
- **给定**：mode=preset、preset 取含负中频控制点的曲线（如 night）
- **当**：覆盖相关频点的确定性激励
- **则**：拟合段数达 6 槽上限、peaking 段含负增益（cut）；volumePercent/maxBoostDb 不影响
  目标（仅 auto 消费）；精确波形以冻结向量界定

### GWT-LC-05：custom 分组 / 钳制 / 丢弃语义
- **给定**：mode=custom、bands 含 low/high 组多控制点（gain 越界 → ±24 钳制后求均值）、
  mid 组 |gain|<0.25 项与多项 peaking 控制点
- **当**：宽频确定性激励
- **则**：low/high shelf 取组均值（钳制后）、|gain|<0.25 的 mid 项被丢弃、mid 按 |gain| 降序
  取前 4 再按频率升序；volumePercent/maxBoostDb 不参与；语义随向量载荷固化

### GWT-LC-06：逐块平滑爬升
- **给定**：目标增益非零的任意向量（auto 低音量 / preset / custom）
- **当**：按向量 blockSize 分块处理（首块从 currentGains=0 出发）
- **则**：各段增益按 §4.3 一阶律逐块逼近目标（1e−9 钉扎），无采样级跳变；爬升轨迹由
  冻结向量逐样本界定（时标 smoothingSeconds 的任何偏差——含公式形态、块长耦合——必然超差）

### GWT-LC-07：blockSize 是行为参数（爬升型；模块特有）
- **给定**：目标增益非零的任意参数
- **当**：比较不同 blockSize 的回放
- **则**：输出**依赖 blockSize**（平滑 alpha 随块长变化；实证差异 1e−2..1e−1 量级）——
  两支线对拍必须按向量固定 blockSize 回放；目标恒 0（恒等型）时输出与 blockSize 无关
  （分块 vs 整块逐位一致，实证）

### GWT-LC-08：reset 语义 = 跳过平滑直接到位（行为事实，由单元测试覆盖，不入向量）
- **给定**：已 setParams（目标非零）但未处理或已处理若干块的实例
- **当**：reset() 后继续处理
- **则**：currentGains 被钉到 targetGains 并立即重算系数（无爬升）——reset 后重放与首次
  从零爬升的处理**不同**（实证，见 §4.5.5）；TDF2 状态清零。向量格式无法表达中途 reset，
  由单元测试与引擎启用时机消费该语义

### GWT-LC-09：极值参数无数值事故（由单元测试覆盖，不入向量）
- **给定**：volumePercent/maxBoostDb/smoothingSeconds 双向越界钳制、bands gain 越界 ±24、
  非有限数值回落（clamp 对 NaN/Infinity 返回下限）
- **当**：构造与常规激励
- **则**：全程无 NaN / Infinity、有界

### GWT-LC-10：抛错路径（由单元测试覆盖，不入向量）
- **给定**：fs ≤ 0 或非有限
- **当**：构造 LoudnessComp
- **则**：抛 `Error('invalid sample rate')`

## 六、向量用例

**本模块全部定量行为以 `specs/dsp/vectors/loudness-comp.<case>.json` + 同名 `.f32` 冻结夹具
为准**；本章仅列预期 case 覆盖面（维度清单，不含具体参数数值），最终以导出工具产出并冻结的
内容为唯一判据。格式契约见 [`specs/README.md`](../README.md) §三；JSON 合法性由 Schema 校验。

预期覆盖面（case1–case4）：

1. **auto 满音量恒等锚点**：volumePercent=100，输出与输入逐位一致（GWT-LC-01）；
2. **auto 低音量提升**：volumePercent=20、maxBoostDb=12——5 活动段（双 shelf + 3 peaking）
   含逐块爬升轨迹；与 case1 构成 volumePercent 对照对（GWT-LC-02/03/06）；
3. **custom 分组/钳制/丢弃**：bands 覆盖 low 组均值（含 gain 越界钳制）、mid 丢弃项与
   peaking、high 组；volumePercent 取非中性值以固化"custom 不消费该字段"语义
   （GWT-LC-05）；
4. **preset 曲线 6 段满配**：preset=night（含负增益 peaking；shelf 正负混合），
   volumePercent/maxBoostDb 不参与（GWT-LC-04）。

维度说明：reset 钉扎语义（GWT-LC-08）、极值钳制、ratio 类边界由单元测试覆盖。采样率维度：
四条向量均取 48000（fs 仅进入系数设计与平滑系数；多采样率属单元测试维度，不入本批向量，
与 eq-chain 批次同一取法）。帧数对 blockSize 非整除（含末块短块）。**爬升型向量（case2/3/4）
的输出依赖 blockSize（GWT-LC-07），两支线必须按向量固定的 blockSize 回放。**

.f32 数据按 [README §3.3](../README.md) 四段 planar 小端布局存放；分块驱动方式按 §3.4
（本模块原生 `processStereo` 就地语义）。

## 七、关联文件

- 总纲契约：[`specs/README.md`](../README.md)
- Schema：[`specs/schema/vector-case.schema.json`](../schema/vector-case.schema.json)
- 行为事实标准：`src/dsp/LoudnessComp.ts`；参数类型：`LoudnessCompParams`（同文件）、
  `CompensationMode`（`src/types.ts`）；引擎层开关：`LoudnessCompSettings.enabled`
  （`src/types.ts`，引擎阶段门控消费，模块不读）
- TS 实现契约：`src/dsp/API_SPEC.md` 模块 12
- 基本单元规格：[biquad](biquad.md)（RBJ shelf/peaking 公式与 TDF2 递推）
- 引擎接线（enabled 门控与 reset 时机）：`src/engine/HyperSoundEngine.ts`
  （loudness-compensation 阶段；重新启用时调用 reset）
- 参考单元测试：`test/loudnesscomp.test.ts`
- 兄弟规格：[biquad](biquad.md) ｜ [limiter](limiter.md) ｜ [compressor](compressor.md) ｜
  [bass-enhancer](bass-enhancer.md) ｜ [mid-side](mid-side.md) ｜ [eq-chain](eq-chain.md) ｜
  [reverb-simple](reverb-simple.md) ｜ [fdn-reverb](fdn-reverb.md) ｜ [deesser](deesser.md)
