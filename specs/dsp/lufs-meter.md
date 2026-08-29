# 规格：lufs-meter —— BS.1770 / EBU R128 响度计量（计量型读数驱动 + readings 向量扩展）

> **规格属性**：本文件是双支线共享规格。行为事实标准 = `src/dsp/LufsMeter.ts`；
> 格式契约见 [`specs/README.md`](../README.md) §三，
> 向量合法性由 [`specs/schema/vector-case.schema.json`](../schema/vector-case.schema.json) 约束。
> 本模块是**向量驱动模型的首个计量型（`moduleKind='meter'`）形态**，也是 Schema
> `readings` 标量读数扩展的首个使用者：计量模块无音频输出段，行为契约由
> readings 标量承载，.f32 收窄为两段输入布局（§三）——对总纲 §3.3 的这一
> ADDITIVE 扩展以本文件 §三与 Schema 为准，总纲 §三的对应回写由规格协调者执行。

---

## 一、模块概述

- **定位**：DSP 内核级响度测量仪（ITU-R BS.1770-4 / EBU R128）——K 加权两级滤波、
  400ms/75% 重叠块统计、双门限整合响度、瞬时/短时响度、LRA、样本峰值与 4× 过采样
  真峰值。它是**分析型模块**：不实现 `StereoProcessor` 的音频输出语义（`processStereo`
  为就地分析、不改写缓冲、无输出），不进入引擎 22 级处理链的音频路径，供引擎/宿主
  侧做响度表与导出计量。向量驱动采用专用的计量型读数模型（§三）。
- **无参数模块**：构造仅 `fs`（`new LufsMeter(fs)`），无 `setParams`；向量 `params`
  恒为 `{}`。非法采样率（fs ≤ 0 或非有限）抛错（GWT-LUFSMETER-09，单元测试覆盖）。
- **出处/许可**：算法与公开公式来自 ITU-R BS.1770-4（K 加权两级滤波、400ms 块、
  双门限整合）与 EBU Tech 3341（瞬时/短时）、EBU Tech 3342（LRA）；滤波器系数对照
  libebur128 与 FFmpeg f_ebur128.c 的公开实现（LGPL，仅对照公式不引入代码）；
  本实现为自研 TS 代码。
- **确定性**：同输入同读数；无随机、无 Date、无 console。

## 二、接口签名（事实标准摘录）

```ts
export class LufsMeter {
  constructor(fs: number)
  /** 就地分析立体声：L/R 均过 K 加权（TDF2），z = 左右 K 加权之和；不改写 l/r，无输出 */
  processStereo(l: Float32Array, r: Float32Array): void
  getIntegratedLufs(): number   // 整合响度 LUFS（绝对 -70 + 相对 -10 双门限）；未测到 NaN
  getMomentaryLufs(): number    // 最后一个完整 400ms 块响度；未测到 NaN
  getShortTermLufs(): number    // 最近 3s = 30 块功率均值；不足 30 块（或全静音）NaN
  getLra(): number              // EBU Tech 3342 LRA（LU，10/95 百分位差）；未测到 NaN
  getPeakDb(): number           // 样本峰值 dBFS（20·log10）；全静音 -Infinity
  getTruePeakDb(): number       // 4× 过采样真峰值 dBFS；全静音 -Infinity
  reset(): void                 // 全部状态归零，回到未测量状态
}
```

六个 getter 构成 readings 的**全部合法读数名**（§四/§五）；驱动器不调用
`reset()`（全新实例零初始状态）。

## 三、向量驱动模型（计量型 readings 扩展——本规格特有的扩展，两支线必须逐字一致）

对总纲 §3.4 流式分块语义的**计量型重述**（JSON 公共字段与容差判定骨架不变）：

1. **`moduleKind='meter'` 与两段布局**：计量模块无音频输出，对总纲 §3.3 四段布局的
   计量型收窄为——期望输出两段为零长，.f32 只落盘 `[输入左][输入右]` 两段，
   **文件总长 = 8 × frames 字节**；音频段不参与比对（无期望可比，Schema `frames`
   字段说明同步收窄）。
2. **readings 契约**：JSON 顶层可选对象，与 `moduleKind` 双向绑定（Schema allOf：
   有 readings 则必为 meter、为 meter 则必有 readings）：读数名 → `{ want, tol }`。
   - `want` 为有限数 → **绝对容差**判定 `|got − want| ≤ tol`（与音频段的相对制公式
     §3.5 并立、互不混用）；
   - `want` ∈ `"NaN"` / `"+Infinity"` / `"-Infinity"`（字符串哨兵，JSON 数值无法表达
     非有限值）→ **哨兵等值**判定（got 必须同为 NaN / 同号无穷大；tol 不参与）。
