# 规格：eq-chain —— 多段参数 EQ 级联 + 级联 Q 补偿

> **规格属性**：本文件是双支线共享规格。行为事实标准 = `src/dsp/EqChain.ts`；
> 参数字段名一律以该源码（`EqBandParam`）为准，本规格不得臆造字段。
> 格式契约见 [`specs/README.md`](../README.md) §三，
> 向量合法性由 [`specs/schema/vector-case.schema.json`](../schema/vector-case.schema.json) 约束。

---

## 一、模块概述

- **定位**：链路级多段参数 EQ——每段一个 RBJ peaking 双二阶滤波器依段序级联
  （引擎内 Pre-EQ 与 IEQ 两处接入，段数由构造参数决定）；提供自研「级联 Q 补偿」：
  在各段控制频点测量级联幅频响应，按 dB 误差迭代修正各段增益，把相邻段叠加导致的
  级联响应偏差压回目标附近。
- **出处**：每段滤波器公式为 RBJ Audio EQ Cookbook peaking（见 [biquad](biquad.md)）；
  级联 Q 补偿算法为自研（Gauss-Seidel 式逐段顺序迭代 + 0.8 阻尼，历史实测可把相邻段
  叠加的 ~10dB 级误差压到 0.03dB 量级）。
- **确定性**：同输入同参数必同输出；无随机、无 Date、无 console；process/processStereo
  内零分配（补偿迭代只发生在 `setBands` / `setQCompensation` 的非实时路径）。
- **采样率**：构造时固定；fs ≤ 0 或非有限值抛 `Error('invalid sample rate')`。

## 二、接口签名（事实标准摘录）

```ts
export interface EqBandParam { frequency: number; gain: number; q: number }

export class EqChain {
  constructor(fs: number, bandCount?: number)   // 默认 20；生效值 = max(1, floor(bandCount))
  setBands(bands: EqBandParam[]): void          // 重算系数；若 qCompensation 已开则先补偿迭代
  setQCompensation(enabled: boolean): void      // 开启时补偿迭代 + 重算系数；关闭时仅翻标志
  responseAt(freqs: number[]): Float32Array     // 级联幅频响应测量（分析用途，不进向量）
  process(x: number): number                    // 单样本级联
  processBlock(input: Float32Array, output: Float32Array): void
  processStereo(l: Float32Array, r: Float32Array): void   // 就地处理
  reset(): void
}
```

## 三、参数表（向量 `params` 快照字段）

向量 JSON 的 `params` 对象固定使用以下三字段；采样率 fs 不进 params，取自向量顶层
`sampleRate` 字段：

| 字段 | 类型 | 有效域（clamp 后生效值） | 默认值 | clamp / 边界行为 |
|---|---|---|---|---|
| `bandCount` | number（整数语义） | ≥ 1 | 20 | 生效值 = `Math.max(1, Math.floor(bandCount))`；向量固定显式给出，不依赖缺省 |
| `qCompensation` | boolean | true / false | 内部初值 false | 开启时触发 §4.3 补偿迭代；关闭时仅翻标志（见 §4.3 关闭语义） |
| `bands` | EqBandParam[] | 每项 `{ frequency, gain, q }` | — | 数组长度可与 `bandCount` 不等，语义见 §4.2；三个数值字段的钳制见下三行 |
| `bands[].frequency` | number（Hz） | `[20, min(20000, fs/2 × 0.999)]` | （填充段）1000 | 越界双向钳制：低于 20 Hz 钳到 20；高于 `fmax = min(20000, fs/2 × 0.999)` 钳到 fmax |
| `bands[].gain` | number（dB） | `[−24, 24]` | （填充段）0 | 越界双向钳制；补偿迭代内的中间增益同样按 ±24 再钳制（§4.3） |
| `bands[].q` | number | `[0.1, 18]` | （填充段）1 | 越界双向钳制 |

补充说明：

- EqChain 域比内层 biquad 设计域（f ≥ 10 Hz、q ≥ 1e−6、gain ±60，见 [biquad](biquad.md) §三）
  更窄，正常参数域内 biquad 内层钳制不会被触发（属实现防御层）；
- 每段传入 `Biquad.setParams('peaking', frequency, q, gain)` 的实参为 **clamp 后生效值**，
  向量载荷中的钳制语义以生效值进入系数。

