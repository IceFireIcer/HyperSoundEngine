# 规格：limiter —— 前瞻限幅器 + 真峰值检测

> **规格属性**：本文件是双支线共享规格。行为事实标准 = `src/dsp/Limiter.ts`；
> 参数字段名一律以该源码（`LimiterSettings`）为准。格式契约见 [`specs/README.md`](../README.md) §三，
> 向量合法性由 [`specs/schema/vector-case.schema.json`](../schema/vector-case.schema.json) 约束。

---

## 一、模块概述

- **定位**：输出级 brickwall 限幅器——输入延迟 L（lookahead）个样本，检测窗峰值驱动平滑增益，
  在瞬态到达输出前预先压低增益，实现零过冲限幅且无增益跳变咔哒声。
- **出处**：前瞻限幅结构取自项目《音频算法技术文档》§3.3 设计意图；真峰值检测采用
  ITU-R BS.1770 的 4× 过采样思路（窗函数 sinc 插值取峰）。本实现为自研 TypeScript。
- **确定性**：同输入同参数必同输出；无随机、无 Date、无 console；process 内零分配
  （延迟线 / 检测队列 / 插值历史全部预分配）。
- **采样率**：构造时固定；fs ≤ 0 或非有限值抛 `Error('invalid sample rate')`。

## 二、接口签名（事实标准摘录）

```ts
export class Limiter {
  constructor(fs: number)
  setParams(p: LimiterSettings): void
  processStereo(l: Float32Array, r: Float32Array): void   // 就地处理立体声
  reset(): void
  getReductionDb(): number        // 当前增益衰减 dB（<= 0）
  getLatencySamples(): number     // 引入延迟 = lookahead 样本
}
```

参数类型 `LimiterSettings` 定义于 `src/types.ts`：
`{ enabled, thresholdDb, lookaheadMs, attackMs, releaseMs, truePeak }`。

## 三、参数表（向量 `params` 快照字段）

| 字段 | 类型 | 有效域（clamp 后生效值） | 默认值¹ | clamp / 边界行为 |
|---|---|---|---|---|
| `enabled` | boolean | true / false | true | false 时 `processStereo` 直接返回（不改写缓冲、逐位直通），`getReductionDb` 报 0；**从禁用切回启用瞬间清空全部管线状态**（见 §4.4） |
| `thresholdDb` | number（dB） | `[−60, 0]` | −1 | 越界双向钳制；生效值为线性阈值 `thresholdLin = 10^(thresholdDb/20)` |
| `lookaheadMs` | number（ms） | 样本数 `[0, floor(fs × 0.1)]` | 5 | 先换算并四舍五入 `round(lookaheadMs × fs / 1000)`，再钳到 `[0, floor(fs·0.1)]`；**改变该值会重分配延迟线与检测队列并清空管线** |
| `attackMs` | number（ms） | 生效下限 0.05 ms | 0.5 | 经 `onePoleCoef` 换算：`coef = 1 − exp(−1 / ((max(ms, 0.05)/1000) × fs))`；ms ≤ 0.05 时按 0.05 生效 |
| `releaseMs` | number（ms） | 生效下限 0.05 ms | 150 | 同 attackMs 换算方式 |
| `truePeak` | boolean | true / false | true | true = 4× 过采样 sinc 插值真峰值检测；false = 数字样点峰值 |

¹ 「默认值」为引擎默认快照（`createDefaultParams` 的 limiter 组）与构造函数内置默认一致；
向量必须提供完整六字段快照。

## 四、处理语义

### 4.1 流式时序模型

在输入时刻 idx 已知 x[0..idx]，输出：

```text
y[idx] = x[idx − L] × g[idx]
L      = lookahead（样本数）
g[idx] = 平滑后的增益标量
```

g 由检测窗峰值决定——瞬时峰值在到达输出前约 L 个样本即被检出并预压增益。
目标增益：`target = min(1, thresholdLin / max(peak, 1e−12))`；
平滑规则：target < gain 用 attack 系数一阶逼近，否则用 release 系数一阶逼近。

### 4.2 峰值检测（两种模式）

- **数字峰值模式（truePeak=false）**：检测窗 `[idx − L, idx]` 内 `max(|x_L|, |x_R|)`
  的滑动最大值，用单调递减队列实现（相等值保留最新）；
- **真峰值模式（truePeak=true）**：每声道维护 8 样本历史环，用 3 相位 × 8 taps 的
  Blackman 窗 sinc 插值（窗支撑 [−5, 5]）对窗内信号过采样取峰；检测值定位在 idx−3
  （插值居中所需），有效检测窗为 `[idx − L − 3, idx − 3]`。相位 2 利用 sinc 对称性合并系数
  相同的 tap 对——属数值优化，检测结果与未优化实现在浮点容差 ≤1e−6 内一致，不改变限幅行为。

### 4.3 输出与统计

- 输出 = 延迟 L 个样本后的音频 × 当前平滑增益（就地写回 l/r 数组）；
- `getReductionDb() = 20 × log10(gain)`，恒 ≤ 0；
- **延迟报告**：`getLatencySamples()` 恒等于 lookahead 样本数（两种模式、任意参数下均成立）。

### 4.4 状态集合与管线清理

内部状态：延迟线（左右）、单调检测队列、真峰值插值历史（左右）、增益标量、样本计数器。

以下两个事件会**清空全部管线状态**（增益回到 1、缓冲清零），属于可观测行为：

