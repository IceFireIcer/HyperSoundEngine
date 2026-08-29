# 规格：hse-stretch —— 变速 / 变调（自研相位声码器）

> **规格属性**：本文件是双支线共享规格。行为事实标准 = `src/dsp/HseStretch.ts`；
> 参数字段名以该源码（`HseStretchParams`）为准，本规格不得臆造字段。
> 格式契约见 [`specs/README.md`](../README.md) §三，
> 向量合法性由 [`specs/schema/vector-case.schema.json`](../schema/vector-case.schema.json) 约束。
> 本模块是**向量驱动模型的块窗映射形态**（§4.6）：`processStereo` 非就地且输出长度随参数
> 变化，向量按 §4.6 的截断/补零映射回定长网格；f32 四段布局、JSON 字段、容差公式**不变**。
> **跨实现对拍以算术调度等价为前提、实践目标为逐位一致**（§4.7，与 [fft](fft.md) §四同一纪律）。

---

## 一、模块概述

- **定位**：STFT 相位声码器变速/变调——时间伸缩 `rate`（输出长度 ≈ 输入 × rate，不变调）、
  频率伸缩 `2^(semitones/12)`（变调，与 rate 独立）。**不内联进引擎主链**：引擎经
  `getStretch()` 暴露实例，供 gapless/离线导出场景按整段调用（`HyperSoundEngine.ts`
  头注与 `_stretch` 字段）。
- **出处**：相位声码器算法原理（相位差 → 瞬时频率 → 按目标步进累积相位）为公开知识，
  本实现为自研 TypeScript；STFT 内核复用 `src/dsp/fft.ts` 的基-4 复 FFT
  （hse-core 批次五已移植并具备逐位对拍基线）；变调阶段的重采样用自研多相窗口化 sinc
  `Resampler`（speexdsp 思路，无第三方代码）。signalsmith-stretch（MIT）仅作为可选动态
  适配目标（见 §4.9），不进入向量。
- **确定性**：无 Math.random / Date / console；同输入同参数同输出（逐位可复现，实证）。
- **框架常量**：分析窗长 `N = 2048`、分析 hop `HOP = 512`（75% 重叠）、Hann 窗
  `w[i] = 0.5·(1 − cos(2πi/N))`——三者与采样率无关。STFT 本身不消费 fs；fs 仅进入
  （a）构造校验（`fs ≤ 0` 或非有限抛错）与（b）变调阶段 Resampler 速率比的 f64 构成
  （`(fs·ps)/fs` 不逐位等于 `ps`）——**采样率是行为参数，向量固定 48000**。

## 二、接口签名（事实标准摘录）

```ts
export interface HseStretchParams {
  semitones: number   // 半音数（-36..36，超出 clamp）
  rate: number        // 时间伸缩速率（0.1..8，超出 clamp；1=原速）
}

export class HseStretch {
  readonly fs: number
  readonly channels: number           // API 兼容保留；processStereo 固定处理双声道
  constructor(fs: number, channels?: number)
  setParams(p: HseStretchParams): void
  // 非就地：返回新数组，长度 ≈ 输入 × rate（±3% 量级），左右声道各自独立全流程
  processStereo(l: Float32Array, r: Float32Array): { l: Float32Array; r: Float32Array }
  reset(): void                       // 清零相位累积 / 上一帧频谱 / 窗内缓冲
  static async isSignalsmithAvailable(): Promise<boolean>   // 探测 signalsmith 适配（见 §4.9）
}
```

## 三、参数表（向量 `params` 快照字段）

| 字段 | 类型 | 有效域（clamp 后生效值） | 默认值 | clamp / 边界行为 |
|---|---|---|---|---|
| `semitones` | number | `[-36, 36]` | 0 | 双向钳制；换算 `pitchScale = 2^(semitones/12)` |
| `rate` | number | `[0.1, 8]` | 1 | 双向钳制 |

**序列扩展字段（仅参数突变 case 使用，§4.6.3）**：

| 字段 | 类型 | 说明 |
|---|---|---|
| `initialParams` | `{semitones, rate}` | 省略时=单次 `setParams`（总纲 §3.4 标准路径）；存在时驱动器先以它调用 `setParams` |
| `switchAtBlock` | number（整数，0 起） | 处理第 `switchAtBlock` 块**之前**以最终快照 `{semitones, rate}` 调用一次 `setParams` |

