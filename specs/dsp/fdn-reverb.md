# 规格：fdn-reverb —— FDN 网络混响（反馈延迟网络，算法创新模块）

> **规格属性**：本文件是双支线共享规格。行为事实标准 = `src/dsp/FdnReverb.ts`；
> 参数字段名一律以该源码（`FdnReverbParams`）为准，本规格不得臆造字段。
> 格式契约见 [`specs/README.md`](../README.md) §三，
> 向量合法性由 [`specs/schema/vector-case.schema.json`](../schema/vector-case.schema.json) 约束。

---

## 一、模块概述

- **定位**：算法混响的 FDN（Feedback Delay Network，反馈延迟网络）实现，引擎混响路由
  `reverb.mode = 'fdn'` 时启用（与 `reverb-simple` 并列的另一套算法混响音色）。
- **出处**：结构（N 条互质反馈延迟线 + 正交反馈矩阵 + 每线一阶低通阻尼）源自公开文献：
  Jot (1991, ICMC)「An improved digital reverberator using a feedback delay network」；
  Rocchesso & Smith (1997, IEEE TSAP)「Circulant and elliptic feedback delay networks for
  artificial reverberation」；Zölzer（编）「DAFX」2nd ed. §2.5。反馈矩阵取 Householder
  反射阵 H = I − (2/N)·u·uᵀ（u = [1,…,1]ᵀ）：H 正交（能量不增）且 H·v = v − (2/N)·(Σv)·u
  只需 O(N)。type 房间参数表为本项目自定义。本实现为按公开思路独立编写的 TypeScript 代码。
- **结构要点**：
  - 默认 N=8 条延迟线（支持 2/4/8/16），长度取素数（同一表内两两互质 → 无共同周期共振）；
    左右各持一套**不同**素数表 → 真实立体声去相关（左右是两个独立 FdnNetwork 实例）；
  - 反馈回路（每样本）：各线读出 → 一阶低通阻尼（damp1/damp2，与 Freeverb damping 同语义）
    → Householder 正交混合 → 乘反馈增益 g → 写回延迟线；环路往返增益 = g²·(低通 ≤1) < 1
    （g clamp ≤ 0.98）且正交矩阵不增能 → **无条件稳定，任何输入不发散**；
  - 输入按 1/√N 均匀注入各线（能量归一），输出取各线等权平均（1/N）；
  - preDelay 为输入侧独立环形延迟线（左右各持独立缓冲与独立位置指针）；
  - width 立体声交叉混合公式与 reverb-simple 完全一致（wet1/wet2）；
  - type 表提供 (roomSize, damping, delayScale) 基准，用户参数在基准附近 ±0.25 微调。
- **确定性**：同输入同参数必同输出；无随机、无 Date、无 console；process 内零分配
  （延迟缓冲 Float32Array[]、长度/位置 Int32Array、暂存 out/filt 均在构造时按最大配置
  16 线 × 最长延迟预分配；setParams 只改系数与长度，不重新分配）。
- **采样率**：构造时固定；fs ≤ 0 或非有限值抛 `Error('invalid sample rate')`。

## 二、接口签名（事实标准摘录）

```ts
export interface FdnReverbParams {
  roomSize: number      // 空间大小 0..1（→ 反馈增益，clamp ≤0.98）
  damping: number       // 阻尼 0..1（反馈回路一阶低通，与 Freeverb damping 同语义）
  wet: number           // 湿声增益 0..4
  dry: number           // 干声增益 0..4
  preDelayMs: number    // 预延迟 ms，0..1000
  width: number         // 立体声宽度 0..2（1=无交叉）
  type?: ReverbType     // 'hall' | 'room' | 'plate' | 'spring' | 'stage'（src/types.ts），默认 'hall'
  lines?: number        // 延迟线数 2/4/8/16，默认 8
}

export class FdnReverb {
  constructor(fs: number)
  setParams(p: FdnReverbParams): void
  processStereo(l: Float32Array, r: Float32Array): void   // 就地处理立体声
  reset(): void
}
```

## 三、参数表（向量 `params` 快照字段）

