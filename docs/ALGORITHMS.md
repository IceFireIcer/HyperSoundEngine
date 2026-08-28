# 音频算法参考文档（原理与实现）

> 本文是引擎各 DSP 模块的算法原理速查，与 `src/dsp/` 各实现一一对应；
> 全部内容来自公开学术资料、开源库源码与公开文档（文末参考资料附公开链接）。
> 覆盖范围：频谱分析、参数均衡、卷积/算法混响、压缩限幅、齿音抑制、虚拟低频、
> 等响度补偿、响度归一化、M/S 处理、变调变速、音高检测、声源分离、特征提取、
> 重采样、频响补偿，以及 Web Audio / AudioWorklet 实时实现要点。

---

## 0. 预备知识

| 概念 | 说明 |
|---|---|
| 采样率 Fs | 每秒采样数。CD 44.1 kHz、视频 48 kHz；Nyquist 频率 = Fs/2。 |
| 数字滤波器 | FIR（有限脉冲响应，线性相位，阶数高）；IIR（无限脉冲响应，阶数低、有相位失真）。 |
| z 变换 | 离散信号工具，滤波器表示为 H(z) = B(z)/A(z)。 |
| 分贝 | 幅度比 20·log10(a)；功率比 10·log10(p)。 |
| 响度单位 | dB SPL（声压级）、phon（等响度级）、LUFS/LKFS（节目响度，1 LUFS = 1 dB）。 |
| STFT | 短时傅里叶变换：加窗分帧后逐帧 FFT，得到时频表示（频谱图）。 |

**FFT（快速傅里叶变换）**（实现：`src/dsp/fft.ts`）
- DFT：X[k] = Σ_{n=0}^{N-1} x[n]·e^{−j2πkn/N}，直接计算 O(N²)。
- 蝶形分解（Cooley–Tukey）把复杂度降到 O(N·log₂N)，要求长度 N 为 2 的幂（或混合基）。
  本引擎实现为 radix-4 合并蝶形（相比 radix-2 复数乘法约省 25%，N≥1024 时实测 ≥15% 提升），
  twiddle 表预计算缓存。
- 注意：矩形窗有频谱泄漏，常用 Hann/Hamming/Blackman 窗（引擎提供 `hannWindow`）；
  频谱分辨率 Δf = Fs/N。

---

## 1. 参数均衡器（PEQ）与双二阶滤波器

### 1.1 双二阶（biquad）基础（实现：`src/dsp/biquad.ts`）
所有常规 EQ 均可由二阶 IIR（biquad）实现：

    H(z) = (b0 + b1·z⁻¹ + b2·z⁻²) / (1 + a1·z⁻¹ + a2·z⁻²)

差分方程（Direct Form 1）：y[n] = b0·x[n] + b1·x[n−1] + b2·x[n−2] − a1·y[n−1] − a2·y[n−2]。

**实现选择**：引擎采用**转置直接 II 型（TDF2）**，系数量化误差对极点影响更小，数值更稳定
（DSPFilters 等专业实现均采用 TDF2 或 DF1 双精度累加）。

### 1.2 RBJ Audio EQ Cookbook（系数公式）
来源：Robert Bristow-Johnson《Cookbook formulae for audio EQ biquad filter coefficients》。
定义：A = 10^(dBgain/40)、w0 = 2π·f0/Fs、α = sin(w0)/(2·Q)（peaking/带通）等。

**Peaking EQ（峰形均衡，最常用）**：
- b0 = 1 + α·A；b1 = −2·cos(w0)；b2 = 1 − α·A
- a0 = 1 + α/A；a1 = −2·cos(w0)；a2 = 1 − α/A

**低架 Low Shelf / 高架 High Shelf**（S 为架斜率，S=1 时 α = sin(w0)/2·√2）：
- 低架：b0 = A[(A+1) − (A−1)cos(w0) + 2√A·α]，a0 = (A+1) + (A−1)cos(w0) + 2√A·α（b1/b2、a1/a2 见原文）
- 高架：cos(w0) 项符号翻转（见原文公式）