`setParams` 即时生效：与当前值不同的 `rate`/`semitones` 会触发内部 `reset()`（§4.5）。

## 四、处理语义（两支线必须逐字一致）

### 4.1 两阶段流水线（单声道核 ×2，无声道耦合）

```text
stretched = vocoderStretch(x, rate · pitchScale)     # 阶段一：时间伸缩（不变调）
|pitchScale − 1| < 1e-9 → 输出 = stretched           # semitones=0 免重采样
否则 输出 = Resampler(fs·pitchScale, fs, 1, 8).process(stretched)
                                                      # 阶段二：按 1/pitchScale 重采样（变调）
```

左右声道各自独立跑完整流水线（同一参数、互不相交的状态），无任何声道耦合。

### 4.2 STFT 分帧与输出长度

- 帧数：`full = len ≥ N ? floor((len − N)/HOP) + 1 : 0`；`partial = full·HOP < len ? 1 : 0`；
  `M = max(1, full + partial)`。尾部部分帧越界**补零**（保证输出覆盖完整尾段）；
- 合成 hop：`Hs = max(1, round(HOP · factor))`，`factor = rate·pitchScale`（非整数 factor
  为有效速率近似，帧位置取整偏差 < 0.5 样本/帧）；
- 输出长度：`outLen = (M − 1)·Hs + N`（分析帧集覆盖输入至 `(M−1)·HOP + N`，故 factor > 1
  时输出短于理想 `len·factor`——长度语义为「≈ 输入 × rate（±3% 量级）」的实测契约）。

实测长度表（fs=48000，激励形态无关）：

| len | semitones / rate | outLen |
|---|---|---|
| 6400 | 0 / 1 | 6656 |
| 3000 | 0 / 2 | 4096 |
| 2048 | 0 / 2 | 3072 |
| 2048 | +5 / 1 | 2046（< len） |
| 1904 | +5 / 1 | 1534（< len） |
| 2048 | +3 / 1.5 | 2490 |
| 256 | +3 / 1.5 | 1722 |

### 4.3 相位声码器（阶段一）

- 分析帧：`anaRe[i] = (start+i < len ? x[start+i] : 0)·w[i]`、`anaIm = 0`，`start = m·HOP`，
  原位复 FFT（`fft.ts`，正变换不缩放）；
- 合成相位：帧 0 `synPhase[k] = atan2(anaIm[k], anaRe[k])`；帧 m>0 按帧间复相位差累积——
  `Δφ = atan2(im·prevRe − re·prevIm, re·prevRe + im·prevIm)`（数值稳定形）、
  `dev = Δφ − HOP·wk` 折叠回 `(−π, π]`、瞬时角频率 `winst = wk + dev/HOP`、
  `synPhase[k] += Hs·winst`，其中 `wk = 2πk/N`；
- 合成谱：幅度取分析帧 `|X[k]|`，Hermitian 重构（DC/Nyquist 强制实值、按 `cos(ph)` 符号；
  其余 `mag·cos(ph) / mag·sin(ph)` 镜像共轭），逆 FFT；
- `prevRe/prevIm` 保存当前分析帧供下一帧差分（仅同一次调用内消费，见 §4.5）。

### 4.4 WOLA 归一化与窗边缘保留区

- 每帧合成贡献 `w[i]·synRe[i]` 重叠相加，同步累加窗平方和 `S(t) = Σ_m w(t − m·Hs)²`；
- 逐样本：`S(t) > 0.01 → out[t] /= S(t)`（精确恢复 x(t)）；`S(t) ≤ 0.01 → 保留 w·synRe`
  （窗边缘 w² < 0.01 区域，防极小 S 放大部分帧相位误差——**该区域不重构输入**，输出为
  淡入/淡出的小值）；
- **行为事实（实证）**：整段单块驱动的 rate=1 输出**不是输入的逐位直通**，也非严格近似
  恒等——重构区（S > 0.01，约整段中腹）与输入相对偏差 ~1e-5 量级（相位累积噪声，
  实测中腹 rms 误差 ~7.7e-6、逐位相等样本仅 149/5800），两端各约 210 样本的保留区输出
  为 1e-12..1e-9 量级的小值（S 从 0 爬升至 0.01 前）。

### 4.5 状态与参数突变 reset 语义

- `setParams`：`rate`/`semitones` 任一变化 → 更新参数并 `reset()`（清零相位累积/上一帧
  频谱/窗内缓冲）；未变化仅更新参数；