| 字段 | 类型 | 有效域（clamp 后生效值） | 默认值 | clamp / 边界行为 |
|---|---|---|---|---|
| `roomSize` | number | 输入钳制到 [0,1] 后参与混合 | 0.5¹ | `clamp01` 后与 type 基准混合：`effRoom = min(0.98, max(0, t.roomSize + (roomSize−0.5) × 0.5))`，作为反馈增益 g（≤0.98 保证环路 g²<1 无条件稳定） |
| `damping` | number | 输入钳制到 [0,1] 后参与混合 | 0.5¹ | `clamp01` 后同式混合：`effDamp = min(0.99, max(0.01, t.damping + (damping−0.5) × 0.5))`；反馈低通系数 damp1=effDamp、damp2=1−effDamp |
| `wet` | number | `[0, 4]` | 0.3¹ | 双向钳制；湿路总增益基准 |
| `dry` | number | `[0, 4]` | 0.7¹ | 双向钳制；干路直通增益 |
| `preDelayMs` | number（ms） | `[0, 1000]` | 0 | 双向钳制后 `preDelayLen = round(preDelayMs × fs / 1000)` 样本；**仅作用于湿路输入侧**（干路不经 preDelay）；=0 时延迟读取短路为恒等 |
| `width` | number | `[0, 2]` | 1 | 钳制后决定湿路交叉混合：`wet1 = wet × (width/2 + 0.5)`、`wet2 = wet × ((1 − width)/2)`；width>1 时 wet2 为负（超宽反相交叉） |
| `type` | string（五种枚举） | hall / room / plate / spring / stage | hall | 查 §3.2 房间参数表；未知值在运行时回退 hall（防御逻辑，向量不得依赖非法枚举） |
| `lines` | number（整数语义） | 2 / 4 / 8 / 16 | 8 | 仅允许这四个值（素数表齐备）；`Math.trunc` 后其余值（含 0/3/5 等）抛 `Error('FdnReverb: lines 必须为 2/4/8/16, 收到 <v>')` |

¹ 本模块类本身不设内置默认值（`setParams` 必须收到完整快照）；「默认值」列取引擎层默认快照
（`src/types.ts` `createDefaultParams` 的 reverb.algorithmic 组）。向量必须提供完整八字段
（`type`/`lines` 虽为可选，向量固定显式给出）。

### 3.2 type → 房间参数表（行为标准的一部分，FDN 自有调音，与 reverb-simple 的表不同）

| type | 基准 roomSize | 基准 damping | delayScale |
|---|---|---|---|
| `hall` | 0.7 | 0.4 | 1.3 |
| `room` | 0.4 | 0.6 | 0.6 |
| `plate` | 0.6 | 0.2 | 0.7 |
| `spring` | 0.3 | 0.8 | 0.35 |
| `stage` | 0.55 | 0.5 | 1.5 |

语义：type 提供 (roomSize, damping, delayScale) 基准特性，用户同名 roomSize/damping 参数在
基准附近 ±0.25 范围内微调（用户值 0.5 时即类型本身）；delayScale 缩放全部延迟线长度。

### 3.3 素数延迟表（@44.1kHz 基底，行为标准的一部分）

| lines | 左线（DELAYS_L） | 右线（DELAYS_R） |
|---|---|---|
| 2 | 499, 547 | 521, 563 |
| 4 | 599, 641, 677, 709 | 607, 653, 683, 727 |
| 8 | 701, 719, 733, 757, 773, 797, 811, 823 | 709, 727, 739, 761, 787, 809, 821, 829 |
| 16 | 701, 719, 733, 757, 773, 797, 811, 823, 827, 839, 853, 857, 859, 863, 877, 881 | 709, 727, 739, 761, 787, 809, 821, 829, 839, 853, 857, 859, 863, 877, 881, 883 |

实际延迟长度 = `max(1, round(基底 × type.delayScale × fs / 44100))`（互质属性在取整后近似保持）。

## 四、处理语义

### 4.1 每样本状态方程（两支线必须逐字一致）

单声道网络（左/右各持一个实例，仅素数表不同）对每条线 j：

