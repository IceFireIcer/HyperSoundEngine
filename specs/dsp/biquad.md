# 规格：biquad —— RBJ 双二阶滤波器（系数设计 + TDF2 实现）

> **规格属性**：本文件是双支线共享规格。行为事实标准 = `src/dsp/biquad.ts`；
> 参数字段名一律以该源码为准，本规格不得臆造字段。格式契约见 [`specs/README.md`](../README.md) §三，
> 向量合法性由 [`specs/schema/vector-case.schema.json`](../schema/vector-case.schema.json) 约束。

---

## 一、模块概述

- **定位**：引擎中最基础的单输入单输出递归滤波核，是 EqChain 等级联结构的基本单元。
- **出处**：系数公式取自 Robert Bristow-Johnson《Audio EQ Cookbook》公开公式；
  TDF2（转置直接 II 型）状态更新思路参考 DSPFilters（Vinnie Falco，MIT）；实现为原创 TypeScript。
- **滤波器类型**：peaking / lowshelf / highshelf / lowpass / highpass / bandpass / notch / allpass 共八种。
- **确定性**：同输入同参数必同输出；无 Math.random、无 Date、无 console；
  process/processBlock 内零分配（TDF2 状态为两个双精度标量）。
- **采样率**：在构造时固定；fs ≤ 0 或非有限值抛 `Error('invalid sample rate')`；缺省 fs = 48000。

## 二、接口签名（事实标准摘录）

```ts
export type BiquadType =
  | 'peaking' | 'lowshelf' | 'highshelf' | 'lowpass'
  | 'highpass' | 'bandpass' | 'notch' | 'allpass'

export interface BiquadCoeffs { b0: number; b1: number; b2: number; a1: number; a2: number }

export function designBiquad(type: BiquadType, f0: number, q: number, gainDb: number, fs: number): BiquadCoeffs

export class Biquad {
  constructor(type?: BiquadType, f0?: number, q?: number, gainDb?: number, fs?: number)
  setCoeffs(c: BiquadCoeffs): void
  setParams(type: BiquadType, f0: number, q: number, gainDb: number): void
  process(x: number): number          // TDF2 单样本，返回 y
  processBlock(input: Float32Array, output: Float32Array): void
  reset(): void
  magnitudeAt(freqHz: number, fs: number): number   // |H(e^jw)| 线性幅度（分析用途）
}
```

## 三、参数表（向量 `params` 快照字段）

向量 JSON 的 `params` 对象**固定使用以下四个字段**（对应 `setParams(type, f0, q, gainDb)` 形参名；
采样率 fs 不进 params，取自向量顶层 `sampleRate` 字段）：

| 字段 | 类型 | 有效域（clamp 后生效值） | 默认值¹ | clamp 行为 |
|---|---|---|---|---|
| `type` | string（上列八种枚举） | 八种之一 | 无（构造可选参省略时不设置参数） | 不做 clamp；枚举外取值属非法域，向量不得依赖 |
| `f0` | number（Hz） | `[10, fs/2 × (1 − 1e−9)]` | 1000 | 低于 10 Hz 钳到 10 Hz（过低频率 BLT 系数病态）；≥ fs/2 钳到 `nyq × (1 − 1e−9)` |
| `q` | number | `[1e−6, ∞)` | 1 | `q < 1e−6` 钳到 `1e−6`（必须为正，防除零/不稳定极点） |
| `gainDb` | number（dB） | `[−60, 60]` | 0 | 越界双向钳制；**仅 peaking / lowshelf / highshelf 使用**，其余类型忽略该字段 |

¹ 「默认值」指构造函数可选参的缺省值；向量必须提供完整四字段快照，不依赖缺省。

## 四、处理语义

### 4.1 传递函数与系数设计

双二阶传递函数：

```text
H(z) = (b0 + b1·z⁻¹ + b2·z⁻²) / (1 + a1·z⁻¹ + a2·z⁻²)
```

RBJ 公式要点（w0 = 2π·f0/fs，BLT 预畸变已内置；α = sin(w0)/(2q) 为低通/高通/带通/陷波/全通共用）：

