//! hse_stretch —— 变速 / 变调（自研相位声码器 + 多相窗口化 sinc 重采样）。
//!
//! 行为事实标准：仓库根 `src/dsp/HseStretch.ts`；规格：`specs/dsp/hse-stretch.md`。
//! 本模块是向量驱动模型的**块窗映射形态**（§4.6）：内部 `processStereo` 非就地
//! 且输出长度 `outLen = (M−1)·Hs + N` 随参数变化；[`HseStretchStage`] 把变长输出
//! 按 §4.6 映射回定长 `(left, right)` 网格（取前 len 个样本，超出截断、不足补零）。
//!
//! # 移植纪律（specs/dsp/hse-stretch.md §4.7——算术调度等价、实践目标逐位一致）
//!
//! - **框架常量与采样率解耦**（§一）：分析窗长 `N = 2048`、分析 hop `HOP = 512`、
//!   Hann 窗 `w[i] = 0.5·(1 − cos(2πi/N))` 与 fs 无关；fs 仅进入构造校验与变调
//!   阶段 Resampler 速率比的 f64 构成（`(fs·ps)/fs` 不逐位等于 `ps`，行为参数）；
//! - **Hann 窗是 HseStretch.ts 模块内私有 `/n` 周期式**（HseStretch.ts L323–L327），
//!   与 fft.ts 的 `/(n−1)` 对称式**不是同一函数**——此处按周期式逐字复刻，
//!   `Math.cos` 用 [`ts_trig::cos`]（V8 fdlibm 逐位复刻）；
//! - **STFT 内核复用 [`crate::fft::Fft`]**（基-4 复 FFT，正变换不缩放 / 逆变换
//!   ÷N；N=2048 带基-2 尾路径，已被冻结向量覆盖）；
//! - **f32 落点逐字对齐 TS**（全部 Float32Array 存储点）：分析帧 `anaRe[i] =
//!   (x[j] 或 0)·w[i]`、合成相位 `synPhase`（f32 存储、推进后写回）、合成谱
//!   `synRe/synIm`、WOLA 累加 `out += w·synRe` 与 `sArr += w²`、逐样本归一化
//!   `out /= S`（S > 0.01 才除）——每个落点 f64 运算后 `as f32`；
//! - **相位声码器**（§4.3）：帧 0 `synPhase = atan2(im, re)`；帧间复相位差
//!   `Δφ = atan2(im·prevRe − re·prevIm, re·prevRe + im·prevIm)`（数值稳定形）、
//!   `dev = Δφ − HOP·wk` 折叠回 `(−π, π]`（`dev −= 2π·round(dev/2π)`，
//!   [`js_round`] = ties 向 +∞）、`winst = wk + dev/HOP`、`synPhase += Hs·winst`；
//! - **跨调用无状态**（§4.5）：帧 0 重置全部 `synPhase`、帧间覆盖 prev/ana/syn
//!   缓冲——任意预热后处理与全新实例逐位一致；`set_params` 的参数突变 reset
//!   在当前实现下对输出不可观测，仍按 TS 契约保留；
//! - **驱动器从不调用 `isSignalsmithAvailable()`**（§4.9）：本实现只有自研相位
//!   声码器路径，不存在替代后端；
//! - 变调阶段的 [`Resampler`] 是 `src/dsp/Resampler.ts` 一次性 `process()` 的
//!   逐行移植（多相窗口化 sinc，speexdsp 思路无第三方代码）；TS 在每次
//!   `_processChannel` 中新建实例——系数由构造确定性决定，本实现按 `set_params`
//!   缓存重建，数值等价（非实时控制路径）。
//!
//! # 与 TS 源码的逐行对应关系（HseStretch.ts 行号）
//!
//! - 构造（L63–L76）→ `HseStretchStage::from_params`（校验/缓冲分配同序）；
//! - setParams（L79–L95）→ `set_params`（clamp、`2^(s/12)`、突变检测 reset）；
//! - processStereo（L101–L109）→ [`Stage::process`] 的块窗映射包装
//!   （自研路径唯一，signalsmith 分支 L102–L105 按规格 §4.9 永不命中）；
//! - `_processChannel`（L150–L156）→ `process_channel`（伸缩 → 免重采样判定
//!   `|ps−1| < 1e-9` → Resampler）；
//! - `_vocoderStretch`（L162–L261）→ `vocoder_stretch`（帧数/Hs/outLen 公式、
//!   分析帧、相位累积、Hermitian 重构、WOLA + S(t) 阈值归一化逐行同序）；
//! - 模块内 hannWindow（L323–L327）→ [`hann_window_stretch`]；
//! - Resampler.ts（L24–L208）→ [`Resampler`]（构造钳制、buildTable、process）。

use crate::fft::{ts_trig, Fft};
use crate::Stage;

/// 分析窗长（HseStretch.ts L38）。
const N: usize = 2048;
/// 分析 hop（75% 重叠；HseStretch.ts L39）。
const HOP: usize = 512;
const TWO_PI: f64 = 2.0 * std::f64::consts::PI;

/// 复刻 JS `Math.max(a, b)` 的 NaN 传播语义（理由同 biquad.rs 的同名助手）。
fn js_max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a > b {
        a
    } else {
        b
    }
}

/// 复刻 JS `Math.min(a, b)` 的 NaN 传播语义（理由同 js_max）。
fn js_min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a < b {
        a
    } else {
        b
    }
}

/// 复刻 JS `Math.round` 的「ties 向 +∞」语义（mod_effects.rs 同款实现）。
///
/// 本文件的取值域：`HOP·factor > 0`、`nFrames·outRate/inRate ≥ 0`、
/// `dev/2π ∈ [−256.5, 0.5]`——均为有限值，`floor(x+0.5)` 与 V8 逐位一致。
fn js_round(x: f64) -> f64 {
    (x + 0.5).floor()
}

