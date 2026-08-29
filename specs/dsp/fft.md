# 规格：fft —— 基-4 复 FFT（非流式变换 + 向量驱动模型扩展）

> **规格属性**：本文件是双支线共享规格。行为事实标准 = `src/dsp/fft.ts`；
> 格式契约见 [`specs/README.md`](../README.md) §三，
> 向量合法性由 [`specs/schema/vector-case.schema.json`](../schema/vector-case.schema.json) 约束。
> 本模块是**向量驱动模型的首个非流式变换形态**：f32 四段布局的「声道」语义重解释为
> 复数平面（§三），布局与分块契约本身不变（总纲 §3.3/§3.4 继续全文适用）。

---

## 一、模块概述

- **定位**：DSP 内核级离散傅里叶变换工具——原位复数 FFT（Cooley–Tukey DIT，基-4 合并蝶形，
  log2(N) 为奇数时补一个基-2 尾 stage）。它是 Convolver（分区卷积）等频域模块的运算内核，
  本身不是 `StereoProcessor` 形态的处理级，**没有 setParams/processStereo 接口**；
  向量驱动采用专用的非流式变换模型（§三）。
- **出处**：蝶形分解与位反转思路参考 kissfft（BSD-3-Clause）；基-4 合并蝶形 = 两轮基-2
  stage 的代数合并（每 4 点 3 次复数乘、±j 免乘），为公开算法结构的自研 TS 实现。
- **确定性**：纯函数，同输入同输出；无随机、无 Date、无 console。twiddle 表按 N 模块级缓存
  （首次调用后零堆分配）；缓存只影响性能，不影响数值（表值由 `Math.cos/Math.sin` 唯一决定）。
- **无采样率概念**：变换本身与 fs 无关；向量顶层 `sampleRate` 仅用于输入生成时定义正弦频率
  （与 mid-side 的「sampleRate 为契约字段」同一取法）。
- **无窗语义**：`fft()` 是纯变换，**不加任何窗**；`hannWindow()` 是独立工具函数，不参与变换，
  也不进入向量（泄漏形态即矩形窗/无窗形态）。

## 二、接口签名（事实标准摘录）

```ts
export function fft(real: Float32Array, imag: Float32Array, inverse: boolean): void
// 原位复 FFT：real/imag 等长且为 2 的幂；inverse=true 做逆变换并整体 ÷N；
// 长度非 2 的幂或两数组长度不一致抛 Error。正变换不缩放。

export function nextPow2(n: number): number          // ≥n 的最小 2 的幂
export function hannWindow(n: number): Float32Array  // 对称 Hann（独立工具，不入向量）
export function magnitudeSpectrum(re, im): Float32Array  // |X[k]|，N/2+1 bin（分析工具）
export function frequencyBins(n: number, fs: number): Float32Array  // bin 中心频率（分析工具）
```

进入向量驱动的只有 `fft()`；`nextPow2`/`hannWindow`/`magnitudeSpectrum`/`frequencyBins`
属单元测试与分析域，不设向量条款。

## 三、向量驱动模型（非流式变换——本规格特有的扩展，两支线必须逐字一致）

总纲 §3.4 的流式分块语义对 `fft` 重解释如下（f32 四段布局、JSON 字段、容差公式**不变**）：

1. **平面映射**：输入段一 `inL` = 复数输入的 **Re 平面**，输入段二 `inR` = **Im 平面**；
   期望输出段三 `wantL` = 变换后 **Re 谱**，段四 `wantR` = 变换后 **Im 谱**。
   即 **(L, R) = (Re, Im) 平面**；`channels=2` 保留契约值，其语义在此模型中是「两个复数平面」。
2. **单块整段变换**：`frames = blockSize = N = fftSize`（N 为 2 的幂），向量恰好覆盖一个变换块；
   驱动器按 §3.4 切块后，对每块调用一次 `fft(re, im, inverse)`（原位），变换结果即该块输出。
3. **无状态**：变换无跨块状态；本批向量固定单块驱动。blockSize 与 frames 必须相等且为 2 的幂
   （`fft` 对非 2 的幂长度抛错，驱动器不得依赖该抛错——向量合法性由本条款约束）。
4. **params 快照**：`{ "inverse": boolean }`（当前批次全部为 `false`，即正变换）。
   驱动器据此选择变换方向；逆变换路径（含 ÷N 归一化）经 Convolver 向量的湿路（IFFT）间接冻结，
   不单设向量 case。
5. **一句话定义**：**FFT 非流式变换：(L,R)=(Re,Im) 平面、单块 N=fftSize、输出=就地变换结果。**

## 四、变换语义（两支线必须逐字一致）

以下按 TS 事实源码逐字固化；**跨实现对拍在本模块上的 1e-6 相对容差以「算术调度等价」为前提**
——频谱空 bin / 深零点 bin 的取值本身就是逐级 f32 舍入的实现噪声（GWT-FFT-02 注记），
移植须复刻同一运算序与同一 f32 落点纪律，实践目标为逐位一致（与既有 11 组向量的对拍纪律相同）。