- **lowpass / highpass**：二阶 Butterworth 型响应（RBJ 标准式）；
- **bandpass**：常数 0 dB 峰值增益型（RBJ 两种带通定义之一，b0 = α）；
- **notch**：中心频率处陷波深度趋零；
- **allpass**：全频带幅度恒 1，仅改变相位；
- **peaking / shelf**：使用 `A = 10^(gainDb/40)`；shelf 采用 S = 1 默认斜率
  （α_shelf = sin(w0)/2 × √2），lowshelf 与 highshelf 公式镜像对称；
- 归一化：全部系数除以 a0；若 a0 非法（≤ 0 或非有限）则防御性回退直通系数
  （b0=1，其余 0）。正常参数域内此路径不可达，属实现防御逻辑。

### 4.2 TDF2 差分方程（逐样本递推）

```text
y[n]  = b0·x[n] + s1[n−1]
s1[n] = b1·x[n] − a1·y[n] + s2[n−1]
s2[n] = b2·x[n] − a2·y[n]
```

- 初始状态 s1 = s2 = 0（构造即零状态）；`reset()` 将两状态归零；
- `processBlock(input, output)` 要求两数组等长，否则抛
  `Error('biquad: input/output length mismatch')`。

### 4.3 参数更新与状态语义

- `setParams(...)` 重算系数并**即时生效**；TDF2 状态 s1/s2 **保留不清零**
  （换系数不清历史，音频上表现为无缝切换）；
- `setCoeffs(...)` 直接写入外部系数，同样不动状态。

### 4.4 延迟报告

Biquad **不引入算法延迟**（无前瞻、无延迟线；模块未提供延迟报告接口）。
滤波器相位响应带来的等效群延迟属于频域特性，不属于延迟报告范畴，也不参与向量对齐。

## 五、立体声映射规则（模块特有）

`src/dsp/biquad.ts` 的 TS 实现是**单声道核**（`processBlock`，无 `processStereo`）。
为满足共享向量契约的 `processStereo` 统一语义，本模块的向量行为定义为：

1. 左、右声道各持一个**独立** `Biquad` 实例——以相同参数构造、相同零初始状态；
2. 每个 `blockSize` 块内，分别对两实例调用 `processBlock`（各处理各自声道）；
3. 因此任一声道的行为与其单声道参考行为完全一致，两声道状态互不影响、互不串扰；
4. Rust 支线实现必须采用同一映射（左右独立实例、独立状态），否则对拍必然失败。

## 六、GWT 行为条款

> 定量断言一律以 `specs/dsp/vectors/biquad.*.json` 冻结夹具 + 容差公式判定（README §3.5），
> 条款内不内嵌具体数值。

### GWT-BQ-01：peaking 零增益全频平坦
- **给定**：type='peaking' 且 gainDb=0（f0、q 取任意合法值）
- **当**：送入任意允许输入（正弦、扫频、脉冲、满幅序列）
- **则**：幅频响应处处为 1（线性），时域输出与输入在容差公式意义下一致（分子分母多项式解析相同）

### GWT-BQ-02：peaking 中心频率提升量符合设定
- **给定**：type='peaking'、gainDb=g（g≠0）、f0=F、q=Q
- **当**：以 F 为中心的稳态正弦激励
- **则**：稳态幅度趋近线性 `10^(g/20)`（精确判定引用冻结向量）

### GWT-BQ-03：lowpass 阻带衰减单调有效
- **给定**：type='lowpass'、f0=F、q=Q
- **当**：远高于 F 的正弦激励
- **则**：输出相对输入明显衰减（衰减深度引用冻结向量），且不出现增益抬升

### GWT-BQ-04：shelf 渐近平台增益等于设定值
- **给定**：type='lowshelf' 或 'highshelf'、gainDb=g
- **当**：沿通带方向（lowshelf 向低频端、highshelf 向高频端）远离转折频率激励
- **则**：幅度渐近线性 `10^(g/20)`；过渡带形状由冻结向量界定

### GWT-BQ-05：bandpass 为常数 0 dB 峰值型
- **给定**：type='bandpass'
- **当**：中心频率 f0 处稳态正弦激励
- **则**：幅度趋近 1（0 dB），带宽随 q 收窄

### GWT-BQ-06：notch 中心频率深陷波
- **给定**：type='notch'、f0=F
- **当**：F 处稳态正弦激励
- **则**：输出幅度趋近 0；偏离 F 的频点不受深衰影响

