# 规格：convolver —— 非均匀分区卷积混响 + IR 去周期化

> **规格属性**：本文件是双支线共享规格。行为事实标准 = `src/dsp/Convolver.ts`；
> 参数字段名以本规格 §三为准（模块无 `setParams`——构造选项与 `loadIR`/`setMix`/
> `setPreDelayMs` 实参共同构成参数快照），本规格不得臆造字段。
> 格式契约见 [`specs/README.md`](../README.md) §三，
> 向量合法性由 [`specs/schema/vector-case.schema.json`](../schema/vector-case.schema.json) 约束。
> 频域内核 = [fft](fft.md)（基-4 复 FFT，原位，正变换不缩放 / 逆变换 ÷N）。

---

## 一、模块概述

- **定位**：链路级卷积混响——非均匀分区卷积（non-uniform partitioned convolution）：
  IR 前部（瞬态/听感关键段）用短分区保证低延迟，后部（长尾）用长分区减少 FFT 次数；
  两级分区在同一个逐声道累加器上 overlap-add，数学上严格等价于完整线性卷积。
  另提供 IR 去周期化（尾部指数衰减窗），消除循环卷积伪影。
- **出处**：分区卷积思路源自 W. Gardner《Efficient Convolution without Input-Output Delay》
  （JAES 1995）与 DAFX-03 分区卷积论文；去周期化为本项目自研方法（技术文档 §2.1）。
- **确定性**：同输入同 IR 同参数必同输出；无随机、无 Date、无 console；`processStereo`
  稳态零分配（全部缓冲在 `loadIR` 非实时路径预分配；pending 队列的突发扩容只发生在
  输入块长超出容量的非常规驱动下，见 §4.4 注记）。
- **采样率**：构造时固定；fs ≤ 0 或非有限值抛 `Error('invalid sample rate')`。

## 二、接口签名（事实标准摘录）

```ts
export interface ConvolverOptions {
  partitionSize?: number      // 最短分区长 Ls（默认 512），clamp [32, 8192]；= 湿路延迟
  longPartitionSize?: number  // 长分区长（默认 4096），生效为 Ls 的 2 的幂整数倍
  shortRegionMs?: number      // IR 前部短分区时长 ms（默认 100），clamp [0, 5000]
  dePeriodize?: boolean       // 是否对 IR 去周期化（默认 true）
}
export class Convolver {
  constructor(fs: number, opts?: ConvolverOptions)
  loadIR(ir: Float32Array, irName?: string): void  // 空/含 NaN/Inf/全零 IR 抛 Error
  setMix(mix: number): void                        // clamp [0,1]；1=纯湿
  setPreDelayMs(ms: number): void                  // clamp [0,1000]；湿路预延迟
  process(x: Float32Array): Float32Array           // 单声道一次性卷积（单元测试域，不入向量）
  processStereo(l: Float32Array, r: Float32Array): void  // 流式立体声就地处理（向量驱动入口）
  getLatencySamples(): number                      // 恒 = partitionSize（见 §4.5）
  reset(): void
}
```

向量驱动入口是 `processStereo`；`process`（返回新数组的一次性卷积）与 `reset`、`loadIR`
的抛错路径由单元测试覆盖（§五）。

## 三、参数表（向量 `params` 快照字段）

向量 JSON 的 `params` 对象固定使用以下七字段；采样率 fs 不进 params，取自向量顶层
`sampleRate` 字段：

| 字段 | 类型 | 有效域（clamp 后生效值） | 默认值 | clamp / 边界行为 |
|---|---|---|---|---|
| `partitionSize` | number（整数语义） | `[32, 8192]` | 512 | round 后双向钳制；即湿路延迟 Ls（`getLatencySamples()`） |
| `longPartitionSize` | number | ≥ 1（生效值 = Ls·k） | 4096 | k = 2^ceil(log2(want/Ls))：自动向上取整为 Ls 的 2 的幂整数倍；want ≤ Ls 时 k=1（退化为均匀分区） |
| `shortRegionMs` | number（ms） | `[0, 5000]` | 100 | round 后钳制；换算为样本数 S = round(ms/1000·fs) |
| `dePeriodize` | boolean | true / false | true | `loadIR` 时是否执行 §4.3 去周期化 |
| `mix` | number | `[0, 1]` | 1（类字段初值） | 双向钳制；out = (1−mix)·dry + mix·wet |
| `preDelayMs` | number（ms） | `[0, 1000]` | 0 | 双向钳制；湿路预延迟样本 = round(ms·fs/1000) |
| `ir` | object（IR 配方） | 见 §4.2（kind ∈ delta / expNoise） | — | 确定性配方，两支线按 §4.2 逐字复现 |

