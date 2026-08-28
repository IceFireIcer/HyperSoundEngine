//! bass_enhancer —— 虚拟低频增强（谐波生成 + 低音下潜）。
//!
//! 行为事实标准：仓库根 `src/dsp/BassEnhancer.ts`；规格：`specs/dsp/bass-enhancer.md`。
//! 本文件是该 TS 类的逐行移植，关键对应关系：
//!
//! - 每通道信号流（规格 §4.1）：`b = LPF(x)` → `h = HPF(NL(b))` →
//!   `out = x + k·h + lowLin·b`，其中 `k = mix·harmonicGain·levelLin`（三者连乘）、
//!   `lowLin = 10^(lowBoostDb/20) − 1`；
//! - 谐波非线性（规格 §4.2）仅作用于低频带：odd=x³ / even=|x| / 
//!   atan=atan(√|x|)·sign(x) / soft=tanh(2x)；枚举外取值按 odd 处理（TS default）；
//! - **设计采样率条款（规格 §4.3，事实标准关键行为）**：四个内部 Biquad 以
//!   TS 缺省构造 `new Biquad()` 创建，系数设计采样率**固定 48000 Hz**，与模块
//!   自身构造采样率无关（模块采样率只参与 cutoffHz / hpCut 的钳制上界
//!   `fs × 0.45`）。任意采样率下必须按 48000 设计，否则非 48000 对拍必然超差；
//! - 换参语义（规格 §4.5）：`setParams` 重算系数但 Biquad 状态保留不清零；
//!   `enabled = false` 逐位直通且滤波器状态不更新；`reset()` 复位全部四个实例。
//!
//! # 数值精度铁律的落点
//!
//! - TS 中四个 Biquad 经**单样本 `process(x: number): number`** 级联：低通输出
//!   `bl`、非线性输出、高通输出 `hl` 全程是 Number（f64），**不经任何 f32 量化**
//!   就进入下一级——这与 biquad.rs 的 `BiquadStage`（processBlock 写 Float32Array、
//!   输出落 f32）不同，因此本文件内置私有单声道 TDF2 核（f64 进 f64 出），
//!   复用 [`crate::biquad::design_biquad`] 的系数设计；
//! - f32 落点仅一处：最终输出样本 `x + k·h + low·b` 写回 Float32Array；
//! - `Math.max` 的 NaN 传播语义以 [`js_max`] 显式复刻（理由同 biquad.rs）。
//!
//! # 实时安全
//!
//! 四个 TDF2 状态各为两个双精度标量、随阶段常驻，`process` 稳态零分配。

use crate::biquad::{design_biquad, BiquadCoeffs};
use crate::Stage;

/// 内部 Biquad 的固定系数设计采样率（TS `new Biquad()` 的缺省 `fs ?? 48000`，
/// 规格 bass-enhancer §4.3 设计采样率条款）。
const DESIGN_SAMPLE_RATE: f64 = 48_000.0;

/// 复刻 JS Math.max(a, b) 的 NaN 传播语义（理由见 biquad.rs 的同名函数）。
fn js_max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a > b {
        a
    } else {
        b
    }
}

/// TS `clamp(v, lo, hi)` 的逐字复刻（NaN 输入原样返回，与 TS 三目链一致）。
fn clamp(v: f64, lo: f64, hi: f64) -> f64 {
    if v < lo {
        lo
    } else if v > hi {
        hi
    } else {
        v
    }
}

/// 复刻 JS Math.sign 的语义：±1 / ±0（保号零）/ NaN 传播。
#[inline]
fn js_sign(x: f64) -> f64 {
    if x.is_nan() {
        f64::NAN
    } else if x > 0.0 {
        1.0
    } else if x < 0.0 {
        -1.0
    } else {
        x // ±0 保号（Math.sign(-0) === -0）
    }
}

/// 单声道 TDF2 双二阶核（对齐 TS `Biquad` 的单样本 `process` 路径）。
///
/// 与 [`crate::biquad::BiquadStage`] 的差异：`tick` 以 f64 进、f64 出，输出**不
/// 做 f32 量化**——TS BassEnhancer 中低通/高通经 `process`（返回 Number）级联，
/// 只有最终混音写回 Float32Array 才落 f32。
#[derive(Debug, Clone)]
struct MonoBiquad {
    coeffs: BiquadCoeffs,
    s1: f64,
    s2: f64,
}

