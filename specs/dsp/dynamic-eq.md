# 规格：dynamic-eq —— 自适应动态均衡（全通交叉分带 + 频谱包络驱动增益）

> **规格属性**：本文件是双支线共享规格。行为事实标准 = `src/dsp/DynamicEq.ts`；
> 参数字段名一律以该源码（`DynamicEqParams` / `DynamicEqBandParam`）为准，本规格不得
> 臆造字段。格式契约见 [`specs/README.md`](../README.md) §三，
> 向量合法性由 [`specs/schema/vector-case.schema.json`](../schema/vector-case.schema.json) 约束。

---

## 一、模块概述

- **定位**：链路级自适应动态均衡——固定 5 带「一阶全通交叉」分带网络逐样本分带 →
  按分析块累加各带平方能量 → dB 域软拐点压缩曲线由带电平相对阈值生成各带目标增益 →
  静态目标曲线（`targetGainDb`）与压缩量经 `strength` 干湿混合 → 逐样本一阶平滑后
  加权求和写回。引擎链中位于 analysis 取样之后、LUFS 采样之前（`dynamic-eq` 阶段）。
- **出处**：分带网络采用经典「全通交叉」结构——LP = (1+A)/2、HP = (1−A)/2 共享同一个
  一阶全通 A，代数恒等 LP+HP = 1 ⇒ 单位增益时各带信号之和精确重建输入、无静态染色；
  dB 域软拐点压缩公式为 DAW 压缩器通用形式（与 [compressor](compressor.md) 同公式族，
  行业公开形式，未复制第三方代码）；整体组合实现为自研。
- **确定性**：同输入同参数必同输出；无随机、无 Date、无 console；processStereo 内
  零分配（交叉树 8×2 个 Biquad 与全部带状态在构造期预分配）。
- **采样率**：构造时固定；fs ≤ 0 或非有限值抛 `Error('invalid sample rate')`。

## 二、接口签名（事实标准摘录）

```ts
export interface DynamicEqBandParam {
  enabled: boolean        // 该带是否参与动态处理（关闭则该带目标增益恒 1）
  frequency: number       // 交叉频率 Hz：该带与下一带的分界；末带忽略
  targetGainDb?: number   // 静态目标偏移 dB（低于阈值时该带保持此静态增益）
}

export interface DynamicEqParams {
  enabled?: boolean       // 总开关：false 时硬直通
  strength?: number       // 整体强度 0..1：0=直通，1=完全生效
  thresholdDb?: number    // 触发阈值 dB（默认 -20）
  ratio?: number          // 每带压缩比（默认 2）
  kneeDb?: number         // 软拐点 dB（默认 6）
  attackMs?: number       // 增益平滑 attack ms（默认 20）：增益下降速度
  releaseMs?: number      // 增益平滑 release ms（默认 200）：增益恢复速度
  blockSize?: number      // 分析块大小（默认 128）：每块计算一次各带能量
  bands?: DynamicEqBandParam[]  // 固定 5 带；数组不足 5 项时其余带保持当前/默认配置，超出忽略
}

export class DynamicEq {
  constructor(fs: number, params?: Partial<DynamicEqParams>)
  setParams(p: Partial<DynamicEqParams>): void
  processStereo(l: Float32Array, r: Float32Array): void   // 就地处理
  getBandGains(): number[]        // 当前每带平滑增益（线性，5 项；调试/UI 用，不进向量）
  getBandLevelsDb(): number[]     // 最近一次分析的各带电平 dB（调试/UI 用，不进向量）
  getBandNames(): string[]        // ['low','low-mid','mid','high-mid','high']
  reset(): void
}
```

## 三、参数表（向量 `params` 快照字段）

向量 `params` 为模块完整形状（全部字段显式给出，不依赖缺省）；采样率 fs 不进 params，
取自向量顶层 `sampleRate` 字段。