补充说明：

- 模块无 `setParams`；驱动器按**引擎接线顺序**一次性施加快照：
  `new Convolver(fs, {partitionSize, longPartitionSize, shortRegionMs, dePeriodize})`
  → `loadIR(buildIrRecipe(params.ir), irName)` → `setMix(mix)` → `setPreDelayMs(preDelayMs)`
  （`src/engine/HyperSoundEngine.ts` 卷积混响阶段同序）；
- 向量必须提供完整七字段（不依赖类字段初值）；
- `irName` 不进 params（仅影响 `getIrName()` 标签，无数值行为）。

## 四、处理语义

### 4.1 分区规划（`loadIR` 内，两支线必须逐字一致）

```text
Ls = clamp(round(partitionSize), 32, 8192)          # 最短分区长 = 湿路延迟
k  = 1；若 wantLl > Ls：k = 2^ceil(log2(wantLl / Ls))   # 向上取 2 的幂倍数
Ll = Ls · k                                          # 长分区长
S  = round(clamp(shortRegionMs, 0, 5000) / 1000 · fs)  # 短区段样本数
M  = IR 长度（§4.3 去周期化不改变长度）
Ps = max(1, ceil(S / Ls))；若 Ps < k−1 → Ps = max(1, k−1)  # 保证长分区写入偏移非负
longStart = Ps · Ls
Pl = (longStart < M) ? max(1, ceil((M − longStart) / Ll)) : 0
若 Pl = 0（IR 短于短区段规划）：Ps = max(1, ceil(M / Ls))，longStart 随之重算但不使用
  （退化为均匀分区）
Ns = nextPow2(2·Ls)；Nl = nextPow2(2·Ll)             # 短/长 FFT 尺寸
P_total = Ps + Pl·k                                  # 按短块粒度折算的总分区数
accLen  = (P_total + 2)·Ls                           # 逐声道累加器与 pending 容量
```

- IR 被切分为 Ps 个短分区（IR[0..Ps·Ls)，每段 Ls 补零到 Ns 做 FFT）与 Pl 个长分区
  （IR[longStart..M)，每段 Ll 补零到 Nl 做 FFT）；分区频谱在 `loadIR` 内预计算（f32 存储）；
- **延迟与 IR 长度解耦**：湿路延迟恒为 Ls，与 M/Ps/Pl/k 无关（§4.5）。

### 4.2 IR 配方（向量 `params.ir`，两支线必须逐字一致）

IR 由确定性配方生成，不随向量携带二进制 IR。全部配方以 **f64 求值、存入 Float32Array
时一次量化为 f32**；LCG 与导出工具的伪噪声同族（禁 Math.random）：

- **delta**（单点冲激，逐位锚点用）：

```text
输入：delay（整数 ≥ 0）
length = delay + 1；ir[delay] = 1.0；其余全 0
```

- **expNoise**（确定性种子 LCG 指数衰减噪声，真实混响尾用）：

```text
输入：length（整数 ≥ 2）、seed（uint32）、decay（> 0）、amp
s = seed >>> 0
对 i = 0 .. length−1：
    s = (imul(s, 1664525) + 1013904223) >>> 0     # 先推进；首样本亦先推进一次
    u = s / 4294967296
    ir[i] = ((u * 2 - 1) * amp) * exp((-decay * i) / (length - 1))
```

（乘法结合序、表达式形态逐字固化——f64 乘法不可交换结合，任何重排都会改变 f32 量化结果。）

- 未来新增配方 kind 属规格扩展：先增补本节并新增向量，再纳入两侧门禁。

### 4.3 IR 去周期化（`dePeriodize=true` 时 `loadIR` 前置执行，两支线必须逐字一致）

对调用方传入的 IR（不改写原数组）计算：

```text
W    = max(4, round(0.01 · fs))        # 10ms 包络窗，half = W >> 1
env(n) = sqrt( (Σ_{j=max(0,n−half)}^{min(M,n+half+1)−1} ir[j]²) / (hi−lo) )   # 移动平均 RMS
peakIdx/peakVal = env 的首个最大值（严格大于才更新）
peakVal ≤ 1e-12 → 原样返回（防御分支；loadIR 已挡全零）
threshold = peakVal · 1e-3             # −60dB
lastAbove = 从 n = peakIdx 扫到 M−1，最后一个 env(n) > threshold 的 n（后缀判定，
            而非"首次低于"——避免稀疏 IR 如延迟冲激被误衰减）
n0 = lastAbove + 1；n0 ≥ M → 原样返回（尾部未过阈 → 无操作）
τ = 0.05 · fs                          # ≈50ms
对 n = n0 .. M−1：out[n] = f32( out[n] · exp(−(n − n0) / τ) )
```