1. `setParams` 导致延迟线尺寸或队列容量变化（即 lookahead 改变）；
2. enabled 从 false 切回 true（禁用期间延迟线未更新，恢复时清空以丢弃陈旧样本）。

`reset()` 同样清空上述全部状态。中途改参的管线清理语义无法用当前向量格式表达，
由单元测试覆盖；向量仅覆盖单一参数快照下的连续流处理。

## 五、GWT 行为条款

> 定量断言一律以 `specs/dsp/vectors/limiter.*.json` 冻结夹具 + 容差公式判定（README §3.5）。

### GWT-LM-01：禁用即直通
- **给定**：enabled=false 的任意参数组合
- **当**：送入任意信号（含静音、满幅、瞬态）
- **则**：输出与输入**逐位一致**（缓冲未被改写），`getReductionDb()` 报 0

### GWT-LM-02：静音输入静音输出
- **给定**：enabled=true、任意合法阈值与前瞻
- **当**：送入全零输入长序列
- **则**：输出全零；增益沿 release 方向回升趋近 1，衰减报告趋于 0 dB

### GWT-LM-03：满幅稳态正弦被压至阈值附近（brickwall）
- **给定**：enabled=true、thresholdDb=T、稳态正弦输入其峰值高于线性阈值
- **当**：信号达到稳态后
- **则**：输出峰值被压至 T dBFS 附近（attack/release 平滑带来的瞬态残差由冻结向量界定）

### GWT-LM-04：突发瞬态防过冲
- **给定**：enabled=true、lookaheadMs > 0
- **当**：送入突然出现的满幅方波 / 脉冲串
- **则**：因前瞻窗口提前检出峰值并压低增益，输出过冲不超过「阈值 + 容差界」
  （具体量化以冻结向量为准）

### GWT-LM-05：truePeak 开关行为对照
- **给定**：同一满幅正弦输入与相同其余参数
- **当**：分别以 truePeak=true 与 truePeak=false 各跑一条向量
- **则**：true 模式的检测峰值不低于数字样点峰值（限幅更保守或持平）；两模式输出差异可观测，
  差异量以成对冻结向量界定

### GWT-LM-06：极值参数无数值事故
- **给定**：thresholdDb 取两端（−60 与 0）、lookaheadMs=0 与较大值、attackMs/releaseMs 取 0
  （按 0.05 ms 下限生效）等极值组合
- **当**：常规信号激励
- **则**：全程无 NaN / Infinity，输出有界；具体波形以冻结向量为准

### GWT-LM-07：跨块状态连续性（逐位一致）
- **给定**：任意参数与输入序列
- **当**：分别按 blockSize=k 分块处理与一次性整块处理（末块可短）
- **则**：两种方式输出**逐位一致**（延迟线、队列、历史、增益均为纯逐样本递推状态，
  切块不改变运算序列）

### GWT-LM-08：延迟报告恒等
- **给定**：任意合法参数（两种 truePeak 模式）
- **当**：查询 `getLatencySamples()`
- **则**：返回值恒等于 lookahead 换算后的样本数（§三钳制规则）

### GWT-LM-09：reset 后行为可复现
- **给定**：已处理过信号的实例
- **当**：reset() 后重放同一输入
- **则**：输出与首次从零状态处理的输出完全一致

### GWT-LM-10：lookahead 变更清空管线（由单元测试覆盖，不入向量）
- **给定**：流式处理若干块后调用 setParams 改变 lookaheadMs
- **当**：继续处理
- **则**：管线状态被清空（陈旧延迟内容不残留、增益回到 1 起点）

### GWT-LM-11：非法采样率抛错（由单元测试覆盖，不入向量）
- **给定**：fs ≤ 0 或非有限
- **当**：构造 Limiter
- **则**：抛 `Error('invalid sample rate')`

## 六、向量用例

**本模块全部定量行为以 `specs/dsp/vectors/limiter.<case>.json` + 同名 `.f32` 冻结夹具为准**；
本章仅列预期 case 覆盖面（维度清单，不含具体参数数值）。格式契约见
[`specs/README.md`](../README.md) §三；JSON 合法性由 Schema 校验。

预期覆盖面：

1. **典型工作点**：引擎默认参数附近至少一条（enabled=true、中等阈值与前瞻）；
2. **truePeak 成对对照**：同一输入下 truePeak=true/false 各至少一条（GWT-LM-05）；
3. **禁用直通**：enabled=false 至少一条（GWT-LM-01）；
4. **静音输入**至少一条；
5. **满幅激励**：满幅稳态正弦、突发方波/脉冲串各至少一条（GWT-LM-03/04）；
6. **极值参数**：thresholdDb 两端、lookaheadMs=0 与大前瞻、attackMs/releaseMs 极小值，
   各至少一条（GWT-LM-06）;
7. **跨块状态连续性**：blockSize 显著小于 frames 且不整除（末块短块）至少一条（GWT-LM-07）；
8. **多采样率**：默认 48000 之外至少再取一档。

.f32 数据按 [README §3.3](../README.md) 四段 planar 小端布局存放；
分块驱动方式按 §3.4（本模块原生 `processStereo` 就地语义）。

## 七、关联文件

- 总纲契约：[`specs/README.md`](../README.md)
- Schema：[`specs/schema/vector-case.schema.json`](../schema/vector-case.schema.json)
- 行为事实标准：`src/dsp/Limiter.ts`；参数类型：`src/types.ts`（LimiterSettings）；
  TS 实现契约：`src/dsp/API_SPEC.md` 模块 7
- 兄弟规格：[biquad](biquad.md) ｜ [reverb-simple](reverb-simple.md)