# 规格：reverb-simple —— 算法混响（Freeverb 类）

> **规格属性**：本文件是双支线共享规格。行为事实标准 = `src/dsp/ReverbSimple.ts`；
> 参数字段名一律以该源码（`ReverbSimpleParams`）为准。格式契约见 [`specs/README.md`](../README.md) §三，
> 向量合法性由 [`specs/schema/vector-case.schema.json`](../schema/vector-case.schema.json) 约束。

---

## 一、模块概述

- **定位**：引擎的算法混响路由（对应引擎参数 `reverb.mode = 'algorithmic'`），
  提供五种房间类型的长尾混响。
- **出处**：结构（8 立体声梳状 + 4 全通、damping 反馈低通、wet/dry/width 混音）源自
  Jezar 的 Freeverb（公有领域）公开结构，并参考 stk FreeVerb（MIT）思路；
  五种 type 的房间参数表为本项目自定义。本实现为按公开结构独立编写的 TypeScript 代码。
- **结构要点**：
  - 左右各 4 路梳状滤波器并联（左右延迟互质防共振），反馈回路含一阶低通（damping）；
  - 梳状求和后串接 4 个全通滤波器（反馈系数固定 0.5）；
  - 独立 preDelay 延迟线（仅作用于湿路）；湿路宽度 width 做左右交叉混合。
- **确定性**：同输入同参数必同输出；无随机、无 Date、无 console；process 内零分配
  （缓冲按最大延迟长度预分配）。
- **采样率**：构造时固定；fs ≤ 0 或非有限值抛 `Error('invalid sample rate')`。

## 二、接口签名（事实标准摘录）

```ts
export interface ReverbSimpleParams {
  roomSize: number
  damping: number
  wet: number
  dry: number
  preDelayMs: number
  width: number
  type: ReverbType   // 'hall' | 'room' | 'plate' | 'spring' | 'stage'（定义于 src/types.ts）
}

export class ReverbSimple {
  constructor(fs: number)
  setParams(p: ReverbSimpleParams): void
  processStereo(l: Float32Array, r: Float32Array): void   // 就地处理立体声
  reset(): void
}
```

## 三、参数表（向量 `params` 快照字段）

### 3.1 用户参数字段

| 字段 | 类型 | 有效域（clamp 后生效值） | 默认值¹ | clamp / 边界行为 |
|---|---|---|---|---|
| `roomSize` | number | 输入钳制到 [0,1] 后参与混合 | 0.5 | `clamp01` 后与 type 基准混合：`effRoom = min(0.98, max(0, t.roomSize + (roomSize−0.5) × 0.5))`，作为梳状反馈系数（≤0.98 保证稳定不发散） |
| `damping` | number | 输入钳制到 [0,1] 后参与混合 | 0.5 | `clamp01` 后同式混合：`effDamp = min(0.99, max(0.01, t.damping + (damping−0.5) × 0.5))`；反馈低通系数 damp1=effDamp、damp2=1−effDamp |
| `wet` | number | `[0, 4]` | 0.3 | 双向钳制；湿路总增益基准 |
| `dry` | number | `[0, 4]` | 0.7 | 双向钳制；干路直通增益 |
| `preDelayMs` | number（ms） | `[0, 1000]` | 0 | 双向钳制后 `preDelayLen = round(preDelayMs × fs / 1000)` 样本；**仅作用于湿路** |
| `width` | number | `[0, 2]` | 1 | 钳制后决定湿路交叉混合：`wet1 = wet × (width/2 + 0.5)`、`wet2 = wet × ((1 − width)/2)` |
| `type` | string（五种枚举） | hall / room / plate / spring / stage | hall | 查 §3.2 房间参数表；未知值在运行时回退 hall（防御逻辑，见 GWT-RS-11，向量不得依赖非法枚举） |

¹ 本模块类本身不设内置默认值（`setParams` 必须收到完整快照）；「默认值」列取引擎层
默认快照（`src/types.ts` `createDefaultParams` 的 reverb.algorithmic 组）。向量必须提供完整七字段。

### 3.2 type → 房间参数表（行为标准的一部分）