**高通/低通/带通/陷波/全通**：原文各给一组公式（如高通 b0 = (1+cos w0)/2, b1 = −(1+cos w0), b2 = b0, a0 = 1+α, a1 = −2cos w0, a2 = 1−α）。

**关键性质**：boost 与 cut 对称的 peaking（同 f0、同 Q）级联后恰好为平直响应（"wire"），
这是设计多段 EQ 的重要不变量。BLT 双线性变换需对 f0 预畸变（prewarp），公式已内置。

### 1.3 级联 EQ 与 Q 补偿（实现：`src/dsp/EqChain.ts`，块级向量化处理）
- 多段 EQ = 多个 peaking biquad 串联，每段独立 (f, Q, gain)，逐段系数随参数更新重算。
- **级联问题**：相邻频段的提升/衰减会叠加，实际响应 ≠ 各段响应之和，
  在控制点测得的总响应与目标曲线有误差（实测可达 ~10 dB 级）。
- **Q 补偿思路**（`eq.qCompensation`）：
  1. 计算各段在各自中心频率处的级联总响应（离线仿真，用 H(z) 幅值）；
  2. 用误差值修正各段增益：gain_i ← gain_i − error_i（迭代 2–3 次收敛）；
  3. 或对相邻段 Q 做缩放（bandwidth 重叠补偿），保持过零平坦。

### 1.4 智能 EQ（IEQ）/自动目标曲线（引擎内置，见 `src/engine/HyperSoundEngine.ts`）
- 思路：测量输入信号的长时平均频谱（或按播放内容实时统计），与目标响度曲线
  （如等响度曲线、用户自绘曲线）做差，反解出 EQ 增益。
- 数学上即"频谱整形"：目标 log 频谱 − 实测 log 频谱 = EQ log 增益谱，
  再把它拟合为 N 段参数 EQ（见 §14 的最小二乘拟合方法）。

---

## 2. 混响：卷积混响与算法混响

### 2.1 卷积混响（实现：`src/dsp/Convolver.ts`，非均匀分区）
输出 = 输入与房间脉冲响应（IR）的卷积：
y[n] = Σ_{k=0}^{M−1} h[k]·x[n−k]

- **直接卷积**：每样本 O(M)，M 为 IR 长度（2 s @48 kHz = 96000 抽头），实时不可行。
- **FFT 卷积（Overlap-Add）**：把信号分块（块长 L），每块 FFT 后与 IR 的 FFT 相乘再 IFFT，
  相邻块重叠区相加。复杂度降为每块 O((L+M)·log(L+M))。缺点：需要至少一个块长的延迟。
- **分区卷积（Partitioned Convolution）**（DAFX 经典方案）：
  把长 IR 切成 P 块 h_p（每块 L 点），输入块与每个分区做 FFT 卷积，结果按对应延迟累加：
    y[n] = Σ_p (x ⊛ h_p)[n − p·L]
  引擎采用**非均匀分区**：瞬态区（默认前 ~100 ms）用短分区保低延迟，
  尾部用长分区（4096 点）摊薄 CPU；支持任意块长流式输入。
- **IR 去周期化（`dePeriodize` 选项）**：直接从真实/合成 IR 采样时，
  尾部若被硬截断，循环卷积会产生周期感/伪影。做法：检测 IR 能量包络，从衰减开始点
  施加指数衰减窗，把尾部平滑归零，消除不连续。

### 2.2 算法混响（实现：`src/dsp/ReverbSimple.ts` 与 `src/dsp/FdnReverb.ts`）
- **Schroeder/Freeverb 类（ReverbSimple）**：8 个并联 stereo comb + 4 个 allpass（stk FreeVerb/MIT 语义），
  damping 控制高频衰减，preDelay 与 width 可调。质量高、CPU 极低。
- **FDN 网络混响（FdnReverb）**：反馈延迟网络，密度/扩散可控，适合 hall 类长尾。
- 适用场景：对"空间感"要求不苛刻、想省 CPU 时用算法混响；卷积混响音质上限更高、可导入真实空间 IR。

---

## 3. 动态处理：压缩器 / 限幅器

