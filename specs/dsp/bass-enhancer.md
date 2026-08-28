# 规格：bass-enhancer —— 虚拟低频增强（谐波生成 + 低音下潜）

> **规格属性**：本文件是双支线共享规格。行为事实标准 = `src/dsp/BassEnhancer.ts`；
> 参数字段名一律以该源码（`BassEnhancerSettings`，定义于 `src/types.ts`）为准。
> 格式契约见 [`specs/README.md`](../README.md) §三，
> 向量合法性由 [`specs/schema/vector-case.schema.json`](../schema/vector-case.schema.json) 约束。

---

## 一、模块概述

- **定位**：虚拟低音增强器——对低通提取的低频带做非线性谐波生成（Missing Fundamental
  心理声学：大脑从 2/3/4 次谐波重建基频音高），谐波路径提供感知低频，低音下潜路径
  （lowBoostDb）提供真实低频能量提升。
- **出处**：IEEE 虚拟低音论文思路（Gerstle et al. / "Synthesis of polynomial-based nonlinear
  device..." / "Virtual Bass Enhancement Based on Harmonics Control"）及项目《音频算法技术文档》
  §5；本实现为自研 TypeScript。
- **确定性**：同输入同参数必同输出；无随机、无 Date、无 console；process 内零分配
  （四个内部 Biquad 实例在构造时创建）。
- **采样率**：构造时固定；fs ≤ 0 或非有限值抛 `Error('invalid sample rate')`。
  **注意**：内部滤波器系数设计采样率与本模块构造采样率解耦，见 §4.3——这是对拍必须
  复刻的事实标准行为。

## 二、接口签名（事实标准摘录）

```ts
export class BassEnhancer {
  constructor(fs: number)
  setParams(p: BassEnhancerSettings): void
  processStereo(l: Float32Array, r: Float32Array): void   // 就地处理立体声
  reset(): void
}
```

参数类型 `BassEnhancerSettings` 定义于 `src/types.ts`：
`{ enabled, cutoffHz, q, harmonicType, harmonicGain, mix, levelDb, lowBoostDb? }`；
`harmonicType: 'odd' | 'even' | 'atan' | 'soft'`（`HarmonicType`）。

## 三、参数表（向量 `params` 快照字段）

| 字段 | 类型 | 有效域（clamp 后生效值） | 默认值¹ | clamp / 边界行为 |
|---|---|---|---|---|
| `enabled` | boolean | true / false | 构造内置 true；引擎默认快照 false | false 时 `processStereo` 直接返回（不改写缓冲、逐位直通），内部滤波器状态不更新 |
| `cutoffHz` | number（Hz） | `[20, fs × 0.45]` | 90 | 越界双向钳制；决定低通提取带与谐波整形高通下限（见 §4.3） |
| `q` | number | `[0.1, 20]` | 0.7 | 越界双向钳制；仅作用于低通（谐波高通固定 Q 0.707） |
| `harmonicType` | string（上列四种枚举） | 四种之一 | 'odd' | 不做 clamp；枚举外取值属非法域，向量不得依赖（防御分支按 odd 处理） |
| `harmonicGain` | number | `[0, 1]` | 0.6 | 越界双向钳制 |
| `mix` | number | `[0, 1]` | 0.5 | 越界双向钳制 |
| `levelDb` | number（dB） | `[−6, 6]` | 0 | 越界双向钳制；生效值为线性 `levelLin = 10^(levelDb/20)`，仅作用于谐波路径 |
| `lowBoostDb` | number（dB） | `[−6, 12]` | 0 | 越界双向钳制；**可选字段**——缺省或非有限值（`Number.isFinite` 防御）按 0 处理（兼容旧参数快照），防 NaN 污染 |

¹ 「默认值」为构造函数内置默认（`applyParams` 构造快照）；引擎默认快照
（`createDefaultParams` 的 bassEnhancer 组）中 `enabled` 为 false，其余字段一致。
向量必须提供完整八字段快照（`lowBoostDb` 虽为可选，向量固定显式给出）。

## 四、处理语义

### 4.1 每通道信号流

左右声道各自独立走同构路径（无交叉串扰）：

```text
b = LPF(x)                    # 低通提取低频带：lowpass，截止 cutoffHz、Q = q
h = HPF(NL(b))                # 非线性谐波生成 + 高通整形（见 §4.2 / §4.3）
out = x + k × h + lowLin × b  # 混合：dry 不变 + 谐波路径 + 低音下潜路径
k = mix × harmonicGain × levelLin
lowLin = 10^(lowBoostDb / 20) − 1
```

- dry 主体不经任何处理，谐波与下潜均为加性叠加；
- 谐波路径电平 = `mix × harmonicGain × levelLin` 三者连乘；
- 低音下潜路径以 `lowLin` 混回低频带：等价于以 cutoffHz 为中心的 low-shelf 真实低频提升；
  `lowBoostDb > 0` 提升、`= 0` 关闭、`< 0` 衰减低频带。

