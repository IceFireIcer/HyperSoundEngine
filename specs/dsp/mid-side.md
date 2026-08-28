# 规格：mid-side —— M/S 立体声编解码 + 宽度 / 人声比例

> **规格属性**：本文件是双支线共享规格。行为事实标准 = `src/dsp/MidSide.ts`；
> 参数（width / voiceBalance）字段名与语义一律以该源码为准。
> 格式契约见 [`specs/README.md`](../README.md) §三，
> 向量合法性由 [`specs/schema/vector-case.schema.json`](../schema/vector-case.schema.json) 约束。

---

## 一、模块概述

- **定位**：M/S（中/侧）域立体声处理器——将立体声信号变换到中/侧域，按宽度（width）与
  人声比例（voiceBalance）施加域内增益后逆变换回 L/R。width 控制立体声宽度，
  voiceBalance 以对称的「中衰减 / 侧衰减」线性混合实现人声（中）与伴奏（侧）的比例调节。
- **出处**：自研。M/S 变换（M=(L+R)/2、S=(L−R)/2 及逆变换 L=M+S、R=M−S）为音频处理
  公有知识，无第三方代码；人声比例的对称衰减语义来自仓库审计修复（电平安全：
  vb≠0 只衰减不提升）。
- **确定性**：同输入同参数必同输出；无随机、无 Date、无 console；process 内零分配。
- **采样率**：**本模块无采样率概念**——构造函数无参、无内部滤波与状态、行为与 fs 完全无关。
  向量契约的 `sampleRate` 字段按 §三填写（驱动器不将其传入模块），统一取 48000。

## 二、接口签名（事实标准摘录）

```ts
export class MidSide {
  constructor()                                  // 无参：初始 midGain=1、sideGain=1（恒等）
  setParams(width: number, voiceBalance: number): void   // 位置参数，非对象快照
  processStereo(l: Float32Array, r: Float32Array): void  // 就地 M/S 编解码
  reset(): void                                  // 无内部状态，空操作（接口一致性）
}
```

## 三、参数表（向量 `params` 快照字段）

向量 JSON 的 `params` 对象**固定使用以下两个字段**；向量驱动器（导出工具与两支线门禁）
以**位置参数**形式传入 `setParams(width, voiceBalance)`（见 §4.4）：

| 字段 | 类型 | 有效域（clamp 后生效值） | 默认值¹ | clamp 行为 |
|---|---|---|---|---|
| `width` | number | `[0, 2]` | 1（构造内置 midGain=1/sideGain=1，即未调 setParams 时恒等） | 越界双向钳制：`w = min(max(width, 0), 2)`；1=原始宽度，0=单声道塌缩，2=最大展宽 |
| `voiceBalance` | number | `[−1, 1]` | 0 | 越界双向钳制：`vb = min(max(voiceBalance, −1), 1)`；−1=仅伴奏（去中）、+1=仅人声（去侧）、0=仅宽度控制 |

¹ 「默认值」指构造内置状态等价值；向量必须提供完整两字段快照，不依赖缺省。

## 四、处理语义

### 4.1 M/S 变换与逆变换（逐样本）

```text
m = (li + ri) × 0.5        # 中信号（双精度中间量）
s = (li − ri) × 0.5        # 侧信号（双精度中间量）
l[i] = m × midGain + s × sideGain
r[i] = m × midGain − s × sideGain
```

- 中间计算在双精度域完成（输入为 f32 量化值，×0.5 为精确运算）；
- 长度契约：`l.length !== r.length` 时抛 `Error('midside: L/R length mismatch')`（单元测试
  覆盖路径，向量不覆盖）。

### 4.2 增益语义（width × voiceBalance 对称衰减）

```text
mg = 1 + min(0, vb)          # midGain：vb<0 时衰减中信号，其余为 1
sg = w × (1 − max(0, vb))    # sideGain：vb>0 时按比例衰减侧信号，其余为 w
```

| voiceBalance 区间 | midGain | sideGain | 语义 |
|---|---|---|---|
| vb = 0 | 1 | w | 仅宽度控制（w=1 时恒等） |
| vb > 0 | 1 | w × (1 − vb) | 衰减侧信号 → 人声突出；vb=+1 完全去侧 |
| vb < 0 | 1 + vb | w | 衰减中信号 → 伴奏突出；vb=−1 完全去中 |

- 电平安全：vb≠0 时 `mg ≤ 1` 且 `sg ≤ w`——人声比例只衰减、不提升（审计修复语义）；
- width 与 voiceBalance 独立生效：宽度缩放只进入 sideGain。

### 4.3 恒等锚点与无状态性

- **恒等条件**：width=1 且 voiceBalance=0 时 `mg = sg = 1`，输出与输入**逐样本一致**
  （双精度中间量对 f32 输入无舍入误差：m+s 与 m−s 精确还原 li/ri）；
- **无内部状态**：无滤波、无延迟、无包络——任意分块方式（含逐样本）结果一致，
  `reset()` 为空操作；处理顺序与历史无关。

### 4.4 向量驱动器语义（模块特有，两支线加载器必须一致实现）

- `setParams` 为位置参数接口：驱动器从向量 `params` 快照取
  `setParams(params.width, params.voiceBalance)`；
- 构造不传入采样率：`sampleRate` 字段仅为契约完整性保留，驱动器不得将其传入模块
  （模块构造无参）；
- 处理为标准两参就地调用 `processStereo(l, r)`，无模块特有派生。

