# 规格：compressor —— 动态压缩器（立体声联合包络 + 软拐点 + sidechain）

> **规格属性**：本文件是双支线共享规格。行为事实标准 = `src/dsp/Compressor.ts`；
> 参数字段名一律以该源码（`CompressorSettings`，定义于 `src/types.ts`）为准。
> 格式契约见 [`specs/README.md`](../README.md) §三，
> 向量合法性由 [`specs/schema/vector-case.schema.json`](../schema/vector-case.schema.json) 约束。

---

## 一、模块概述

- **定位**：链路级动态压缩器——立体声联合峰值包络驱动 dB 域压缩曲线（含软拐点 knee），
  对左右声道施加同一增益标量（立体声联动），并以 makeup / outputGain 做补偿。
- **出处**：包络检测 + dB 域压缩曲线 + makeup 补偿取自项目《音频算法技术文档》§3 设计意图；
  软拐点采用行业标准公式（DAW 压缩器通用二次曲线形式）；本实现为自研 TypeScript。
- **确定性**：同输入同参数必同输出；无随机、无 Date、无 console；process 内零分配
  （状态仅两个双精度标量：包络 env 与衰减报告 reductionDb）。
- **采样率**：构造时固定；fs ≤ 0 或非有限值抛 `Error('invalid sample rate')`。

## 二、接口签名（事实标准摘录）

```ts
export class Compressor {
  constructor(fs: number)
  setParams(p: CompressorSettings): void
  // 就地处理立体声；传入 sideL/sideR 时以外部 sidechain 信号驱动包络
  processStereo(l: Float32Array, r: Float32Array, sideL?: Float32Array, sideR?: Float32Array): void
  getReductionDb(): number   // 当前增益衰减 dB（<= 0，不含 makeup/outputGain）
  reset(): void
}
```

参数类型 `CompressorSettings` 定义于 `src/types.ts`：
`{ enabled, thresholdDb, ratio, kneeDb, attackMs, releaseMs, makeupDb, outputGain, sidechainEnabled? }`。

## 三、参数表（向量 `params` 快照字段）

| 字段 | 类型 | 有效域（clamp 后生效值） | 默认值¹ | clamp / 边界行为 |
|---|---|---|---|---|
| `enabled` | boolean | true / false | 构造内置 true；引擎默认快照 false | false 时 `processStereo` 直接返回（不改写缓冲、逐位直通），衰减报告归 0 |
| `thresholdDb` | number（dB） | `[−80, 0]` | −20 | 越界双向钳制 |
| `ratio` | number | `[1, 100]` | 4 | 越界双向钳制；`ratio = 1` 时压缩量恒为 0（见 GWT-CP-08） |
| `kneeDb` | number（dB） | `[0, 40]` | 6 | 越界双向钳制；`kneeDb = 0` 退化为硬拐点 |
| `attackMs` | number（ms） | 生效下限 0.05 ms | 10 | 经 `onePoleCoef` 换算：`coef = 1 − exp(−1 / ((max(ms, 0.05)/1000) × fs))`；ms ≤ 0.05 时按 0.05 生效 |
| `releaseMs` | number（ms） | 生效下限 0.05 ms | 150 | 同 attackMs 换算方式 |
| `makeupDb` | number（dB） | `[−24, 24]` | 0 | 越界双向钳制；生效值为线性 `makeupLin = 10^(makeupDb/20)` |
| `outputGain` | number（线性） | `[0, 2]` | 1 | 越界双向钳制 |
| `sidechainEnabled` | boolean | true / false | false | **模块本身不读取该字段**——它是引擎接线层的标志：置 true 时调用方以四参形式提供外部 sidechain 声道（见 §4.4）；向量驱动器据其决定调用形态（见 §4.5） |

¹ 「默认值」为构造函数内置默认（`applyParams` 构造快照）；引擎默认快照
（`createDefaultParams` 的 compressor 组）中 `enabled` 为 false，其余字段一致。
向量必须提供完整九字段快照（`sidechainEnabled` 为可选字段，但向量固定显式给出）。

## 四、处理语义

### 4.1 包络检测（立体声联合峰值 + 一阶跟随）

逐样本计算检测电平并做一阶峰值跟随（attack / release 双系数分段）：

```text
e       = max(|el|, |er|)            # 联合包络：sidechain 模式下 el/sideL、er/sideR 取外部信号
env    += (e > env ? attackCoef : releaseCoef) × (e − env)
```

- 包络为**单标量立体声联动**状态：左右声道始终共用同一 env、同一增益；
- `attackCoef` / `releaseCoef` 在 `setParams` 时按 §三换算，处理循环内为常量。

### 4.2 dB 域压缩曲线（软拐点三区公式）