## 四、处理语义

### 4.1 级联结构

`bandCount` 段 peaking biquad 依段序串联；第 i 段输出即第 i+1 段输入。
零增益段（gain = 0）的分子分母多项式解析恒等（A = 10^0 = 1 ⇒ b0 = 1、b1 = a1、b2 = a2），
TDF2 状态恒保持为零，时域输出与输入逐位一致（冻结向量所用频点组合经实证）。

### 4.2 setBands 与填充段语义

- 对 `i ∈ [0, bandCount)`：`bands[i]` 存在时按 §三钳制写入 `freqs[i]/userGains[i]/qs[i]`，
  且 `gains[i] = userGains[i]`（补偿从用户目标出发）；`bands[i]` 缺失（数组短于
  bandCount）时该段回退填充值 freq=1000 / gain=0 / q=1（0 dB 直通段）；
- `bands.length > bandCount` 时多出的段被忽略（只取前 bandCount 项）；
- `activeCount = min(bands.length, bandCount)`：**只有前 activeCount 段参与补偿迭代**，
  填充段不参与补偿（保持 0 dB 直通）；
- 收尾：若补偿开关已开则先执行 §4.3 迭代；随后无条件重算全部段系数。

### 4.3 级联 Q 补偿（精确迭代语义——两支线必须逐字一致）

触发点仅有两处：`setBands`（当补偿开关已开启时）与 `setQCompensation(false → true)`。
补偿基准是 `userGains`（用户目标增益，迭代期间固定不变）；迭代从**当前** `gains` 出发
（单次驱动语义下即 `gains = userGains` 起点）。过程逐字复刻 TS：

```text
① 先按当前 gains 同步全部段系数（updateCoeffs）
② 至多 5 轮迭代；每轮按段序 i = 0 .. activeCount−1 顺序执行（Gauss-Seidel）：
   mag    = Π_{j=0..bandCount−1} magnitudeAt(段 j 系数, freqs[i], fs)
            # 在段 i 控制频点测整条级联（含填充段）的线性幅度
   target = 10^(userGains[i] / 20)
   errDb  = 20 · log10( target / max(mag, 1e−12) )
   gains[i] = clamp(gains[i] + 0.8 × errDb, −24, 24)     # 0.8 阻尼 + 增益域再钳制
   立即用 (freqs[i], qs[i], gains[i]) 重算段 i 系数        # 后续段测量看到前面的修正
   maxErrDb = max(maxErrDb, |errDb|)
③ 一轮结束时若 maxErrDb < 0.05 dB 则提前终止
④ 迭代完成后（各触发点）再整体重算一次全部段系数
```

- 收敛性：0.8 阻尼使迭代为收缩映射；典型相邻段叠加场景 2–3 轮内收敛
  （冻结向量 case2 实测 2 轮收敛）；
- **关闭语义（行为事实）**：`setQCompensation(true → false)` 仅翻转标志，
  **不重算系数**——已补偿的 `gains` 与系数保留到下一次 `setBands`；
- **调用顺序无关（实证）**：`setBands → setQCompensation(true)` 与
  `setQCompensation(true) → setBands` 的终态逐位一致（两种顺序下补偿迭代的起点
  状态相同）。向量驱动器采用引擎接线顺序（`HyperSoundEngine.ts`：先 `setBands`
  后 `setQCompensation`）。

### 4.4 立体声映射与块内顺序（模块特有——两支线必须一致）

- 左右声道**共享同一条级联滤波状态**（TS `processStereo` 对同一组 biquad 先整条处理
  L 块、再整条处理 R 块）；这与 biquad 规格 §五的「左右独立实例」映射不同；
- 块内处理顺序固定：**band0 → … → band(n−1) 依次跑完整个 L 块，再 band0 → … →
  band(n−1) 依次跑完整个 R 块**；R 块从 L 块结束时的级联状态继续演化；
- 推论（实证记录）：**输出依赖 blockSize**——跨声道状态交错历史随块长变化
  （L 声道从第二个数据块起、R 声道从第一个数据块起偏离「整块一次性处理」参照，
  偏离量可达 1e−1 量级）。因此对立体声入口而言 blockSize 是行为参数，
  由冻结向量的 `blockSize` 字段固定；两支线对拍必须按同一 blockSize 回放同一声道排列；