### 4.2 谐波非线性函数（仅作用于低频带，避免全频互调）

| harmonicType | 公式 | 谐波特征 |
|---|---|---|
| `odd` | `x³` | 奇次谐波为主（3 次） |
| `even` | `\|x\|`（全波整流） | 偶次谐波为主（2 次）+ DC（DC 由后级高通去除） |
| `atan` | `atan(√\|x\|) × sign(x)` | ATSR 器件曲线，奇次谐波，解析可控 |
| `soft` | `tanh(2x)` | 软削波，奇次谐波，幅度衰减快 |

### 4.3 内部滤波器（事实标准关键条款）

模块内部持四个 `Biquad` 实例（lpL/lpR/hpL/hpR），在每次 `setParams` 时重设系数：

- 低通（lpL/lpR）：`lowpass`，截止 `cutoffHz`（clamp 后）、Q = clamp 后 `q`；
- 谐波高通（hpL/hpR）：`highpass`，截止 `hpCut = clamp(max(150, cutoffHz × 1.5), 20, fs × 0.45)`、
  Q 固定 0.707——只保留基频整数倍谐波，去除 `even` 型产生的 DC 与低频互调成分；
- **设计采样率条款**：四个内部 Biquad 均以**缺省构造**创建（`new Biquad()`），其系数设计
  采样率固定为 **48000 Hz**（`Biquad.setParams` 使用实例自身构造时的 fs）——
  **与 BassEnhancer 自身构造采样率无关**。任意 `sampleRate` 下内部滤波器均按 48000 Hz 设计，
  这是 TS 事实标准的真实行为；Rust 支线实现必须复刻该行为，否则任意非 48000 采样率下的
  对拍必然超差。当前本模块全部向量取 48000 Hz（此时两种设计采样率巧合一致），
  非 48000 采样率向量暂不冻结（是否固化该耦合属行为修订决策，须走向量修订流程）；
- 内部 Biquad 状态跨块保持；`reset()` 复位全部四个实例。

### 4.4 低音下潜路径（lowBoostDb 语义）

```text
lowLin = 10^(clamp(lowBoostDb, −6, 12) / 20) − 1
out += lowLin × b（b 为低通提取的低频带）
```

- `lowBoostDb = 0` ⇒ `lowLin = 0` ⇒ 下潜项对有限样本逐位消失（`x + 0 × b = x`），
  输出与「不含下潜路径的实现」**逐位一致**——这是默认关闭的精确定义；
- 旧参数快照缺省该字段时按 0 处理（`Number.isFinite` 防御），行为同上。

### 4.5 状态集合与生命周期

内部状态：四个 Biquad 的 TDF2 状态（每实例两个双精度标量）。

- `setParams` 重算滤波器系数，**Biquad 状态保留不清零**（与 biquad 规格的换参语义一致）；
- `enabled = false` 时逐样本循环整体跳过：缓冲不被改写（逐位直通），滤波器状态不更新；
- `reset()` 将四个 Biquad 状态归零。

## 五、GWT 行为条款

> 定量断言一律以 `specs/dsp/vectors/bass-enhancer.*.json` 冻结夹具 + 容差公式判定
> （README §3.5），条款内不内嵌具体参数数值与期望值。

### GWT-BE-01：低通带提取
- **给定**：enabled=true、任意合法 cutoffHz/q
- **当**：送入低于截止频率的正弦激励
- **则**：低频带 b 逼近输入（低通通带），谐波路径与下潜路径均由该低频带驱动；
  精确响应以冻结向量为准

### GWT-BE-02：奇次谐波生成（odd）
- **给定**：harmonicType='odd'、enabled=true
- **当**：以单一低频正弦（频率低于 cutoffHz）稳态激励
- **则**：输出频谱出现基频整数倍（奇次为主）的新成分，dry 基频保持不变；
  谐波成分的精确幅度与相位以冻结向量为准

### GWT-BE-03：偶次谐波生成（even，与 odd 成对对照）
- **给定**：同一低频正弦输入、除 harmonicType 外其余参数相同
- **当**：分别以 harmonicType='even' 与 'odd' 各跑一条向量
- **则**：两模式输出差异可观测（偶次/奇次谐波结构不同），DC 成分被高通路径去除；
  差异量以成对冻结向量界定

### GWT-BE-04：atan 谐波生成（ATSR 器件曲线）
- **给定**：harmonicType='atan'
- **当**：低频正弦稳态激励
- **则**：输出谐波成分按 ATSR 曲线生成，全程有界无发散；精确波形以冻结向量为准

### GWT-BE-05：谐波路径增益标量
- **给定**：enabled=true、任意合法 mix/harmonicGain/levelDb
- **当**：任意激励
- **则**：谐波注入量恰为三者线性连乘 `mix × harmonicGain × 10^(levelDb/20)`；
  levelDb 越界按 ±6 钳制生效（精确量化以冻结向量为准）

