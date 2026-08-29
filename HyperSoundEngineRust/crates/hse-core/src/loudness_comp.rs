//! loudness_comp —— 等响度补偿阶段（ISO 226 简化近似 + biquad 拟合）。
//!
//! 行为事实标准：仓库根 `src/dsp/LoudnessComp.ts`；规格：`specs/dsp/loudness-comp.md`。
//! 本文件是该 TS 类的逐行移植，关键对应关系：
//!
//! - 目标曲线（规格 §4.1，`setParams` 时执行一次）：auto=音量线性等响度
//!   `maxBoostDb × (1 − v) × w(f)`（1/3 倍频程 31 点表）；preset=六条固定场景曲线的
//!   对数线性插值；custom=low/high 组钳制均值 shelf + mid 组 peaking。拟合为至多
//!   6 槽（2 shelf + 4 peaking），peaking 从候选频点按 |增益| 降序（同值频率升序）
//!   取前 4 再按频率升序排列（TS `Array.sort` 稳定语义 → Rust `sort_by` 稳定排序）。
//! - **本模块没有 `enabled` 字段**（规格 §一行为事实）——`enabled` 属引擎层
//!   `LoudnessCompSettings`，由引擎阶段门控消费；模块自身恒处理。
//! - biquad 拟合（规格 §4.2）：RBJ shelf（S=1，α = sin(w0)/2·√2）/ peaking（Q=1.0），
//!   设计频率钳制 [1, fs×0.45]。**注意 TS LoudnessComp 自带 designShelf/designPeaking
//!   （钳制域与增益处理均与共享 designBiquad 不同），此处按其本地实现逐行复刻，
//!   不复用 [`crate::biquad::design_biquad`]**。
//! - 逐块增益平滑（规格 §4.3，模块特有）：每次 `processStereo` 开头按本块帧数 B
//!   计算 `alpha = 1 − exp(−B / (smoothingSeconds × fs))`，对 currentGains ≠
//!   targetGains 的槽做一阶逼近（|g − target| < 1e−9 钉扎）并重算该槽系数——
//!   **输出因此依赖 blockSize**（爬升型向量必须按向量固定 blockSize 回放，
//!   GWT-LC-07）；恒等型（目标全 0）时无重算、与块长无关。
//! - 立体声（规格 §4.4）：6 段 TDF2 级联，左右声道各自独立状态、系数共享；
//!   每样本 L 链与 R 链交错推进；**级联中间量全程 f64（TS `biquadStep` 返回
//!   Number），仅最终写回 `l[i]`/`r[i]` 量化 f32**——与 eq-chain 的段间 f32 落点
//!   不同，勿混淆。
//! - reset 语义（规格 §4.5.5）：currentGains 直接钉到 targetGains 并立即重算全部
//!   6 槽系数（**含目标为 0 的槽**——按 targetFreqs=0 钳到 1Hz 设计出的系数并非
//!   恒等，这是 TS 行为事实），TDF2 状态清零；reset 后重放与首次从零爬升不同。
//!
//! # 数值精度铁律的落点
//!
//! - 一切以 TS Number（f64）参与运算的中间量——目标表、currentGains/targetGains、
//!   alpha、全部 biquad 系数与 TDF2 状态——全部用 f64 复刻，运算顺序逐行一致；
//! - LoudnessComp 的 `clamp` 对 NaN/Infinity 返回下限（TS `Number.isFinite` 防御），
//!   以 [`clamp_finite`] 复刻；design 函数内的 `Math.min/Math.max` 以
//!   [`js_min`]/[`js_max`] 显式复刻 NaN 传播语义（理由同 biquad.rs）。
//!
//! # 实时安全
//!
//! 6 槽系数/状态/目标均为构造期定容数组；`process` 稳态零分配、零锁、零系统调用
//! （目标计算与系数设计只发生在 `from_settings`/`configure`/`reset` 非实时路径）。

use crate::biquad::BiquadCoeffs;
use crate::Stage;

/// 1/3 倍频程中心频率（31 段，20Hz–20kHz；与 TS `THIRD_OCTAVE_FREQS` 逐项一致）。
const THIRD_OCTAVE_FREQS: [f64; 31] = [
    20.0, 25.0, 31.5, 40.0, 50.0, 63.0, 80.0, 100.0, 125.0, 160.0, 200.0, 250.0, 315.0, 400.0,
    500.0, 630.0, 800.0, 1000.0, 1250.0, 1600.0, 2000.0, 2500.0, 3150.0, 4000.0, 5000.0, 6300.0,
    8000.0, 10000.0, 12500.0, 16000.0, 20000.0,
];

/// 中频 peaking 候选频点（auto/preset 拟合用；均在 1/3 倍频程表内）。
const PEAKING_CANDIDATES: [f64; 7] = [315.0, 630.0, 1000.0, 1600.0, 2500.0, 4000.0, 6300.0];

/// 最大段数：2 shelf + 4 peaking（TS `MAX_BANDS`）。
const MAX_BANDS: usize = 6;

/// 目标段类型（对齐 TS `targetTypes` 编码）。
const TYPE_LOW_SHELF: i32 = 0;
const TYPE_HIGH_SHELF: i32 = 1;
const TYPE_PEAKING: i32 = 2;

/// 对齐 TS `LoudnessCompParams` 的参数快照（字段名蛇形转换）。
#[derive(Debug, Clone)]
pub struct LoudnessCompSettings {
    pub volume_percent: f64,
    pub max_boost_db: f64,
    /// preset 模式预设 id：flat / bass / vocal / warm / bright / night；
    /// 未知 id 回退 flat 曲线（空表 → 全 0 目标）。
    pub preset: String,
    /// custom 模式目标曲线控制点。
    pub bands: Vec<LoudnessBandParam>,
    /// 'auto' | 'preset' | 'custom'；枚举外值回退 'auto'。
    pub mode: String,
    pub smoothing_seconds: f64,
}