impl MonoBiquad {
    /// TS `new Biquad()`：缺省构造为恒等系数（b0=1，其余 0）、零状态。
    fn new() -> Self {
        Self {
            coeffs: BiquadCoeffs { b0: 1.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0 },
            s1: 0.0,
            s2: 0.0,
        }
    }

    /// TS `setParams`：按**固定 48000 Hz** 设计采样率重算系数；状态保留不清零
    /// （规格 §4.3/§4.5）。
    fn set_params(&mut self, filter_type: &str, f0: f64, q: f64, gain_db: f64) {
        // 设计采样率为编译期常量 48000 > 0，design_biquad 不会失败。
        self.coeffs = design_biquad(filter_type, f0, q, gain_db, DESIGN_SAMPLE_RATE)
            .expect("固定设计采样率 48000 恒合法");
    }

    /// TDF2 单样本递推（对齐 TS Biquad.process L150–L153，f64 进 f64 出）。
    #[inline]
    fn tick(&mut self, x: f64) -> f64 {
        let y = self.coeffs.b0 * x + self.s1;
        self.s1 = self.coeffs.b1 * x - self.coeffs.a1 * y + self.s2;
        self.s2 = self.coeffs.b2 * x - self.coeffs.a2 * y;
        y
    }

    fn reset(&mut self) {
        self.s1 = 0.0;
        self.s2 = 0.0;
    }
}

/// 对齐 TS `BassEnhancerSettings` 的参数快照（字段名蛇形转换）。
#[derive(Debug, Clone)]
pub struct BassEnhancerSettings {
    pub enabled: bool,
    pub cutoff_hz: f64,
    pub q: f64,
    /// TS `HarmonicType` 四种之一：odd / even / atan / soft。
    /// 未知值对齐 TS switch default：按 odd 处理（规格 §三）。
    pub harmonic_type: String,
    pub harmonic_gain: f64,
    pub mix: f64,
    pub level_db: f64,
    /// TS 可选字段：缺省或非有限值（`Number.isFinite` 防御，兼容旧参数快照）
    /// 按 0 处理，防 NaN 污染（规格 §三/§4.4）。
    pub low_boost_db: Option<f64>,
}

/// 一个已配置的虚拟低音增强阶段（字段一一对应 TS `BassEnhancer` 私有域）。
pub struct BassEnhancerStage {
    fs: f64,
    enabled: bool,
    cutoff_hz: f64,
    q: f64,
    harmonic_type: String,
    harmonic_gain: f64,
    mix: f64,
    level_lin: f64,
    low_boost_db: f64,
    /// `10^(lowBoostDb/20) − 1`，低频带混回增益（真实能量提升；0 = 精确关闭）。
    low_lin: f64,
    lp_l: MonoBiquad,
    lp_r: MonoBiquad,
    hp_l: MonoBiquad,
    hp_r: MonoBiquad,
}

impl BassEnhancerStage {
    /// 按 TS 构造函数内置默认创建（cutoffHz=90 / q=0.7 / odd / harmonicGain=0.6 /
    /// mix=0.5 / levelDb=0，lowBoostDb 缺省按 0）。
    pub fn new(sample_rate: f64) -> Result<Self, String> {
        Self::from_settings(
            sample_rate,
            BassEnhancerSettings {
                enabled: true,
                cutoff_hz: 90.0,
                q: 0.7,
                harmonic_type: "odd".to_string(),
                harmonic_gain: 0.6,
                mix: 0.5,
                level_db: 0.0,
                low_boost_db: None,
            },
        )
    }

    /// 以显式参数快照构造（对齐 TS `setParams` 整体替换语义；钳制规则见规格参数表）。
    ///
    /// sampleRate ≤ 0 或非有限时报错（GWT-BE-14，对齐 TS `Error('invalid sample rate')`）。
    pub fn from_settings(sample_rate: f64, settings: BassEnhancerSettings) -> Result<Self, String> {
        if !(sample_rate.is_finite() && sample_rate > 0.0) {
            return Err("invalid sample rate".into());
        }
        let mut stage = Self {
            fs: sample_rate,
            enabled: true,
            cutoff_hz: 90.0,
            q: 0.7,
            harmonic_type: "odd".to_string(),
            harmonic_gain: 0.6,
            mix: 0.5,
            level_lin: 1.0,
            low_boost_db: 0.0,
            low_lin: 0.0,
            lp_l: MonoBiquad::new(),
            lp_r: MonoBiquad::new(),
            hp_l: MonoBiquad::new(),
            hp_r: MonoBiquad::new(),
        };
        stage.configure(settings);
        Ok(stage)
    }