### GWT-BE-06：低音下潜真实能量提升
- **给定**：enabled=true、lowBoostDb 取正值（含上界 12 与越界钳制）
- **当**：低频能量显著的激励
- **则**：低频带按 `10^(lowBoostDb/20) − 1` 线性混回（正值为真实能量提升）；
  精确增益以冻结向量为准

### GWT-BE-07：lowBoostDb=0 逐位等价无下潜
- **给定**：enabled=true、lowBoostDb=0（含缺省字段等价形态）
- **当**：低频能量显著的激励
- **则**：输出与不含下潜路径的实现**逐位一致**（lowLin=0，加性项逐位消失）——
  谐波路径单独可观测

### GWT-BE-08：lowBoostDb 越界钳制
- **给定**：lowBoostDb 取低于 −6 的越界值
- **当**：低频能量显著的激励
- **则**：按 −6 下界生效（lowLin 为负，低频带被衰减），clamp 行为以冻结向量为准

### GWT-BE-09：静音输入零输出
- **给定**：任意合法参数、全新实例（或刚 reset）
- **当**：送入全零输入
- **则**：输出逐位为全零（低通/高通状态保持零，非线性在零点输出为零或被高通去除）

### GWT-BE-10：极值参数无数值事故
- **给定**：cutoffHz 取下界 20 与越界值、q 取两端、harmonicGain/mix 取 1、levelDb 越界、
  lowBoostDb 双向越界等极值组合
- **当**：常规信号激励
- **则**：全程无 NaN / Infinity，输出有界（含谐波高通上限 `fs × 0.45` 钳制路径）

### GWT-BE-11：跨块状态连续性（逐位一致）
- **给定**：任意参数与输入序列
- **当**：分别按 blockSize=k 分块处理与一次性整块处理（末块可短）
- **则**：两种方式输出**逐位一致**（四个 Biquad 为纯逐样本递推状态，切块不改变运算序列）

### GWT-BE-12：reset 后行为可复现
- **给定**：已处理过信号的实例
- **当**：reset() 后重放同一输入
- **则**：输出与首次从零状态处理的输出完全一致

### GWT-BE-13：禁用即直通（由单元测试覆盖，不入向量）
- **给定**：enabled=false 的任意参数组合
- **当**：送入任意信号
- **则**：输出与输入逐位一致（缓冲未被改写），滤波器状态不更新

### GWT-BE-14：非法采样率抛错（由单元测试覆盖，不入向量）
- **给定**：fs ≤ 0 或非有限
- **当**：构造 BassEnhancer
- **则**：抛 `Error('invalid sample rate')`

### GWT-BE-15：内部滤波器设计采样率固定（事实标准条款，非独立断言）
- 内部四个 Biquad 的系数设计采样率恒为 48000 Hz（§4.3），任何采样率下的对拍实现
  必须复刻；该条款由全部向量间接锚定（当前向量采样率均为 48000）。

## 六、向量用例

**本模块全部定量行为以 `specs/dsp/vectors/bass-enhancer.<case>.json` + 同名 `.f32` 冻结夹具
为准**；本章仅列预期 case 覆盖面（维度清单，不含具体参数数值）。格式契约见
[`specs/README.md`](../README.md) §三；JSON 合法性由 Schema 校验。

预期覆盖面：

1. **偶次谐波生成**：低频正弦（低于截止）+ harmonicType='even' 至少一条（GWT-BE-03）；
2. **奇次谐波生成**：与偶次条目同输入成对对照的 harmonicType='odd' 至少一条（GWT-BE-02/03）；
3. **低音下潜真实能量路径**：lowBoostDb 取正常正值 + 低频显著的音乐性双频激励至少一条
   （GWT-BE-06）；
4. **lowBoostDb 越界钳制**：取越界负值按 −6 生效至少一条，兼覆盖另一种谐波类型
   （GWT-BE-08/04）；
5. **lowBoostDb=0 逐位等价**：默认关闭形态（GWT-BE-07）至少一条；
6. **跨块状态连续性**：blockSize 与 frames 的整除/非整除形态兼有（GWT-BE-11）；
7. **采样率**：全部取 48000——因内部滤波器设计采样率固定 48000（§4.3），
   多采样率向量会固化该耦合，暂不冻结，留待行为修订决策。

.f32 数据按 [README §3.3](../README.md) 四段 planar 小端布局存放；
分块驱动方式按 §3.4（本模块原生 `processStereo` 就地语义）。

## 七、关联文件

- 总纲契约：[`specs/README.md`](../README.md)
- Schema：[`specs/schema/vector-case.schema.json`](../schema/vector-case.schema.json)
- 行为事实标准：`src/dsp/BassEnhancer.ts`；参数类型：`src/types.ts`
  （BassEnhancerSettings / HarmonicType）；TS 实现契约：`src/dsp/API_SPEC.md`
- 依赖模块规格：[biquad](biquad.md)（内部低通/高通核及其换参不清状态语义）
- 兄弟规格：[compressor](compressor.md) ｜ [mid-side](mid-side.md)