```text
读出与阻尼低通（Freeverb damping 语义）：
  out_j  = buf_j[pos_j]
  filt_j = out_j × damp2 + store_j × damp1
  store_j = filt_j
Householder 正交混合 + 反馈增益（u·uᵀ/ (N/2) 形式只需行和）：
  u = (2/N) × Σ_i filt_i
  buf_j[pos_j] = (1/√N) × x + g × (filt_j − u)      # 注入 1/√N（能量归一）+ 反馈
  pos_j 前进 1（环绕：np = pos_j + 1; if np ≥ len_j then np = 0）
输出（等权平均）：
  y = (1/N) × Σ_j out_j
```

立体声输出混合（与 reverb-simple 相同公式）：

```text
l[i] = xl × dry + wetL × wet1 + wetR × wet2
r[i] = xr × dry + wetR × wet1 + wetL × wet2        # 交叉项使用对方声道湿声
```

- **preDelay（输入侧）**：左右各一个独立环形缓冲与独立位置指针（共用指针会使有效延迟减半，
  实现注释明确此点）。每样本：`读出 pos−preDelayLen 处旧值 → 写入当前输入 → 指针前进`；
  `preDelayLen = 0` 时直接短路返回输入（恒等）。网络收到的是 preDelay 后的信号；
  **干路不经 preDelay**。
- 块长语义：全部状态（延迟缓冲、pos、store、preDelay 环形缓冲与指针）均为**纯逐样本递推**，
  分块处理与一次性整块处理**逐位一致**（实证，见 §4.4）。

### 4.2 参数更新与状态语义

- `setParams` 即时生效：重算 effRoom/effDamp/wet1/wet2/dry/preDelayLen/各线长度与系数；
  内部缓冲、位置指针与 store 状态**保留**（不随 setParams 清零）——唯一例外：**lines 线数
  结构变化时调用 `reset()` 清空全部状态**（避免残留数据跨结构泄漏）；
- `reset()`：清空左右两个网络的延迟缓冲、pos、store，以及 preDelay 缓冲与指针；
- 构造期预分配上限：`maxDelay = ceil(883 × 1.5 × fs / 44100) + 2`（最长素数基底 × 最大
  delayScale），16 条线；preDelay 缓冲 `ceil(fs) + 1` 样本（1000ms 上限 @ 任意 fs 均可容纳）。

### 4.3 稳定性与量级

- 反馈增益 g = effRoom ≤ 0.98 → 环路往返增益 g²·(低通 ≤1) < 1；Householder 阵正交不增能
  → **无条件稳定**：输入停止后尾音单调衰减趋零（局部纹波允许），无自激发散；
- 湿声稳态量级 ≈ E(x²)/(N·(1−g²))（注入 1/√N、输出 1/N 的归一结果），安全有界；
  极端 wet=4/wet1=6（width=2）时输出可放大但保持有限。

### 4.4 实证行为记录（导出工具冻结前逐项验证）

以下事实由确定性探针在 TS 实现上实测（本规格写作时），作为两支线对拍的语义依据：

1. **wet=0 且 dry=1 → 输出与输入逐位一致**（左右声道皆然；`x×1 + wet×0 项` 的浮点求和
   不改变非零有限值；所用确定性输入不含 −0）。含 preDelayMs>0（按上界 1000 生效）时同样
   逐位一致——preDelay 只改变湿路，不影响干路。本模块的逐位锚点即此形态。
2. **跨块状态连续性逐位成立**：冲激响应与极值参数组合下，blockSize 分块处理与一次性整块
   处理输出逐位一致（全部状态为逐样本递推，无块级耦合）。
3. **width=0 同源单声道输入的左右一致是 1 ulp f32 量级、非逐位**：实测最大左右差
   ≈7.5e−9。原因：`l = x·dry + (wetL·wet1 + wetR·wet2)` 与 `r = x·dry + (wetR·wet1 +
   wetL·wet2)` 在 wet1=wet2 时数学上相等，但浮点加法不满足结合律（(x+A)+B ≠ (x+B)+A）。
   故本规格**不主张** width=0 单声道塌缩逐位一致，只主张 ≤1e−6 量级一致（与单元测试
   `test/fdnreverb.test.ts` 的 1e−6 容差一致）。