- **单声道入口无此耦合**：`processBlock`（同一缓冲或分离缓冲）为纯逐样本级联，
  分块与整块逐位一致（实证）；
- f32 落点：每段输出写回 f32 缓冲后才进入下一段（**级联段间信号经 f32 量化**）；
  段内 TDF2 状态 s1/s2 与全部系数为 f64；
- `processStereo` 就地改写缓冲；L/R 长度不匹配抛
  `Error('eqchain: L/R length mismatch')`；`processBlock` 长度不匹配抛
  `Error('eqchain: input/output length mismatch')`。

### 4.5 reset 语义

`reset()` 将全部段 TDF2 状态清零；系数、`gains/userGains/freqs/qs`、`activeCount` 与
补偿开关**全部保留**（对齐 biquad 的 reset 只清状态语义）。

## 五、GWT 行为条款

> 定量断言一律以 `specs/dsp/vectors/eq-chain.<case>.json` 冻结夹具 + 容差公式判定
> （README §3.5），条款内不内嵌具体参数数值与期望值。

### GWT-EQ-01：零增益全直通（逐位锚点）
- **给定**：bandCount 段全部 gain=0（frequency/q 取各自合法值）、qCompensation=false
- **当**：送入任意允许输入（正弦叠加、确定性伪噪声）
- **则**：级联退化为恒等系统，输出与输入**逐位一致**（左、右声道皆然，含共享状态续跑的
  右声道）——最强跨实现精度锚点，捕获任何系数/落点纪律偏差

### GWT-EQ-02：boost/cut 混合级联响应
- **给定**：少量活动段（含正增益与负增益混合）、qCompensation=true
- **当**：在活动段频点有能量的确定性激励下长序列处理
- **则**：输出相对输入呈现频段相关的提升/衰减（补偿后控制频点级联增益趋近用户目标，
  残差由冻结向量界定）；全程无 NaN / Infinity，输出有界

### GWT-EQ-03：Q 补偿 on/off 成对差异
- **给定**：两条参数仅 `qCompensation` 不同的成对向量（其余字段与输入完全相同）
- **当**：同一确定性激励分别处理
- **则**：补偿开启与关闭的输出显著可区分（相邻段叠加下补偿关闭的级联响应偏离用户目标、
  开启的收敛到目标附近）；驱动器若漏做补偿迭代或错置补偿基准必然超差

### GWT-EQ-04：段数变体与填充段直通
- **给定**：bandCount 分别取 5 / 10 / 20（覆盖构造域的代表点），其中部分向量
  `bands.length < bandCount`（尾部填充段生效）
- **当**：常规激励
- **则**：级联段数与 bandCount 一致；填充段保持 0 dB 直通语义（等价 GWT-EQ-01 的恒等段），
  不参与补偿（activeCount = bands.length）；向量夹具覆盖 5 / 10 / 20 三档

### GWT-EQ-05：越界参数钳制
- **给定**：frequency 低于 20 Hz 与高于 fmax、gain 双向越界 ±24、q 双向越界 [0.1, 18]
  的组合（按 §三生效值进入系数）
- **当**：常规激励
- **则**：clamp 按生效值精确等效（等效于直接按边界值配置），全程无数值事故；
  钳制语义随向量载荷固化

### GWT-EQ-06：补偿迭代收敛有界
- **给定**：qCompensation=true、活动段含相邻频点提升组合
- **当**：补偿迭代执行（至多 5 轮）
- **则**：迭代中增益始终落在 [−24, 24]（±24 再钳制），最终增益有限且与冻结向量一致；
  阻尼 0.8 与逐段（Gauss-Seidel）更新顺序任何偏差都会造成可测差异

### GWT-EQ-07：共享状态块内顺序与 blockSize 敏感性（行为事实）
- **给定**：任意启用参数
- **当**：按冻结向量的 blockSize 分块、每块内先整条 L 后整条 R 地调用 processStereo
- **则**：两支线按同一块长与同一声道排列回放时输出一致（对拍判定）；
  改变块长会改变共享状态交错历史从而改变输出（实证：L 自第二块、R 自首块偏离整块参照），
  故跨块状态连续性以「同一 blockSize 下逐块回放可复现」表述，
  立体声入口不主张与块长无关