1. **位反转排列（原位，DIT 前置）**：i 从 1 到 n−1，j 按二进制位累进（`bit = n>>1` 起逐位
   折转），`i < j` 时同步交换 real[i]/real[j] 与 imag[i]/imag[j]。
2. **twiddle 表**（f64，按 N 缓存）：基-4 stage 的块长 len = 4, 16, 64, … ≤ N，每表存
   quarter = len/4 条记录 `[cosθk, sinθk, cos2θk, sin2θk, cos3θk, sin3θk]`，
   θk = 2πk/len（k < len/4）；log2(N) 为奇数时追加基-2 尾表（len = N，N/2 条 cos/sin，
   θk = 2πk/N）。逆变换不另建表：sin 分量在消费处取共轭（变号）。
3. **基-4 蝶形**（len = 4, 16, … ≤ N 逐级）：每 4 点子蝶形做 **3 次复数乘**
   （t1 = W¹·x 位相序对调说明：位反转后子块相位序为 (0,2,1,3)，故 pos1 消费 e^{−j4πk/len}、
   pos2 消费 e^{−j2πk/len}，输出落点仍按位置 0..3）+ ±j 免乘组合；**复数乘与加减全部在
   f64 内完成，每个落点写回 f32**（逐级 f32 量化是误差特性的来源，也是对拍纪律的锚点）。
   逆变换：twiddle 取共轭（sign = +1），±j 组合项取共轭（jSign = −1）。
4. **基-2 尾 stage**（仅 log2(N) 奇数）：k = 0..N/2−1，u ± W^k·v（W = e^{∓j2πk/N}，
   逆变换共轭），f64 累加、f32 写回。
5. **缩放**：正变换**不缩放**（X[k] = Σ x[n]·e^{−j2πkn/N} 的原始 DFT 尺度，直流 bin = Σx）；
   逆变换末尾对 real/imag 整体乘 1/N（f64 除法后写回 f32）。
6. **误差特性（实证）**：逐级 f32 写回 → 正逆往返误差 1e-7 量级（N ≤ 1024）；
   δ 脉冲的频谱为**逐位精确**的全 1 / 全 +0（任何正确 DFT 调度下均精确，见 GWT-FFT-01）。
7. **抛错路径（由单元测试覆盖，不入向量）**：长度非 2 的幂、或 real/imag 长度不一致 →
   `Error('fft: length must be a power of two')` / `Error('fft: real/imag length mismatch')`。

## 五、GWT 行为条款

> 定量断言一律以 `specs/dsp/vectors/fft.<case>.json` 冻结夹具 + 容差公式判定
> （README §3.5），条款内不内嵌具体参数数值与期望值。

### GWT-FFT-01：时域脉冲 → 频谱平坦（逐位锚点）
- **给定**：Re 平面为首样本 1、其余全零的 δ；Im 平面全零；正变换；块长 N 覆盖基-4 主路径
  （log2(N) 为偶数）
- **当**：按 §三 单块驱动执行原位变换
- **则**：Re 谱**逐位全 1**、Im 谱**逐位全 +0**（δ 含全部频率分量且幅度相等）。该锚点对
  算术调度鲁棒——蝶形只触碰精确零与 1 的加减，任何正确 DFT 实现均应逐位一致，
  是本模块最强跨实现精度锚点

### GWT-FFT-02：整 bin 单频正弦 → 共轭对称双谱线
- **给定**：Re 平面为单位幅度正弦、频率恰落在整数 bin k0（fs 仅用于把 k0 换算为 Hz）；
  Im 平面全零；块长 N 覆盖基-2 尾路径（log2(N) 为奇数）
- **当**：按 §三 单块驱动执行正变换
- **则**：Im 谱在 k0 与 N−k0 出现 ∓(N/2)·amp 的共轭对称谱线（解析尺度，精确值以冻结向量
  为准），Re 谱在该两 bin 近零；**其余 bin 的微小取值是逐级 f32 舍入的实现噪声**——冻结值即
  TS 的噪声实现，跨实现对拍须调度等价（§四纪律），驱动器漏掉 Im 平面或错误缩放必然超差

### GWT-FFT-03：双平面复输入（实部虚部都有能量）
- **给定**：Re 平面与 Im 平面各为一组互不相同频点/相位的正弦叠加（复数输入，两平面都有能量）
- **当**：按 §三 单块驱动执行正变换
- **则**：输出 Re 谱与 Im 谱**两平面均有全谱能量**（复输入不具实信号共轭对称性）；
  驱动器若把 Im 平面置零、或把两平面拆成两次实变换，必然超差