3. **驱动语义**：
   1. 以 `sampleRate` 构造模块实例（全新零初始状态，不额外调用 `reset`；无
      `setParams`，`params = {}`）；
   2. 将 `inL`/`inR` 按 `blockSize` 自头至尾顺序切块（**末块允许短于 blockSize**），
      逐块调用 `processStereo(l, r)`（就地分析，输入缓冲不被改写）；
   3. **模块内部状态跨块保持**，且**读数与 blockSize 无关**——块边界以跨调用累计
      样本数判定、逐样本运算次序与分块无关，任意分块（含单块整段）产生逐位相同的
      六项读数（实证，GWT-LUFSMETER-05）；`blockSize` 仅约定驱动分块口径，两支线
      按同一 blockSize 回放；
   4. 全部块馈入完成后，一次性读取六项读数与 `readings` 逐项判定；`readings` 中未
      出现的读数名不做判定，出现未知名视为向量非法（加载器拒绝）。
4. **一句话定义**：**计量型向量：.f32 两段输入布局、readings 标量即契约、
   绝对容差 + 非有限哨兵等值。**

## 四、测量语义（两支线必须逐字一致）

以下按 TS 事实源码逐字固化（含全部精度纪律——落盘量化与 f64 累计的分布是
跨实现对拍的前提）：

1. **K 加权**：每通道两级串联 biquad（TDF2 形态）：
   ① **RLB 高通**：RBJ/BLT 二阶高通，f0 = 38.135822 Hz、Q = 0.5；
   ② **高频搁架 +4 dB**：常数-Q 搁架，f0 = 1681.974450955533、gDb = 3.999843853973347、
   Q = 0.7071752369554196、vb = vh^0.4996667741545416（vh = 10^(gDb/20)），
   k = tan(π·f0/fs)（48 kHz 下与 BS.1770 公布系数逐位一致）。
   **采样率路径**：44100/48000 用本采样率精确系数；**其余采样率一律按 48000 系数
   近似**（blockLen/hopLen 仍按实际 fs 缩放，GWT-LUFSMETER-07）。
2. **块统计**：blockLen = round(0.4·fs)、hopLen = round(0.1·fs)（400ms 窗 / 100ms
   步进 = 75% 重叠）；块边界以**跨调用累计样本数**判定：`totalSamples ≥ blockLen 且
   (totalSamples − blockLen) % hopLen === 0` 时记录一块（与分块无关）；块内统计
   z = 左右通道 K 加权输出之和（BS.1770 通道求和）。
3. **落盘量化纪律**：滑动窗环形 zBuf 为 **f32**（逐样本写入 f32 量化后的 z，
   `sumSq += z² − evict²` 中 evict² 取 f32 量化值的平方；sumSq 本身为 f64 累加）；
   块响度 Lk 与块功率 p 落盘于 **f32 数组**——门限判定、整合均值、LRA 排序消费的
   都是 f32 量化后的值；峰值/真峰值与全部门限常量比较在 f64。
4. **块响度**：p = sumSq / blockLen（块内 z² 均值），Lk = −0.691 + 10·log10(p)；
   **p ≤ 1e-30 的静音块 Lk 记 NaN**（防 −Infinity 泄漏进门限统计），p 照常落盘。
5. **整合响度（BS.1770 双门限）**：门 1 = Lk ≥ −70（绝对；NaN 块不过门）；相对门 =
   过门 1 块的 **Lk 均值 − 10**；结果 = 过双门块的**功率 p 均值** → −0.691 +
   10·log10；无块过门 1 或无块过门 2 → NaN。
6. **momentary**：最后一个完整块的 Lk（NaN 块透传 NaN）。
7. **shortTerm**：最近 30 块（3s）的**功率 p** 均值 → −0.691 + 10·log10；块数 < 30
   → NaN；30 块功率和 ≤ 1e-30（全静音）→ NaN。
8. **LRA（EBU Tech 3342）**：门 1 = Lk ≥ −70；相对门 = 过门 1 块的 Lk 均值 − **20**；
   过双门的 Lk 升序排序后取线性插值百分位 p10/p95（rank = p·(n−1)，lo 向下取整、
   hi = lo+1 截断、frac 线性内插），LRA = p95 − p10；总块数 < 2 或过门 1 块 < 2 或
   过双门块 < 2 → NaN。
9. **峰值**：逐样本 max(|L|,|R|)（f32 输入取绝对值比较）；getPeakDb = 20·log10(peak)；
   全静音（peak = 0）→ −Infinity。
10. **真峰值（4× 过采样多相内插）**：TRUE_PEAK_OVS = 4、每相 24 抽头、核长 48；
    核 = Blackman 窗 sinc（sinc 截止 = 4× 率的 1/4，窗 0.42 + 0.5·cos(π·u/24) +
    0.08·cos(2π·u/24)，u = j − 23 + φ/4），逐相除以核和归一化；左右通道共用一个
    48 样本 f32 环形历史（每样本推进一次写入游标）；**历史写满一圈后**才启用插值，
    对滞后 TAPS = 24 的因果位置插值（t = totalSamples − 1 − 24），4 相输出取 |y|
    最大值维护 truePeak；getTruePeakDb = 20·log10(truePeak)；全静音 → −Infinity。