```text
levelDb = 20 × log10(env + 1e−12)          # +1e−12 为静音地板，防 log(0)
invRatio = 1 − 1/ratio
kneeHalf = kneeDb / 2

kneeDb ≤ 0（硬拐点）：reduction = levelDb > thr ? (levelDb − thr) × invRatio : 0
kneeDb  > 0（软拐点）：
  levelDb < thr − kneeHalf          → reduction = 0
  levelDb > thr + kneeHalf          → reduction = (levelDb − thr) × invRatio
  其余（膝区内部）                   → x = levelDb − (thr − kneeHalf)
                                       reduction = (invRatio × x²) / (2 × kneeDb)
```

- 膝区公式在区间端点与两线性段连续（`x = kneeHalf` 时二次值等于线性值）；
- `ratio = 1` ⇒ `invRatio = 0` ⇒ 任意电平下 reduction 恒为 0。

### 4.3 增益与就地应用

```text
g = 10^(−reduction / 20) × makeupLin × outputGain
l[i] = l[i] × g ;  r[i] = r[i] × g      # 左右同增益（立体声联动）
getReductionDb() = −reduction（恒 ≤ 0，不含 makeup/outputGain）
```

### 4.4 sidechain 语义（外部包络驱动）

- `sidechainEnabled` 属于 `CompressorSettings`，但 **`Compressor` 类自身不读取它**；
  它由引擎接线层消费：`src/engine/HyperSoundEngine.ts` 在 sidechain 激活且该标志为 true 时，
  以 `processStereo(L, R, sideL, sideR)` 四参形式提供外部检测信号；
- 模块行为：**提供了 sideL/sideR 两参时，包络检测改用外部信号（`el = sideL[i]`、`er = sideR[i]`），
  音频路径仍处理 l/r 本体**；未提供时退化为内部联合包络（el/er 取 l/r 自身）；
- sidechain 内容不在模块内定义——它是调用方的接线决策。

### 4.5 向量驱动器语义（模块特有，两支线加载器必须一致实现）

向量为单参数快照 + 单一输入夹具，sidechain 信号须从夹具确定性派生。
**当向量 `params.sidechainEnabled === true` 时**，向量驱动器（导出工具与两支线门禁）按以下规则
构造 sidechain 通道——**单声道和（mono sum）派生**，取本块就地处理前的原始输入：

```text
sideL[n] = sideR[n] = inL[n] + inR[n]     # n 为块内样本下标；加法以双精度执行后直接喂给模块
```

- 派生加法必须以**双精度（f64）**执行（两侧一致；f32 输入的 f64 和无量化损失）；
- sideL 与 sideR 内容相同（同一派生数组传入两参即可，模块对 side 通道只读）；
- 快照时机：必须在就地处理**之前**完成派生（推荐先复制输入再计算派生与处理）；
- **当 `sidechainEnabled` 为 false 或缺省时**，以两参形式 `processStereo(l, r)` 调用（内部包络）。

> 说明：联合包络取 `max(|sideL|, |sideR|)`，若以「左右互换」派生 sidechain，其联合包络与
> 内部模式恒等（max 的对称性），无法区分内外路径——故本模块向量采用单声道和派生，
> 它与内部联合峰值包络产生可观测且数值稳定的差异。

### 4.6 状态集合与生命周期

内部状态仅两项：包络 `env`、衰减报告 `reductionDb`。

- `setParams` **保留包络状态**（参数即时生效、不清历史，避免改参爆音）——与 limiter 的
  管线清空语义不同；
- `enabled = false` 时逐样本循环整体跳过：缓冲不被改写（逐位直通），`reductionDb` 置 0；
- `reset()` 将 `env` 与 `reductionDb` 归零。

## 五、GWT 行为条款

> 定量断言一律以 `specs/dsp/vectors/compressor.*.json` 冻结夹具 + 容差公式判定（README §3.5），
> 条款内不内嵌具体参数数值与期望值。

### GWT-CP-01：阈下直通（逐位一致）
- **给定**：enabled=true、makeup 与 outputGain 为单位值（0 dB / 1.0），输入峰值使包络稳态
  低于阈值下膝点（thr − knee/2）
- **当**：送入低电平信号长序列
- **则**：压缩量恒为 0，增益恰为 1，输出与输入**逐位一致**，衰减报告恒 0

### GWT-CP-02：重压缩稳态收敛
- **给定**：enabled=true、硬拐点（kneeDb=0）、高压缩比，稳态正弦输入远高于线性阈值
- **当**：信号经 attack 段进入稳态后
- **则**：输出稳态电平收敛至「阈值 + makeup 偏移」附近（attack/release 平滑残差由冻结向量界定），
  衰减报告趋近稳态负值

### GWT-CP-03：软拐点膝区 + kneeDb 上界钳制
- **给定**：enabled=true、kneeDb 取越界值（按上界 40 生效）、输入电平使包络稳态落于膝区内部
- **当**：稳态激励持续
- **则**：压缩量按二次曲线软化（小于同电平下的硬拐点压缩量），clamp 生效且无数值事故；
  精确波形以冻结向量为准

### GWT-CP-04：立体声联动同增益
- **给定**：任意启用状态参数
- **当**：送入左右同相同幅的信号
- **则**：输出左右声道逐位一致（联合包络对两声道施加同一增益标量，声像不漂移）