| type | 基准 roomSize | 基准 damping | delayScale |
|---|---|---|---|
| `hall` | 0.7 | 0.4 | 1.0 |
| `room` | 0.4 | 0.6 | 0.8 |
| `plate` | 0.6 | 0.2 | 0.7 |
| `spring` | 0.3 | 0.8 | 0.5 |
| `stage` | 0.5 | 0.5 | 1.2 |

语义：type 提供 (roomSize, damping) 基准特性，用户同名参数在基准附近 ±0.25 范围内微调
（用户值 0.5 时即类型本身）；delayScale 缩放全部延迟线长度。

## 四、处理语义

### 4.1 结构与信号流

每样本处理顺序（左右声道对称）：

```text
输入 x ──┬───────────────────────────────────────────────┬──> out = x×dry + wetL×wet1 + wetR×wet2
         │                                               │         （右声道交叉使用 wetR/wetL）
         └─ preDelay(仅湿路) ─> 8×梳状并联 ─> 4×全通串联 ─┘
                             （每路: 反馈低通 damp1/damp2 + 反馈 effRoom）
                              梳状并联求和后乘 WET_GAIN = 0.25 补偿
```

- **延迟长度**：标准 Freeverb 调音 × type.delayScale × fs/44100，四舍五入且下限 1 样本。
  梳状延迟 @44.1kHz：左 [1116, 1188, 1277, 1356]、右 [1101, 1173, 1256, 1344]；
  全通延迟（左右共用）：[556, 441, 341, 225]；
- **梳状单路递推**：`out = buf[p]`；`filt = out×damp2 + store×damp1`；`store = filt`；
  `buf[p] = input + filt×feedback`；
- **全通单路递推**（反馈系数 0.5）：`bufout = buf[p]`；`apOut = −input + bufout`；
  `buf[p] = input + bufout×0.5`；
- **WET_GAIN = 0.25** 为四路梳状求和的电平补偿常数（无补偿时宽带稳态幅度可超输入数倍）；
- **输出混合**：`l[i] = xl×dry + accL×wet1 + accR×wet2`；
  `r[i] = xr×dry + accR×wet1 + accL×wet2`（width=1 时 wet2=0 无交叉；width=0 时湿路左右对半交叉）；
- **干路不经 preDelay**：预延迟只作用于湿路，干路逐样本直通乘 dry。

### 4.2 参数更新与状态语义

- `setParams` 即时生效（重算反馈/阻尼/干湿/宽度/延迟长度/preDelay 长度）；
  内部缓冲与游标位置保留（不随 setParams 清零）；
- `reset()` 清空全部缓冲并归零游标与 store 状态；
- **延迟报告**：本模块不引入算法延迟，也未提供延迟报告接口（湿路群延迟属频域特性，
  不属于延迟报告范畴）。

## 五、GWT 行为条款

> 定量断言一律以 `specs/dsp/vectors/reverb-simple.*.json` 冻结夹具 + 容差公式判定（README §3.5）。

### GWT-RS-01：纯干声恒等直通
- **给定**：wet=0 且 dry=1（其余任意合法参数）
- **当**：送入任意信号
- **则**：输出与输入**逐位一致**（湿路项精确为零，干路乘 1.0 为精确乘法）

### GWT-RS-02：静音输入收敛至静音
- **给定**：任意合法参数（effRoom ≤ 0.98）
- **当**：送入全零输入长序列（含尾音延续阶段）
- **则**：输出能量衰减趋于零，无自激发散、无周期性回升伪影

### GWT-RS-03：冲击响应能量整体衰减
- **给定**：enabled 语义下的任一典型参数组合
- **当**：送入单位冲击（或短脉冲）后接静音
- **则**：湿路能量包络整体单调衰减、尾部趋零（局部纹波允许；量化判定引用冻结向量）

### GWT-RS-04：满幅激励有界不发散
- **给定**：任意合法参数组合（含 wet=dry=4 上限）
- **当**：送入 |x| ≤ 1 的满幅冲击 / 阶跃 / 正弦长序列
- **则**：输出始终有限（无 NaN / Infinity），长时间运行不发散