11. **读数非有限语义总表**（哨兵判定的唯一依据）：

   | 读数名 | NaN 条件 | −Infinity 条件 |
   |---|---|---|
   | `integratedLufs` | 无块 / 无块过绝对门限 / 无块过相对门限 | — |
   | `momentaryLufs` | 无块 / 末块为静音块 | — |
   | `shortTermLufs` | 块数 < 30 / 30 块功率和 ≤ 1e-30 | — |
   | `lra` | 总块数 < 2 / 过绝对门限块 < 2 / 过相对门限块 < 2 | — |
   | `peakDb` | — | 全静音（peak = 0） |
   | `truePeakDb` | — | 全静音（truePeak = 0） |

12. **环形容量**：块历史 36000 块（1 小时 @100ms 步进）、短时环形 30 块，超容量
    滚动覆盖（本批向量不触及容量回绕路径）。
13. **reset()**：滑动窗、块历史、短时环形、峰值、真峰值历史与游标、全部滤波器
    状态归零，回到未测量状态（GWT-LUFSMETER-08，单元测试覆盖）。

## 五、readings 容差表（实证确定）

TS 侧同输入逐位确定（测量全程 f64，仅 §四第 3 条列出的落盘点量化 f32）；容差只为
Rust 移植的浮点结合序差异留安全网，取值高出预期噪声若干数量级：

| 读数名 | tol | 依据 |
|---|---|---|
| `integratedLufs` | 0.1 LU | 块功率累加与对数域的 f64 结合序差异实测 ≤ 1e-10 LU；0.1 LU 约一个感知阈（JND）量级的宽松安全网 |
| `momentaryLufs` | 0.1 LU | 同上 |
| `shortTermLufs` | 0.1 LU | 同上 |
| `lra` | 0.5 LU | 排序 + 百分位插值对个别块响度的微小扰动最敏感，取 5× 安全系数 |
| `peakDb` | 0.05 dB | 逐样本 max 理论上跨实现逐位一致（同一 f32 输入），取小安全网 |
| `truePeakDb` | 0.1 dB | 48 抽头多相内插和的浮点序敏感度最高的一路 |

tol 为**绝对容差**（`|got − want| ≤ tol`）；`want` 为哨兵时 tol 不参与（等值判定）。

## 六、GWT 行为条款

> 定量断言一律以 `specs/dsp/vectors/lufs-meter.<case>.json` 冻结夹具（readings +
> 两段输入 .f32）+ §五容差表判定；条款内不内嵌具体参数数值与期望值。

### GWT-LUFSMETER-01：EBU 合成基准信号读数锚点（case1 承载）
- **给定**：48000 Hz，双声道同相 997 Hz 正弦（幅度按整合响度落在 −23 LUFS 附近
  标定，即 EBU R128 目标响度）
- **当**：3.33 s（恰产生 30 个分析块）按 blockSize 分块馈入完成后读取六项读数
- **则**：六项读数全部有限——integrated/momentary/shortTerm 收敛于 −23 LUFS 附近、
  LRA 近零（稳态节目）、peak/truePeak ≈ 20·log10(幅度)；精确值以冻结向量为准

### GWT-LUFSMETER-02：静音 → 非有限读数哨兵（case2 承载）
- **给定**：48000 Hz 全零输入（产生完整分析块但全部为静音块）
- **当**：馈入完成后读取六项读数
- **则**：integrated/momentary/shortTerm/lra = `NaN`、peakDb/truePeakDb =
  `-Infinity`，全部按哨兵等值判定；静音块响度必须记 NaN（不得以 −Infinity/0/有限值
  进入门限统计——任何哨兵语义替代必然暴露）

### GWT-LUFSMETER-03：突强突发的 momentary/峰值路径（case3 承载）
- **给定**：44100 Hz（精确系数路径），头部静音 + 响信号（正弦 + 去相关 LCG 噪声）
  直达结尾
- **当**：馈入完成后读取六项读数
- **则**：momentary 为响段热区值（高于 0 LUFS，末块即响段）、peakDb 接近满幅、
  truePeak 为 4× 过采样内核的确定性指纹（全带宽噪声下内插峰值**低于**样本峰值，
  移植须复刻同一核窗与滞后调度）；总块数 < 30 → shortTerm = `NaN`