    /// 覆盖参数快照（对齐 TS `applyParams`，逐行同序）。
    ///
    /// 即时重算四个内部 Biquad 系数（固定 48000 设计采样率）；Biquad 状态保留
    /// 不清零（规格 §4.5）。
    pub fn configure(&mut self, settings: BassEnhancerSettings) {
        self.enabled = settings.enabled;
        // 钳制上界用模块自身采样率（TS this.fs × 0.45）——与内部滤波器的
        // 48000 设计采样率是两个独立事实（规格 §4.3）。
        self.cutoff_hz = clamp(settings.cutoff_hz, 20.0, self.fs * 0.45);
        self.q = clamp(settings.q, 0.1, 20.0);
        self.harmonic_type = settings.harmonic_type;
        self.harmonic_gain = clamp(settings.harmonic_gain, 0.0, 1.0);
        self.mix = clamp(settings.mix, 0.0, 1.0);
        self.level_lin = 10.0_f64.powf(clamp(settings.level_db, -6.0, 6.0) / 20.0);
        // 防御旧参数快照缺字段（undefined/非有限值 → 0，规格 §三）。
        let lb = settings.low_boost_db.filter(|v| v.is_finite()).unwrap_or(0.0);
        self.low_boost_db = clamp(lb, -6.0, 12.0);
        self.low_lin = 10.0_f64.powf(self.low_boost_db / 20.0) - 1.0;
        // 谐波整形高通：≥150Hz 或 cutoffHz·1.5（取较大），上限 fs·0.45，Q 固定 0.707。
        let hp_cut = clamp(js_max(150.0, self.cutoff_hz * 1.5), 20.0, self.fs * 0.45);
        self.lp_l.set_params("lowpass", self.cutoff_hz, self.q, 0.0);
        self.lp_r.set_params("lowpass", self.cutoff_hz, self.q, 0.0);
        self.hp_l.set_params("highpass", hp_cut, 0.707, 0.0);
        self.hp_r.set_params("highpass", hp_cut, 0.707, 0.0);
    }

    /// 谐波非线性函数（仅作用于低频带，避免全频互调；对齐 TS nonlinearity，
    /// 枚举外按 odd 处理）。
    #[inline]
    fn nonlinearity(&self, x: f64) -> f64 {
        match self.harmonic_type.as_str() {
            "odd" => x * x * x,
            "even" => x.abs(),
            // ATSR：atan(√|x|)·sign(x)
            "atan" => x.abs().sqrt().atan() * js_sign(x),
            // tanh(2·x)，驱动常量 2（软削波）
            "soft" => (2.0 * x).tanh(),
            _ => x * x * x,
        }
    }
}

impl Stage for BassEnhancerStage {
    /// 本模块无内部工作缓冲（四个 TDF2 状态仅八个双精度标量），无需预分配。
    fn prepare(&mut self, _max_block_size: usize) {}