- **跨调用无状态（实证，逐位）**：每次 `processStereo` 调用在帧 0 处重置 `synPhase`、
  在帧间覆盖 `prev/ana/syn` 缓冲——先处理任意其他内容（含显式 `reset()` 与否）再处理 B，
  与全新实例直接处理 B **逐位一致**；因此参数突变 reset 与 `reset()` 在当前实现对输出
  **不可观测**；
- 向量仍以两次 `setParams` 的合法序列（case4）固化「参数切换后输出 = 全新实例终参输出」
  的契约——若移植实现携带跨调用状态且切换后未复现全新行为，必然超差。

### 4.6 向量驱动模型（块窗映射——本规格特有的扩展，两支线必须逐字一致）

总纲 §3.4 的流式分块语义对本模块重解释如下（f32 四段布局、JSON 字段、容差公式不变）：

1. **实例化**：`new HseStretch(sampleRate, 2)` →（见第 3 条的序列规则）`setParams(...)`。
2. **每块（块窗映射）**：`out = processStereo(bl, br)`（非就地，读输入不写回）；本块期望
   输出 = **取 `out.l`/`out.r` 的前 `len` 个样本（`len` = 本块帧数），不足 `len` 的尾部
   补零**（outLen 超出 len → 截断；不足 len → 补零，两形态都有 case 覆盖，长度表见 §4.2）。
   驱动器不得就地改写输入，也不得把超出 len 的输出发酵进下一块。
3. **序列驱动（参数突变 case）**：载荷含 `initialParams` 时——先
   `setParams(initialParams)`；按 blockSize 顺序切块，**处理第 `switchAtBlock` 块之前**
   以最终快照 `{semitones, rate}`（params 顶层两字段）调用一次 `setParams`；其后各块用
   终参处理。载荷不含 `initialParams` → 单次 `setParams`（标准路径）。
4. **每块独立**：TS 实现跨调用无状态（§4.5）→ 分块处理 = 逐块独立调用；**blockSize 是
   行为参数**（不同块长 → 不同分析帧集 → 不同输出，rate=2 下整块 vs 3000 块长实测最大
   样本差 ~0.86）→ **向量固定 blockSize，两支线必须按同一块长回放**。
5. **signalsmith 探测永不命中**：驱动器与对拍门禁从不调用 `isSignalsmithAvailable()`，
   静态适配缓存保持空 → `processStereo` 恒走自研相位声码器路径（§4.9）。
6. **一句话定义**：**hse-stretch 向量：每块独立 `processStereo`，输出按块窗映射
   （截断/补零）回填定长网格；参数突变 case 以两次 `setParams` 合法序列驱动。**

### 4.7 精度纪律（跨实现对拍前提）

- 重构区偏差来自相位累积与逐级 f32 量化（FFT 逐级 f32 写回、f32 缓冲），是**实现噪声级**
  行为；窗边缘保留区输出为 ~1e-12..1e-9 的小值，其容差下界（floor 1e-9 的相对式）远小于
  任何非逐位一致的数值差异；
- 故本模块对拍与 [fft](fft.md) §四同一纪律：**以「算术调度等价」为前提，实践目标为
  逐位一致**——FFT 核（hse-core 批次五）、Hann 窗、帧循环运算序、f32 落点纪律必须逐字
  复刻；相对容差 1e-6 是契约判据的统一形态，不是调度自由度。

### 4.8 实证行为记录（导出工具冻结前逐项验证）

1. **rate=1 / semitones=0 非直通**：整段单块（6400 帧）输出 outLen=6656（+256）；中腹
   重构区相对偏差 ~1e-5（rms 7.7e-6）、逐位相等样本 149/5800；两端 ~210 样本为保留区
   淡入淡出小值——「逐位直通」与「严格近似恒等」两种表述均不成立，以冻结向量为准。
2. **跨调用无状态**：全新实例 vs 预热（含 reset / 不含 reset）再处理，输出逐位一致；
   参数切换（双 `setParams` 序列）后与全新终参实例逐位一致。
3. **blockSize 依赖**：rate=2 下 6400 帧整块 vs 3000 块长分块，最大样本差 ~0.86——
   blockSize 必须随载荷固化。
4. **变调频率**：440Hz 正弦、semitones=+5、整段单块 → 过零估计 584.5Hz（解析目标
   440·2^(5/12) ≈ 587.3Hz，估计器边缘效应内一致）。