### GWT-EQ-08：单声道入口分块不变（由单元测试覆盖，不入向量）
- **给定**：任意参数与输入序列
- **当**：`processBlock` 分别按 blockSize=k 分块与一次性整块处理
- **则**：两种方式输出逐位一致（无跨声道状态干扰）

### GWT-EQ-09：reset 后行为可复现（由单元测试覆盖，不入向量）
- **给定**：已处理过信号的实例
- **当**：reset() 后重放同一输入
- **则**：输出与首次从零状态处理的输出完全一致；reset 不改动系数与参数

### GWT-EQ-10：静音输入零输出（由单元测试覆盖，不入向量）
- **给定**：任意参数、零状态实例
- **当**：送入全零输入
- **则**：输出逐位全零，级联状态保持为零（含左右声道先后处理的共享状态路径）

### GWT-EQ-11：满幅输入有界不发散（由单元测试覆盖，不入向量）
- **给定**：含大增益、低 q 段的参数（钳制后仍在域内）
- **当**：|x| ≤ 1 满幅长序列
- **则**：输出可为放大值但始终有限，长时间运行不发散（稳定极点保证）

### GWT-EQ-12：抛错路径（由单元测试覆盖，不入向量）
- **给定**：fs ≤ 0 或非有限；或 processStereo 传入不等长 L/R；或 processBlock 传入
  不等长 input/output
- **当**：构造 / 调用
- **则**：分别抛 `Error('invalid sample rate')`、`Error('eqchain: L/R length mismatch')`、
  `Error('eqchain: input/output length mismatch')`

## 六、向量用例

**本模块全部定量行为以 `specs/dsp/vectors/eq-chain.<case>.json` + 同名 `.f32` 冻结夹具
为准**；本章仅列预期 case 覆盖面（维度清单，不含具体参数数值），最终以导出工具产出并
冻结的内容为唯一判据。格式契约见 [`specs/README.md`](../README.md) §三；JSON 合法性由
Schema 校验。

预期覆盖面（case1–case4）：

1. **零增益全直通锚点**：10 段全 0 增益（frequency/q 取多变体），输出与输入逐位一致
   （GWT-EQ-01）；
2. **补偿开启级联**：少量活动段 boost/cut 混合 + qCompensation=true（GWT-EQ-02/06）；
3. **补偿关闭对照**：与 2 成对（参数与输入全同、仅 qCompensation=false），体现补偿差异
   （GWT-EQ-03）；2/3 同时覆盖 bandCount=5 与「bands 短于 bandCount 的填充段」
   （GWT-EQ-04）；
4. **满配钳制极值**：bandCount=20、20 段全配，frequency/gain/q 三参数双向越界钳制入载荷
   （GWT-EQ-05）。

采样率维度：四条向量均取 48000（模块无 fs 耦合的内部状态，fs 仅进入系数设计与 fmax
钳制；多采样率属单元测试维度，不入本批向量）。帧数对 blockSize 非整除（含末块短块）。

.f32 数据按 [README §3.3](../README.md) 四段 planar 小端布局存放；分块驱动方式按 §3.4
执行，且两支线必须遵守 §4.4 的块内「先整条 L 后整条 R」共享状态顺序。

## 七、关联文件

- 总纲契约：[`specs/README.md`](../README.md)
- Schema：[`specs/schema/vector-case.schema.json`](../schema/vector-case.schema.json)
- 行为事实标准：`src/dsp/EqChain.ts`；参数类型：`EqBandParam`（同文件）；
  TS 实现契约：`src/dsp/API_SPEC.md` 模块 3
- 基本单元规格：[biquad](biquad.md)（peaking 公式与 TDF2 递推、内层钳制域）
- 引擎接线（调用顺序事实）：`src/engine/HyperSoundEngine.ts`（先 setBands 后
  setQCompensation；processStereo 按块调用）
- 兄弟规格：[biquad](biquad.md) ｜ [limiter](limiter.md) ｜ [reverb-simple](reverb-simple.md) ｜
  [compressor](compressor.md) ｜ [bass-enhancer](bass-enhancer.md) ｜ [mid-side](mid-side.md)