/// TS `clamp(v, lo, hi)` 的逐字复刻（HseStretch.ts L318–L320 三目链）。
fn clamp(v: f64, lo: f64, hi: f64) -> f64 {
    if v < lo {
        lo
    } else if v > hi {
        hi
    } else {
        v
    }
}

/// 参数快照（TS `HseStretchParams`）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HseStretchParams {
    /// 半音数（-36..36，超出 clamp）。
    pub semitones: f64,
    /// 时间伸缩速率（0.1..8，超出 clamp；1 = 原速）。
    pub rate: f64,
}

/// HseStretch.ts 模块内私有 Hann 窗（**周期式 /n**，L323–L327）。
///
/// 与 fft.ts 的对称式 `/(n−1)` hannWindow 不是同一函数——帧归一化 WOLA 的
/// 数值依赖这一形态，不得混用。`Math.cos` 用 [`ts_trig::cos`] 逐位复刻。
fn hann_window_stretch(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| {
            (0.5 * (1.0 - ts_trig::cos((TWO_PI * i as f64) / n as f64))) as f32
        })
        .collect()
}

/// 一阶修正 Bessel 函数 I0(x)（Resampler.ts L197–L208 逐行移植）：
/// 标准幂级数 Σ((x/2)^k / k!)²，`term < 1e-16·sum` 提前断（k ≤ 40）。
fn bessel_i0(x: f64) -> f64 {
    let x = if x < 0.0 { -x } else { x };
    let mut sum = 1.0;
    let mut term = 1.0;
    // TS：const x2 = (x / 2) * (x / 2)。
    let x2 = (x / 2.0) * (x / 2.0);
    for k in 1..=40_i64 {
        // TS：term *= x2 / (k * k)——先除后乘，结合序逐字固化。
        term *= x2 / (k * k) as f64;
        sum += term;
        if term < 1e-16 * sum {
            break;
        }
    }
    sum
}

/// 多相窗口化 sinc 重采样（Resampler.ts 的单声道一次性 `process()` 移植）。
///
/// 只移植 HseStretch 变调阶段用到的形态：`channels = 1`、一次性
/// [`Resampler::process`]（`processStreaming` / 环形缓冲不在使用域，不移植——
/// 与 TS 源的流式语义无关，HseStretch.ts L154 的调用即一次性形态）。
#[derive(Debug)]
pub struct Resampler {
    in_rate: f64,
    out_rate: f64,
    taps: usize,
    /// L = taps/2（内核半宽）。
    half: usize,
    /// 输入样本数 / 输出样本数（每个输出样本的输入相位步进）。
    ratio: f64,
    cutoff: f64,
    /// 相位数（固定 256）。
    ph: f64,
    /// 多相表：(PH+1) 行 × taps 列（f32 存储，Resampler.ts L32）。
    table: Vec<f32>,
}

impl Resampler {
    /// 构造（Resampler.ts L40–L56 逐行移植；channels 固定 1）。
    ///
    /// `inRate ≤ 0` 或非有限时报错（对齐 TS `Error('invalid sample rate')`）。
    /// `quality 0..10`：抽头数 `4 << (q>>1)`（8 → 64）与 Kaiser β。
    pub fn new(in_rate: f64, out_rate: f64, quality: f64) -> Result<Self, String> {
        if !in_rate.is_finite() || !out_rate.is_finite() || in_rate <= 0.0 || out_rate <= 0.0 {
            return Err("invalid sample rate".into());
        }
        // TS：const q = Math.min(10, Math.max(0, Math.floor(quality)))。
        let q = js_min(10.0, js_max(0.0, quality.floor()));
        // TS：this.taps = 4 << (q >> 1)——q 为 [0,10] 内整数，移位等价。
        let taps = 4usize << ((q as i64 >> 1) as u32);
        let half = taps / 2;
        let ratio = in_rate / out_rate;
        // 归一化截止（输入采样率单位）：降采样截止到输出 Nyquist。
        let cutoff = js_min(1.0, out_rate / in_rate);
        let ph = 256.0_f64;
        // TS：this.table = this.buildTable(6 + q * 0.35)。
        let beta = 6.0 + q * 0.35;
        let table = Self::build_table(beta, taps, half, cutoff, ph);
        Ok(Self { in_rate, out_rate, taps, half, ratio, cutoff, ph, table })
    }

    /// 多相表构建（Resampler.ts L59–L81 逐行移植）。
    ///
    /// 行 = 相位 p/PH（p = 0..PH，第 PH 行与 0 行同构用于环绕插值）；
    /// 内核 `h(u) = cutoff·sinc(cutoff·u)·kaiser(u/L)`，表达式结合序逐字固化
    /// （f64 乘法不可交换结合）；表值 f32 存储。
    fn build_table(beta: f64, taps: usize, half: usize, cutoff: f64, ph: f64) -> Vec<f32> {
        let rows = ph as usize + 1;
        let i0b = bessel_i0(beta);
        let l = half as f64;
        let mut t = vec![0.0_f32; rows * taps];
        for p in 0..rows {
            let f = p as f64 / ph;
            let base = p * taps;
            for k in 0..taps {
                // TS：const u = k - (L - 1) - f（内核中心在 k = L−1）。
                let u = k as f64 - (l - 1.0) - f;
                let mut h = 0.0_f64;
                if u > -l && u < l {
                    // TS：u === 0 ? 1 : Math.sin(Math.PI * cutoff * u) / (Math.PI * cutoff * u)
                    let s = if u == 0.0 {
                        1.0
                    } else {
                        ts_trig::sin(std::f64::consts::PI * cutoff * u)
                            / (std::f64::consts::PI * cutoff * u)
                    };
                    // TS：besselI0(beta * Math.sqrt(Math.max(0, 1 - (u/L)*(u/L)))) / i0b
                    let w = bessel_i0(beta * js_max(0.0, 1.0 - (u / l) * (u / l)).sqrt()) / i0b;
                    h = cutoff * s * w;
                }
                t[base + k] = h as f32;
            }
        }
        t
    }