## 五、GWT 行为条款

> 定量断言一律以 `specs/dsp/vectors/mid-side.*.json` 冻结夹具 + 容差公式判定（README §3.5），
> 条款内不内嵌具体参数数值与期望值。

### GWT-MS-01：恒等锚点（逐位一致）
- **给定**：width=1、voiceBalance=0
- **当**：送入任意允许输入（去相关立体声、单声道内容、满幅序列）
- **则**：输出与输入**逐位一致**（M/S 正逆变换在双精度下精确还原 f32 输入）

### GWT-MS-02：单声道塌缩
- **给定**：width=0、voiceBalance=0
- **当**：送入左右去相关的立体声输入
- **则**：侧信号被完全去除，左右输出均为中信号 M（左右输出逐位相等）；输入本身为
  纯中信号（左右相同）时输出与输入一致

### GWT-MS-03：宽度展宽
- **给定**：width>1、voiceBalance=0
- **当**：送入含侧信号的立体声输入
- **则**：侧成分按 width 线性放大、中成分不变（立体声展宽）；width=2 为上界工作点；
  精确增益以冻结向量为准

### GWT-MS-04：width 越界钳制
- **给定**：width 取越界值（低于 0 或高于 2）
- **当**：任意激励
- **则**：按 [0, 2] 边界生效（负值等效单声道塌缩、超上界等效 2），clamp 行为以冻结向量为准

### GWT-MS-05：人声路径（vb>0 侧衰减）
- **给定**：voiceBalance 取 (0, 1] 区间值
- **当**：送入含中/侧成分的立体声输入
- **则**：中成分保持单位增益、侧成分按 `w × (1 − vb)` 衰减；侧衰减量以冻结向量为准

### GWT-MS-06：伴奏路径（vb<0 中衰减）与 vb 越界钳制（由单元测试覆盖，不入向量）
- **给定**：voiceBalance 取负值（含越界负值按 −1 生效）
- **当**：送入含中/侧成分的立体声输入
- **则**：中成分按 `1 + vb` 衰减（vb=−1 完全去中，midGain=0）、侧成分按 width 增益保留

### GWT-MS-07：电平安全（只衰减不提升）
- **给定**：voiceBalance ≠ 0 的任意合法组合
- **当**：任意激励
- **则**：midGain ≤ 1 且 sideGain ≤ width（人声/伴奏调节不引入额外提升；
  width 本身的展宽增益除外，其属宽度语义）

### GWT-MS-08：任意分块一致（无状态）
- **给定**：任意参数与输入序列
- **当**：以任意 blockSize（含 1 与大于 frames 的值）分块处理
- **则**：所有分块方式输出与整块处理**逐位一致**（无跨样本状态）

### GWT-MS-09：reset 为空操作（由单元测试覆盖，不入向量）
- **给定**：已处理过信号的实例
- **当**：调用 reset() 后重放同一输入
- **则**：输出与此前逐位一致（无状态可清）

### GWT-MS-10：长度不匹配抛错（由单元测试覆盖，不入向量）
- **给定**：l.length ≠ r.length
- **当**：调用 processStereo
- **则**：抛 `Error('midside: L/R length mismatch')`

### GWT-MS-11：采样率无关（事实标准条款，非独立断言）
- 模块行为与采样率完全无关（§一/§4.4）；向量 `sampleRate` 字段仅满足契约格式，
  两支线加载器不得将其传入模块。

## 六、向量用例

**本模块全部定量行为以 `specs/dsp/vectors/mid-side.<case>.json` + 同名 `.f32` 冻结夹具为准**；
本章仅列预期 case 覆盖面（维度清单，不含具体参数数值）。格式契约见
[`specs/README.md`](../README.md) §三；JSON 合法性由 Schema 校验。

预期覆盖面：

1. **宽度展宽 + width 上界钳制**：width 取越界正值按 2 生效、voiceBalance=0 至少一条
   （GWT-MS-03/04）；
2. **单声道塌缩**：width=0、voiceBalance=0 至少一条（GWT-MS-02）；
3. **人声路径**：voiceBalance 取 (0, 1) 区间值至少一条（GWT-MS-05）；
4. **恒等锚点**：width=1、voiceBalance=0 逐位一致至少一条（GWT-MS-01）；
5. **跨块一致性**：blockSize 整除与不整除（末块短块）形态兼有（GWT-MS-08）；
6. **采样率**：向量统一取 48000（对本模块无行为影响，仅为契约字段完整性，
   见 GWT-MS-11）。

> 未入向量、由单元测试覆盖的维度：伴奏路径与 vb 越界钳制（GWT-MS-06）、
> width 下界钳制（width<0 按 0 生效，语义与单声道塌缩一致）。

.f32 数据按 [README §3.3](../README.md) 四段 planar 小端布局存放；
分块驱动方式按 §3.4（本模块原生 `processStereo` 就地语义）。

## 七、关联文件

- 总纲契约：[`specs/README.md`](../README.md)
- Schema：[`specs/schema/vector-case.schema.json`](../schema/vector-case.schema.json)
- 行为事实标准：`src/dsp/MidSide.ts`；TS 实现契约：`src/dsp/API_SPEC.md` 模块 4
- 兄弟规格：[biquad](biquad.md) ｜ [limiter](limiter.md) ｜ [reverb-simple](reverb-simple.md) ｜
  [compressor](compressor.md) ｜ [bass-enhancer](bass-enhancer.md)