4. **preDelay 首峰位移**：脉冲响应整体平移 ≈ preDelayMs（输入侧延迟），preDelay 期内湿路
   输出为零。
5. **type 特性可区分**：同用户参数下 hall（基准 roomSize 0.7、delayScale 1.3）尾音显著长于
   room（0.4/0.6）（单元测试实测 0.2s 处能量差 >5dB）。

## 五、GWT 行为条款

> 定量断言一律以 `specs/dsp/vectors/fdn-reverb.*.json` 冻结夹具 + 容差公式判定（README §3.5），
> 条款内不内嵌具体参数数值与期望值。

### GWT-FDN-01：纯干声恒等直通（逐位锚点）
- **给定**：wet=0 且 dry=1（其余任意合法参数，含 preDelayMs 越上界按 1000 生效）
- **当**：送入任意允许输入（确定性伪噪声、多频正弦叠加）
- **则**：输出与输入**逐位一致**（左右声道皆然）——最强跨实现精度锚点，捕获任何混音系数、
  注入/输出增益或反馈路径的偏差

### GWT-FDN-02：冲激响应高密度尾音
- **给定**：纯湿声（dry=0）典型参数组合
- **当**：单声道首帧单位冲激（对侧声道静音）后接静音
- **则**：湿路呈现互质延迟网络特征——早期离散回声后尾音密度增长、50ms 后仍有非零尾音能量；
  冲激响应完整波形以冻结向量界定（驱动器任何延迟长度/矩阵/阻尼偏差必然超差）

### GWT-FDN-03：满幅激励有界不发散
- **给定**：任意合法参数组合（含 roomSize/wet/width 上界钳制形态）
- **当**：|x| ≤ 1 的满幅长序列或能量突发后接静音
- **则**：输出始终有限（无 NaN / Infinity），突发后尾音衰减、无自激回升（环路增益 < 1 保证）

### GWT-FDN-04：type/roomSize 基准语义
- **给定**：除 type 外相同的参数，分别取 hall 与 room（两种 delayScale/基准组合）
- **当**：同一激励分别处理
- **则**：衰减特性显著可区分（hall 长尾、room 短尾）；用户 roomSize/damping 以 type 基准
  ±0.25 微调的公式按 §三生效，精确波形以冻结向量界定

### GWT-FDN-05：width 交叉混合行为
- **给定**：同一双声道输入
- **当**：分别以 width=1 与 width=0（或 width=2 负交叉）各跑一条向量
- **则**：width=1 时湿路无左右交叉（wet2=0）；width=0 时湿路对半交叉；width=2 时 wet2 为负。
  width=0 且左右同源输入时左右输出 1 ulp f32 量级一致（实证，见 §4.4.3，非逐位）；
  差异量以成对冻结向量界定

### GWT-FDN-06：preDelay 仅作用于湿路
- **给定**：preDelayMs > 0、dry > 0
- **当**：送入任意信号
- **则**：干路成分逐样本直通（时序不变），湿路成分推迟 preDelayLen 样本；preDelayMs=0 时
  延迟读取短路为恒等（与显式 0 长度环形缓冲行为一致）

### GWT-FDN-07：极值参数钳制无数值事故
- **给定**：roomSize/damping 双向越界（clamp01 后按 §三混合式生效）、wet/dry 越界（[0,4]
  钳制）、preDelayMs 越上界（1000 生效）、width 越上界（2 生效，wet2 负）、lines=16 满配
- **当**：能量突发 + 静音衰减激励
- **则**：clamp 全部按生效值精确等效，全程无 NaN / Infinity、有界不发散；钳制语义随向量
  载荷固化

### GWT-FDN-08：跨块状态连续性（逐位一致）
- **给定**：任意参数与输入序列
- **当**：分别按 blockSize=k 分块处理与一次性整块处理（末块可短）
- **则**：两种方式输出**逐位一致**（延迟线/低通 store/preDelay 均为纯逐样本递推，切块不改变
  运算序列；实证）

### GWT-FDN-09：reset 后行为可复现（由单元测试覆盖，不入向量）
- **给定**：已处理过信号的实例
- **当**：reset() 后重放同一输入
- **则**：输出与首次从零状态处理的输出逐位一致；reset 清空延迟缓冲、指针、store 与 preDelay