/// 对齐 TS `LoudnessCompParams.bands` 元素 `{ frequency, gain }`。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LoudnessBandParam {
    pub frequency: f64,
    pub gain: f64,
}

/// 复刻 JS Math.min(a, b) 的 NaN 传播语义（理由见 biquad.rs 的同名函数）。
fn js_min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a < b {
        a
    } else {
        b
    }
}

/// 复刻 JS Math.max(a, b) 的 NaN 传播语义（理由同上）。
fn js_max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a > b {
        a
    } else {
        b
    }
}

/// TS LoudnessComp 的 `clamp(v, lo, hi)` 逐字复刻：拒绝 NaN/Infinity——
/// 非法参数回落下限（否则平滑系数/滤波器系数 NaN → 全链 NaN）。
fn clamp_finite(v: f64, lo: f64, hi: f64) -> f64 {
    if !v.is_finite() {
        return lo;
    }
    if v < lo {
        lo
    } else if v > hi {
        hi
    } else {
        v
    }
}

/// TS `average(arr)`：算术平均（空表 → 0）。
fn average(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut s = 0.0;
    for &v in values {
        s += v;
    }
    s / values.len() as f64
}

/// auto 模式 1/3 倍频程权重 w(f)（ISO 226 简化近似；对齐 TS `autoWeight`）。
fn auto_weight(f: f64) -> f64 {
    if f <= 100.0 {
        return 1.0;
    }
    if f < 250.0 {
        let t = (f.log10() - 100.0_f64.log10()) / (250.0_f64.log10() - 100.0_f64.log10());
        return 1.0 - t;
    }
    if f < 2000.0 {
        return 0.0;
    }
    if f < 10000.0 {
        let t = (f.log10() - 2000.0_f64.log10()) / (10000.0_f64.log10() - 2000.0_f64.log10());
        return 0.15 / 0.35 * t;
    }
    0.15 / 0.35
}

/// 控制点对数线性插值（频带外取端点值；对齐 TS `interpLogCurve`，排序为稳定排序）。
fn interp_log_curve(f: f64, pts: &[(f64, f64)]) -> f64 {
    if pts.is_empty() {
        return 0.0;
    }
    if pts.len() == 1 {
        return pts[0].1;
    }
    let mut sorted = pts.to_vec();
    sorted.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    if f <= sorted[0].0 {
        return sorted[0].1;
    }
    let last = sorted.len() - 1;
    if f >= sorted[last].0 {
        return sorted[last].1;
    }
    for i in 0..last {
        let (f0, g0) = sorted[i];
        let (f1, g1) = sorted[i + 1];
        if f >= f0 && f <= f1 {
            if f == f0 {
                return g0;
            }
            if f == f1 {
                return g1;
            }
            let t = (f.log10() - f0.log10()) / (f1.log10() - f0.log10());
            return g0 + t * (g1 - g0);
        }
    }
    0.0
}

/// 场景预设曲线（预设 id → 控制点 频率→dB；与 TS `PRESET_CURVES` 逐项一致）。
fn preset_curve(preset: &str) -> &'static [(f64, f64)] {
    match preset {
        "bass" => &[
            (63.0, 6.0),
            (100.0, 5.0),
            (160.0, 4.0),
            (250.0, 2.5),
            (400.0, 1.5),
            (630.0, 0.5),
            (1000.0, 0.0),
            (2000.0, 0.0),
            (4000.0, -0.5),
            (8000.0, -1.0),
            (12000.0, -1.5),
        ],
        "vocal" => &[
            (100.0, 0.0),
            (200.0, 0.5),
            (400.0, 1.5),
            (800.0, 2.5),
            (1000.0, 3.0),
            (2000.0, 3.5),
            (3000.0, 3.0),
            (5000.0, 2.0),
            (8000.0, 1.0),
            (12000.0, 0.5),
        ],
        "warm" => &[
            (63.0, 2.0),
            (100.0, 2.5),
            (200.0, 3.0),
            (400.0, 2.5),
            (800.0, 1.5),
            (1600.0, 0.5),
            (3000.0, 0.0),
            (6000.0, -1.0),
            (10000.0, -1.5),
            (16000.0, -2.0),
        ],
        "bright" => &[
            (63.0, 0.0),
            (200.0, 0.0),
            (500.0, 0.5),
            (1000.0, 1.0),
            (2000.0, 1.5),
            (4000.0, 2.5),
            (6300.0, 3.0),
            (10000.0, 3.0),
            (16000.0, 2.5),
        ],
        "night" => &[
            (63.0, 4.0),
            (100.0, 3.5),
            (200.0, 2.5),
            (400.0, 1.5),
            (800.0, 0.5),
            (1600.0, 0.0),
            (3000.0, -1.0),
            (6000.0, -2.0),
            (10000.0, -2.5),
            (16000.0, -3.0),
        ],
        // flat 与未知 id 同路径：空表 → 全 0 目标。
        _ => &[],
    }
}

/// 1/3 倍频程表内定点查找（对齐 TS `THIRD_OCTAVE_FREQS.indexOf(f)`；
/// 调用点全部是编译期常量频点，必在表内）。
fn third_octave_index(f: f64) -> usize {
    THIRD_OCTAVE_FREQS
        .iter()
        .position(|&x| x == f)
        .expect("调用点频点必须位于 1/3 倍频程常量表内")
}

/// RBJ peaking（对齐 TS LoudnessComp 本地 `designPeaking`：f0 钳制 [1, fs×0.45]，
/// 增益/Q 不在此钳制——与共享 designBiquad 的钳制域不同）。
fn design_peaking(f0: f64, gain_db: f64, q: f64, fs: f64) -> BiquadCoeffs {
    let f = js_min(js_max(f0, 1.0), fs * 0.45);
    let a = 10.0_f64.powf(gain_db / 40.0);
    let w0 = (2.0 * std::f64::consts::PI * f) / fs;
    let alpha = w0.sin() / (2.0 * q);
    let cw = w0.cos();
    let b0 = 1.0 + alpha * a;
    let b1 = -2.0 * cw;
    let b2 = 1.0 - alpha * a;
    let a0 = 1.0 + alpha / a;
    let a1 = -2.0 * cw;
    let a2 = 1.0 - alpha / a;
    BiquadCoeffs { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0 }
}