### GWT-RS-05：width 交叉混合行为
- **给定**：同一双声道输入
- **当**：分别以 width=1 与 width=0 各跑一条向量
- **则**：width=1 时湿路无左右交叉（wet2=0，左输出只含左湿路分量）；width=0 时湿路左右对半
  交叉——若输入左右相同则输出左右保持相等；差异量以成对冻结向量界定

### GWT-RS-06：五种 type 特性可区分
- **给定**：除 type 外相同的参数
- **当**：分别以 hall / room / plate / spring / stage 各跑一条向量
- **则**：衰减时间与频响特征可观测差异（spring 尾最短、stage 声场最长等），
  具体以五条冻结向量界定

### GWT-RS-07：preDelay 仅作用于湿路
- **给定**：preDelayMs > 0、dry > 0
- **当**：送入前沿陡峭的瞬态信号
- **则**：干路成分时序不变（无延迟），湿路成分推迟 preDelayLen 样本出现

### GWT-RS-08：极值参数钳制无数值事故
- **给定**：roomSize/damping 取 [0,1] 外越界值、wet/dry 取 4 上限或负值、preDelayMs 取 0 与 1000 边界
- **当**：常规信号激励
- **则**：§三 clamp 全部生效，等效于边界值处理；全程无 NaN / Infinity

### GWT-RS-09：跨块状态连续性（逐位一致）
- **给定**：任意参数与输入序列
- **当**：分别按 blockSize=k 分块处理与一次性整块处理（末块可短）
- **则**：两种方式输出**逐位一致**（梳状/全通/preDelay 均为纯逐样本递推状态，
  切块不改变运算序列）

### GWT-RS-10：reset 后行为可复现
- **给定**：已处理过信号的实例
- **当**：reset() 后重放同一输入
- **则**：输出与首次从零状态处理的输出完全一致

### GWT-RS-11：未知 type 运行时回退 hall（由单元测试覆盖，不入向量）
- **给定**：type 为五种枚举之外的字符串（TS 类型系统外，仅运行时可达）
- **当**：调用 setParams
- **则**：按 hall 的房间参数表生效（防御逻辑）；向量夹具不得依赖此非法域

## 六、向量用例

**本模块全部定量行为以 `specs/dsp/vectors/reverb-simple.<case>.json` + 同名 `.f32` 冻结夹具为准**；
本章仅列预期 case 覆盖面（维度清单，不含具体参数数值）。格式契约见
[`specs/README.md`](../README.md) §三；JSON 合法性由 Schema 校验。

预期覆盖面：

1. **五种 type** 各至少一条向量（GWT-RS-06）；
2. **纯干声恒等**（wet=0, dry=1）至少一条（GWT-RS-01）；**纯湿声**（dry=0）至少一条；
3. **width 对照**：width=1 与 width=0 至少成对一条（GWT-RS-05）；
4. **preDelay 两端**：preDelayMs=0 与较大值至少各一条（GWT-RS-07）；
5. **极值 clamp**：roomSize/damping/wet/dry 越界用例至少一条（GWT-RS-08）；
6. **静音输入**与**满幅冲击/阶跃**至少各一条（GWT-RS-02/04）;
7. **跨块状态连续性**：blockSize 显著小于 frames 且不整除（末块短块）至少一条（GWT-RS-09）；
8. **多采样率**：默认 48000 之外至少再取一档（延迟长度随 fs/44100 缩放的行为被覆盖）。

.f32 数据按 [README §3.3](../README.md) 四段 planar 小端布局存放；
分块驱动方式按 §3.4（本模块原生 `processStereo` 就地语义）。

## 七、关联文件

- 总纲契约：[`specs/README.md`](../README.md)
- Schema：[`specs/schema/vector-case.schema.json`](../schema/vector-case.schema.json)
- 行为事实标准：`src/dsp/ReverbSimple.ts`；类型定义：`src/types.ts`（ReverbType）；
  TS 实现契约：`src/dsp/API_SPEC.md` 模块 10
- 兄弟规格：[biquad](biquad.md) ｜ [limiter](limiter.md)