- 去周期化**不改变 IR 长度 M**，因此不影响 §4.1 的任何规划量；
- 对尾部包络未跌破 −60dB 的 IR（含 δ 脉冲与缓衰减 IR），本步骤为精确无操作（实证：
  delta IR 下 dePeriodize on/off 输出逐位一致）。

### 4.4 流式处理（`processStereo`，两支线必须逐字一致）

记 B = min(l.length, r.length)，Ls = partitionSize：

1. **输入按 Ls 分组**：逐样本写入当前短输入块；块满（Ls 样本）时执行一次湿块生产：
   - 长输入块累积（仅 Pl > 0 时）：本短块复制进 longIn[blockIdx mod k] 段；每第 k 个短块
     （长块满）做一次长 FFT（Nl）→ 与 Pl 个长分区频谱复乘、IFFT（÷Nl）、overlap-add 写入
     逐声道累加器 outAccum（前半起点 = (longStart + p·Ll) − (k−1)·Ls，后半 = 前半 + Ll）；
   - 短 FFT（Ns）→ 与 Ps 个短分区频谱复乘、IFFT（÷Ns）、overlap-add 写入 outAccum
     （前半 p·Ls，后半 (p+1)·Ls）；
   - 取 outAccum[0..Ls) 写入 pending 队列尾；outAccum 左移 Ls、尾部清零
     （**跨块累加语义**：块处理前不得清零 outAccum——左移保留的正是各分区历史贡献）；
   - 完成块计数 +1。左右声道各持独立的 inputBlock/longIn/outAccum/pending，
     但**共享同一套记账**（inputPos/pendingPos/pendingLen/completedBlocks/totalOut/
     totalWetOut）；pending 为滑动窗口队列（队尾越界时 copyWithin 压缩；突发超容量时
     扩容）——均为容量管理，无数值影响；
2. **湿路逐样本放行**（支持任意块长）：对每个输出样本 i，wetIdx = totalOut − Ls；
   当 `pendingLen > 0 && wetIdx ≥ 0 && wetIdx < completedBlocks·Ls && totalWetOut === wetIdx`
   时从 pending 队头取一样本（FIFO），否则湿样本 = 0；
3. **preDelay**：湿样本经环形延迟线（容量 fs 样本，预分配）；preDelaySamples = 0 时直通；
4. **干湿混合**：`out = (1 − mix)·dry + mix·wet`——dryGain = 1 − mix 与 wetGain = mix 均为
   f64，乘加在 f64 内完成后写回 f32；**干路不延迟**；
5. 全部 FFT/复乘/IFFT 的算术纪律同 [fft](fft.md) §四（f64 累加、f32 落点、调度等价要求）。

**blockSize 无关性（实证）**：湿块生产按 Ls 记账、湿路按样本放行，两者都与驱动调用的切块
方式无关——同输入同参数下，驱动 blockSize 取 128/333/384/512/700/4096 的输出**逐位一致**
（对比 eq-chain/loudness-comp 的块长敏感形态）。因此本模块对拍对块长鲁棒；向量的
blockSize 字段仍按契约冻结（驱动回放的确定性形式）。

### 4.5 延迟语义（IR 长度与 partition 的交互——行为事实）

- `getLatencySamples()` **恒等于 partitionSize（Ls）**，与 IR 长度 M、分区数 Ps/Pl、
  长分区倍数 k、mix、preDelay 均无关（实证：M = 1 / 2048 / 48000 @Ls=512 均返回 512；
  IR 长度只改变 Ps/Pl 与累加器长度 accLen，见 §4.1）；
- 湿路相对干路的总延迟 = Ls + preDelaySamples（后者不反映在 `getLatencySamples()`，
  由引擎侧按需补偿）；
- **湿路尾块缓冲**：湿输出以块粒度生产、按样本放行——输出位置 i 的湿样本需要输入位置
  i−Ls 及其所在块已完成。输入流结束后，尚缺后续输入触发的尾部湿样本（≤ Ls + IR 尾）
  不再释放。冻结向量即这一有限窗口语义：两侧按同一 frames 窗口回放即自然一致；