### GWT-FFT-04：直流分量 + 非整周期泄漏（无窗语义）
- **给定**：Re 平面 = 直流常量 + 频率非整数 bin 的正弦；Im 平面全零；正变换
- **当**：按 §三 单块驱动执行正变换
- **则**：直流分量全部落入 X[0]（Re 谱直流 bin = DC·N 解析尺度）；非整 bin 正弦呈矩形窗
  泄漏裙摆（连续分布、无谱线集中——本模块不加窗）；全程无 NaN/Infinity，输出有界，
  精确波形以冻结向量界定

### GWT-FFT-05：非 2 的幂与长度不一致抛错（由单元测试覆盖，不入向量）
- **给定**：长度非 2 的幂的等长平面；或 2 的幂但不等长的两平面
- **当**：调用 fft
- **则**：分别抛幂长错误与长度不一致错误

### GWT-FFT-06：逆变换 ÷N 与往返还原（由单元测试覆盖，不入向量）
- **给定**：任意复输入
- **当**：正变换后接逆变换
- **则**：还原误差 1e-7 量级（逐级 f32 写回所致，N ≤ 1024）；逆变换整体 ÷N 的归一化语义
  经 convolver 向量的湿路（分区卷积 IFFT 路径）间接冻结

### GWT-FFT-07：无状态（由单元测试覆盖，不入向量）
- **给定**：任意输入平面
- **当**：同数据分多次调用与单次调用对比
- **则**：变换无跨调用状态，同输入同输出；向量固定单块驱动（§三第 3 条）

## 六、向量用例

**本模块全部定量行为以 `specs/dsp/vectors/fft.<case>.json` + 同名 `.f32` 冻结夹具为准**；
本章仅列预期 case 覆盖面（最终以导出工具产出并冻结的内容为唯一判据）。格式契约见
[`specs/README.md`](../README.md) §三（本模块按 §三驱动模型重解释平面语义）；
JSON 合法性由 Schema 校验。

预期覆盖面（case1–case4）：

1. **脉冲平坦谱锚点**：Re 平面 δ 脉冲、Im 平面全零，N 取基-4 纯路径（log2(N) 偶数）；
   输出 Re 谱逐位全 1、Im 谱逐位全 +0（GWT-FFT-01）；
2. **整 bin 谱线**：Re 平面单位幅度正弦、频率恰为整数 bin；Im 平面全零；N 取基-2 尾路径
   （log2(N) 奇数）；Im 谱共轭对称双谱线、Re 谱近零（GWT-FFT-02）；
3. **双平面复输入**：Re/Im 两平面各为双频正弦叠加（频点互不相同、相位各异、均非整数 bin）；
   N 取基-4 纯路径；输出两平面均有全谱能量（GWT-FFT-03）；
4. **直流 + 非整周期泄漏**：Re 平面 = 直流常量 + 非整 bin 正弦；Im 平面全零；N 取基-2 尾路径
   的更大块长；X[0] 承载直流、其余呈泄漏裙摆（GWT-FFT-04）。

维度说明：四条 case 覆盖 4 个不同块长（1024/2048/4096/8192 形态），且 log2(N) 奇偶两类
蝶形调度各覆盖两次（§四第 3/4 条两条代码路径全部入向量）。params 均为 `{inverse:false}`
（正变换；逆变换经 Convolver 间接冻结，GWT-FFT-06）。`sampleRate` 仅用于输入频率定义，
统一取 48000。**blockSize = frames = N（单块驱动，§三）**。

.f32 数据按 [README §3.3](../README.md) 四段 planar 小端布局存放，段语义按本规格 §三
重解释为 (Re, Im, Re 谱, Im 谱)；驱动方式按 §三非流式变换模型执行。

## 七、关联文件

- 总纲契约：[`specs/README.md`](../README.md)（§3.3/§3.4 布局与分块契约；
  本规格 §三为其非流式变换重解释）
- Schema：[`specs/schema/vector-case.schema.json`](../schema/vector-case.schema.json)
- 行为事实标准：`src/dsp/fft.ts`（`fft`/`nextPow2`/`hannWindow`/`magnitudeSpectrum`/
  `frequencyBins`）
- 下游消费（间接冻结 IFFT 路径）：[convolver](convolver.md)（分区卷积的频域内核即本模块）
- 参考单元测试：`test/fft.test.ts`
- 兄弟规格：[biquad](biquad.md) ｜ [limiter](limiter.md) ｜ [reverb-simple](reverb-simple.md) ｜
  [compressor](compressor.md) ｜ [bass-enhancer](bass-enhancer.md) ｜ [mid-side](mid-side.md) ｜
  [eq-chain](eq-chain.md) ｜ [fdn-reverb](fdn-reverb.md) ｜ [deesser](deesser.md) ｜
  [loudness-comp](loudness-comp.md) ｜ [dynamic-eq](dynamic-eq.md) ｜ [mod-effects](mod-effects.md) ｜
  [convolver](convolver.md)