### GWT-LUFSMETER-04：两电平节目的 LRA 全路径（case4 承载）
- **给定**：48000 Hz 10 s 两电平节目（响段与静段衔接，左右去相关）
- **当**：馈入完成后读取六项读数
- **则**：LRA ≈ 20 LU（绝对 −70 + 相对 −20 双门限 + 升序排序 + 线性插值百分位
  全路径）；integrated 不被静段拉低（相对门限剔除）；shortTerm（30 块功率环形）与
  momentary 取自尾部静段电平；六项读数全部有限

### GWT-LUFSMETER-05：分块不变性（定性）
- **给定**：任意输入与任意 blockSize
- **当**：同一输入按不同分块馈入（含单块整段与向量 blockSize）
- **则**：六项读数**逐位一致**——块边界以跨调用累计样本数判定、逐样本运算次序与
  分块无关（实证：本批四条 case 的读数在单块整段与向量 blockSize 下逐位相同）

### GWT-LUFSMETER-06：整合门限行为（case4 承载）
- **给定**：响段与静段共存的节目
- **当**：整合响度两级门限判定（绝对 −70 LUFS / 相对 −10 LU）
- **则**：静段块被门限剔除，integrated 收敛于响段电平附近（精确值以冻结向量
  case4 为准）

### GWT-LUFSMETER-07：采样率路径（case1/case4 = 48k、case3 = 44.1k 承载）
- **给定**：44100/48000 → 本采样率精确 K 加权系数；其余采样率 → 一律按 48000
  系数近似（blockLen/hopLen 仍随实际 fs 缩放）
- **当**：馈入同形信号
- **则**：44.1k 与 48k 向量读数分别冻结（两套精确系数不同 → 读数可区分）；近似
  路径不抛错、读数有限无 NaN（如 32000 Hz，由单元测试覆盖，不入向量）

### GWT-LUFSMETER-08：reset 复现性（由单元测试覆盖，不入向量）
- **给定**：已积累读数的实例
- **当**：`reset()` 后再次馈入同输入
- **则**：回到未测量状态（integrated = NaN、peakDb = −Infinity）且二次读数与首次一致

### GWT-LUFSMETER-09：非法采样率抛错（由单元测试覆盖，不入向量）
- **给定**：fs ≤ 0 或非有限的采样率
- **当**：构造 LufsMeter
- **则**：抛 Error

## 七、向量用例

**本模块全部定量行为以 `specs/dsp/vectors/lufs-meter.<case>.json` + 同名 `.f32`
冻结夹具为准**；四条 case 全部为 `moduleKind='meter'`：`params = {}`、.f32 为两段
输入布局（8 × frames 字节，无期望输出段）、readings 六读数全量冻结（want + tol，
§四/§五）。格式契约见本规格 §三与 Schema；JSON 合法性由 Schema 校验。

预期覆盖面（case1–case4）：

1. **EBU 合成基准信号**：48 kHz / 160000 帧（恰 30 个分析块）/ 块 512，同相
   997 Hz 正弦（−23 LUFS 附近标定）——六读数全有限（GWT-01）；
2. **静音**：48 kHz / 24000 帧 / 块 256——六读数全哨兵 NaN/−Infinity（GWT-02）；
3. **突强突发**：44100 / 52920 帧 / 块 500——momentary/峰值路径 + 44.1k 精确系数
   路径 + truePeak 内插指纹 + 块数不足语义（GWT-03）；
4. **两电平 10 s 节目**：48 kHz / 480000 帧 / 块 1024——LRA 全路径 + 整合门限
   行为 + 长跑读数稳定性（GWT-04/06）。

维度说明：覆盖 48k/44.1k 两条精确系数路径、有限/非有限两类读数、momentary/
shortTerm/LRA/真峰值各代码路径、静音块与块数不足的边界语义；四条 blockSize 均与
frames 非整除（末块短块覆盖）。读数与分块无关（GWT-05）。**readings 为标量读数
契约（±0.1 LU 等绝对容差），非逐位对拍**。

## 八、关联文件

- 总纲契约：[`specs/README.md`](../README.md) §三（本规格 §三为计量型 readings
  扩展 + 两段布局收窄；总纲回写由规格协调者执行）
- Schema：[`specs/schema/vector-case.schema.json`](../schema/vector-case.schema.json)
  （`moduleKind`/`readings` ADDITIVE 扩展，既有 63 组向量不受影响）
- 行为事实标准：`src/dsp/LufsMeter.ts`
- 参考单元测试：`test/lufsmeter.test.ts`
- 对拍门禁：`test/spec-vectors.test.ts`（TS 侧，含 readings 判定）；
  `HyperSoundEngineRust/crates/hse-parity`（Rust 侧，readings 对拍由向量 runner
  扩展承载，同一容差公式与哨兵语义）
- 兄弟规格：[fft](fft.md) ｜ [convolver](convolver.md) ｜ [modulation-matrix](modulation-matrix.md) ｜
  [hse-stretch](hse-stretch.md) ｜ [loudness-comp](loudness-comp.md)