/// RBJ shelf（对齐 TS LoudnessComp 本地 `designShelf`；S=1：α = sin(w0)/2·√2，
/// 签名不含 Q）。
fn design_shelf(is_low: bool, f0: f64, gain_db: f64, fs: f64) -> BiquadCoeffs {
    let f = js_min(js_max(f0, 1.0), fs * 0.45);
    let a = 10.0_f64.powf(gain_db / 40.0);
    let w0 = (2.0 * std::f64::consts::PI * f) / fs;
    let alpha = (w0.sin() / 2.0) * std::f64::consts::SQRT_2;
    let cw = w0.cos();
    let sa = a.sqrt();
    if is_low {
        let b0 = a * ((a + 1.0) - (a - 1.0) * cw + 2.0 * sa * alpha);
        let b1 = 2.0 * a * ((a - 1.0) - (a + 1.0) * cw);
        let b2 = a * ((a + 1.0) - (a - 1.0) * cw - 2.0 * sa * alpha);
        let a0 = (a + 1.0) + (a - 1.0) * cw + 2.0 * sa * alpha;
        let a1 = -2.0 * ((a - 1.0) + (a + 1.0) * cw);
        let a2 = (a + 1.0) + (a - 1.0) * cw - 2.0 * sa * alpha;
        return BiquadCoeffs { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0 };
    }
    let b0 = a * ((a + 1.0) + (a - 1.0) * cw + 2.0 * sa * alpha);
    let b1 = -2.0 * a * ((a - 1.0) + (a + 1.0) * cw);
    let b2 = a * ((a + 1.0) + (a - 1.0) * cw - 2.0 * sa * alpha);
    let a0 = (a + 1.0) - (a - 1.0) * cw + 2.0 * sa * alpha;
    let a1 = 2.0 * ((a - 1.0) - (a + 1.0) * cw);
    let a2 = (a + 1.0) - (a - 1.0) * cw - 2.0 * sa * alpha;
    BiquadCoeffs { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0 }
}

/// TDF2 单槽递推（对齐 TS `biquadStep`：y = b0·x + z1；z1 = b1·x − a1·y + z2；
/// z2 = b2·x − a2·y；全程 f64，无中间量化）。
#[inline]
fn biquad_step(z: &mut (f64, f64), c: &BiquadCoeffs, x: f64) -> f64 {
    let y = c.b0 * x + z.0;
    z.0 = c.b1 * x - c.a1 * y + z.1;
    z.1 = c.b2 * x - c.a2 * y;
    y
}

/// 一个已配置的等响度补偿阶段（字段一一对应 TS `LoudnessComp` 私有域）。
pub struct LoudnessCompStage {
    fs: f64,

    // 当前目标参数（set_params 计算）。
    mode: String,
    volume_percent: f64,
    max_boost_db: f64,
    preset: String,
    smoothing_seconds: f64,
    target_gains: [f64; MAX_BANDS],
    target_freqs: [f64; MAX_BANDS],
    target_types: [i32; MAX_BANDS],
    current_gains: [f64; MAX_BANDS],

    // 内部 biquad 链（6 段，0 增益时为恒等）；左右声道各自独立状态、系数共享。
    coeffs: [BiquadCoeffs; MAX_BANDS],
    z_l: [(f64, f64); MAX_BANDS],
    z_r: [(f64, f64); MAX_BANDS],
}

impl LoudnessCompStage {
    /// 以显式参数快照构造（对齐 TS「构造即恒等链 + `setParams(p)`」组合语义）。
    ///
    /// fs ≤ 0 或非有限时报错（对齐 TS `Error('invalid sample rate')`）。
    pub fn from_settings(sample_rate: f64, settings: LoudnessCompSettings) -> Result<Self, String> {
        if !(sample_rate.is_finite() && sample_rate > 0.0) {
            return Err("invalid sample rate".into());
        }
        let mut stage = Self {
            fs: sample_rate,
            mode: "auto".to_string(),
            volume_percent: 100.0,
            max_boost_db: 12.0,
            preset: "flat".to_string(),
            smoothing_seconds: 0.2,
            target_gains: [0.0; MAX_BANDS],
            target_freqs: [0.0; MAX_BANDS],
            target_types: [0; MAX_BANDS],
            current_gains: [0.0; MAX_BANDS],
            // 初始为恒等链（0 增益），与 currentGains=0 一致（TS 构造器）。
            coeffs: [BiquadCoeffs { b0: 1.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0 }; MAX_BANDS],
            z_l: [(0.0, 0.0); MAX_BANDS],
            z_r: [(0.0, 0.0); MAX_BANDS],
        };
        stage.configure(settings);
        Ok(stage)
    }

    /// 覆盖参数快照（对齐 TS `setParams`，逐行同序）。
    ///
    /// 只重算目标段数组（不改动滤波器当前状态与 currentGains——参数即时生效、
    /// 历史爬升保留，由后续 process 平滑逼近新目标）。
    pub fn configure(&mut self, p: LoudnessCompSettings) {
        self.mode = match p.mode.as_str() {
            "auto" | "preset" | "custom" => p.mode.clone(),
            _ => "auto".to_string(),
        };
        self.volume_percent = clamp_finite(p.volume_percent, 0.0, 100.0);
        self.max_boost_db = clamp_finite(p.max_boost_db, 0.0, 24.0);
        self.preset = p.preset;
        self.smoothing_seconds = clamp_finite(p.smoothing_seconds, 0.01, 10.0);

        let (gains, freqs, types) = self.compute_targets(&p.bands);
        self.target_gains = gains;
        self.target_freqs = freqs;
        self.target_types = types;
    }