5. **长度表**：见 §4.2（rate=1 → len+256..len+512 形态；semitones>0 时可短于 len →
   块窗映射补零形态真实存在）。

### 4.9 signalsmith 可选适配（不进入向量）

- `isSignalsmithAvailable()` 动态 import `signalsmith-stretch` 并仅认可「同步纯 DSP 类
  接口」；官方包为 Web Audio/AudioWorklet 异步包装，Node 下探测即使命中类导出，
  适配路径也以同步 `process` 契约为门；
- **向量纪律**：驱动器与对拍门禁从不调用探测（静态缓存保持空），全部向量恒为自研相位
  声码器路径；该事实是本规格的组成条款，移植实现不得在向量回放中启用任何替代后端。

## 五、GWT 行为条款

> 定量断言一律以 `specs/dsp/vectors/hse-stretch.<case>.json` 冻结夹具 + 容差公式判定
> （README §3.5），条款内不内嵌具体参数数值与期望值。

### GWT-HS-01：rate=1 / semitones=0 为 STFT 重构（非直通、非近似恒等）
- **给定**：rate=1、semitones=0；整段单块驱动（blockSize = frames）
- **当**：按 §4.6 处理宽频双声道激励（输出经块窗截断回填）
- **则**：中腹重构区近似输入（相对偏差 ~1e-5 量级、相位累积噪声级），两端窗边缘保留区
  为淡入/淡出小值——输出与输入**既非逐位一致也非 1e-6 内恒等**，精确波形以冻结向量界定；
  驱动器若以「直通/近似恒等」替代 STFT 流水线必然超差

### GWT-HS-02：rate=2 时间伸缩（多块分块依赖）
- **给定**：rate=2、semitones=0；多块驱动（含非整除末块）
- **当**：按 §4.6 块窗映射驱动（每块独立伸缩后截断回填）
- **则**：输出为逐块独立 2× 伸缩的前段拼接（STFT 分帧与驱动块长的交互随载荷固化）；
  输入与 case1 相同（成对对照）、输出显著可区分；精确波形以冻结向量界定

### GWT-HS-03：semitones=+5 变调（正弦激励）
- **给定**：rate=1、semitones=+5；多块驱动；输入为 440Hz 附近双声道正弦（相位去相关）
- **当**：按 §4.6 块窗映射驱动（两阶段：伸缩 ×pitchScale + 重采样 1/pitchScale；块输出
  短于块长 → 补零形态）
- **则**：中腹频率 ≈ 440·2^(5/12) Hz（变调生效、长度语义按 §4.2）；逐块补零边界与精确
  波形以冻结向量界定

### GWT-HS-04：参数突变双 setParams 序列
- **给定**：载荷含 `initialParams`（rate=1、semitones=0）与最终快照（rate>1、
  semitones≠0）、`switchAtBlock` 落在段中
- **当**：按 §4.6.3 序列驱动
- **则**：切换前各块 = 全新初始参实例逐块输出；切换后各块 = 全新终参实例逐块输出
  （参数突变 reset 语义的契约固化，§4.5）；切换边界逐样本以冻结向量界定

### GWT-HS-05：blockSize 是行为参数（模块特有）
- **给定**：任意 rate ≠ 1 或 semitones ≠ 0 的参数
- **当**：比较不同 blockSize 的回放
- **则**：输出**依赖 blockSize**（STFT 分析帧集随块长变化，实证差异显著）——两支线对拍
  必须按向量固定 blockSize 回放

### GWT-HS-06：跨调用无状态 / reset 不可观测（行为事实，由单元测试覆盖，不入向量）
- **给定**：同一实例先处理任意内容（含显式 reset 与参数切换）再处理目标内容
- **当**：与全新实例直接处理目标内容对比
- **则**：输出逐位一致（§4.5 实证）；`reset()` 与参数突变 reset 在当前实现下对输出无影响

### GWT-HS-07：输出长度语义（由单元测试覆盖，不入向量）
- **给定**：任意参数
- **当**：整段处理
- **则**：输出长度 ≈ 输入 × rate（±3% 量级，Hs 取整与帧集覆盖所致偏差内）；左右声道
  等长；输入缓冲不被改写