    /// 一次性重采样（Resampler.ts L87–L122 逐行移植；单声道）。
    ///
    /// 输出长度 `round(N·outRate/inRate)`；头部越界按静音、尾部越界边界保持
    /// （clamp 到末样本）；两相邻相位线性插值。结果写入 `out`（整体替换）。
    pub fn process(&self, input: &[f32], out: &mut Vec<f32>) {
        let n_frames = input.len();
        // TS：const outFrames = Math.round(nFrames * (this.outRate / this.inRate))
        // ——注意是 outRate/inRate，不是 ratio（inRate/outRate）。
        let out_frames =
            js_round(n_frames as f64 * (self.out_rate / self.in_rate)) as usize;
        out.clear();
        out.resize(out_frames, 0.0);
        let l = self.half as isize;
        let last = n_frames as isize - 1;
        for m in 0..out_frames {
            let pos = m as f64 * self.ratio;
            let i_f = pos.floor();
            let f = pos - i_f;
            let ph_real = f * self.ph;
            let p0 = ph_real.floor();
            let fr = ph_real - p0;
            let row0 = p0 as usize * self.taps;
            // 第 PH 行与 0 行同构（rows = PH+1），p0+1 ≤ PH 恒成立。
            let row1 = row0 + self.taps;
            let i = i_f as isize;
            let mut acc = 0.0_f64;
            for k in 0..self.taps {
                // TS：const j = i + k - (L - 1)；头静音、尾边界保持。
                let j = i + k as isize - (l - 1);
                let xv = if j < 0 {
                    0.0
                } else if j > last {
                    f64::from(input[last as usize])
                } else {
                    f64::from(input[j as usize])
                };
                // TS：table[row0+k] + (table[row1+k] - table[row0+k]) * fr
                // ——f32 表值宽化 f64 后线性插值。
                let lo = f64::from(self.table[row0 + k]);
                let hi = f64::from(self.table[row1 + k]);
                let kk = lo + (hi - lo) * fr;
                acc += xv * kk;
            }
            out[m] = acc as f32;
        }
    }
}

/// hse-stretch Stage（块窗映射形态，specs/dsp/hse-stretch.md §4.6）。
///
/// [`Stage::process`] 每块 = `processStereo`（非就地，内部产出变长输出）→
/// 取输出的前 `len` 个样本截断回填、不足 `len` 的尾部补零（§4.6.2；
/// 驱动器不得把超出 len 的输出发酵进下一块——每块独立）。
pub struct HseStretchStage {
    fs: f64,
    channels: usize,
    rate: f64,
    semitones: f64,
    pitch_scale: f64,

    // ---- 预分配缓冲（对齐 TS 构造期分配；process 稳态零分配） ----
    win: Vec<f32>,
    ana_re: Vec<f32>,
    ana_im: Vec<f32>,
    prev_re: Vec<f32>,
    prev_im: Vec<f32>,
    syn_re: Vec<f32>,
    syn_im: Vec<f32>,
    /// N/2+1：合成相位累积（f32 存储，HseStretch.ts L58）。
    syn_phase: Vec<f32>,
    /// N 点 STFT 内核（N 固定 2048，与块长无关）。
    fft: Fft,
    /// 变调阶段重采样（`|ps − 1| ≥ 1e-9` 时存在；set_params 重建，数值等价
    /// TS 每调用新建实例）。
    resampler: Option<Resampler>,
    /// 块窗映射 scratch：单声道变长输出 / 重采样目标 / 窗平方和。
    out_buf: Vec<f32>,
    rs_buf: Vec<f32>,
    s_buf: Vec<f32>,

    // ---- 向量序列驱动（§4.6.3 参数突变 case） ----
    /// 载荷 `initialParams`（存在时构造即以它 set_params，对齐 TS 驱动器）。
    initial_params: Option<HseStretchParams>,
    /// 处理第 `switch_at_block` 块（0 起）之前以终参调用一次 set_params。
    switch_at_block: Option<usize>,
    /// 终参快照（切换目标）。
    pending_final: Option<HseStretchParams>,
    /// 已处理的块数（0 起；驱动器 blockIndex 语义）。
    block_index: usize,
}

impl HseStretchStage {
    /// 完整构造（含向量序列扩展字段，§4.6.3）。
    ///
    /// - `fs ≤ 0` 或非有限 → `Err("invalid sample rate")`；
    /// - `channels` 非正整数 → `Err("invalid channel count")`（对齐 TS
    ///   `Number.isInteger` 语义；向量固定 2）；
    /// - 载荷含 `initial_params` 时：构造即以初参 `set_params`，并把终参
    ///   （`params`）挂到 `switch_at_block` 之前切换——与 TS 驱动器逐字同序；
    ///   不含时单次 `set_params(params)`（标准路径）。
    pub fn from_params(
        sample_rate: f64,
        channels: f64,
        params: HseStretchParams,
        initial_params: Option<HseStretchParams>,
        switch_at_block: Option<usize>,
    ) -> Result<Self, String> {
        if !(sample_rate.is_finite() && sample_rate > 0.0) {
            return Err("invalid sample rate".into());
        }
        // TS：!Number.isInteger(channels) || channels < 1 → throw
        //（NaN/±Inf 的 fract 为 NaN ≠ 0，同样落入抛错域）。
        if !channels.is_finite() || channels.fract() != 0.0 || channels < 1.0 {
            return Err("invalid channel count".into());
        }
        let mut stage = Self {
            fs: sample_rate,
            channels: channels as usize,
            rate: 1.0,
            semitones: 0.0,
            pitch_scale: 1.0,
            win: hann_window_stretch(N),
            ana_re: vec![0.0; N],
            ana_im: vec![0.0; N],
            prev_re: vec![0.0; N],
            prev_im: vec![0.0; N],
            syn_re: vec![0.0; N],
            syn_im: vec![0.0; N],
            syn_phase: vec![0.0; N / 2 + 1],
            fft: Fft::new(N)?,
            resampler: None,
            out_buf: Vec::new(),
            rs_buf: Vec::new(),
            s_buf: Vec::new(),
            initial_params: None,
            switch_at_block: None,
            pending_final: None,
            block_index: 0,
        };
        match initial_params {
            Some(init) => {
                stage.set_params(init);
                stage.initial_params = Some(init);
                stage.switch_at_block = switch_at_block;
                stage.pending_final = Some(params);
            }
            None => stage.set_params(params),
        }
        Ok(stage)
    }