    /// 按模式计算目标曲线并拟合为 2–6 段（对齐 TS `computeTargets`，逐行同序）。
    fn compute_targets(
        &self,
        bands: &[LoudnessBandParam],
    ) -> ([f64; MAX_BANDS], [f64; MAX_BANDS], [i32; MAX_BANDS]) {
        let mut gains = [0.0_f64; MAX_BANDS];
        let mut freqs = [0.0_f64; MAX_BANDS];
        let mut types = [0_i32; MAX_BANDS];
        let mut n = 0_usize;

        if self.mode == "custom" {
            // custom：低/高频段增益取用户低/高频 bands 钳制均值；中频直接用用户
            // bands 做 peaking（钳制 → 丢弃 |gain|<0.25 → |增益| 降序取 4 → 频率升序）。
            let low: Vec<f64> = bands
                .iter()
                .filter(|b| b.frequency <= 250.0)
                .map(|b| clamp_finite(b.gain, -24.0, 24.0))
                .collect();
            let high: Vec<f64> = bands
                .iter()
                .filter(|b| b.frequency >= 6000.0)
                .map(|b| clamp_finite(b.gain, -24.0, 24.0))
                .collect();
            let low_gain = if !low.is_empty() { average(&low) } else { 0.0 };
            let high_gain = if !high.is_empty() { average(&high) } else { 0.0 };
            if low_gain.abs() >= 0.25 {
                gains[n] = low_gain;
                freqs[n] = 120.0;
                types[n] = TYPE_LOW_SHELF;
                n += 1;
            }
            if high_gain.abs() >= 0.25 {
                gains[n] = high_gain;
                freqs[n] = 12000.0;
                types[n] = TYPE_HIGH_SHELF;
                n += 1;
            }
            let mut picked: Vec<(f64, f64)> = bands
                .iter()
                .filter(|b| b.frequency > 250.0 && b.frequency < 6000.0)
                .filter(|b| clamp_finite(b.gain, -24.0, 24.0).abs() >= 0.25)
                .map(|b| (clamp_finite(b.frequency, 20.0, 20000.0), clamp_finite(b.gain, -24.0, 24.0)))
                .collect();
            // TS：sort(|a,b| |b.g| − |a.g| || a.f − b.f)——稳定排序（ES2019+）。
            picked.sort_by(|x, y| {
                y.1.abs()
                    .partial_cmp(&x.1.abs())
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(x.0.partial_cmp(&y.0).unwrap_or(std::cmp::Ordering::Equal))
            });
            picked.truncate(4);
            picked.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap_or(std::cmp::Ordering::Equal));
            for (f, g) in picked {
                gains[n] = g;
                freqs[n] = f;
                types[n] = TYPE_PEAKING;
                n += 1;
            }
            return (gains, freqs, types);
        }

        // auto / preset：先构造 1/3 倍频程目标表。
        let mut table = [0.0_f64; THIRD_OCTAVE_FREQS.len()];
        if self.mode == "preset" {
            let curve = preset_curve(&self.preset);
            for (i, &f) in THIRD_OCTAVE_FREQS.iter().enumerate() {
                table[i] = interp_log_curve(f, curve);
            }
        } else {
            // auto：ISO 226 简化近似，随音量线性。
            let v = self.volume_percent / 100.0;
            for (i, &f) in THIRD_OCTAVE_FREQS.iter().enumerate() {
                table[i] = self.max_boost_db * (1.0 - v) * auto_weight(f);
            }
        }

        // 固定 2 shelf：table(100Hz) 与 table(10kHz)。
        let low_gain = table[third_octave_index(100.0)];
        let high_gain = table[third_octave_index(10000.0)];
        if low_gain.abs() >= 0.25 {
            gains[n] = low_gain;
            freqs[n] = 120.0;
            types[n] = TYPE_LOW_SHELF;
            n += 1;
        }
        if high_gain.abs() >= 0.25 {
            gains[n] = high_gain;
            freqs[n] = 12000.0;
            types[n] = TYPE_HIGH_SHELF;
            n += 1;
        }
        // 中频 peaking 候选（|增益| ≥ 0.25 者按 |增益| 降序取前 4，再按频率升序）。
        let mut picked: Vec<(f64, f64)> = Vec::new();
        for &f in PEAKING_CANDIDATES.iter() {
            let g = table[third_octave_index(f)];
            if g.abs() >= 0.25 {
                picked.push((f, g));
            }
        }
        picked.sort_by(|x, y| {
            y.1.abs()
                .partial_cmp(&x.1.abs())
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(x.0.partial_cmp(&y.0).unwrap_or(std::cmp::Ordering::Equal))
        });
        picked.truncate(4);
        picked.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap_or(std::cmp::Ordering::Equal));
        for (f, g) in picked {
            gains[n] = g;
            freqs[n] = f;
            types[n] = TYPE_PEAKING;
            n += 1;
        }
        (gains, freqs, types)
    }

    /// 按当前平滑增益重算某槽 biquad 系数（对齐 TS `recomputeCoeffs`；
    /// 左右声道共享同一组系数）。
    fn recompute_coeffs(&mut self, idx: usize, gain_db: f64) {
        let f = self.target_freqs[idx];
        let t = self.target_types[idx];
        let c = if t == TYPE_LOW_SHELF {
            design_shelf(true, f, gain_db, self.fs)
        } else if t == TYPE_HIGH_SHELF {
            design_shelf(false, f, gain_db, self.fs)
        } else {
            design_peaking(f, gain_db, 1.0, self.fs)
        };
        self.coeffs[idx] = c;
    }

    /// 目标段快照（诊断/测试用途）：每槽 (gain, freq, type)。
    pub fn targets_snapshot(&self) -> [(f64, f64, i32); MAX_BANDS] {
        let mut out = [(0.0, 0.0, 0); MAX_BANDS];
        for i in 0..MAX_BANDS {
            out[i] = (self.target_gains[i], self.target_freqs[i], self.target_types[i]);
        }
        out
    }
}