### 3.1 包络检测（实现：`src/dsp/Compressor.ts`）
- 检波：|x[n]| 或 x²[n] → 一阶平滑：
  env[n] = α·|x[n]| + (1−α)·env[n−1]，α = 1 − exp(−1/(τ·Fs))
  attack 用短 τ（0.1–5 ms），release 用长 τ（50–500 ms）。
- 更专业的做法：用峰值检测器 + 双时间常数（快速 attack、慢 release），
  或对包络取 dB 域平滑（对数域线性插值，听感更自然）。

### 3.2 压缩曲线
- 阈值 Thr、比率 R（4:1 即超阈部分只放 1/4）、拐点（knee，软拐点平滑过渡）、
  make-up gain 补回损失。增益计算（dB 域）：
  gain_dB = min(0, (level_dB − Thr) · (1 − 1/R))（硬拐点）。
- **限幅器（limiter）**：R → ∞，即 level 超过 Thr 的部分全部压平（brickwall）。
  软限幅（soft clip）：y = tanh(x) 或多项式近似，牺牲少量失真换取无爆音。

### 3.3 前瞻限幅器（Lookahead Limiter，实现：`src/dsp/Limiter.ts`）
1. 输入延迟 L（如 5–10 ms）送入峰值检测，同时把音频本身也延迟 L；
2. 检测窗口内峰值 → 需要的增益 g = Thr/peak；
3. 对增益曲线做 attack/release 平滑（attack 0.1–1 ms，release 100–300 ms），
   避免增益跳变产生咔哒声；
4. 平滑后的增益施加到延迟后的音频上（Brickwall 且零过冲）。
- **True Peak（真峰值）**：数字峰值可能漏掉采样点之间的过冲，
  ITU BS.1770 用 4× 过采样（窗函数 sinc 插值）计算真峰值；限幅器要压真峰值，
  否则 D/A 后仍会削波。实现：3 相位 × 8 抽头 sinc 内插（相位 2 利用 sinc 对称性合并抽头对，8→5 次乘法）。

---

## 4. 齿音抑制（De-esser，实现：`src/dsp/Deesser.ts`）

原理：齿音（sibilance，"s"、"sh"，约 4–8 kHz 频段）能量瞬时爆发，
de-esser 检测该频段能量并只在爆发时压缩，不影响其余频率。

实现（支持外部 sidechain 驱动检测）：
1. **侧链提取**：带通滤波（或高通）取出 4–8 kHz（可调 center/Q），得到 s[n]；
2. **包络检测**：对 s[n] 取平方/绝对值 + attack(1–5 ms)/release(50–100 ms) 平滑，得到 env；
3. **阈值比较**：env 超过阈值 Thr 的部分按比率 R 产生增益衰减（同压缩器）；
4. **增益施加**：两种方式——
   - 宽带式（broadband）：衰减整体信号（简单，但可能压低非齿音内容）；
   - 分带式（split-band，默认）：只衰减高频带（音质更好）；
5. 参数：阈值（灵敏度）、频率（齿音中心）、比率、attack/release、mix。

---

## 5. 虚拟低频增强（实现：`src/dsp/BassEnhancer.ts`）

目标：小扬声器/耳机无法重放 40–80 Hz 真实基频时，利用**缺失基频（Missing Fundamental）**
心理声学现象——大脑能从 2、3、4 次谐波"重建"基频音高——制造低音听感。

实现（谐波生成 + 低音下潜）：
1. **低通提取**：LPF（截止 ~80–120 Hz，可调 `cutoffHz`/`q`）取出低频段 x_bass；
2. **谐波生成**：对 x_bass 施加非线性函数产生谐波——
   - odd：x³（奇次，3 次为主）；
   - even：全波整流 |x|（偶次，2 次为主 + DC，由后级 HPF 去除）；
   - atan：ATSR 器件 atan(√|x|)·sign(x)，谐波结构可解析分析，
     在虚拟低音论文中广泛研究；
   - soft：tanh(2·x)（软削波，奇次谐波）；
3. **高通整形**：把生成的谐波信号经过 HPF（截止 = max(150, cutoffHz·1.5) Hz），
   只保留"基频的整数倍谐波"，避免与中频混叠互调；