### GWT-BQ-07：allpass 幅度恒一仅有相移
- **给定**：type='allpass'、任意合法 f0/q
- **当**：任意稳态正弦激励
- **则**：输出幅度与输入一致（容差内），相位被改变

### GWT-BQ-08：极值参数钳制且无数值事故
- **给定**：任一类型，分别取 f0 < 10Hz、f0 ≥ fs/2、q = 0、q 极大、gainDb = ±120 等越界值
- **当**：setParams 后送入常规信号
- **则**：clamp 按 §三生效（等效于取边界值处理），全程不产生 NaN / Infinity，输出有界

### GWT-BQ-09：静音输入零输出且状态保持零
- **给定**：任意合法参数、全新实例（或刚 reset）
- **当**：送入全零输入
- **则**：输出逐位为全零，内部状态保持为零

### GWT-BQ-10：满幅输入有界不发散
- **给定**：含高增益设置的参数（如 peaking 大增益、低 q）
- **当**：送入 |x| ≤ 1 的满幅正弦 / 方波长序列
- **则**：输出可为放大值但始终有限（无 NaN / Infinity），长时间运行不发散（稳定极点保证）

### GWT-BQ-11：分块状态连续性（逐位一致）
- **给定**：任意参数与输入序列
- **当**：分别按 blockSize=k 分块处理与一次性整块处理（k 整除与否均可，末块可短）
- **则**：两种方式输出**逐位一致**（TDF2 为纯逐样本递推，切块不改变运算序列）

### GWT-BQ-12：reset 后行为可复现
- **给定**：已处理过任意信号的实例
- **当**：调用 reset() 后重放同一输入序列
- **则**：输出与首次从零状态处理的输出完全一致

### GWT-BQ-13：非法采样率抛错（由单元测试覆盖，不入向量）
- **给定**：fs ≤ 0 或非有限
- **当**：构造 Biquad 或调用 designBiquad
- **则**：抛 `Error('invalid sample rate')`

### GWT-BQ-14：processBlock 长度不匹配抛错（由单元测试覆盖，不入向量）
- **给定**：input.length ≠ output.length
- **当**：调用 processBlock
- **则**：抛 `Error('biquad: input/output length mismatch')`

## 七、向量用例

**本模块全部定量行为以 `specs/dsp/vectors/biquad.<case>.json` + 同名 `.f32` 冻结夹具为准**；
本章仅列预期 case 覆盖面（维度清单，不含具体参数数值），最终以导出工具产出并冻结的内容为唯一判据。
格式契约见 [`specs/README.md`](../README.md) §三；JSON 合法性由 Schema 校验。

预期覆盖面：

1. **八种类型典型工作点**：每种 BiquadType 至少一条向量（含 peaking/shelf 的非零 gainDb）；
2. **peaking 零增益平坦**（GWT-BQ-01 对应）至少一条；
3. **clamp 生效极值**：f0 低于下限、f0 接近/越过奈奎斯特、q 过小、gainDb 双向越界，各至少一条；
4. **静音输入**：全零输入至少一条；
5. **满幅输入**：|x|=1 正弦（及一种方波类序列）至少各一条；
6. **跨块状态连续性**：blockSize 显著小于 frames 且不整除（末块短块）至少一条；
7. **多采样率**：默认 48000 之外至少再取一档采样率；
8. 输入序列由导出工具确定（确定性生成，禁止随机时钟依赖），冻结后即为唯一基线。

> 已落地补充：case4 以 **44100 Hz** 提供 highshelf 典型音乐性参数向量（+3dB@8kHz Q0.707），补齐第 7 条「多采样率」维度——既有三例（case1–case3）均为 48000 Hz。

.f32 数据按 [README §3.3](../README.md) 四段 planar 小端布局存放；
分块驱动方式按 §3.4（本模块经 §五立体声映射执行）。

## 八、关联文件

- 总纲契约：[`specs/README.md`](../README.md)
- Schema：[`specs/schema/vector-case.schema.json`](../schema/vector-case.schema.json)
- 行为事实标准：`src/dsp/biquad.ts`；TS 实现契约：`src/dsp/API_SPEC.md` 模块 2
- 兄弟规格：[limiter](limiter.md) ｜ [reverb-simple](reverb-simple.md)