impl Stage for LoudnessCompStage {
    /// 无按块长依赖的工作缓冲（6 槽系数/状态构造期定容）；保留形参以符合 Stage 契约。
    fn prepare(&mut self, _max_block_size: usize) {}

    /// 就地处理一个立体声块；**输出依赖本块帧数 B**（逐块平滑 alpha 按实际块长
    /// 计算，规格 §4.3——与 TS 一致以 `min(l.length, r.length)` 为块长）。
    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        debug_assert_eq!(left.len(), right.len(), "左右声道块长必须一致（Stage 契约）");
        let frames = left.len().min(right.len());
        // 逐块平滑增益（一阶低通，时间常数 smoothingSeconds；TS `-B / (s·fs)`）。
        let alpha = 1.0 - (-(frames as f64) / (self.smoothing_seconds * self.fs)).exp();
        for i in 0..MAX_BANDS {
            let target = self.target_gains[i];
            let current = self.current_gains[i];
            if current != target {
                let mut g = current + alpha * (target - current);
                if (g - target).abs() < 1e-9 {
                    g = target; // 收敛钉扎
                }
                self.current_gains[i] = g;
                self.recompute_coeffs(i, g);
            }
        }

        // 6 段 TDF2 级联；每样本先整条跑 L、再同槽跑 R（两组状态互不相交，
        // 槽内交错次序与 TS 一致）；级联中间量全程 f64，仅最终写回量化 f32。
        let coeffs = self.coeffs;
        let (mut z_l, mut z_r) = (self.z_l, self.z_r);
        for i in 0..frames {
            let mut yl = f64::from(left[i]);
            let mut yr = f64::from(right[i]);
            for k in 0..MAX_BANDS {
                yl = biquad_step(&mut z_l[k], &coeffs[k], yl);
                yr = biquad_step(&mut z_r[k], &coeffs[k], yr);
            }
            left[i] = yl as f32;
            right[i] = yr as f32;
        }
        self.z_l = z_l;
        self.z_r = z_r;
    }

    /// reset()：TDF2 状态清零，currentGains 直接钉到 targetGains 并立即重算全部
    /// 6 槽系数（含目标为 0 的槽——按 targetFreqs=0 钳到 1Hz 设计，TS 行为事实；
    /// 规格 §4.5.5：reset 语义 = 跳过平滑直接到位，与多数模块的"reset 后复现"不同）。
    fn reset(&mut self) {
        for i in 0..MAX_BANDS {
            self.z_l[i] = (0.0, 0.0);
            self.z_r[i] = (0.0, 0.0);
            self.current_gains[i] = self.target_gains[i];
            self.recompute_coeffs(i, self.target_gains[i]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 确定性伪噪声（LCG，无随机依赖），幅度 [-amp, amp)。
    fn lcg_noise(n: usize, seed: u32, amp: f64) -> Vec<f32> {
        let mut u = seed;
        (0..n)
            .map(|_| {
                u = u.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (((f64::from(u) / 4_294_967_296.0) * 2.0 - 1.0) * amp) as f32
            })
            .collect()
    }

    fn sine_sum(n: usize, fs: f64, parts: &[(f64, f64, f64)]) -> Vec<f32> {
        (0..n)
            .map(|i| {
                let mut acc = 0.0;
                for &(freq, amp, phase) in parts {
                    acc += (2.0 * std::f64::consts::PI * freq * i as f64 / fs + phase).sin() * amp;
                }
                acc as f32
            })
            .collect()
    }

    fn settings_auto(volume_percent: f64, smoothing: f64) -> LoudnessCompSettings {
        LoudnessCompSettings {
            volume_percent,
            max_boost_db: 12.0,
            preset: "flat".to_string(),
            bands: Vec::new(),
            mode: "auto".to_string(),
            smoothing_seconds: smoothing,
        }
    }

    fn settings_case3() -> LoudnessCompSettings {
        LoudnessCompSettings {
            volume_percent: 42.0,
            max_boost_db: 12.0,
            preset: "flat".to_string(),
            mode: "custom".to_string(),
            smoothing_seconds: 0.05,
            bands: vec![
                LoudnessBandParam { frequency: 60.0, gain: 18.0 },
                LoudnessBandParam { frequency: 30.0, gain: 200.0 },
                LoudnessBandParam { frequency: 300.0, gain: 0.1 },
                LoudnessBandParam { frequency: 1000.0, gain: -6.0 },
                LoudnessBandParam { frequency: 4000.0, gain: 9.0 },
                LoudnessBandParam { frequency: 20000.0, gain: 5.0 },
            ],
        }
    }

    fn settings_case4() -> LoudnessCompSettings {
        LoudnessCompSettings {
            volume_percent: 100.0,
            max_boost_db: 12.0,
            preset: "night".to_string(),
            bands: Vec::new(),
            mode: "preset".to_string(),
            smoothing_seconds: 0.05,
        }
    }

    /// 与 Node 直跑 TS 源取得的黄金参考比对（f64 域，跨库超越函数 1 ulp 级差异
    /// 远小于 1e-9 相对容差、远严于任何公式/分组/排序错误）。
    fn assert_f64_close(got: f64, want: f64, label: &str) {
        assert!(
            (got - want).abs() <= 1e-9 * want.abs().max(1e-300),
            "{label}：got {got}，want {want}"
        );
    }

    fn assert_f32_close(got: f32, want: f64, label: &str) {
        let diff = (f64::from(got) - want).abs();
        assert!(
            diff <= 1e-6 * want.abs().max(1e-9),
            "{label}：got {got}，want {want}"
        );
    }

    #[test]
    fn 非法采样率报错_对齐ts错误信息() {
        for bad in [0.0_f64, -44100.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let err = LoudnessCompStage::from_settings(bad, settings_auto(100.0, 0.2))
                .err()
                .expect("非法采样率必须报错");
            assert!(err.contains("invalid sample rate"), "错误信息应与 TS 一致：{err}");
        }
    }

    #[test]
    fn auto满音量_逐位恒等锚点() {
        // GWT-LC-01：volumePercent=100 → 全部目标为 0 → 无槽重算 → 恒等系数链 +
        // 零状态 → 输出与输入逐位一致（精确恒等而非近似）。
        let mut stage = LoudnessCompStage::from_settings(48000.0, settings_auto(100.0, 0.2)).unwrap();
        stage.prepare(256);
        let in_l = sine_sum(
            512,
            48000.0,
            &[
                (63.0, 0.4, 0.0),
                (440.0, 0.3, std::f64::consts::PI / 5.0),
                (4000.0, 0.2, std::f64::consts::FRAC_PI_3),
                (10000.0, 0.15, std::f64::consts::FRAC_PI_2),
            ],
        );
        let in_r = lcg_noise(512, 63_001, 0.6);
        let mut l = in_l.clone();
        let mut r = in_r.clone();
        let mut off = 0;
        while off < 512 {
            let end = (off + 256).min(512);
            stage.process(&mut l[off..end], &mut r[off..end]);
            off = end;
        }
        assert_eq!(l, in_l, "auto 满音量必须逐位恒等（左）");
        assert_eq!(r, in_r, "auto 满音量必须逐位恒等（右）");
    }

    #[test]
    fn auto低音量_目标段命中ts黄金参考() {
        // 黄金参考：node 直跑（case2 形态 volumePercent=20/maxBoostDb=12）。
        let stage = LoudnessCompStage::from_settings(48000.0, settings_auto(20.0, 0.05)).unwrap();
        let targets = stage.targets_snapshot();
        let want_gains = [9.600000000000001, 4.114285714285715, 0.5704328935675077, 1.7719264103591037, 2.9331616235548204, 0.0];
        let want_freqs = [120.0, 12000.0, 2500.0, 4000.0, 6300.0, 0.0];
        let want_types = [0, 1, 2, 2, 2, 0];
        for i in 0..MAX_BANDS {
            assert_f64_close(targets[i].0, want_gains[i], &format!("case2 gains[{i}]"));
            assert_eq!(targets[i].1, want_freqs[i], "case2 freqs[{i}]");
            assert_eq!(targets[i].2, want_types[i], "case2 types[{i}]");
        }
    }

    #[test]
    fn custom分组钳制丢弃_目标段命中ts黄金参考() {
        // GWT-LC-05：low 组钳制均值 +21、mid |0.1|<0.25 丢弃、peaking −6/+9、
        // high +5；volumePercent 不被 custom 消费。
        let stage = LoudnessCompStage::from_settings(48000.0, settings_case3()).unwrap();
        let targets = stage.targets_snapshot();
        let want_gains = [21.0, 5.0, -6.0, 9.0, 0.0, 0.0];
        let want_freqs = [120.0, 12000.0, 1000.0, 4000.0, 0.0, 0.0];
        let want_types = [0, 1, 2, 2, 0, 0];
        for i in 0..MAX_BANDS {
            assert_f64_close(targets[i].0, want_gains[i], &format!("case3 gains[{i}]"));
            assert_eq!(targets[i].1, want_freqs[i], "case3 freqs[{i}]");
            assert_eq!(targets[i].2, want_types[i], "case3 types[{i}]");
        }
    }

    #[test]
    fn preset_night_六段满配_目标段命中ts黄金参考() {
        // GWT-LC-04：night 6 段上限、peaking 含负增益；volumePercent/maxBoostDb 不参与。
        let stage = LoudnessCompStage::from_settings(48000.0, settings_case4()).unwrap();
        let targets = stage.targets_snapshot();
        let want_gains = [3.5, -2.5, 1.8446481713874463, 0.8446481713874464, -1.4150374992788435, -2.047756183225239];
        let want_freqs = [120.0, 12000.0, 315.0, 630.0, 4000.0, 6300.0];
        let want_types = [0, 1, 2, 2, 2, 2];
        for i in 0..MAX_BANDS {
            assert_f64_close(targets[i].0, want_gains[i], &format!("case4 gains[{i}]"));
            assert_eq!(targets[i].1, want_freqs[i], "case4 freqs[{i}]");
            assert_eq!(targets[i].2, want_types[i], "case4 types[{i}]");
        }
    }

    #[test]
    fn 收敛后各槽系数命中ts黄金参考() {
        // 黄金参考：case2 形态零输入 3000 块（blockSize=384）后 currentGains 钉到
        // 目标、系数收敛于 designShelf/designPeaking(目标增益)。
        let mut stage = LoudnessCompStage::from_settings(48000.0, settings_auto(20.0, 0.05)).unwrap();
        stage.prepare(384);
        let mut l = vec![0.0_f32; 384];
        let mut r = vec![0.0_f32; 384];
        for _ in 0..3000 {
            stage.process(&mut l, &mut r);
        }
        let want_gains = [9.600000000000001, 4.114285714285715, 0.5704328935675077, 1.7719264103591037, 2.9331616235548204, 0.0];
        let want_coeffs: [[f64; 5]; 6] = [
            [1.0062355424508314, -1.9830066866530016, 0.9771963491956503, -1.983148889846566, 0.9832896884529171],
            [1.2672349521045, -0.1755007472802113, 0.22172233144184322, 0.13849108800917823, 0.1749654482569538],
            [1.009136020248408, -1.638956720424534, 0.7216745954180408, -1.638956720424534, 0.7308106156664489],
            [1.041679230860166, -1.4130454485263324, 0.5899651093075409, -1.4130454485263324, 0.6316443401677068],
            [1.095088833974348, -1.0362432061161189, 0.4314905237170479, -1.0362432061161189, 0.526579357691396],
            [1.0, 0.0, 0.0, 0.0, 0.0],
        ];
        let names = ["b0", "b1", "b2", "a1", "a2"];
        for i in 0..MAX_BANDS {
            assert_f64_close(stage.current_gains[i], want_gains[i], &format!("收敛 currentGains[{i}]"));
            let vals = [
                stage.coeffs[i].b0,
                stage.coeffs[i].b1,
                stage.coeffs[i].b2,
                stage.coeffs[i].a1,
                stage.coeffs[i].a2,
            ];
            for (k, &v) in vals.iter().enumerate() {
                assert_f64_close(v, want_coeffs[i][k], &format!("slot{i}.{}", names[k]));
            }
        }
    }

    #[test]
    fn 逐块爬升轨迹命中ts黄金参考() {
        // GWT-LC-06：alpha = 1 − exp(−B/(τ·fs)) 逐块计算；零输入下只观察平滑状态。
        let mut stage = LoudnessCompStage::from_settings(48000.0, settings_auto(20.0, 0.05)).unwrap();
        stage.prepare(384);
        let mut l = vec![0.0_f32; 384];
        let mut r = vec![0.0_f32; 384];
        stage.process(&mut l, &mut r);
        let want_block1 = [1.4194196259243712, 0.6083226968247305, 0.08434204629193212, 0.2619903252663992, 0.43368616400853166, 0.0];
        for i in 0..MAX_BANDS {
            assert_f64_close(stage.current_gains[i], want_block1[i], &format!("block1 currentGains[{i}]"));
        }
        stage.process(&mut l, &mut r);
        stage.process(&mut l, &mut r);
        let want_block3 = [3.659679438661048, 1.568434045140449, 0.21745849288801392, 0.6754877761262177, 1.1181699254159616, 0.0];
        for i in 0..MAX_BANDS {
            assert_f64_close(stage.current_gains[i], want_block3[i], &format!("block3 currentGains[{i}]"));
        }
    }

    #[test]
    fn 输出序列命中ts黄金参考() {
        // 黄金参考：case2 形态，LCG 输入（seed 77/78），2 块 384 帧，取末段 12 帧。
        let mut stage = LoudnessCompStage::from_settings(48000.0, settings_auto(20.0, 0.05)).unwrap();
        stage.prepare(384);
        let n = 768;
        let mut l = lcg_noise(n, 77, 0.6);
        let mut r = lcg_noise(n, 78, 0.5);
        stage.process(&mut l[..384], &mut r[..384]);
        stage.process(&mut l[384..], &mut r[384..]);
        let want_l = [
            -0.2514169216156006, -0.537182092666626, 0.43457090854644775, 0.5927199125289917,
            0.039996445178985596, -0.49204403162002563, 0.5905746817588806, -0.06912245601415634,
            -0.6020593047142029, 0.29570868611335754, 0.21290799975395203, 0.37498950958251953,
        ];
        let want_r = [
            -0.48597437143325806, -0.03156710043549538, -0.39061811566352844, -0.22035199403762817,
            0.2950754761695862, -0.3880440294742584, -0.5064814686775208, 0.23671233654022217,
            0.5271447896957397, -0.16049237549304962, -0.00047343436744995415, -0.3047662079334259,
        ];
        for (k, w) in want_l.iter().enumerate() {
            assert_f32_close(l[372 + k], *w, &format!("L[{}]", 372 + k));
        }
        for (k, w) in want_r.iter().enumerate() {
            assert_f32_close(r[756 + k], *w, &format!("R[{}]", 756 + k));
        }
    }

    #[test]
    fn blockSize是行为参数_爬升型输出随块长变化但同调度可复现() {
        // GWT-LC-07：alpha 随块长变化 → 整块与分块输出互不相同（规格 §4.5.6 实证
        // 1e-2..1e-1 量级）；同一块长调度必须逐位可复现。
        let n = 9800;
        let in_l = lcg_noise(n, 63_002, 0.5);
        let in_r = sine_sum(
            n,
            48000.0,
            &[(63.0, 0.35, 0.0), (120.0, 0.3, std::f64::consts::FRAC_PI_6)],
        );
        let run = |block: usize| -> (Vec<f32>, Vec<f32>) {
            let mut stage =
                LoudnessCompStage::from_settings(48000.0, settings_auto(20.0, 0.05)).unwrap();
            stage.prepare(block);
            let mut l = in_l.clone();
            let mut r = in_r.clone();
            let mut off = 0;
            while off < n {
                let end = (off + block).min(n);
                stage.process(&mut l[off..end], &mut r[off..end]);
                off = end;
            }
            (l, r)
        };
        let (whole_l, _) = run(n);
        let (chunk_l, chunk_r) = run(384);
        let (chunk_again_l, chunk_again_r) = run(384);
        assert_eq!(chunk_l, chunk_again_l, "同一块长调度必须逐位可复现（左）");
        assert_eq!(chunk_r, chunk_again_r, "同一块长调度必须逐位可复现（右）");
        let max_diff = whole_l
            .iter()
            .zip(chunk_l.iter())
            .map(|(&x, &y)| (f64::from(x) - f64::from(y)).abs())
            .fold(0.0_f64, f64::max);
        assert!(
            max_diff > 1.0e-3,
            "爬升型参数下整块与分块输出应显著不同（规格 §4.5.6），实际 maxDiff={max_diff}"
        );
    }

    #[test]
    fn reset钉扎语义_直接到位且与首次爬升不同() {
        // GWT-LC-08：reset 将 currentGains 钉到 targetGains 并立即重算系数；
        // reset 后重放与首次从零爬升的处理不同（规格 §4.5.5 行为事实）。
        let mut stage = LoudnessCompStage::from_settings(48000.0, settings_auto(20.0, 0.05)).unwrap();
        stage.reset();
        let want = [9.600000000000001, 4.114285714285715, 0.5704328935675077, 1.7719264103591037, 2.9331616235548204, 0.0];
        for i in 0..MAX_BANDS {
            assert_f64_close(stage.current_gains[i], want[i], &format!("reset 钉扎 [{i}]"));
        }

        // reset 后立即处理（无爬升） vs 全新实例首块（从 0 爬升）→ 输出不同。
        let in_l = lcg_noise(384, 55, 0.5);
        let in_r = lcg_noise(384, 56, 0.5);
        let mut resetted = LoudnessCompStage::from_settings(48000.0, settings_auto(20.0, 0.05)).unwrap();
        resetted.reset();
        resetted.prepare(384);
        let mut rl = in_l.clone();
        let mut rr = in_r.clone();
        resetted.process(&mut rl, &mut rr);
        let mut fresh = LoudnessCompStage::from_settings(48000.0, settings_auto(20.0, 0.05)).unwrap();
        fresh.prepare(384);
        let mut fl = in_l.clone();
        let mut fr = in_r.clone();
        fresh.process(&mut fl, &mut fr);
        let max_diff = rl
            .iter()
            .zip(fl.iter())
            .map(|(&x, &y)| (f64::from(x) - f64::from(y)).abs())
            .fold(0.0_f64, f64::max);
        assert!(
            max_diff > 1.0e-3,
            "reset（直接到位）与首次爬升的首块输出应显著不同，实际 maxDiff={max_diff}"
        );
    }

    #[test]
    fn 静音输入零输出() {
        let mut stage = LoudnessCompStage::from_settings(48000.0, settings_auto(20.0, 0.05)).unwrap();
        stage.prepare(256);
        let mut l = vec![0.0_f32; 9600];
        let mut r = vec![0.0_f32; 9600];
        let mut off = 0;
        while off < 9600 {
            let end = (off + 256).min(9600);
            stage.process(&mut l[off..end], &mut r[off..end]);
            off = end;
        }
        assert!(l.iter().all(|&x| x.to_bits() == 0_u32));
        assert!(r.iter().all(|&x| x.to_bits() == 0_u32));
    }

    #[test]
    fn 极值钳制_非有限值回落下限() {
        // GWT-LC-09：volumePercent/maxBoostDb/smoothingSeconds 双向越界钳制、
        // NaN/Infinity 回落下限（TS Number.isFinite 防御）。
        let mut p = settings_auto(f64::NAN, 0.0);
        p.max_boost_db = -5.0;
        let stage = LoudnessCompStage::from_settings(48000.0, p).unwrap();
        assert_eq!(stage.volume_percent, 0.0, "NaN 回落下限 0");
        assert_eq!(stage.max_boost_db, 0.0, "-5 钳到 0");
        assert_eq!(stage.smoothing_seconds, 0.01, "0 钳到下限 0.01");
        let mut p = settings_auto(150.0, 99.0);
        p.max_boost_db = f64::INFINITY;
        let stage = LoudnessCompStage::from_settings(48000.0, p).unwrap();
        assert_eq!(stage.volume_percent, 100.0, "150 钳到 100");
        assert_eq!(stage.max_boost_db, 0.0, "Infinity 回落下限 0");
        assert_eq!(stage.smoothing_seconds, 10.0, "99 钳到上限 10");
        // 全程有界：钳制极值 + 满幅输入。
        let mut stage = LoudnessCompStage::from_settings(48000.0, settings_auto(0.0, 0.01)).unwrap();
        stage.prepare(256);
        let input = lcg_noise(1024, 99, 1.0);
        let mut l = input.clone();
        let mut r = input.clone();
        let mut off = 0;
        while off < 1024 {
            let end = (off + 256).min(1024);
            stage.process(&mut l[off..end], &mut r[off..end]);
            off = end;
        }
        for i in 0..1024 {
            assert!(l[i].is_finite() && r[i].is_finite(), "输出必须有限 @{i}");
        }
    }

    #[test]
    fn 左右声道状态独立_同源输入输出一致() {
        // 规格 §4.4：左右各自独立 TDF2 状态、共享系数。同源输入下左右输出逐位一致；
        // 且单声道侧跑（另一声道喂零）与立体声运行中对应声道逐位一致（无串扰）。
        let n = 512;
        let input = lcg_noise(n, 71, 0.5);
        let mut stage = LoudnessCompStage::from_settings(48000.0, settings_auto(20.0, 0.05)).unwrap();
        stage.prepare(256);
        let mut l = input.clone();
        let mut r = input.clone();
        let mut off = 0;
        while off < n {
            let end = (off + 256).min(n);
            stage.process(&mut l[off..end], &mut r[off..end]);
            off = end;
        }
        for i in 0..n {
            assert_eq!(l[i].to_bits(), r[i].to_bits(), "同源输入下左右输出应一致 @{i}");
        }
        // 独立性：左声道喂信号、右声道喂零的运行中，左声道输出与双声道同信号运行一致。
        let mut solo = LoudnessCompStage::from_settings(48000.0, settings_auto(20.0, 0.05)).unwrap();
        solo.prepare(256);
        let mut sl = input.clone();
        let mut sink = vec![0.0_f32; n];
        let mut off = 0;
        while off < n {
            let end = (off + 256).min(n);
            solo.process(&mut sl[off..end], &mut sink[off..end]);
            off = end;
        }
        assert_eq!(l, sl, "左声道输出不受右声道输入影响（状态独立）");
    }

    #[test]
    fn 同参数同块长重放确定可复现() {
        let in_l = lcg_noise(9800, 81, 0.5);
        let in_r = lcg_noise(9800, 82, 0.4);
        let run = || -> (Vec<f32>, Vec<f32>) {
            let mut stage =
                LoudnessCompStage::from_settings(48000.0, settings_auto(20.0, 0.05)).unwrap();
            stage.prepare(384);
            let mut l = in_l.clone();
            let mut r = in_r.clone();
            let mut off = 0;
            while off < 9800 {
                let end = (off + 384).min(9800);
                stage.process(&mut l[off..end], &mut r[off..end]);
                off = end;
            }
            (l, r)
        };
        let (a_l, a_r) = run();
        let (b_l, b_r) = run();
        assert_eq!(a_l, b_l, "同参数同块长必须逐位可复现（左）");
        assert_eq!(a_r, b_r, "同参数同块长必须逐位可复现（右）");
    }
}