4. **混合**：原信号 + `10^(levelDb/20)·mix·harmonicGain` 增益的谐波信号；
5. **低音下潜（`lowBoostDb`，-6..+12 dB，默认 0=关闭）**：把低通提取的低频带 x_bass
   按 `10^(lowBoostDb/20) − 1` 混回原信号——等价于以 cutoffHz 为中心的 low-shelf **真实低频能量
   提升**（谐波路径只提供心理声学感知，真实下潜靠这条路径）。

注意：
- 非线性只能作用于低频带（先 LP 再非线性），否则全频带互调失真毁掉中高频；
- 谐波次数选择：2–4 次最有效（更高次听感刺耳、心理声学增益递减）；
- 与"响度提升"区别：这是谐波合成，不是增益。

---

## 6. 等响度补偿（实现：`src/dsp/LoudnessComp.ts`）

- **ISO 226:2003 等响度曲线**：人耳对不同频率灵敏度不同——低音量时对 3–4 kHz 最灵敏、
  对低频/极高频不灵敏；等响度曲线（phon 曲线）描述"听起来一样响"的各频率声压级。
  经典 Fletcher–Munson（1933）为旧版，现行标准为 ISO 226:2003。
- **响度补偿**：音量调低时，按当前音量相对参考音量的曲线差，
  对低频和高频做提升（loudness contour），使低音量下听感依然均衡：
    gain(f) = L_ref(f) − L_vol(f)（同一 phon 级上的 SPL 差，dB）
- **工程实现**：把等响度曲线按倍频程离散化存表；音量变化 → 插值得到补偿增益谱 →
  拟合为 shelving/peaking EQ。引擎 auto 模式：音量越低 → 低频/高频 shelf 提升越大
  （120 Hz/12 kHz shelf，1 kHz 处归零）。
- 注意：补偿量要随音量连续平滑变化，防止切换时爆音；补偿过多会引入底噪（噪声提升）。

---

## 7. 响度归一化（EBU R128 / ITU-R BS.1770，LUFS；实现：`src/dsp/LufsMeter.ts`）

### 7.1 测量算法（BS.1770-4 / EBU R128）
1. **K 加权滤波**：两级滤波器——
   - 前置 RLB 高通（约 38 Hz，−3 dB，12 dB/oct，滤除次声）；
   - 高频搁架（+4 dB @ ~1 kHz 以上，模拟人耳高频灵敏度）；
   48 kHz 下常用固定系数（FFmpeg f_ebur128.c、Audacity EBUR128 类均有公开系数）。
2. **分块统计**：400 ms 窗、75% 重叠（步进 100 ms），每块算 K 加权后均方：
   块响度 L_K = −0.691 + 10·log10(mean(Σ_i g_i²))，单位 **LKFS = LUFS**。
3. **门限（gating）**：只统计"有声"部分——
   - 绝对门限：−70 LUFS 以下的静音块丢弃；
   - 相对门限：先算所有块均值，再丢弃低于 (均值 − 10 LU) 的块，重算。
4. **整合响度（Integrated Loudness）** = 门限后剩余块的能量平均 → 目标通常 −23 LUFS（EBU R128）。
5. **LRA（响度范围）**：块响度直方图第 10 与第 95 百分位之差，描述动态范围。
6. **真峰值**：4× 过采样检测采样点间峰值（见 §3.3）。

### 7.2 归一化应用（引擎 `loudnessNormalization`）
- 目标响度与实测整合响度之差 ΔL = target − measured；
- **实时 AGC 分支**（`useRealtimeMeter`）：逐块估计短时响度，
  用慢时间常数（3 s）的自动增益控制平滑增益，既接近目标又不抽吸；
- **手动增益分支**（`externalGainDb`，调音室音量滑块 0-100% → -60..0 dB）：
  平滑时间常数 **80 ms**——跟手且无咔哒（zipper 由平滑消除）；自动测量分支保留 3 s 防抽吸。

---

## 8. M/S 立体声处理（实现：`src/dsp/MidSide.ts`）