### GWT-FDN-10：lines 结构变化清状态（由单元测试覆盖，不入向量）
- **给定**：已处理过信号的实例
- **当**：setParams 改变 lines（2/4/8/16 之间切换）
- **则**：内部状态被 reset 清空（不残留旧结构数据）；lines 为非法值时抛错

### GWT-FDN-11：静音输入零输出（由单元测试覆盖，不入向量）
- **给定**：任意参数、零状态实例
- **当**：送入全零输入长序列
- **则**：输出逐位全零（注入为 0、反馈回路从零状态出发恒保持零）

### GWT-FDN-12：抛错路径（由单元测试覆盖，不入向量）
- **给定**：fs ≤ 0 或非有限；或 lines 取 2/4/8/16 之外的值（含 0、3、小数截断后非法）
- **当**：构造 / 调用 setParams
- **则**：分别抛 `Error('invalid sample rate')`、
  `Error('FdnReverb: lines 必须为 2/4/8/16, 收到 <v>')`

## 六、向量用例

**本模块全部定量行为以 `specs/dsp/vectors/fdn-reverb.<case>.json` + 同名 `.f32` 冻结夹具
为准**；本章仅列预期 case 覆盖面（维度清单，不含具体参数数值），最终以导出工具产出并冻结的
内容为唯一判据。格式契约见 [`specs/README.md`](../README.md) §三；JSON 合法性由 Schema 校验。

预期覆盖面（case1–case4）：

1. **纯干声恒等锚点**：wet=0/dry=1，输出与输入逐位一致；preDelayMs 取越上界值（按 1000 生效）
   随载荷固化（GWT-FDN-01/06）；
2. **hall 冲激响应快照**：纯湿声、左首帧冲激 + 右静音（不对称激励验证立体声网络去相关）、
   type 基准参数（用户 roomSize/damping=0.5 中性）（GWT-FDN-02/04）；
3. **width=0 交叉混合对照**：type room、左右同源单声道伪噪声输入（GWT-FDN-05；多采样率
   44100 覆盖延迟长度 fs/44100 缩放路径）；
4. **stage 满配极值钳制**：type stage、lines=16、roomSize/damping/wet/dry/width 多项越界
   钳制、能量突发后接静音覆盖衰减尾与稳定性（GWT-FDN-03/07）。

维度说明：reset 复现与 lines 结构变化属过程行为，向量格式（单次 setParams + 分块驱动）无法
表达，由单元测试覆盖（GWT-FDN-09/10）；采样率维度 case3 取 44100，其余 48000。帧数对
blockSize 非整除（含末块短块）。

.f32 数据按 [README §3.3](../README.md) 四段 planar 小端布局存放；分块驱动方式按 §3.4
（本模块原生 `processStereo` 就地语义，与 reverb-simple 相同）。

## 七、关联文件

- 总纲契约：[`specs/README.md`](../README.md)
- Schema：[`specs/schema/vector-case.schema.json`](../schema/vector-case.schema.json)
- 行为事实标准：`src/dsp/FdnReverb.ts`；参数类型：`FdnReverbParams`（同文件）、
  `ReverbType`（`src/types.ts`）
- 兄弟算法混响规格：[reverb-simple](reverb-simple.md)（wet/dry/width 混合公式同源、
  damping 语义同源；type 表与延迟结构不同，两表不得混用）
- 引擎接线（混响路由消费方）：`src/engine/HyperSoundEngine.ts`（`reverb.mode='fdn'` →
  FdnReverb；接线传入 algorithmic 组参数，lines 不经引擎、保持默认 8）
- 参考单元测试：`test/fdnreverb.test.ts`、`test/fdn-engine.test.ts`
- 兄弟规格：[biquad](biquad.md) ｜ [limiter](limiter.md) ｜ [reverb-simple](reverb-simple.md) ｜
  [compressor](compressor.md) ｜ [bass-enhancer](bass-enhancer.md) ｜ [mid-side](mid-side.md) ｜
  [eq-chain](eq-chain.md) ｜ [deesser](deesser.md) ｜ [loudness-comp](loudness-comp.md)