    /// 单次 setParams 的标准构造（等价 TS `new HseStretch(fs, ch)` +
    /// `setParams(p)`；无序列驱动需求时使用）。
    pub fn new(sample_rate: f64, channels: f64, params: HseStretchParams) -> Result<Self, String> {
        Self::from_params(sample_rate, channels, params, None, None)
    }

    /// TS `setParams`（L79–L95 逐行移植）：rate/semitones 钳制、
    /// `pitchScale = 2^(semitones/12)`；任一变化触发内部 reset（§4.5——
    /// 当前实现下不可观测，保留契约）。重采样器随 pitchScale 重建
    /// （系数确定性构造，与 TS 每调用新建逐位等价；非实时控制路径）。
    pub fn set_params(&mut self, p: HseStretchParams) {
        let r = clamp(p.rate, 0.1, 8.0);
        let s = clamp(p.semitones, -36.0, 36.0);
        let ps = 2.0_f64.powf(s / 12.0);
        if r != self.rate || s != self.semitones {
            self.rate = r;
            self.semitones = s;
            self.pitch_scale = ps;
            self.rebuild_resampler();
            self.reset();
        } else {
            self.rate = r;
            self.semitones = s;
            self.pitch_scale = ps;
            self.rebuild_resampler();
        }
    }

    /// TS `reset`（L112–L120）：清零相位累积 / 上一帧频谱 / 窗内缓冲。
    fn reset(&mut self) {
        self.prev_re.fill(0.0);
        self.prev_im.fill(0.0);
        self.syn_re.fill(0.0);
        self.syn_im.fill(0.0);
        self.syn_phase.fill(0.0);
        self.ana_re.fill(0.0);
        self.ana_im.fill(0.0);
    }

    fn rebuild_resampler(&mut self) {
        // TS：new Resampler(this.fs * pitchScale, this.fs, 1, 8)——inRate 的
        // f64 构成 `(fs·ps)` 逐字保持（§一：采样率是行为参数）。
        self.resampler = Some(
            Resampler::new(self.fs * self.pitch_scale, self.fs, 8.0)
                .expect("HseStretch 构造校验已保证 Resampler 速率合法"),
        );
    }

    /// 当前参数下、块长 `max_len` 的最大输出长度（prepare 预分配用；
    /// 公式与 [`Self::vocoder_stretch`] 一致，重采样只缩短不增长）。
    fn max_out_len(&self, max_len: usize) -> usize {
        if max_len == 0 {
            return N;
        }
        let factor = self.rate * self.pitch_scale;
        let hs = js_max(1.0, js_round(HOP as f64 * factor)) as usize;
        let full = if max_len >= N { (max_len - N) / HOP + 1 } else { 0 };
        let partial = usize::from(full * HOP < max_len);
        let m = (full + partial).max(1);
        (m - 1) * hs + N
    }

    /// TS `_processChannel`（L150–L156 逐行移植）：先按 `rate·pitchScale`
    /// 时间伸缩（不变调），`|pitchScale − 1| < 1e-9` 免重采样，否则按
    /// `1/pitchScale` 重采样变调；最后做块窗映射（§4.6.2 截断/补零回填）。
    fn process_channel(&mut self, x: &mut [f32]) {
        let rate = self.rate;
        let ps = self.pitch_scale;
        self.vocoder_stretch(x, rate * ps);
        if !((ps - 1.0).abs() < 1e-9) {
            if let Some(rs) = self.resampler.as_ref() {
                rs.process(&self.out_buf, &mut self.rs_buf);
            }
            std::mem::swap(&mut self.out_buf, &mut self.rs_buf);
        }
        // §4.6.2 块窗映射：取输出的前 len 个样本，不足补零（期望输出为
        // 零初始化网格）。TS 驱动器：outL.set(out.l.subarray(0, min(len))).
        let len = x.len();
        let keep = self.out_buf.len().min(len);
        x[..keep].copy_from_slice(&self.out_buf[..keep]);
        x[keep..].fill(0.0);
    }