- 变换：M = (L + R)·0.5（中），S = (L − R)·0.5（侧）；逆变换：L = M + S，R = M − S。
- 用途：① **宽度控制**：S 增益 >1 变宽、<1 变窄、=0 变单声道；② 只对 S 做 EQ/压缩
  （侧处理不影响中间人声/贝斯）；③ 立体声增强/去宽。
- 某些实现用 M = L+R、S = L−R（未除 2），逆变换后幅度翻倍，需补偿 6 dB 增益。
- 实现：一个 2×2 矩阵即可，注意双精度、防止 L/R 相关性导致的相位抵消。

---

## 9. 变调 / 变速（Time-Stretch & Pitch-Shift；实现：`src/dsp/HseStretch.ts`）

### 9.1 四种经典方法（Signalsmith《Four Ways To Write A Pitch-Shifter》）
1. **颗粒合成（Granular）**：按重叠窗切出小颗粒，改变颗粒播放间距实现变速；
   变速的同时音高也变，需要再对颗粒做重采样补偿。音质一般，胜在简单。
2. **相位声码器（Phase Vocoder）**：STFT 域变速——分析帧相位差推瞬时频率，
   合成时按目标时间步进累积相位；变速不变调。变调则对频谱做频率映射（重采样频谱）。
   问题：瞬态拖尾、相位相干性差（"混浊感"）。
3. **Vocoder 式/混合（HybridPhase）**：在频带上做相位声码 + 额外相位约束。
4. **STFT 相位保持（Signalsmith Stretch 采用）**。

### 9.2 引擎实现
- 自研 TS 相位声码器（Hann N=2048 / hop=512，WOLA 精确窗和归一化，两段式 stretch+resample），
  复用共享 radix-4 FFT；参数变化时状态复位防伪影。变调附加 `voiceBalance` 经 M/S 通道生效。
- 可选运行时探测 npm 包 `signalsmith-stretch`（MIT，WASM）作为质量增强路径，
  缺失时自动回退自研实现——引擎包本体零硬依赖。
- **许可红线**：SoundTouch 系（LGPL-2.1）不进引擎包；宿主应用如需可在引擎包之外自行接入。

---

## 10. 音高检测（Pitch Detection；实现：`src/dsp/PitchYin.ts`）

### 10.1 YIN（经典自相关法，2002）
1. 差分函数 d(τ) = Σ_j (x[j] − x[j+τ])²；
2. 累积均值归一化差分（CMND）：d'(τ) = d(τ) / [(1/τ)·Σ_{j=1..τ} d(j)]，消除高阶谐波偏置；
3. 绝对阈值（默认 0.1–0.15）：找第一个低于阈值的谷点；
4. 抛物线插值求亚采样精度周期 τ* → f0 = Fs/τ*；
5. 可选：在上一帧 f0 ±20% 范围内搜索，抑制倍频/半频跳变（平滑）。
- 优点：实现 ~100 行、实时、无训练。缺点：低音/多音色时易倍频错误。

### 10.2 CREPE（深度学习，ICASSP 2018，MIT）
- 输入：1024 采样帧（46.4 ms @ 22.05 kHz，Hann 窗）；
- 网络：5 层卷积（stride 下采样）+ 3 层全连接 → 输出 360 维 softmax，
  覆盖 C1(32.7 Hz) 到 B7(1975 Hz)、20 cents/格；
- 训练技巧：数据增强（音高搬移/混响）+ harmonic stacking（把谐波能量叠到基频 bin）。
- 优点：鲁棒性显著优于 YIN（论文报告低音误差 −70% 量级）；缺点：需模型推理（可转 ONNX 跑 WASM）。

---

## 11. 声源分离（Source Separation：人声/伴奏；离线队列见 `src/offline/`）

### 11.1 SPLEETER（Deezer，MIT）
- 结构：U-Net（编码-解码 + 跳连）作用于 **STFT 幅度谱**，
  对每类音源（vocals / drums / bass / other / accompaniment）回归一个**软掩码（mask）**；
- 合成：掩码 × 原 STFT（保留原始相位）→ ISTFT → 波形；
- 模型：2 层 U-Net（stem 数 2/4/5），TensorFlow；预训练权重单独下载。