### GWT-HS-08：钳制极值无数值事故（由单元测试覆盖，不入向量）
- **给定**：rate/semitones 双向越界（clamp 至 0.1/8/±36）
- **当**：常规激励
- **则**：全程无 NaN / Infinity、有界不发散

### GWT-HS-09：抛错路径（由单元测试覆盖，不入向量）
- **给定**：fs ≤ 0 或非有限；channels 非正整数
- **当**：构造 HseStretch
- **则**：分别抛 `Error('invalid sample rate')` / `Error('invalid channel count')`

### GWT-HS-10：自研路径唯一性（事实标准条款，非独立断言）
- 向量驱动器与对拍门禁从不调用 `isSignalsmithAvailable()`（§4.9）；全部冻结向量均为
  自研相位声码器路径的产物，移植实现不得在回放中启用替代后端

## 六、向量用例

**本模块全部定量行为以 `specs/dsp/vectors/hse-stretch.<case>.json` + 同名 `.f32` 冻结
夹具为准**；本章仅列预期 case 覆盖面（维度清单，不含具体参数数值），最终以导出工具产出
并冻结的内容为唯一判据。格式契约见 [`specs/README.md`](../README.md) §三（本模块按 §4.6
块窗映射模型驱动）；JSON 合法性由 Schema 校验。

预期覆盖面（case1–case4）：

1. **rate=1 恒速重构**：semitones=0/rate=1，整段单块驱动（blockSize = frames），宽频
   双声道激励（三频正弦叠加 + 固定种子 LCG 伪噪声）；输出经块窗截断（outLen > frames）
   （GWT-HS-01）；
2. **rate=2 变速**：与 case1 同输入（成对对照）、rate=2，多块驱动（3000/3000/400 帧，
   末块 < N 的单帧形态入向量）；每块截断回填（GWT-HS-02/05）；
3. **semitones=+5 变调**：rate=1、正弦激励（440Hz 双声道、相位去相关），多块驱动
   （2048/2048/1904 帧）；每块输出短于块长 → 补零形态入向量（GWT-HS-03）；
4. **参数突变序列**：initialParams（恒速）→ 中途切至终参（rate>1 且 semitones≠0），
   `switchAtBlock` 落在段中；块长覆盖 N 整除块与短末块（GWT-HS-04）。

维度说明：四条 case 覆盖 rate=1/2/1.5、semitones=0/+3/+5、单块与多块、块窗映射的截断与
补零两形态、N 整除块 / 非整除短块（400/256/1904 帧）与参数切换边界。采样率全部 48000
（fs 进入 Resampler 比值的 f64 构成，是行为参数，§一）。rate<1 与钳制极值、抛错、长度
语义由单元测试覆盖（GWT-HS-07/08/09），不入本批向量。**全部向量输出依赖 blockSize
（GWT-HS-05），两支线必须按向量固定的 blockSize 回放；跨实现对拍以逐位一致为实践目标
（§4.7）。**

.f32 数据按 [README §3.3](../README.md) 四段 planar 小端布局存放；分块驱动方式按本规格
§4.6 块窗映射模型执行。

## 七、关联文件

- 总纲契约：[`specs/README.md`](../README.md)
- Schema：[`specs/schema/vector-case.schema.json`](../schema/vector-case.schema.json)
- 行为事实标准：`src/dsp/HseStretch.ts`（`HseStretch`/`HseStretchParams`）；
  依赖内核：`src/dsp/fft.ts`（基-4 复 FFT，[fft](fft.md)）、`src/dsp/Resampler.ts`
  （多相窗口化 sinc）
- 引擎暴露（不内联主链）：`src/engine/HyperSoundEngine.ts`（`getStretch()`）
- 参考单元测试：`test/stretch.test.ts`、`test/stretch-signalsmith.test.ts`
- 兄弟规格：[biquad](biquad.md) ｜ [limiter](limiter.md) ｜ [reverb-simple](reverb-simple.md) ｜
  [compressor](compressor.md) ｜ [bass-enhancer](bass-enhancer.md) ｜ [mid-side](mid-side.md) ｜
  [eq-chain](eq-chain.md) ｜ [fdn-reverb](fdn-reverb.md) ｜ [deesser](deesser.md) ｜
  [loudness-comp](loudness-comp.md) ｜ [dynamic-eq](dynamic-eq.md) ｜ [mod-effects](mod-effects.md) ｜
  [fft](fft.md) ｜ [convolver](convolver.md) ｜ [modulation-matrix](modulation-matrix.md)