| 字段 | 类型 | 有效域（clamp 后生效值） | 默认值 | clamp / 边界行为 |
|---|---|---|---|---|
| `enabled` | boolean | true / false | true | **false 时 `processStereo` 首行直接返回**：缓冲不被改写（逐位直通），增益/目标/包络/滤波器全部状态不推进（实证） |
| `strength` | number | `[0, 1]` | 1 | 越界双向钳制；**`strength ≤ 0` 与 enabled=false 同路径硬直通**（首行联合判定，状态不推进，实证） |
| `thresholdDb` | number（dB） | `[−80, 0]` | −20 | 越界双向钳制 |
| `ratio` | number | `[1, 100]` | 2 | 越界双向钳制；`ratio = 1` 时压缩量恒为 0（invRatio=0，同 compressor 语义） |
| `kneeDb` | number（dB） | `[0, 40]` | 6 | 越界双向钳制；`kneeDb = 0` 走硬拐点分支（reduction = max(over,0)·(1−1/ratio)） |
| `attackMs` | number（ms） | 生效下限 0.05 ms | 20 | 经 `onePoleCoef(p.attackMs, fs, 0.05)` 换算：`coef = 1 − exp(−1 / ((max(ms,0.05)/1000) × fs))` |
| `releaseMs` | number（ms） | 生效下限 1 ms | 200 | 经 `onePoleCoef(p.releaseMs, fs, 1)` 换算（release 下限 1 ms，**与 attack 的 0.05 ms 不同**） |
| `blockSize` | number（整数语义，样本） | `[16, 2048]` | 128 | `max(16, min(2048, floor(x)))` 双向钳制；为**内部分析块长**（与向量顶层驱动分块 `blockSize` 是两个独立参数，见 §4.5） |
| `bands` | DynamicEqBandParam[5] | 每项见下三行 | （带级默认见 §4.7） | 数组语义见本表后补充说明 |
| `bands[].enabled` | boolean | true / false | true | false 时该带目标增益恒为 1（静态直通该带），**不影响该带交叉频率的写入** |
| `bands[].frequency` | number（Hz） | `[30, fs/2 × 0.9]` | 200/800/2500/8000 | 仅 `i < 4` 的带读取（band i 与 band i+1 的分界）；越界双向钳制；**末带（i=4）的 frequency 完全被忽略**（不读取、不钳制，实证：取 0 / 8000 / 99999 输出逐位一致） |
| `bands[].targetGainDb` | number（dB） | `[−12, 12]` | 0 | 越界双向钳制；低于阈值时该带的静态目标增益 |

补充说明（bands 数组语义，实证记录）：

- 向量 `bands` 固定提供**恰好 5 项**且**每项必含 `frequency`**：当 bands 数组被提供时，
  缺失的 `frequency` 字段会经 clamp 原样落为 `undefined` 进入交叉系数（NaN 级联，
  未定义行为）——该形态**禁止进入向量**（实证：全序列 NaN）；
- `bands` 整体缺省（不传）时交叉频率保持默认 200/800/2500/8000，行为有限（实证）；
  向量不使用该形态；
- `bands` 数组短于 5 项时，缺项带保持**当前/默认配置**（enabled=true、交叉默认、
  targetGainDb=0）；超出 5 项的部分被忽略。向量固定 5 项全配，不依赖该回退。

## 四、处理语义

### 4.1 全通交叉分带网络（两支线必须逐字一致）

每通道 8 个 Biquad 组成链式交叉树（左右通道两棵树、状态独立）：

```text
交叉系数（fc 为带 i 与带 i+1 的分界）：
wc = 2π·fc / fs
a1 = −tan(π/4 − wc/2)
LP: b0 = b1 = (1+a1)/2, b2 = 0, a1, a2 = 0     # 一阶低通（TDF2）
HP: b0 = (1−a1)/2, b1 = −(1−a1)/2, b2 = 0, a1, a2 = 0   # 一阶高通（TDF2）

带信号（x 为本通道输入样本；t[k] 为树节点）：
r1 = HP1(x);  band0 = LP1(x)
r2 = HP2(r1); band1 = LP2(r1)
r3 = HP3(r2); band2 = LP3(r2)
r4 = HP4(r3); band3 = LP4(r3)
band4 = r4                                     # 链式残差
```

- LP+HP = 1 代数恒等 ⇒ 所有带增益为 1 时输出精确重建输入（逐位，实证）；
- 树内处理顺序固定：先算 HP 支路残差 r_k，再算同层 LP——逐样本、逐通道同构。

### 4.2 块能量分析（两支线必须逐字一致）

```text
每分析块开始：sumsq[b] = 0（b = 0..4）
块内逐样本：sumsq[b] += bandL[b]² + bandR[b]²        # 立体声联合能量
块末（len = 本块样本数）：
levelDb[b] = 10 × log10( sumsq[b] / (2·len) + 1e-12 )   # +1e-12 静音地板防 log(0)
```

### 4.3 目标增益计算（每分析块末，两支线必须逐字一致）