    /// TS `_vocoderStretch`（L162–L261 逐行移植）：相位声码器时间伸缩。
    ///
    /// 结果写入 `self.out_buf`（整体替换，长度 = outLen）；每块独立
    /// （跨调用无状态：帧 0 重置 synPhase、帧间覆盖 prev/ana/syn）。
    fn vocoder_stretch(&mut self, x: &[f32], factor: f64) {
        let len = x.len();
        if len == 0 {
            self.out_buf.clear();
            return;
        }
        // TS L166–L168：full = len ≥ N ? floor((len−N)/HOP) + 1 : 0；
        // partial = full·HOP < len ? 1 : 0；M = max(1, full + partial)。
        let full = if len >= N { (len - N) / HOP + 1 } else { 0 };
        let partial = usize::from(full * HOP < len);
        let m_count = (full + partial).max(1);
        // TS L170：Hs = max(1, round(HOP · factor))。
        let hs = js_max(1.0, js_round(HOP as f64 * factor)) as usize;
        // TS L171：outLen = (M − 1)·Hs + N。
        let out_len = (m_count - 1) * hs + N;
        self.out_buf.clear();
        self.out_buf.resize(out_len, 0.0);
        self.s_buf.clear();
        self.s_buf.resize(out_len, 0.0);
        let half = N / 2;

        for m in 0..m_count {
            let start = m * HOP;
            // 分析帧（尾部越界补零）：anaRe[i] = (j < len ? x[j] : 0)·win[i]。
            for i in 0..N {
                let j = start + i;
                let xv = if j < len { f64::from(x[j]) } else { 0.0 };
                self.ana_re[i] = (xv * f64::from(self.win[i])) as f32;
                self.ana_im[i] = 0.0;
            }
            // 正变换不缩放（N=2048：log2=11 奇数，走基-2 尾路径）。
            let _ = self.fft.transform(&mut self.ana_re, &mut self.ana_im, false);

            // 合成相位：帧 0 用分析相位初始化；之后按瞬时频率累积（f32 存储）。
            for k in 0..=half {
                let re = f64::from(self.ana_re[k]);
                let im = f64::from(self.ana_im[k]);
                if m == 0 {
                    // TS：synPhase[k] = Math.atan2(im, re)——atan2(y=im, x=re)。
                    self.syn_phase[k] = im.atan2(re) as f32;
                } else {
                    // TS L209–L212：Δφ = ∠(X_m · conj(X_{m−1}))（数值稳定形）。
                    let dphi = (im * f64::from(self.prev_re[k])
                        - re * f64::from(self.prev_im[k]))
                    .atan2(re * f64::from(self.prev_re[k]) + im * f64::from(self.prev_im[k]));
                    // TS L213：wk = (TWO_PI * k) / N。
                    let wk = (TWO_PI * k as f64) / N as f64;
                    // TS L215–L216：dev = dphi − HOP·wk；折叠回 (−π, π]。
                    let mut dev = dphi - HOP as f64 * wk;
                    dev -= TWO_PI * js_round(dev / TWO_PI);
                    // TS L217–L218：winst = wk + dev/HOP；synPhase += Hs·winst。
                    let winst = wk + dev / HOP as f64;
                    self.syn_phase[k] = (f64::from(self.syn_phase[k]) + hs as f64 * winst) as f32;
                }
            }

            // Hermitian 合成频谱（DC/Nyquist 强制实值，按 cos(ph) 符号）。
            for k in 0..=half {
                let re = f64::from(self.ana_re[k]);
                let im = f64::from(self.ana_im[k]);
                // TS：mag = Math.sqrt(anaRe²+anaIm²)（f64）。
                let mag = (re * re + im * im).sqrt();
                let ph = f64::from(self.syn_phase[k]);
                if k == 0 || k == half {
                    self.syn_re[k] = if ts_trig::cos(ph) >= 0.0 { mag as f32 } else { -(mag) as f32 };
                    self.syn_im[k] = 0.0;
                } else {
                    self.syn_re[k] = (mag * ts_trig::cos(ph)) as f32;
                    self.syn_im[k] = (mag * ts_trig::sin(ph)) as f32;
                }
            }
            // 镜像共轭（TS L234–L237；−0 语义与 JS 一致）。
            for k in 1..half {
                self.syn_re[N - k] = self.syn_re[k];
                self.syn_im[N - k] = -self.syn_im[k];
            }

            // 逆变换（÷N）+ 合成窗 OLA（同步累加窗平方和 sArr）。
            let _ = self.fft.transform(&mut self.syn_re, &mut self.syn_im, true);
            let base = m * hs;
            for i in 0..N {
                let o = base + i;
                // TS：out[o] += win[i] * synRe[i]；sArr[o] += win[i]²。
                self.out_buf[o] = (f64::from(self.out_buf[o])
                    + f64::from(self.win[i]) * f64::from(self.syn_re[i])) as f32;
                self.s_buf[o] = (f64::from(self.s_buf[o])
                    + f64::from(self.win[i]) * f64::from(self.win[i])) as f32;
            }

            // 保存当前分析帧供下一帧差分（f32 拷贝）。
            self.prev_re.copy_from_slice(&self.ana_re);
            self.prev_im.copy_from_slice(&self.ana_im);
        }

        // 逐样本除以 S(t)（阈值 0.01：窗边缘 w² < 0.01 区域不重构输入，
        // 输出为淡入/淡出的小值——§4.4 保留区）。
        for i in 0..out_len {
            let s = f64::from(self.s_buf[i]);
            if s > 0.01 {
                self.out_buf[i] = (f64::from(self.out_buf[i]) / s) as f32;
            }
        }
    }

    /// 单声道核公开封装（单元测试 / 诊断用）：返回变长输出拷贝。
    #[cfg(test)]
    fn process_mono_into(&mut self, x: &[f32]) -> Vec<f32> {
        let rate = self.rate;
        let ps = self.pitch_scale;
        self.vocoder_stretch(x, rate * ps);
        if !((ps - 1.0).abs() < 1e-9) {
            if let Some(rs) = self.resampler.as_ref() {
                rs.process(&self.out_buf, &mut self.rs_buf);
            }
            std::mem::swap(&mut self.out_buf, &mut self.rs_buf);
        }
        self.out_buf.clone()
    }
}

impl std::fmt::Debug for HseStretchStage {
    /// 手工实现（[`Fft`] 无 Debug）：只打印参数与缓冲几何，不展开缓冲内容。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HseStretchStage")
            .field("fs", &self.fs)
            .field("channels", &self.channels)
            .field("rate", &self.rate)
            .field("semitones", &self.semitones)
            .field("pitch_scale", &self.pitch_scale)
            .field("block_index", &self.block_index)
            .finish_non_exhaustive()
    }
}

impl Stage for HseStretchStage {
    /// 按 max_block_size 与当前参数预分配块窗映射 scratch（§4.6：变长输出
    /// 不入驱动网格）。参数切换后 outLen 可能增长——`process` 里的
    /// `clear + resize` 按需增长兜底（控制路径一次性行为，稳态零分配）。
    fn prepare(&mut self, max_block_size: usize) {
        let need = self.max_out_len(max_block_size);
        if self.out_buf.len() < need {
            self.out_buf.resize(need, 0.0);
        }
        if self.rs_buf.len() < need {
            self.rs_buf.resize(need, 0.0);
        }
        if self.s_buf.len() < need {
            self.s_buf.resize(need, 0.0);
        }
    }