### 11.2 DEMUCS（Meta，MIT）
- htdemucs：**混合域**——主干是时域波形 U-Net（1-D 卷积编码器），
  另加一个 STFT 分支辅助；损失 = 时域 L1 + 频谱域损失；
- 48 kHz、transformer 增强、MUSDB 数据集训练；多尺度频谱损失抑制伪影。

### 11.3 工程注意
- 这类模型是**离线批处理**（秒级~分钟级），不适合实时；
- 浏览器方案：ONNX Runtime Web / Transformers.js 跑蒸馏模型，走离线任务队列 + WASM 推理。

---

## 12. 音频特征提取（实现：`src/dsp/features.ts`，自研）

常用特征（均可在 FFT 幅度谱/时域上直接算；meyda(MIT) 有同类 TS 实现可对照）：

| 特征 | 定义 | 用途 |
|---|---|---|
| RMS | √(mean(x²)) | 能量/响度粗估 |
| 过零率 ZCR | 符号变化次数/N | 噪声/清浊音判别 |
| 频谱质心 | Σ f·\|X\| / Σ \|X\| | 亮度/明暗感 |
| 频谱滚降 | 累积能量达 95% 的频率 | 高频能量边界 |
| 频谱平坦度 | 几何均值/算术均值 | 音调性（纯音≈0，噪声≈1） |
| 频谱斜率/衰减 | 幅度谱线性回归斜率 | 频谱形状 |
| MFCC | mel 滤波器组 + log + DCT | 音色/识别特征 |
| Chroma | 12 个音级能量 | 和声/和弦 |

> 听力分析演示、频谱可视化、EQ 目标曲线统计都可用这些特征。实时可用
> AnalyserNode 直接取频域数据（内部已是 FFT），无需自写 FFT。

---

## 13. 重采样（Sample Rate Conversion；实现：`src/dsp/Resampler.ts`）

- **多相 FIR（polyphase）**：把插值滤波器按输出相位分成多组子滤波器，
  每组对应输出样本与输入样本的固定相位偏移；每输出样本只需一个子滤波器卷积。
- **speexdsp 思路**（BSD-3）：窗口化 sinc 系数（quality 0–10 级），先做有理数分解
  （2^k·3·5…）逐级变换，每级用 128–256 抽头 sinc，截止频率按输入/输出 min 缩放。
- 对比：libsamplerate（BSD-2）的 sinc 模式质量更高但更贵；
  Web Audio 的 OfflineAudioContext 可做高质量离线转换；
  MediaStreamAudioDestinationNode 自动重采样（非精确可控）。

---

## 14. 频响补偿与 AutoEq 方法（MIT）

> 引擎现不引入真实设备档案；频响补偿以**按音量实施的通用曲线**承担
> （`src/dsp/LoudnessComp.ts` auto 模式，见 §6）。本节保留 AutoEq 通用算法流程作参考。

背景：用设备实测频响曲线生成一组 EQ 设置，让设备回放接近平直/目标曲线。

AutoEq 流程（github.com/jaakkopasanen/AutoEq 的 autoeq 包）：
1. **测量数据**：公开数据库（headphone.com、oratory1990 等）的频响 CSV（幅度 dB @ 频点）；
2. **平滑**：测量曲线有谐振毛刺，做分数倍频程平滑（如 1/6 oct）得到稳定曲线；
3. **目标曲线**：选目标（diffuse field、Harman target、或平直）；两者相减得"补偿目标"；
4. **参数 EQ 拟合**：给定滤波器数量上限与增益范围，用迭代最小二乘/坐标下降优化
   每段 (f, Q, gain)，使拟合曲线与补偿目标误差最小；输出可导出为
   graphic EQ（固定频点增益）或 parametric EQ（biquad 系数）；
5. **验证**：拟合后误差曲线可视化。

---

## 15. 实时实现要点（Web Audio / AudioWorklet）

1. **渲染量子**：AudioWorklet 每次 process 回调 128 帧（~2.7 ms @48 kHz），
   严禁分配内存/阻塞；所有缓冲经 `prepare(maxBlockSize)` 预分配，用环形缓冲/双缓冲。