```text
over      = levelDb[b] − thresholdDb
invRatio  = 1 − 1/ratio
knee ≤ 0 （硬拐点）： reduction = over > 0 ? over × invRatio : 0
knee > 0 （软拐点）：
  over < −knee/2  → reduction = 0
  over >  knee/2  → reduction = over × invRatio
  否则            → x = over + knee/2；reduction = invRatio × x² / (2·knee)
targetDb  = targetGainDb[b] − reduction
targetLin = 10^(targetDb / 20)
mixed     = 1 + strength × (targetLin − 1)              # strength 干湿混合
targets[b] = bands[b].enabled ? clamp(mixed, 0, 3) : 1  # 增益钳制 [0, 3]
```

- `ratio = 1` ⇒ invRatio = 0 ⇒ 压缩量恒 0，目标 = 静态曲线（同 compressor GWT-CP-08 语义）；
- 带禁用（`bands[b].enabled = false`）⇒ 该带目标恒 1，但**带信号仍参与求和**
  （输出 = Σ gain·band，禁用带以增益 1 加入）。

### 4.4 增益平滑与输出（逐样本，两支线必须逐字一致）

```text
gains[b] += (targets[b] < gains[b] ? attackCoef : releaseCoef) × (targets[b] − gains[b])
out = Σ_b gains[b] × band[b]                            # 就地写回 L/R
```

- 目标低于当前增益走 attackCoef（下降快），高于走 releaseCoef（恢复慢）；
- 所有带增益为 1 时 Σ band = x（LP+HP=1 重建），输出与输入逐位一致。

### 4.5 控制节奏与分块耦合（模块特有行为事实，两支线必须一致）

- 控制延迟一个分析块：第 k 块的能量决定第 k+1 块的目标增益；增益平滑逐样本掩盖块粒度；
- **内部分析块边界 = min(params.blockSize, 本次 processStereo 调用的剩余样本数)**：
  sumsq 在每个分析块开始清零、**不跨 processStereo 调用累积**——因此当向量顶层驱动
  分块 blockSize **不是** `params.blockSize` 的整数倍时，每次驱动调用都会在调用边界
  提前触发一次控制更新，**输出依赖驱动分块**（实证：chunk=333 vs chunk=512 最大差
  2.1e−2 量级）；
- 驱动分块为 `params.blockSize` 整数倍时，分块处理与一次性整块处理**逐位一致**（实证）；
- 因此对立体声入口而言**向量顶层 blockSize 是行为参数**，由冻结向量固定；两支线对拍
  必须按同一 blockSize 回放（同 [eq-chain](eq-chain.md) §4.4 / [loudness-comp](loudness-comp.md) 的既有约定）。

### 4.6 向量驱动器语义

- 构造 `DynamicEq(sampleRate, params)`（params 直传构造，等价构造后一次 setParams——
  两者走同一 applyParams）；逐块 `processStereo(l, r)` 就地处理，按 [README §3.4](../README.md)
  分块、末块可短、状态跨块保持；
- 向量 `params` 为 §三的模块完整形状；不使用 sidechain/外部检测等扩展形态。

### 4.7 引擎接线事实（`src/engine/HyperSoundEngine.ts`，两支线引擎层对齐用）

- 引擎只传 `enabled / strength / thresholdDb / ratio / attackMs / releaseMs` 与
  `bands`，**不传 `kneeDb` / `blockSize`**（模块默认 kneeDb=6、blockSize=128 生效）；
- `bands` 由引擎侧设置形状 `DynamicEqSettings.bands: { enabled, targetGainDb }[]` 映射而来，
  **交叉频率由引擎固定注入**：`frequency: DYNAMIC_EQ_CROSSOVERS[i] ?? 0`
  （常量 `[200, 800, 2500, 8000]`；末带取 0，被模块忽略）；
- 实证：引擎注入形状（frequency=200/800/2500/8000、末带 0）与按 §三显式携带同值
  frequency 的模块形状（末带 frequency 任取）输出**逐位一致**——向量显式携带 frequency
  是引擎行为的超集固化；
- 引擎 stage 门控 `active: () => params.dynamicEq.enabled` 与模块自身 `enabled` 双重判定，
  两者皆真才处理；引擎在 `dynamicEq.enabled` false→true 迁移时调用 `reset()`。

### 4.8 状态集合与生命周期

内部状态：交叉树 8×2 个 Biquad 的 TDF2 状态 + `sumsq/levelsDb`（5 项）+
`targetGains/gains`（5 项）。

- `setParams` **保留增益/目标/电平/滤波器状态**（参数即时生效、不清历史，避免改参爆音）
  ——仅钳制参数并重算交叉树系数与 attack/release 系数；
- `enabled = false`（或 `strength ≤ 0`）时逐样本循环整体跳过：缓冲不被改写（逐位直通），
  **所有状态不推进**；
- `reset()` 清空全部滤波器状态并将 sumsq/levelsDb 归零、targetGains/gains 归 1
  （重放与首次一致）。