- 首 Ls 个输出样本的湿路精确为 0（wetIdx < 0 不放行）——跨实现锚点（控制流精确，
  输出逐位 +0）。

### 4.6 reset 与抛错（由单元测试覆盖，不入向量）

- `reset()` 清零全部流式状态（输入块/pending/outAccum/长输入块/延迟线/记账计数）；
  分区频谱与分区规划保留（IR 不重载）；
- `loadIR` 对空数组、含 NaN/Infinity、全零 IR 分别抛 `Error`；未载入 IR 时调用
  `processStereo`/`process` 抛 `Error('no impulse response loaded')`。

## 五、GWT 行为条款

> 定量断言一律以 `specs/dsp/vectors/convolver.<case>.json` 冻结夹具 + 容差公式判定
> （README §3.5），条款内不内嵌具体参数数值与期望值。

### GWT-CV-01：δ IR 延迟直通锚点
- **给定**：ir 配方 = delta(delay=0)（单点冲激）、mix=1、默认分区（Ls=512、均匀单短分区）
- **当**：按冻结向量分块送入左右声道（输入带直流偏置，幅值全程有界远离零）
- **则**：首 Ls 个输出样本**逐位 +0**（§4.5 控制流锚点）；其后湿路 = 输入延迟 Ls 的直通，
  偏差为分区卷积 FFT 往返舍入（1e-7 量级，由冻结向量与容差界定）；`getLatencySamples()`
  = Ls；dePeriodize 对 δ IR 为精确无操作（后缀判定，实证）

### GWT-CV-02：expNoise IR 真实卷积尾（非均匀分区参与）
- **给定**：ir 配方 = expNoise（长度超过短区段 → 长分区 Pl ≥ 1 参与运算）、dePeriodize=false、
  mix=1
- **当**：宽频确定性激励长序列流式处理
- **则**：湿路 = 输入与该 IR 的完整线性卷积延迟 Ls 对齐后的流式窗口（分区卷积数学等价于
  线性卷积）；全程无 NaN/Infinity、有界；精确波形（含长分区贡献）以冻结向量界定——
  驱动器漏做长分区累加或错置写入偏移必然超差

### GWT-CV-03：dePeriodize on/off 成对对照
- **给定**：两条除 `dePeriodize` 外完全相同的成对向量（同 IR 配方、同输入、同块长），
  IR 尾部包络跌破 −60dB（去周期化真实触发）
- **当**：同一确定性激励分别处理
- **则**：去周期化触发点之后的尾段两向量显著可区分（触发点之前逐样本一致）；
  去周期化的包络窗、−60dB 后缀判定、τ 常数任何偏差都会造成可测差异；
  对照的「无操作侧」同时固化 §4.3 的不触发语义

### GWT-CV-04：非整除块长流式 + mix 干湿混合
- **给定**：驱动 blockSize 与 Ls 非整除（且 B > Ls，单次调用生产多个湿块）、mix ∈ (0,1)、
  ir 配方 = expNoise（短 IR，均匀多短分区）、partitionSize 取非默认值
- **当**：按冻结向量分块流式处理
- **则**：out = (1−mix)·dry + mix·wet（dryGain = 1−mix 的 f64 语义）；干路不延迟、湿路延迟
  Ls；任意块长下无丢块/无 NaN（逐样本放行 + 突发扩容语义，§4.4）；精确波形以冻结向量界定

### GWT-CV-05：驱动 blockSize 无关（行为事实，实证）
- **给定**：任意参数与输入
- **当**：仅改变驱动切块方式（B < / = / > Ls、整除或非整除）
- **则**：输出**逐位一致**（湿块按 Ls 记账、湿路按样本放行均与调用切分无关）——
  本模块对拍对块长鲁棒，与 eq-chain/loudness-comp 的块长敏感形态相反

### GWT-CV-06：延迟语义与 IR 长度解耦
- **给定**：同一 partitionSize、不同长度的合法 IR（含短于与长于短区段两态）
- **当**：分别 loadIR 并查询
- **则**：`getLatencySamples()` 恒等于 partitionSize（IR 长度只影响分区数规划）；
  湿路总延迟 = Ls + preDelay（后者单独设置、不含在该查询内）；湿路尾块按块粒度缓冲
  （§4.5 有限窗口语义）

### GWT-CV-07：IR 配方确定性
- **给定**：§4.2 定义的 delta / expNoise 配方
- **当**：两支线按配方逐字重建 IR（同 seed 同 length 同 decay 同 amp，f32 量化点一致）
- **则**：重建 IR 逐位一致（LCG 同族、表达式结合序固化），卷积结果随冻结向量对拍；
  配方语义随向量载荷固化