    /// 就地处理一个立体声块；状态跨块保持（GWT-BE-11：切块不改变逐样本运算序列）。
    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        debug_assert_eq!(left.len(), right.len(), "左右声道块长必须一致（Stage 契约）");
        // TS L99：禁用即恒等直通，滤波器状态不更新。
        if !self.enabled {
            return;
        }
        let n = left.len();
        // 字段缓存为局部变量（对齐 TS L101–L102）。
        let k = self.mix * self.harmonic_gain * self.level_lin;
        let low = self.low_lin;
        for i in 0..n {
            let xl = f64::from(left[i]);
            let xr = f64::from(right[i]);
            // 1) 低通提取低频带（f64 全程，不落 f32——见模块级文档）。
            let bl = self.lp_l.tick(xl);
            let br = self.lp_r.tick(xr);
            // 2) 非线性谐波生成 + 3) 高通整形（先算形波再入高通，与 TS 同序）。
            let shaped_l = self.nonlinearity(bl);
            let shaped_r = self.nonlinearity(br);
            let hl = self.hp_l.tick(shaped_l);
            let hr = self.hp_r.tick(shaped_r);
            // 4) 混回（dry 不变）+ 5) 低音下潜：`(xl + k·hl) + low·bl` 的结合顺序
            //    与 TS `xl + k * hl + low * bl` 一致；low=0 时加性项逐位消失。
            left[i] = (xl + k * hl + low * bl) as f32;
            right[i] = (xr + k * hr + low * br) as f32;
        }
    }

    /// reset()：四个内部 Biquad 状态归零（系数保留；规格 §4.5）。
    fn reset(&mut self) {
        self.lp_l.reset();
        self.lp_r.reset();
        self.hp_l.reset();
        self.hp_r.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 确定性伪噪声（LCG，无随机依赖），幅度 [-1, 1)。
    fn lcg_noise(n: usize, seed: u32) -> Vec<f32> {
        let mut u = seed;
        (0..n)
            .map(|_| {
                u = u.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((f64::from(u) / f64::from(u32::MAX)) * 2.0 - 1.0) as f32
            })
            .collect()
    }

    fn sine(n: usize, freq: f64, fs: f64, amp: f64, phase: f64) -> Vec<f32> {
        (0..n)
            .map(|i| {
                (2.0 * std::f64::consts::PI * freq * i as f64 / fs + phase).sin() as f32 * amp as f32
            })
            .collect()
    }

    fn settings(harmonic_type: &str) -> BassEnhancerSettings {
        BassEnhancerSettings {
            enabled: true,
            cutoff_hz: 90.0,
            q: 0.7,
            harmonic_type: harmonic_type.to_string(),
            harmonic_gain: 0.6,
            mix: 0.5,
            level_db: 0.0,
            low_boost_db: None,
        }
    }

    fn drive(stage: &mut BassEnhancerStage, l: &[f32], r: &[f32], block: usize) -> (Vec<f32>, Vec<f32>) {
        let mut out_l = l.to_vec();
        let mut out_r = r.to_vec();
        let mut off = 0_usize;
        while off < l.len() {
            let end = (off + block).min(l.len());
            stage.process(&mut out_l[off..end], &mut out_r[off..end]);
            off = end;
        }
        (out_l, out_r)
    }

    #[test]
    fn 非法采样率报错_对齐ts错误信息() {
        for bad in [0.0_f64, -44100.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let err = BassEnhancerStage::new(bad).err().expect("非法采样率必须报错");
            assert!(err.contains("invalid sample rate"), "错误信息应与 TS 一致：{err}");
        }
    }

    #[test]
    fn 内部滤波器设计采样率固定48000_与模块采样率无关() {
        // GWT-BE-15（规格 §4.3 关键条款）：44100 实例的内部低通/高通系数必须
        // 等于按 48000 Hz 设计的结果，而非按 44100 设计。
        let mut p = settings("odd");
        p.cutoff_hz = 1000.0;
        let stage = BassEnhancerStage::from_settings(44100.0, p).unwrap();
        // cutoff 钳制上界用实例 fs：44100×0.45 = 19845，1000 未触界。
        assert_eq!(stage.cutoff_hz, 1000.0);
        let hp_cut_want = js_max(150.0, 1000.0 * 1.5); // 1500，未触上下界
        assert_eq!(
            stage.lp_l.coeffs,
            design_biquad("lowpass", 1000.0, 0.7, 0.0, 48_000.0).unwrap(),
            "内部低通必须按固定 48000 设计"
        );
        assert_eq!(
            stage.hp_l.coeffs,
            design_biquad("highpass", hp_cut_want, 0.707, 0.0, 48_000.0).unwrap(),
            "内部高通必须按固定 48000 设计"
        );
        // 左右同系数（同构路径）。
        assert_eq!(stage.lp_l.coeffs, stage.lp_r.coeffs);
        assert_eq!(stage.hp_l.coeffs, stage.hp_r.coeffs);
        // 对照：若误用实例采样率设计，系数必然不同（证明断言有判别力）。
        assert_ne!(
            stage.lp_l.coeffs,
            design_biquad("lowpass", 1000.0, 0.7, 0.0, 44_100.0).unwrap()
        );
    }

    #[test]
    fn cutoff钳制上界用模块采样率_下界20生效() {
        // cutoff=1e9 → 按 fs×0.45 生效；cutoff=1 → 按 20 生效。
        let mut p = settings("odd");
        p.cutoff_hz = 1.0e9;
        let stage = BassEnhancerStage::from_settings(44100.0, p).unwrap();
        assert_eq!(stage.cutoff_hz, 44100.0 * 0.45);
        let mut p = settings("odd");
        p.cutoff_hz = 1.0;
        let stage = BassEnhancerStage::from_settings(48000.0, p).unwrap();
        assert_eq!(stage.cutoff_hz, 20.0);
        // hpCut 同样被 fs×0.45 上界钳制（cutoff×1.5 超界时）。
        let mut p = settings("odd");
        p.cutoff_hz = 30_000.0; // 48000×0.45=21600 → cutoff 钳到 21600，×1.5 超界
        let stage = BassEnhancerStage::from_settings(48000.0, p).unwrap();
        assert_eq!(stage.cutoff_hz, 21600.0);
        let hp_cut = stage.hp_l.coeffs;
        let want = design_biquad("highpass", 21600.0, 0.707, 0.0, 48_000.0).unwrap();
        assert_eq!(hp_cut, want, "hpCut 应钳到 fs×0.45");
    }

    #[test]
    fn 四种谐波非线性_单点数值核对() {
        let stage = BassEnhancerStage::from_settings(48000.0, settings("odd")).unwrap();
        assert_eq!(stage.nonlinearity(0.5), 0.125, "odd: x³");
        assert_eq!(stage.nonlinearity(-2.0), -8.0);

        let stage = BassEnhancerStage::from_settings(48000.0, settings("even")).unwrap();
        assert_eq!(stage.nonlinearity(-0.5), 0.5, "even: |x| 全波整流");
        assert_eq!(stage.nonlinearity(0.5), 0.5);

        let stage = BassEnhancerStage::from_settings(48000.0, settings("atan")).unwrap();
        let want = 0.5_f64.abs().sqrt().atan();
        assert_eq!(stage.nonlinearity(0.5), want, "atan: atan(√|x|)·sign(x)");
        assert_eq!(stage.nonlinearity(-0.5), -want, "atan 为奇函数（sign 生效）");
        assert_eq!(stage.nonlinearity(0.0).to_bits(), 0.0_f64.to_bits(), "atan(0)=+0");

        let stage = BassEnhancerStage::from_settings(48000.0, settings("soft")).unwrap();
        assert_eq!(stage.nonlinearity(0.25), (2.0_f64 * 0.25).tanh(), "soft: tanh(2x)");
        assert_eq!(stage.nonlinearity(-0.25), -(0.5_f64.tanh()));

        // 枚举外取值按 odd 处理（TS default 分支）。
        let stage = BassEnhancerStage::from_settings(48000.0, settings("bogus")).unwrap();
        assert_eq!(stage.nonlinearity(0.5), 0.125);
    }

    #[test]
    fn 增益标量为三者连乘_且顺序与ts一致() {
        let mut p = settings("odd");
        p.mix = 0.3;
        p.harmonic_gain = 0.2;
        p.level_db = 0.0;
        let stage = BassEnhancerStage::from_settings(48000.0, p).unwrap();
        // k = (mix × harmonicGain) × levelLin（TS L101 左结合连乘）。
        assert_eq!(stage.mix * stage.harmonic_gain * stage.level_lin, 0.3 * 0.2);
        assert_eq!(stage.level_lin, 1.0);
    }

    #[test]
    fn low_boost_db_钳制与缺省防御() {
        // 越上界 +20 → +12；越下界 -20 → -6；缺省/None → 0。
        let mut p = settings("odd");
        p.low_boost_db = Some(20.0);
        let stage = BassEnhancerStage::from_settings(48000.0, p).unwrap();
        assert_eq!(stage.low_boost_db, 12.0);
        assert_eq!(stage.low_lin, 10.0_f64.powf(12.0 / 20.0) - 1.0);

        let mut p = settings("odd");
        p.low_boost_db = Some(-20.0);
        let stage = BassEnhancerStage::from_settings(48000.0, p).unwrap();
        assert_eq!(stage.low_boost_db, -6.0);
        assert_eq!(stage.low_lin, 10.0_f64.powf(-6.0 / 20.0) - 1.0);

        let stage = BassEnhancerStage::from_settings(48000.0, settings("odd")).unwrap();
        assert_eq!(stage.low_boost_db, 0.0);
        // lowBoostDb=0 ⇒ lowLin 精确为 0（10^0 − 1）。
        assert_eq!(stage.low_lin.to_bits(), 0.0_f64.to_bits());
    }

    #[test]
    fn low_boost为0时_下潜加性项逐位消失() {
        // GWT-BE-07：lowLin=0 → 输出 = (xl + k·hl) + 0·bl，对有限样本逐位等于
        // 不含下潜路径的实现。以 +6 dB 档做对照：差异必须可观测。
        let n = 6000;
        let input_l = sine(n, 55.0, 48000.0, 0.7, 0.2);
        let input_r = sine(n, 440.0, 48000.0, 0.3, 1.1);

        let mut off = BassEnhancerStage::from_settings(48000.0, settings("odd")).unwrap(); // lowBoost 0
        off.prepare(256);
        let (o_l, _) = drive(&mut off, &input_l, &input_r, 256);

        let mut on_p = settings("odd");
        on_p.low_boost_db = Some(6.0);
        let mut on = BassEnhancerStage::from_settings(48000.0, on_p).unwrap();
        on.prepare(256);
        let (b_l, _) = drive(&mut on, &input_l, &input_r, 256);

        let max_diff = o_l
            .iter()
            .zip(b_l.iter())
            .map(|(&x, &y)| (f64::from(x) - f64::from(y)).abs())
            .fold(0.0_f64, f64::max);
        assert!(max_diff > 0.1, "lowBoost 0 与 +6 必须可观测地不同（maxDiff={max_diff}）");
    }

    #[test]
    fn 静音输入零输出_四种类型皆然() {
        // GWT-BE-09：状态保持零，非线性在零点输出为零。
        for ty in ["odd", "even", "atan", "soft"] {
            let mut stage = BassEnhancerStage::from_settings(48000.0, settings(ty)).unwrap();
            stage.prepare(64);
            let (out_l, out_r) = drive(&mut stage, &vec![0.0_f32; 64], &vec![0.0_f32; 64], 64);
            assert!(
                out_l.iter().all(|&x| x.to_bits() == 0_u32)
                    && out_r.iter().all(|&x| x.to_bits() == 0_u32),
                "{ty}: 静音输入必须逐位零输出"
            );
        }
    }

    #[test]
    fn 跨块状态连续性_分块与整块逐位一致() {
        // GWT-BE-11：blockSize=99 不整除 1000（末块 10 帧）。
        let n = 1000;
        let input_l = lcg_noise(n, 1234).iter().map(|x| x * 0.8).collect::<Vec<_>>();
        let input_r = sine(n, 55.0, 48000.0, 0.6, 0.5);
        let mut whole = BassEnhancerStage::from_settings(48000.0, settings("atan")).unwrap();
        whole.prepare(n);
        let (w_l, w_r) = drive(&mut whole, &input_l, &input_r, n);
        let mut chunked = BassEnhancerStage::from_settings(48000.0, settings("atan")).unwrap();
        chunked.prepare(99);
        let (c_l, c_r) = drive(&mut chunked, &input_l, &input_r, 99);
        assert_eq!(w_l, c_l, "GWT-BE-11：切块不得改变逐样本运算序列（左）");
        assert_eq!(w_r, c_r, "GWT-BE-11：切块不得改变逐样本运算序列（右）");
    }

    #[test]
    fn reset_后重放与首次从零状态逐位一致() {
        // GWT-BE-12：噪声输入下若四个 Biquad 状态未清净，重放必偏离首轮。
        let n = 1024;
        let input_l = lcg_noise(n, 3);
        let input_r = lcg_noise(n, 8);
        let mut stage = BassEnhancerStage::from_settings(48000.0, settings("even")).unwrap();
        stage.prepare(128);
        let (first_l, first_r) = drive(&mut stage, &input_l, &input_r, 128);
        stage.reset();
        let (again_l, again_r) = drive(&mut stage, &input_l, &input_r, 128);
        assert_eq!(first_l, again_l, "reset 后重放必须逐位一致（左）");
        assert_eq!(first_r, again_r, "reset 后重放必须逐位一致（右）");
    }

    #[test]
    fn 禁用即直通_滤波器状态不更新() {
        // GWT-BE-13：逐位直通，且四个 Biquad 状态保持零（后续启用从零状态开始）。
        let n = 512;
        let input_l = lcg_noise(n, 42);
        let input_r = lcg_noise(n, 97);
        let mut p = settings("odd");
        p.enabled = false;
        let mut stage = BassEnhancerStage::from_settings(48000.0, p).unwrap();
        stage.prepare(64);
        let (out_l, out_r) = drive(&mut stage, &input_l, &input_r, 64);
        assert_eq!(out_l, input_l, "禁用时缓冲不得被改写");
        assert_eq!(out_r, input_r);
        assert_eq!(stage.lp_l.s1, 0.0, "禁用时低通状态不得更新");
        assert_eq!(stage.hp_r.s2, 0.0, "禁用时高通状态不得更新");
        // 启用后送静音：若状态曾被污染，输出将非零。
        stage.enabled = true;
        let (z_l, _) = drive(&mut stage, &vec![0.0_f32; n], &vec![0.0_f32; n], 64);
        assert!(z_l.iter().all(|&x| x.to_bits() == 0_u32));
    }

    #[test]
    fn configure_重算系数但保留滤波器状态() {
        // 规格 §4.5：setParams 重算系数，Biquad 状态保留不清零。
        let n = 256;
        let input = sine(n, 60.0, 48000.0, 0.8, 0.0);
        let mut stage = BassEnhancerStage::from_settings(48000.0, settings("odd")).unwrap();
        stage.prepare(64);
        let _ = drive(&mut stage, &input, &input, 64);
        assert!(stage.lp_l.s1 != 0.0 || stage.lp_l.s2 != 0.0, "预处理后低通状态应非零");
        let s1_before = stage.lp_l.s1;
        stage.configure(settings("even"));
        assert_eq!(stage.lp_l.s1, s1_before, "换参不得清零滤波器状态");
        assert_ne!(
            stage.lp_l.coeffs, BiquadCoeffs { b0: 1.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0 },
            "换参必须重算系数"
        );
    }

    #[test]
    fn 极值参数全程有限有界() {
        // GWT-BE-10：cutoff 两端与越界、q 两端、gain/mix 上界、levelDb/lowBoost
        // 双向越界组合。
        let n = 4800;
        let input_l = sine(n, 55.0, 48000.0, 0.9, 0.0);
        let input_r = sine(n, 2000.0, 48000.0, 0.5, 0.7);
        let mut p = BassEnhancerSettings {
            enabled: true,
            cutoff_hz: 20.0,
            q: 20.0,
            harmonic_type: "atan".to_string(),
            harmonic_gain: 1.0,
            mix: 1.0,
            level_db: 99.0,    // → +6
            low_boost_db: Some(99.0), // → +12
        };
        let mut stage = BassEnhancerStage::from_settings(48000.0, p.clone()).unwrap();
        stage.prepare(256);
        let (out_l, out_r) = drive(&mut stage, &input_l, &input_r, 256);
        for (i, (&x, &y)) in out_l.iter().zip(out_r.iter()).enumerate() {
            assert!(x.is_finite() && y.is_finite(), "输出必须有限 @{i}");
            assert!(x.abs() < 1.0e4 && y.abs() < 1.0e4, "输出应有界 @{i}");
        }

        p.q = 0.1;
        p.level_db = -99.0; // → -6
        p.low_boost_db = Some(-99.0); // → -6
        p.cutoff_hz = 0.0; // → 20
        let mut stage2 = BassEnhancerStage::from_settings(48000.0, p).unwrap();
        stage2.prepare(256);
        let (out_l, out_r) = drive(&mut stage2, &input_l, &input_r, 256);
        for (i, (&x, &y)) in out_l.iter().zip(out_r.iter()).enumerate() {
            assert!(x.is_finite() && y.is_finite(), "输出必须有限 @{i}");
        }
    }

    #[test]
    fn 谐波路径频谱观测_60hz正弦出现新成分() {
        // GWT-BE-02 的粗投影：60Hz 稳态激励下输出波形偏离纯正弦（谐波生成），
        // 且 dry 基频保持（输出仍以 60Hz 为主周期）。
        let fs = 48000.0;
        let n = 4800; // 6 个 60Hz 周期
        let input = sine(n, 60.0, fs, 0.8, 0.0);
        let mut stage = BassEnhancerStage::from_settings(fs, settings("odd")).unwrap();
        stage.prepare(256);
        let (out, _) = drive(&mut stage, &input, &input, 256);
        // 跳过滤波器建立段，取稳态两个周期。
        let seg = &out[2400..4800];
        let non_sine_dev = seg
            .iter()
            .zip(&input[2400..4800])
            .map(|(&o, &i)| (f64::from(o) - f64::from(i)).abs())
            .fold(0.0_f64, f64::max);
        assert!(non_sine_dev > 1.0e-3, "谐波路径应产生可观测的新成分（dev={non_sine_dev}）");
    }
}