### 4.9 实证行为记录（导出工具冻结前逐项验证）

1. **enabled=false → 输出与输入逐位一致**（左右声道皆然，全部状态不推进）；
2. **strength=0 → 与 enabled=false 同路径逐位直通**（gains 保持 1，实证）；
3. **输出依赖驱动分块与 params.blockSize 的整除关系**（实证：非整数倍时 chunk=333 vs
   512 最大差 2.1e−2 量级；整数倍时与整块处理逐位一致）；
4. **强激励使带包络超阈 → gains 稳态下探至 0.45–0.58 量级**；**静态目标提升 → gains
   爬升至 strength 混合目标**（实证，具体轨迹由冻结向量界定）；
5. **极值参数钳制与「直接按生效值配置」逐位一致**（strength/thresholdDb/ratio/kneeDb/
   attackMs/releaseMs/blockSize/bands[].frequency/bands[].targetGainDb 全维度，48000 与
   44100 两采样率实证）；
6. **末带 frequency 被完全忽略**（0 / 8000 / 99999 输出逐位一致，实证）；
7. **bands 项缺 frequency → NaN 级联**（未定义行为，实证全序列 NaN；禁止进入向量，
   向量 bands 项必含 frequency）；
8. 交叉树上界随采样率变化：`fs/2 × 0.9`（48000 → 21600，44100 → 19845，实证）。

## 五、GWT 行为条款

> 定量断言一律以 `specs/dsp/vectors/dynamic-eq.<case>.json` 冻结夹具 + 容差公式判定
> （README §3.5），条款内不内嵌具体参数数值与期望值。

### GWT-DY-01：禁用即直通（逐位锚点）
- **给定**：enabled=false 的任意参数组合（其余参数取激进值随载荷固化）
- **当**：送入任意信号（含全频带能量）
- **则**：输出与输入**逐位一致**（左右声道皆然），增益/目标/电平/滤波器全部状态不推进

### GWT-DY-02：全带静态提升 + strength 干湿
- **给定**：enabled=true、5 带全部启用、targetGainDb 全为正（静态提升曲线）、中等 strength、
  阈值较高（激励电平多在阈下，reduction 小）
- **当**：常规确定性激励长序列
- **则**：各带增益从 1 沿 release 路径爬升至 `1 + strength × (10^(targetDb/20) − 1)` 附近的
  稳态（具体轨迹以冻结向量界定）；输出相对输入呈现频带相关的提升；全程无 NaN / Infinity

### GWT-DY-03：阈值触发压缩与 attack/release 轨迹
- **给定**：enabled=true、阈值取值使强激励带电平稳定高于阈值；激励为前半段强信号、
  后半段静音的确定性序列
- **当**：长序列分块处理
- **则**：前半段超阈带增益沿 attack 路径下探至压缩稳态（<1），后半段沿 release 路径恢复；
  增益轨迹（含一个分析块的控制延迟）整段冻结，任何增益公式/平滑系数/块长偏差必然超差

### GWT-DY-04：部分带禁用隔离
- **给定**：enabled=true、部分带 enabled=false、其余带启用
- **当**：全频带激励
- **则**：禁用带目标增益恒为 1（该带静态直通参与求和），启用带按 §4.3 动态；
  禁用带选择任何偏差都会在输出中产生可测差异

### GWT-DY-05：极值参数钳制
- **给定**：strength > 1、thresholdDb 越下界、ratio 越上界、kneeDb 越上界、attackMs=0、
  releaseMs=0、blockSize 越下界、bands[].frequency 双向越界（含 44100 下 nyq 上界
  生效）、targetGainDb 双向越界 ±12、末带 frequency 取超大值（被忽略）的组合
- **当**：常规激励
- **则**：clamp 按生效值精确等效（实证：与直接按边界值配置输出逐位一致），全程无数值事故；
  钳制语义随向量载荷固化

### GWT-DY-06：控制节奏与分块耦合（行为事实）
- **给定**：任意启用参数、params.blockSize 取默认值
- **当**：按冻结向量的顶层 blockSize 分块逐块调用 processStereo
- **则**：两支线按同一块长回放时输出一致（对拍判定）；顶层 blockSize 非 params.blockSize
  整数倍时，控制更新在每次调用边界提前触发、输出随块长变化（实证），故本模块不主张
  与驱动块长无关，只主张同一块长下逐块回放可复现

### GWT-DY-07：单位增益精确重建（由单元测试覆盖，不入向量）
- **给定**：enabled=true、全部 targetGainDb=0、激励电平稳定低于阈值（reduction 恒 0、
  目标恒 1）