    /// 每块（§4.6.2）：序列驱动（§4.6.3）→ 左右声道各自独立全流程 →
    /// 块窗映射截断/补零回填定长网格。左右声道无声道耦合（§4.1）。
    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        // TS 驱动器（export-vectors.mjs hse-stretch 分支）：
        // if (sequenced && blockIndex === switchAtBlock) setParams(final)；
        // blockIndex++（比较发生在处理该块之前）。
        if let (Some(switch_at), Some(final_params)) = (self.switch_at_block, self.pending_final)
        {
            if self.block_index == switch_at {
                self.set_params(final_params);
                self.switch_at_block = None;
                self.pending_final = None;
            }
        }
        self.block_index += 1;
        self.process_channel(left);
        self.process_channel(right);
    }

    /// TS `reset`（清零相位累积 / 上一帧频谱 / 窗内缓冲）；块计数属驱动器
    /// 簿记，不随 DSP reset 回退。
    fn reset(&mut self) {
        self.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试激励：与导出工具同族的固定种子 LCG 伪噪声（禁随机源）。
    fn lcg_noise(n: usize, seed: u32, amp: f64) -> Vec<f32> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((f64::from(s) / 4294967296.0) * 2.0 - 1.0) * amp as f64
            })
            .map(|v| v as f32)
            .collect()
    }

    fn sine_sum(n: usize, fs: f64, comps: &[(f64, f64, f64)]) -> Vec<f32> {
        (0..n)
            .map(|i| {
                let mut acc = 0.0_f64;
                for (freq, amp, phase) in comps {
                    acc += amp * ts_trig::sin((TWO_PI * freq * i as f64) / fs + phase);
                }
                acc as f32
            })
            .collect()
    }

    fn f32_bits(x: f32) -> u32 {
        x.to_bits()
    }

    /// 黄金参考：node 直跑 src/dsp/HseStretch.ts（esbuild bundle，与导出工具
    /// 同一加载策略）冻结的 f32 位型与长度——位级对拍锚点。
    mod golden {
        // G1：fs 48000、{semitones 0, rate 1}、输入 lcg(2048, 99002, 0.5) /
        // lcg(2048, 31337, 0.5)——outLen 2560，输出样本位型。
        pub const G1_OUTLEN: usize = 2560;
        pub const G1_L: [(usize, u32); 8] = [
            (0, 0x0000_0000),
            (1, 0xac12_62ff),
            (2, 0xad96_ec0e),
            (3, 0xae87_d6cd),
            (1024, 0x3ede_42d2),
            (1025, 0xbeb7_be5e),
            (2047, 0x3ef3_552d),
            (2559, 0xab4a_5197),
        ];
        pub const G1_R: [(usize, u32); 8] = [
            (0, 0x0000_0000),
            (1, 0x2b7d_0153),
            (2, 0x2c0a_a311),
            (3, 0x2e93_f745),
            (1024, 0xbea3_d3f2),
            (1025, 0xbee8_435a),
            (2047, 0x3c98_21fa),
            (2559, 0xac1b_91bd),
        ];
        // G2：{0, 2}、输入 lcg(3000, 99002, 0.5) / lcg(3000, 31337, 0.5)——
        // outLen 4096。
        pub const G2_OUTLEN: usize = 4096;
        pub const G2_L: [(usize, u32); 6] = [
            (0, 0x0000_0000),
            (1, 0xac12_62ff),
            (2, 0xad96_ec0e),
            (2999, 0x3c9d_addf),
            (3000, 0x3e7d_03ec),
            (4095, 0x34e0_1817),
        ];
        pub const G2_R: [(usize, u32); 6] = [
            (0, 0x0000_0000),
            (1, 0x2b7d_0153),
            (2, 0x2c0a_a311),
            (2999, 0xbda8_9053),
            (3000, 0x3e36_9f06),
            (4095, 0x326a_62c0),
        ];
        // G3：{5, 1}、440Hz 正弦 len 2048（R 相位 π/4 去相关）——outLen 2046。
        pub const G3_OUTLEN: usize = 2046;
        pub const G3_L: [(usize, u32); 6] = [
            (0, 0xaca2_297a),
            (1, 0xadaa_295f),
            (2, 0x2c7f_d6b8),
            (500, 0x3ef2_110e),
            (1000, 0x3f2f_6942),
            (2045, 0x33b2_0576),
        ];
        pub const G3_R: [(usize, u32); 6] = [
            (0, 0xac19_6c7a),
            (1, 0x2b86_56fc),
            (2, 0x2f23_647f),
            (500, 0x3f33_072b),
            (1000, 0x3f06_b597),
            (2045, 0xb3c6_456e),
        ];
    }

    fn check_bits(out: &[f32], table: &[(usize, u32)], label: &str) {
        for (idx, want) in table {
            assert_eq!(
                f32_bits(out[*idx]),
                *want,
                "{label}[{idx}]：got {:08x} want {want:08x}",
                f32_bits(out[*idx])
            );
        }
    }

    #[test]
    fn rate1_恒速重构_命中_ts_黄金位型() {
        // GWT-HS-01：rate=1/semitones=0 为 STFT 重构（非直通、非近似恒等）；
        // 中腹重构区相位累积噪声级、两端保留区小值——位型逐一对齐 TS。
        let mut stage = HseStretchStage::new(
            48000.0,
            2.0,
            HseStretchParams { semitones: 0.0, rate: 1.0 },
        )
        .unwrap();
        let left = lcg_noise(2048, 99002, 0.5);
        let right = lcg_noise(2048, 31337, 0.5);
        let out_l = stage.process_mono_into(&left);
        let out_r = stage.process_mono_into(&right);
        assert_eq!(out_l.len(), golden::G1_OUTLEN);
        assert_eq!(out_r.len(), golden::G1_OUTLEN);
        check_bits(&out_l, &golden::G1_L, "G1 L");
        check_bits(&out_r, &golden::G1_R, "G1 R");
        // 输出既非逐位直通也非 1e-6 内恒等（规格 §4.4/§4.8.1 行为事实）：
        // 重构区与输入存在 ~1e-5 量级相对偏差、[0] 为保留区 +0。
        let mut identical = 0_usize;
        for i in 512..1536 {
            if out_l[i].to_bits() == left[i].to_bits() {
                identical += 1;
            }
        }
        assert!(identical < 1024, "重构区不应逐位直通（identical={identical}）");
        assert_eq!(f32_bits(out_l[0]), 0x0000_0000, "窗边缘保留区首样本为 +0");
        // 输入不被改写（非就地）。
        assert_eq!(left[512].to_bits(), lcg_noise(2048, 99002, 0.5)[512].to_bits());
    }

    #[test]
    fn rate2_时间伸缩_命中_ts_黄金位型() {
        // GWT-HS-02：rate=2（G1 同族输入、len 3000 → outLen 4096）。
        let mut stage = HseStretchStage::new(
            48000.0,
            2.0,
            HseStretchParams { semitones: 0.0, rate: 2.0 },
        )
        .unwrap();
        let left = lcg_noise(3000, 99002, 0.5);
        let right = lcg_noise(3000, 31337, 0.5);
        let out_l = stage.process_mono_into(&left);
        let out_r = stage.process_mono_into(&right);
        assert_eq!(out_l.len(), golden::G2_OUTLEN);
        assert_eq!(out_r.len(), golden::G2_OUTLEN);
        check_bits(&out_l, &golden::G2_L, "G2 L");
        check_bits(&out_r, &golden::G2_R, "G2 R");
    }

    #[test]
    fn semitones5_变调_命中_ts_黄金位型() {
        // GWT-HS-03：两阶段（伸缩 ×2^(5/12) + Resampler 1/pitchScale）；
        // 440Hz 正弦、outLen 2046（< len → 块窗映射补零形态）。
        let mut stage = HseStretchStage::new(
            48000.0,
            2.0,
            HseStretchParams { semitones: 5.0, rate: 1.0 },
        )
        .unwrap();
        let left = sine_sum(2048, 48000.0, &[(440.0, 0.7, 0.0)]);
        let right = sine_sum(2048, 48000.0, &[(440.0, 0.7, std::f64::consts::PI / 4.0)]);
        let out_l = stage.process_mono_into(&left);
        let out_r = stage.process_mono_into(&right);
        assert_eq!(out_l.len(), golden::G3_OUTLEN);
        assert_eq!(out_r.len(), golden::G3_OUTLEN);
        check_bits(&out_l, &golden::G3_L, "G3 L");
        check_bits(&out_r, &golden::G3_R, "G3 R");
    }

    #[test]
    fn 输出长度表_命中规格_4_2_七行() {
        // GWT-HS-07：实测长度表（specs/dsp/hse-stretch.md §4.2，激励形态无关）。
        let table = [
            (6400_usize, 0.0_f64, 1.0_f64, 6656_usize),
            (3000, 0.0, 2.0, 4096),
            (2048, 0.0, 2.0, 3072),
            (2048, 5.0, 1.0, 2046),
            (1904, 5.0, 1.0, 1534),
            (2048, 3.0, 1.5, 2490),
            (256, 3.0, 1.5, 1722),
        ];
        for (len, semitones, rate, want) in table {
            let mut stage = HseStretchStage::new(
                48000.0,
                2.0,
                HseStretchParams { semitones, rate },
            )
            .unwrap();
            let out = stage.process_mono_into(&vec![0.0_f32; len]);
            assert_eq!(out.len(), want, "len={len} s={semitones} r={rate}");
        }
    }

    #[test]
    fn 参数切换_等于_全新终参实例_逐位一致() {
        // GWT-HS-04/§4.5：双 setParams 合法序列——切换前各块 = 全新初参实例
        // 逐块输出；切换后各块 = 全新终参实例逐块输出（逐位）。
        let initial = HseStretchParams { semitones: 0.0, rate: 1.0 };
        let final_p = HseStretchParams { semitones: 3.0, rate: 1.5 };
        let mut seq = HseStretchStage::from_params(48000.0, 2.0, final_p, Some(initial), Some(2))
            .unwrap();
        let mut fresh_initial =
            HseStretchStage::new(48000.0, 2.0, initial).unwrap();
        let mut fresh_final = HseStretchStage::new(48000.0, 2.0, final_p).unwrap();

        let left = lcg_noise(4 * 2048, 99004, 0.5);
        let right = lcg_noise(4 * 2048, 31339, 0.5);
        for b in 0..4 {
            let (l0, r0) = (b * 2048, b * 2048 + 2048);
            let mut seq_l = left[l0..r0].to_vec();
            let mut seq_r = right[l0..r0].to_vec();
            seq.prepare(2048);
            seq.process(&mut seq_l, &mut seq_r);
            // 期望：块 0/1 用全新初参实例，块 2/3 用全新终参实例。
            let reference = if b < 2 { &mut fresh_initial } else { &mut fresh_final };
            let want_l = reference.process_mono_into(&left[l0..r0]);
            let want_r = reference.process_mono_into(&right[l0..r0]);
            let keep_l = want_l.len().min(2048);
            let mut exp_l = vec![0.0_f32; 2048];
            let mut exp_r = vec![0.0_f32; 2048];
            exp_l[..keep_l].copy_from_slice(&want_l[..keep_l]);
            exp_r[..keep_l].copy_from_slice(&want_r[..keep_l]);
            assert_eq!(seq_l, exp_l, "参数切换序列 块 {b} 左");
            assert_eq!(seq_r, exp_r, "参数切换序列 块 {b} 右");
        }
    }

    #[test]
    fn 跨调用无状态_预热后处理等于全新实例() {
        // GWT-HS-06：同实例先处理任意内容（含显式 reset 与参数切换）再处理
        // 目标内容，与全新实例直接处理逐位一致。
        let target_l = lcg_noise(2048, 424242, 0.4);
        let target_r = lcg_noise(2048, 424243, 0.4);
        let mut fresh = HseStretchStage::new(
            48000.0,
            2.0,
            HseStretchParams { semitones: 0.0, rate: 1.5 },
        )
        .unwrap();
        let want_l = fresh.process_mono_into(&target_l);
        let want_r = fresh.process_mono_into(&target_r);

        let mut warm = HseStretchStage::new(
            48000.0,
            2.0,
            HseStretchParams { semitones: 0.0, rate: 1.0 },
        )
        .unwrap();
        let _ = warm.process_mono_into(&lcg_noise(1024, 1, 0.3));
        warm.reset();
        warm.set_params(HseStretchParams { semitones: 0.0, rate: 1.5 });
        let got_l = warm.process_mono_into(&target_l);
        let got_r = warm.process_mono_into(&target_r);
        assert_eq!(got_l.len(), want_l.len());
        assert_eq!(got_r.len(), want_r.len());
        assert_eq!(got_l, want_l, "左声道逐位一致");
        assert_eq!(got_r, want_r, "右声道逐位一致");
    }

    #[test]
    fn 输出依赖_blockSize_分析帧集随块长变化() {
        // GWT-HS-05：STFT 分析帧集随块长变化——rate=2 下整块 vs 分块输出
        // 必须可区分（规格 §4.8.3 实证差异显著）。
        let frames = 6000_usize;
        let left = lcg_noise(frames, 99002, 0.5);
        let right = lcg_noise(frames, 31337, 0.5);
        let drive = |block: usize| {
            let mut stage = HseStretchStage::new(
                48000.0,
                2.0,
                HseStretchParams { semitones: 0.0, rate: 2.0 },
            )
            .unwrap();
            stage.prepare(block);
            let mut out_l = vec![0.0_f32; frames];
            let mut out_r = vec![0.0_f32; frames];
            let mut off = 0;
            while off < frames {
                let end = (off + block).min(frames);
                out_l[off..end].copy_from_slice(&left[off..end]);
                out_r[off..end].copy_from_slice(&right[off..end]);
                stage.process(&mut out_l[off..end], &mut out_r[off..end]);
                off = end;
            }
            (out_l, out_r)
        };
        let whole = drive(frames);
        let chunked = drive(3000);
        assert_ne!(whole, chunked, "不同 blockSize 输出必须可区分");
    }

    #[test]
    fn 抛错路径_对齐_ts_错误信息() {
        // GWT-HS-09：fs ≤ 0 或非有限 → 'invalid sample rate'；channels 非正
        // 整数 → 'invalid channel count'。
        for fs in [0.0_f64, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let err = HseStretchStage::new(
                fs,
                2.0,
                HseStretchParams { semitones: 0.0, rate: 1.0 },
            )
            .unwrap_err();
            assert_eq!(err, "invalid sample rate");
        }
        for ch in [0.0_f64, -1.0, 1.5, f64::NAN, f64::INFINITY] {
            let err = HseStretchStage::new(
                48000.0,
                ch,
                HseStretchParams { semitones: 0.0, rate: 1.0 },
            )
            .unwrap_err();
            assert_eq!(err, "invalid channel count");
        }
        // Resampler 速率校验（HseStretch 构造已挡住非法 fs，直测防御路径）。
        assert_eq!(
            Resampler::new(0.0, 48000.0, 8.0).unwrap_err(),
            "invalid sample rate"
        );
    }

    #[test]
    fn 钳制极值_无数值事故() {
        // GWT-HS-08：rate/semitones 双向越界 clamp（0.1/8/±36）后常规激励
        // 全程有限、有界不发散。
        for (semitones, rate) in [
            (100.0_f64, 100.0_f64),
            (-100.0, -5.0),
            (36.0, 8.0),
            (-36.0, 0.1),
        ] {
            let mut stage = HseStretchStage::new(
                48000.0,
                2.0,
                HseStretchParams { semitones, rate },
            )
            .unwrap();
            let out = stage.process_mono_into(&lcg_noise(2048, 777, 0.5));
            assert!(out.iter().all(|v| v.is_finite()), "s={semitones} r={rate} 发散");
        }
    }

    #[test]
    fn resampler_构造钳制与表几何_对齐_ts() {
        // quality 8：taps = 4 << 4 = 64、L = 32、表 257×64；quality 越界钳制。
        let rs = Resampler::new(48000.0 * 1.334_839_854_170_034_4, 48000.0, 8.0).unwrap();
        assert_eq!(rs.taps, 64);
        assert_eq!(rs.half, 32);
        assert_eq!(rs.table.len(), 257 * 64);
        let rs_q = Resampler::new(96000.0, 48000.0, 99.0).unwrap();
        assert_eq!(rs_q.taps, 128, "quality 99 → 钳到 10 → 4 << 5");
        let rs_q0 = Resampler::new(96000.0, 48000.0, -3.0).unwrap();
        assert_eq!(rs_q0.taps, 4, "quality −3 → 钳到 0 → 4 << 0");
        // 单位速率（cutoff = 1）：f = 0 处内核 sinc 整数采样恒 0 → 直流样本
        // 近恒等（±表插值误差级）。
        let rs_id = Resampler::new(48000.0, 48000.0, 8.0).unwrap();
        let input: Vec<f32> = (0..64).map(|i| (i as f32 * 0.1)) .collect();
        let mut out = Vec::new();
        rs_id.process(&input, &mut out);
        assert_eq!(out.len(), 64);
        assert!((f64::from(out[10]) - f64::from(input[10])).abs() < 1e-5);
    }
}