### GWT-CV-08：reset / 抛错 / 钳制（由单元测试覆盖，不入向量）
- **给定**：空/全零/含 NaN 的 IR；未载入 IR 即处理；越界 mix/preDelayMs/分区参数
- **当**：调用对应接口
- **则**：分别抛错或按 §三钳制生效；reset 清空流式状态、保留 IR 与分区规划；
  reset 后重放与首次从零状态处理输出一致

## 六、向量用例

**本模块全部定量行为以 `specs/dsp/vectors/convolver.<case>.json` + 同名 `.f32` 冻结夹具
为准**；本章仅列预期 case 覆盖面（最终以导出工具产出并冻结的内容为唯一判据）。格式契约
见 [`specs/README.md`](../README.md) §三；JSON 合法性由 Schema 校验。

预期覆盖面（case1–case4）：

1. **δ IR 延迟直通锚点**：ir={kind:"delta",delay:0}、mix=1、默认分区（512/4096/100ms/
   dePeriodize=true——对 δ 为无操作）、blockSize=Ls、帧数恰整除；输入带直流偏置使输出
   幅值远离零（首 Ls 逐位 +0 + 延迟直通，GWT-CV-01）；
2. **expNoise 真实卷积尾（去周期化关）**：ir={kind:"expNoise"}（长度超短区段 → Pl≥1 长分区
   参与）、dePeriodize=false、mix=1、blockSize 与 Ls 非整除（GWT-CV-02）；
3. **去周期化开（与 2 成对对照）**：参数与输入与 2 完全相同、仅 dePeriodize=true——
   IR 尾部跌破 −60dB 触发尾部指数衰减，尾段显著可区分（GWT-CV-03）；
4. **非整除块长 + mix 混合 + 非默认分区**：partitionSize 取非默认短分区（长分区按倍数规则
   生效）、B > Ls 的非整除驱动块长、mix ∈ (0,1)、短 expNoise IR（均匀多短分区）、
   dePeriodize=false（GWT-CV-04）。

维度说明：覆盖 δ/配方噪声两类 IR、均匀单分区/多短分区/短+长非均匀三种分区形态、
mix=1 与 mix∈(0,1) 两态、dePeriodize 触发与不触发（含对 δ 无操作）两侧、B < / = / > Ls
多种块长关系、帧数对 blockSize 整除与非整除两态。`reset`/抛错/越界钳制由单元测试覆盖
（GWT-CV-08）。采样率维度：四条向量均取 48000（fs 进入分区规划与去周期化常数；
多采样率属单元测试维度）。本模块输出与驱动 blockSize 无关（GWT-CV-05，实证逐位一致），
向量的 blockSize 字段按契约冻结回放形式。

.f32 数据按 [README §3.3](../README.md) 四段 planar 小端布局存放；分块驱动方式按 §3.4
执行（本模块原生 `processStereo` 就地语义，驱动顺序见 §三补充说明）。

## 七、关联文件

- 总纲契约：[`specs/README.md`](../README.md)
- Schema：[`specs/schema/vector-case.schema.json`](../schema/vector-case.schema.json)
- 行为事实标准：`src/dsp/Convolver.ts`；选项类型：`ConvolverOptions`（同文件）
- 频域内核：[fft](fft.md)（基-4 复 FFT 与算术调度纪律——本模块对拍纪律的组成部分）
- TS 实现契约：`src/dsp/API_SPEC.md` 模块 9
- 引擎接线（驱动顺序事实）：`src/engine/HyperSoundEngine.ts`
  （构造(dePeriodize) → loadIR → setMix → setPreDelayMs → 逐块 processStereo）
- 参考单元测试：`test/convolver.test.ts`、`test/convolver-blocklen.test.ts`
- 兄弟规格：[biquad](biquad.md) ｜ [limiter](limiter.md) ｜ [reverb-simple](reverb-simple.md) ｜
  [compressor](compressor.md) ｜ [bass-enhancer](bass-enhancer.md) ｜ [mid-side](mid-side.md) ｜
  [eq-chain](eq-chain.md) ｜ [fdn-reverb](fdn-reverb.md) ｜ [deesser](deesser.md) ｜
  [loudness-comp](loudness-comp.md) ｜ [dynamic-eq](dynamic-eq.md) ｜ [mod-effects](mod-effects.md) ｜
  [fft](fft.md)