2. **参数更新**：系数不要在 process 内实时计算（三角函数昂贵）——
   主线程/控制线程算好系数 → MessagePort 传值 → worklet 内对增益类参数按帧平滑
   （线性/指数滑变，防爆音）。
3. **重活下放**：卷积混响用预计算 FFT 频谱 + 分区调度；实时图保持轻量
   （biquad 链、包络、增益）。
4. **双路径一致**：实时路径（AudioWorklet + AudioNode 图）与离线路径
   （纯 TS 引擎逐块 process）共用同一套 `HyperSoundEngine.process`，保证实时/离线/导出一致。
5. **测试**：算法纯函数用确定性输入做单元测试（白噪声→EQ→对照参考实现幅度谱）；
   规格化模块以冻结向量对拍（`specs/`，容差 1e-6，跨实现逐位一致为目标）。
6. **特征/可视化**：AnalyserNode 免费提供 FFT 数据；`src/dsp/features.ts` 可在其数据上直接算特征。

---

## 16. 参考资料（全部公开）

1. RBJ Audio EQ Cookbook https://webaudio.github.io/Audio-EQ-Cookbook/
2. Signalsmith Stretch 设计 https://signalsmith-audio.co.uk/writing/2023/stretch-design/
3. Four Ways To Write A Pitch-Shifter（ADC'22）https://signalsmith-audio.co.uk/code/stretch/
4. ITU-R BS.1770-4 / EBU R128；FFmpeg f_ebur128.c（LGPL，仅参考公式）；Audacity EBUR128 类
5. 分区卷积 DAFX 论文（Wefers）https://www.dafx.de/paper-archive/2003/DAFX03_Paper_Wefers.pdf
6. ISO 226:2003 等响曲线 https://www.iso.org/standard/34222.html
7. Virtual Bass：IEEE "Synthesis of polynomial-based nonlinear device..."、
   Gerstle 硕士论文、"Virtual Bass Enhancement Based on Harmonics Control"
8. YIN：de Cheveigné & Kawahara (2002) "YIN, a fundamental frequency estimator"
9. CREPE：arXiv:1802.06182
10. Demucs：arXiv:2111.03600；Spleeter：arXiv:2005.01808
11. AutoEq：https://github.com/jaakkopasanen/AutoEq
12. kissfft（BSD-3）、DSPFilters（MIT）、stk FreeVerb（MIT）、speexdsp（BSD-3）、
    signalsmith-stretch（MIT）——各仓库 LICENSE 见其官方发布

---

## 17. "能用库 / 要自研" 速查（按本文档章节归纳）

| 章节 | 算法 | 可套用（MIT/BSD） | 必须自研 |
|---|---|---|---|
| §0 FFT | 蝶形 FFT | kissfft(BSD-3) 移植；浏览器 AnalyserNode 内置 | 引擎自研 radix-4 |
| §1 参数 EQ | biquad 系数/TDF2 | DSPFilters(MIT)、AutoEq peq.py(MIT) 移植 | 级联 Q 补偿迭代 |
| §2.1 卷积混响 | 分区 FFT 卷积 | FFT 内核 kissfft；Rust fft-convolver(MIT) 思路 | 非均匀分区调度、IR 去周期化 |
| §2.2 算法混响 | Schroeder/Freeverb/FDN | stk FreeVerb(MIT)、DaisySP reverbsc(MIT) 移植 | FDN 网络 |
| §3 压限 | 包络/压缩曲线/lookahead | DaisySP compressor/limiter(MIT)、signalsmith-basics limiter.h(MIT) 移植 | 真峰值过采样扩展 |
| §4 de-esser | 侧链带通+包络 | —（克隆库均无） | 全部 |
| §5 虚拟低频 | 谐波生成 + lowBoostDb | — | 全部 |
| §6 等响度 | ISO 226 补偿 | —（标准数据公开） | 全部 |
| §7 LUFS | BS.1770 K 加权 | —（FFmpeg LGPL/Audacity GPL 仅对照） | 全部（公式公开） |
| §8 M/S | 2×2 矩阵 | — | 全部（几行代码） |
| §9 变速变调 | 相位保持 STFT | signalsmith-stretch(MIT) 运行时探测（npm WASM） | 自研 TS 相位声码器（已落地） |
| §10 音高 | YIN / CREPE | crepe(MIT) 离线套用 | YIN 实时自研（已落地） |
| §11 分离 | U-Net 掩码 | spleeter/demucs(MIT) 离线/ONNX | 任务队列与 UI |
| §12 特征 | 频谱特征 | meyda(MIT) 可对照 | 自研 features.ts（已落地） |
| §13 重采样 | 多相 FIR | speexdsp(BSD-3)、libsamplerate(BSD-2) 移植 | — |
| §14 频响补偿 | 频响→参数EQ | AutoEq(MIT) 借鉴流程 + results/ 数据 | TS 拟合器 |
| §15 实时框架 | AudioWorklet | Tone.js(MIT) 借鉴架构 | worklet 处理器与消息管道 |