### GWT-CP-05：sidechain 外部驱动（单声道和派生）
- **给定**：sidechainEnabled=true、左右声道响度结构使「单声道和包络」与「内部联合峰值包络」
  显著不同（去相关的双正弦激励）
- **当**：按 §4.5 派生 sidechain 并四参调用
- **则**：包络由派生信号驱动、音频路径仍处理输入本体；输出与同参数内部包络模式
  （sidechainEnabled=false）显著可区分，差异量以成对冻结向量/参数对照界定

### GWT-CP-06：静音输入静音输出
- **给定**：enabled=true、任意合法压缩参数
- **当**：送入全零输入长序列
- **则**：输出全零；包络沿 release 方向衰减趋 0（levelDb 地板由 `env + 1e−12` 保证，不产生
  NaN），衰减报告趋 0

### GWT-CP-07：极值参数无数值事故
- **给定**：thresholdDb 取两端（−80 与 0）、ratio 取两端（1 与 100）及越界值、kneeDb 取 0 与
  越界大值、makeupDb 双向越界（按 ±24 生效）、outputGain 取 0 与越界值、attack/release 取 0
  （按 0.05 ms 下限生效）等极值组合
- **当**：常规信号激励
- **则**：全程无 NaN / Infinity，输出有界；具体波形以冻结向量为准

### GWT-CP-08：ratio=1 恒不压缩
- **给定**：ratio=1（含越下界取值）、任意 knee 与输入电平
- **当**：任意激励
- **则**：压缩量恒为 0，输出仅为 makeup × outputGain 的常数缩放

### GWT-CP-09：跨块状态连续性（逐位一致）
- **给定**：任意参数与输入序列
- **当**：分别按 blockSize=k 分块处理与一次性整块处理（末块可短）
- **则**：两种方式输出**逐位一致**（包络与增益为纯逐样本递推，切块不改变运算序列）

### GWT-CP-10：reset 后行为可复现
- **给定**：已处理过信号的实例
- **当**：reset() 后重放同一输入
- **则**：输出与首次从零状态处理的输出完全一致

### GWT-CP-11：禁用即直通（由单元测试覆盖，不入向量）
- **给定**：enabled=false 的任意参数组合
- **当**：送入任意信号
- **则**：输出与输入逐位一致（缓冲未被改写），衰减报告归 0

### GWT-CP-12：流中改参保留包络（由单元测试覆盖，不入向量）
- **给定**：流式处理若干块后调用 setParams（不改变 enabled）
- **当**：继续处理
- **则**：包络状态保留不清零（无增益跳变），新参数即时生效

### GWT-CP-13：非法采样率抛错（由单元测试覆盖，不入向量）
- **给定**：fs ≤ 0 或非有限
- **当**：构造 Compressor
- **则**：抛 `Error('invalid sample rate')`

## 六、向量用例

**本模块全部定量行为以 `specs/dsp/vectors/compressor.<case>.json` + 同名 `.f32` 冻结夹具为准**；
本章仅列预期 case 覆盖面（维度清单，不含具体参数数值）。格式契约见
[`specs/README.md`](../README.md) §三；JSON 合法性由 Schema 校验。

预期覆盖面：

1. **阈下直通锚点**：低电平输入 + 单位补偿，输出与输入逐位一致（GWT-CP-01）至少一条；
2. **重压缩稳态**：硬拐点 + 高压缩比 + makeup 补偿至少一条（GWT-CP-02）；
3. **软拐点膝区 + kneeDb 钳制**至少一条（GWT-CP-03）；
4. **sidechain 外部驱动**：sidechainEnabled=true 至少一条，输入形态须使单声道和包络与内部
   联合峰值包络显著可区分（GWT-CP-05）；
5. **极值参数**：kneeDb/makeupDb 越界钳制、attack/release 下限至少各涉一条；
6. **跨块状态连续性**：blockSize 显著小于 frames 且不整除（末块短块）至少一条（GWT-CP-09）；
7. **多采样率**：默认 48000 之外至少再取一档（attack/release 系数随 fs 变化，压缩器无内部
   采样率耦合缺陷，可安全多率覆盖）。

.f32 数据按 [README §3.3](../README.md) 四段 planar 小端布局存放；
分块驱动方式按 §3.4；sidechain 模式的驱动器派生规则按 §4.5 执行。

## 七、关联文件

- 总纲契约：[`specs/README.md`](../README.md)
- Schema：[`specs/schema/vector-case.schema.json`](../schema/vector-case.schema.json)
- 行为事实标准：`src/dsp/Compressor.ts`；参数类型：`src/types.ts`（CompressorSettings）；
  TS 实现契约：`src/dsp/API_SPEC.md`
- 引擎接线（sidechain 消费方）：`src/engine/HyperSoundEngine.ts`（compressor 阶段四参调用）
- 兄弟规格：[biquad](biquad.md) ｜ [limiter](limiter.md) ｜ [reverb-simple](reverb-simple.md) ｜
  [bass-enhancer](bass-enhancer.md) ｜ [mid-side](mid-side.md)