- **当**：任意激励稳态后
- **则**：全部带增益收敛回 1，输出与输入逐位一致（LP+HP=1 重建 + 增益 1）

### GWT-DY-08：静音输入（由单元测试覆盖，不入向量）
- **给定**：enabled=true、任意合法参数
- **当**：送入全零输入长序列
- **则**：输出全零；levelDb 落在 `10·log10(1e-12)` 地板（无 NaN）；目标 = 静态曲线

### GWT-DY-09：reset 后行为可复现（由单元测试覆盖，不入向量）
- **给定**：已处理过信号的实例
- **当**：reset() 后重放同一输入
- **则**：输出与首次从零状态处理的输出完全一致（滤波器/能量/增益/目标全部复位）

### GWT-DY-10：抛错路径（由单元测试覆盖，不入向量）
- **给定**：fs ≤ 0 或非有限；或 processStereo 传入不等长 L/R
- **当**：构造 / 调用
- **则**：分别抛 `Error('invalid sample rate')`、`Error('dynamiceq: L/R length mismatch')`

### GWT-DY-11：输出有界不发散
- **给定**：任意合法参数（含极值钳制组合）
- **当**：|x| ≤ 1 满幅长序列
- **则**：每带增益钳制在 [0, 3]，交叉树单位增益精确重建 ⇒ 输出有限、长时间运行不发散

## 六、向量用例

**本模块全部定量行为以 `specs/dsp/vectors/dynamic-eq.<case>.json` + 同名 `.f32` 冻结夹具
为准**；本章仅列预期 case 覆盖面（维度清单，不含具体参数数值），最终以导出工具产出并
冻结的内容为唯一判据。格式契约见 [`specs/README.md`](../README.md) §三；JSON 合法性由
Schema 校验。

预期覆盖面（case1–case4）：

1. **禁用恒等锚点**：enabled=false，输出与输入逐位一致；输入含低/中/高频正弦叠加与
   固定种子 LCG 噪声（GWT-DY-01）；
2. **全带静态提升**：5 带全启用、目标曲线全正、中等 strength、阈值较高（静态曲线主导），
   顶层 blockSize 取非 params.blockSize 整数倍（控制节奏耦合入载荷）（GWT-DY-02/06）；
3. **阈值/attack-release 行为**：阈值较低、前半段强激励（多带超阈、attack 下探）、后半段
   静音（release 恢复）；顶层 blockSize 为 params.blockSize 整数倍（GWT-DY-03/06）；
4. **部分带禁用 + 钳制极值**：bands[1]/bands[3] 禁用、全参数域越界钳制（含 44100 下
   crossover 上界与末带 frequency 忽略语义）、params.blockSize 越下界；多采样率 44100
   覆盖（GWT-DY-04/05）。

采样率维度：case4 取 44100，其余 48000（attack/release 系数与 crossover 钳制上界随 fs
变化）。帧数对顶层 blockSize 非整除（含末块短块）。`getBandGains` / `getBandLevelsDb`
为调试接口，不进向量。

.f32 数据按 [README §3.3](../README.md) 四段 planar 小端布局存放；分块驱动方式按 §3.4
（本模块原生 `processStereo` 就地语义），且两支线必须遵守 §4.5 的同一块长回放约定。

## 七、关联文件

- 总纲契约：[`specs/README.md`](../README.md)
- Schema：[`specs/schema/vector-case.schema.json`](../schema/vector-case.schema.json)
- 行为事实标准：`src/dsp/DynamicEq.ts`；参数类型：`DynamicEqParams` / `DynamicEqBandParam`
  （同文件）；引擎侧设置形状：`src/types.ts`（DynamicEqSettings）
- 基本单元规格：[biquad](biquad.md)（交叉树一阶 LP/HP 的 TDF2 递推）
- 压缩公式同族规格：[compressor](compressor.md)（dB 域软拐点曲线、ratio=1 语义）
- 引擎接线（调用顺序事实）：`src/engine/HyperSoundEngine.ts`（`DYNAMIC_EQ_CROSSOVERS`
  固定注入交叉频率；dynamic-eq 阶段门控与 false→true reset）
- 参考单元测试：`test/dynamiceq.test.ts`（模块级）、`test/dynamic-eq-engine.test.ts`（引擎级）
- 兄弟规格：[biquad](biquad.md) ｜ [limiter](limiter.md) ｜ [compressor](compressor.md) ｜
  [deesser](deesser.md) ｜ [eq-chain](eq-chain.md) ｜ [loudness-comp](loudness-comp.md) ｜
  [mod-effects](mod-effects.md)