---

## 附录 A. 实时性能预算（@48 kHz，128 帧回调）

| 模块 | 每帧 MAC 估算 | 说明 |
|---|---|---|
| 20 段 biquad × 2ch | ~20×5×2×128 ≈ 25.6k | TDF2 每样本 5 MAC |
| 卷积混响（2 s IR, L=512, WASM FFT） | ~0.9M（分块摊还） | 主要开销，WASM 单线程可承受 |
| Deesser + Limiter + Bass | ~10k | 包络/增益 |
| 等响度/智能（主线程） | 0（低频更新） | 3 s 时间常数 |
| 合计 | ~1M MAC/帧 ≈ 480 MMAC/s | 现代 CPU 实时无压力；低端设备可切算法混响 + 短 IR |

**降级策略**：设备能力检测 → 高档（卷积+全链）/中档（算法混响+EQ）/低档（EQ+限幅器）。

## 附录 B. 测试与验证策略

1. **系数级单测**（vitest）：RBJ 公式对拍参考实现输出（同一 (f,Q,gain) 的系数误差 < 1e-9）；
   Q 补偿迭代收敛断言；分享串往返一致。
2. **频响断言**：白噪声 → 各模块 → FFT 实测幅频响应 vs 理论目标（≤0.1 dB 容差），
   覆盖全部控制点 + 边界频点（20 Hz / 20 kHz）。
3. **时域断言**：限幅器峰值 ≤ 阈值；de-esser 只在齿音频段触发（合成"s"信号）；
   卷积混响 IR 尾部平滑无周期伪影（自相关无尖峰）。
4. **实时/离线一致性**：同一参数、同一输入，worklet 输出与离线引擎输出逐样本误差 < 1e-6。
5. **响度断言**：合成 -23 LUFS 信号，LufsMeter 测量误差 ≤ 0.1 LU。
6. **冻结向量对拍**（`specs/`）：biquad/limiter/reverb-simple 规格化向量，
   TS/Rust 双支线对拍容差 1e-6（当前逐位一致），`scripts/export-vectors.mjs` 逐字节守卫。
7. **回归套件**：既有断言体系（400+ 用例），每功能合入带 3–5 项新断言。

## 附录 C. 许可证合规清单

- **引擎核心许可：CC-BY-NC-ND-4.0**（见 LICENSE）；引擎包（npm `hypersoundengine`）**零 LGPL 依赖**。
- 保留版权头：从 MIT/BSD 库（DSPFilters/kissfft/speexdsp/stk/DaisySP/signalsmith 等）移植任何代码，
  必须在文件头保留原声明。
- 直接套用的 npm 包：分发物附 THIRD_PARTY_NOTICES.md（如 signalsmith-stretch 可选运行时依赖）。
- 仅学习不引入：无许可证的示例代码；FFmpeg ebur128（LGPL，只参考公式与系数，不复制代码）。
- **LGPL（SoundTouch 系）**：引擎包一律不引入；宿主应用如需（如 @soundtouchjs/audio-worklet），
  在引擎包之外的宿主侧自行引入并满足 LGPL-2.1 义务。GPL/AGPL 类不引入。
- 模型权重：demucs/spleeter/crepe 权重按各仓库 LICENSE，随产品分发需保留声明。
- 依赖审计：npm 依赖入库前核对 SPDX 许可。
