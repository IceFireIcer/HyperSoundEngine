//! biquad —— RBJ 双二阶滤波器阶段（Phase 1 试点模块）。
//!
//! 行为事实标准：仓库根 src/dsp/biquad.ts；规格：specs/dsp/biquad.md。
//! 立体声语义（规格 §五）：左右声道各持独立 TDF2 实例——同系数、状态独立跨块保持。
//!
//! 数值精度铁律：TS 中一切以 Number（f64）参与运算的中间量（系数、TDF2 状态 s1/s2）
//! 在此必须用 f64 复刻；仅写回 Float32Array 的样本落点取 f32。
//!
//! 与 TS 源码的逐行对应关系（biquad.ts 行号）：
//! - designBiquad（L39–L112）→ design_biquad：公式、钳制顺序、归一化与防御兜底逐一对照；
//! - Biquad.processBlock（L156–L170）→ Stage::process：TDF2 三行的求值顺序原样保留；
//! - Biquad.reset（L172–L175）→ 只清状态、不动系数。

use crate::Stage;

/// RBJ 设计并归一化后的五系数（全部 f64，对齐 TS 的 BiquadCoeffs）。
///
/// 差分方程：H(z) = (b0 + b1·z⁻¹ + b2·z⁻²) / (1 + a1·z⁻¹ + a2·z⁻²)。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BiquadCoeffs {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

impl BiquadCoeffs {
    /// TS L109 的防御性兜底直通系数（a0 非法时返回）。
    const BYPASS: Self = Self { b0: 1.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0 };
}

/// 复刻 JS Math.min(a, b) 的 NaN 传播语义。
///
/// Rust 的 f64::min 在任一操作数为 NaN 时返回另一个操作数，而 JS Math.min 只要
/// 见到 NaN 就返回 NaN——该差异会改变钳制结果并走不同的数值路径，因此显式复刻
/// JS 语义（NaN 输入最终落入 TS L109 的防御直通分支）。
fn js_min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a < b {
        a
    } else {
        b
    }
}

/// 复刻 JS Math.max(a, b) 的 NaN 传播语义（理由见 js_min）。
fn js_max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a > b {
        a
    } else {
        b
    }
}

/// RBJ 系数设计（逐行复刻 TS designBiquad，L39–L112）。
///
/// - fs <= 0 或 NaN 时报错（对齐 TS L40 抛 Error(invalid sample rate)）；
/// - f0 越界自动钳制：下限 10 Hz（过低频率 BLT 系数病态），上限 nyq×(1−1e−9)；
/// - q 下限 1e−6；gainDb 双向钳制 ±60 dB（仅 peaking/shelf 使用，其余类型忽略）；
/// - 归一化除以 a0；a0 非法（≤0 或非有限）时按 TS L109 兜底为直通系数；
/// - 枚举外类型：对齐 TS switch 无匹配时的路径——分子保持初值 0、a0 保持 1，
///   归一化后得全零系数（输出静音）。枚举外取值属非法域（规格 §三），向量不得依赖，
///   此处仅为忠实复刻。
pub fn design_biquad(
    filter_type: &str,
    f0: f64,
    q: f64,
    gain_db: f64,
    fs: f64,
) -> Result<BiquadCoeffs, String> {
    // TS L40: if (!(fs > 0)) throw new Error("invalid sample rate")
    if !(fs > 0.0) {
        return Err("invalid sample rate".to_string());
    }
    let nyq = fs / 2.0;
    // TS L44：下限 10 Hz；上限奈奎斯特内侧一个相对保护带。
    let f = js_min(js_max(f0, 10.0), nyq * (1.0 - 1e-9));
    // TS L45：q 必须为正，防除零与不稳定极点。
    let qq = js_max(q, 1e-6);
    // TS L46：增益双向钳制。
    let g = js_min(js_max(gain_db, -60.0), 60.0);

    // TS L48–L51：BLT 预畸变角频率与共用 α。
    let w0 = (2.0 * std::f64::consts::PI * f) / fs;
    let cosw = w0.cos();
    let sinw = w0.sin();
    let alpha = sinw / (2.0 * qq);

    // TS L53：先给全套初值（未知类型的全零系数语义正来源于这些初值）。
    let (mut b0, mut b1, mut b2) = (0.0_f64, 0.0_f64, 0.0_f64);
    let mut a0 = 1.0_f64;
    let (mut a1, mut a2) = (0.0_f64, 0.0_f64);

    // TS L54–L106：八种类型的公式逐项照抄（含括号分组与求值顺序）。
    match filter_type {
        "lowpass" => {
            b0 = (1.0 - cosw) / 2.0;
            b1 = 1.0 - cosw;
            b2 = (1.0 - cosw) / 2.0;
            a0 = 1.0 + alpha;
            a1 = -2.0 * cosw;
            a2 = 1.0 - alpha;
        }
        "highpass" => {
            b0 = (1.0 + cosw) / 2.0;
            b1 = -(1.0 + cosw);
            b2 = (1.0 + cosw) / 2.0;
            a0 = 1.0 + alpha;
            a1 = -2.0 * cosw;
            a2 = 1.0 - alpha;
        }
        "bandpass" => {
            // 常数 0 dB 峰值增益型（RBJ 两种带通之一）。
            b0 = alpha;
            b1 = 0.0;
            b2 = -alpha;
            a0 = 1.0 + alpha;
            a1 = -2.0 * cosw;
            a2 = 1.0 - alpha;
        }
        "notch" => {
            b0 = 1.0;
            b1 = -2.0 * cosw;
            b2 = 1.0;
            a0 = 1.0 + alpha;
            a1 = -2.0 * cosw;
            a2 = 1.0 - alpha;
        }
        "allpass" => {
            b0 = 1.0 - alpha;
            b1 = -2.0 * cosw;
            b2 = 1.0 + alpha;
            a0 = 1.0 + alpha;
            a1 = -2.0 * cosw;
            a2 = 1.0 - alpha;
        }
        "peaking" => {
            let a_gain = 10.0_f64.powf(g / 40.0);
            b0 = 1.0 + alpha * a_gain;
            b1 = -2.0 * cosw;
            b2 = 1.0 - alpha * a_gain;
            a0 = 1.0 + alpha / a_gain;
            a1 = -2.0 * cosw;
            a2 = 1.0 - alpha / a_gain;
        }
        "lowshelf" => {
            let a_gain = 10.0_f64.powf(g / 40.0);
            // S=1（默认斜率）时 α_shelf = sin(w0)/2·√2。
            let ashelf = (sinw / 2.0) * std::f64::consts::SQRT_2;
            let sq_a = a_gain.sqrt();
            b0 = a_gain * ((a_gain + 1.0) - (a_gain - 1.0) * cosw + 2.0 * sq_a * ashelf);
            b1 = 2.0 * a_gain * ((a_gain - 1.0) - (a_gain + 1.0) * cosw);
            b2 = a_gain * ((a_gain + 1.0) - (a_gain - 1.0) * cosw - 2.0 * sq_a * ashelf);
            a0 = (a_gain + 1.0) + (a_gain - 1.0) * cosw + 2.0 * sq_a * ashelf;
            a1 = -2.0 * ((a_gain - 1.0) + (a_gain + 1.0) * cosw);
            a2 = (a_gain + 1.0) + (a_gain - 1.0) * cosw - 2.0 * sq_a * ashelf;
        }
        "highshelf" => {
            let a_gain = 10.0_f64.powf(g / 40.0);
            let ashelf = (sinw / 2.0) * std::f64::consts::SQRT_2;
            let sq_a = a_gain.sqrt();
            b0 = a_gain * ((a_gain + 1.0) + (a_gain - 1.0) * cosw + 2.0 * sq_a * ashelf);
            b1 = -2.0 * a_gain * ((a_gain - 1.0) + (a_gain + 1.0) * cosw);
            b2 = a_gain * ((a_gain + 1.0) + (a_gain - 1.0) * cosw - 2.0 * sq_a * ashelf);
            a0 = (a_gain + 1.0) - (a_gain - 1.0) * cosw + 2.0 * sq_a * ashelf;
            a1 = 2.0 * ((a_gain - 1.0) - (a_gain + 1.0) * cosw);
            a2 = (a_gain + 1.0) - (a_gain - 1.0) * cosw - 2.0 * sq_a * ashelf;
        }
        // 未知类型：保持 TS 初值（全零分子 + a0=1），见函数级文档。
        _ => {}
    }

    // TS L109：归一化前的防御性兜底（正常参数域不可达）。
    if !(a0 > 0.0) || !a0.is_finite() {
        return Ok(BiquadCoeffs::BYPASS);
    }
    // TS L110–L111：除以 a0 完成归一化。
    let inv = 1.0 / a0;
    Ok(BiquadCoeffs {
        b0: b0 * inv,
        b1: b1 * inv,
        b2: b2 * inv,
        a1: a1 * inv,
        a2: a2 * inv,
    })
}

/// 一个已就绪的双二阶阶段：单组系数 + 左右两份独立 TDF2 状态。
///
/// 参数快照不落地存储——与 TS Biquad 一致，构造（或未来的 setParams）只把参数
/// 折算进系数；后续若需要运行期参数更新，在此追加快照字段即可。
pub struct BiquadStage {
    /// 归一化后的五系数（f64，跨左右共用）。
    coeffs: BiquadCoeffs,
    /// 左声道 TDF2 状态 s1/s2。
    s1_l: f64,
    s2_l: f64,
    /// 右声道 TDF2 状态 s1/s2（与左完全独立，规格 §五）。
    s1_r: f64,
    s2_r: f64,
}

impl BiquadStage {
    /// 构造并按 TS 语义计算初始系数（参数钳制规则见规格参数表）。
    ///
    /// filter_type 取 TS BiquadType 的八种字符串之一：
    /// peaking / lowshelf / highshelf / lowpass / highpass / bandpass / notch / allpass。
    pub fn new(sample_rate: f64, filter_type: &str, f0: f64, q: f64, gain_db: f64) -> Result<Self, String> {
        if !(sample_rate.is_finite() && sample_rate > 0.0) {
            return Err("sampleRate 必须为正有限数".into());
        }
        let coeffs = design_biquad(filter_type, f0, q, gain_db, sample_rate)?;
        // TS：构造即零状态（L129 调用 reset）。
        Ok(Self { coeffs, s1_l: 0.0, s2_l: 0.0, s1_r: 0.0, s2_r: 0.0 })
    }

    /// 当前归一化系数快照（诊断/测试用途）。
    pub fn coeffs(&self) -> BiquadCoeffs {
        self.coeffs
    }

    /// TDF2 单样本递推（逐行复刻 TS process，L149–L154）。
    ///
    /// 求值顺序即行为：y = b0·x + s1；s1 = b1·x − a1·y + s2（先减后加 s2）；
    /// s2 = b2·x − a2·y。输入样本 f32→f64 后全程双精度运算，输出写回落点转 f32
    /// （等价于向 Float32Array 写入的 round-to-nearest-even）。
    #[inline]
    fn tick(coeffs: &BiquadCoeffs, s1: &mut f64, s2: &mut f64, x: f32) -> f32 {
        let xf = f64::from(x);
        let y = coeffs.b0 * xf + *s1;
        *s1 = coeffs.b1 * xf - coeffs.a1 * y + *s2;
        *s2 = coeffs.b2 * xf - coeffs.a2 * y;
        y as f32
    }
}

impl Stage for BiquadStage {
    /// 本模块无内部工作缓冲（TDF2 状态仅两个标量、随阶段常驻），无需预分配；
    /// 保留 max_block_size 形参以符合 Stage 契约的调用时序语义。
    fn prepare(&mut self, _max_block_size: usize) {}

    /// 就地处理一个立体声块：左右各一份独立 TDF2 状态递推（同系数、互不串扰），
    /// 状态跨块保持；块长可短于 prepare 声明的最大值（末块短块语义）。
    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        debug_assert_eq!(left.len(), right.len(), "左右声道块长必须一致（Stage 契约）");
        // 局部缓存系数与状态（对齐 TS L158–L160 的解构 + 局部变量递推）。
        let coeffs = self.coeffs;
        let (mut s1, mut s2) = (self.s1_l, self.s2_l);
        for sample in left.iter_mut() {
            *sample = Self::tick(&coeffs, &mut s1, &mut s2, *sample);
        }
        self.s1_l = s1;
        self.s2_l = s2;

        let (mut s1, mut s2) = (self.s1_r, self.s2_r);
        for sample in right.iter_mut() {
            *sample = Self::tick(&coeffs, &mut s1, &mut s2, *sample);
        }
        self.s1_r = s1;
        self.s2_r = s2;
    }

    /// 清零左右两份 TDF2 状态；系数保留（对齐 TS reset，L172–L175）。
    fn reset(&mut self) {
        self.s1_l = 0.0;
        self.s2_l = 0.0;
        self.s1_r = 0.0;
        self.s2_r = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 与 Node 直接执行 TS 源码取得的黄金参考比对（相对容差 1e-12：
    /// 远宽于跨库 libm 1~2 ulp 差异，远严于任何公式/钳制错误）。
    fn assert_coeffs_close(actual: BiquadCoeffs, want: [f64; 5], label: &str) {
        let names = ["b0", "b1", "b2", "a1", "a2"];
        let vals = [actual.b0, actual.b1, actual.b2, actual.a1, actual.a2];
        for (i, w) in want.iter().enumerate() {
            let scale = w.abs().max(1e-300);
            assert!(
                (vals[i] - w).abs() <= 1e-12 * scale,
                "{}.{}：got {}，want {}",
                label,
                names[i],
                vals[i],
                w
            );
        }
    }

    /// 黄金参考：node 直跑仓库根 src/dsp/biquad.ts 导出（JSON 最短往返表示）。
    mod golden {
        pub const PEAKING_1K_Q12_DB4_48K: [f64; 5] =
            [1.0242211803989167, -1.9007757055135743, 0.892956243749246, -1.9007757055135743, 0.9171774241481628];
        pub const LOWPASS_10HZ_Q0707_48K: [f64; 5] =
            [4.279719945670057e-7, 8.559439891340114e-7, 4.279719945670057e-7, -1.9981485202262093, 0.9981502321141874];
        pub const NOTCH_60HZ_Q8_48K: [f64; 5] =
            [0.9995093720284218, -1.9989570896106674, 0.9995093720284218, -1.9989570896106674, 0.9990187440568437];
        pub const LOWSHELF_300HZ_DB6_48K: [f64; 5] =
            [1.0096825329078238, -1.9527478321240501, 0.9451938509263544, -1.9532787065074981, 0.9543455094507296];
        pub const HIGHSHELF_8K_DBM12_48K: [f64; 5] =
            [0.40568213684880555, -0.0964143506718329, 0.07366185483026001, -0.9546028024758237, 0.33753244348305655];
        pub const BANDPASS_500HZ_Q2_44K1: [f64; 5] =
            [0.017483317066548147, 0.0, -0.017483317066548147, -1.9600493567583055, 0.9650333658669037];
        pub const ALLPASS_1200HZ_Q05_48K: [f64; 5] =
            [0.729453817281745, -1.7081613709269332, 1.0, -1.7081613709269332, 0.729453817281745];
        pub const HIGHPASS_80HZ_Q071_48K: [f64; 5] =
            [0.9926522745967857, -1.9853045491935715, 0.9926522745967857, -1.9852501199473345, 0.9853589784398085];
        /// new Biquad(peaking,1000,1.2,4,48000) 对固定输入序列的 process 返回值
        /// （f64 精度，写 Float32Array 之前的数值）。
        pub const TDF2_PEAKING_SEQ: [f64; 8] = [
            0.5121105901994584,
            -0.23303577946916793,
            0.777192913423656,
            1.0664049046073152,
            -0.9411069625825662,
            0.15484384253936598,
            -0.8692612659509191,
            0.04452212385902522,
        ];
    }

    #[test]
    fn peaking_1k_加4db_五系数命中_ts黄金参考() {
        let c = design_biquad("peaking", 1000.0, 1.2, 4.0, 48000.0).expect("合法参数");
        assert_coeffs_close(c, golden::PEAKING_1K_Q12_DB4_48K, "peaking");
    }

    #[test]
    fn 其余七种类型_系数命中_ts黄金参考() {
        let cases: [(&str, f64, f64, f64, f64, [f64; 5]); 7] = [
            ("lowpass", 10.0, 0.707, 0.0, 48000.0, golden::LOWPASS_10HZ_Q0707_48K),
            ("notch", 60.0, 8.0, 0.0, 48000.0, golden::NOTCH_60HZ_Q8_48K),
            ("lowshelf", 300.0, 0.9, 6.0, 48000.0, golden::LOWSHELF_300HZ_DB6_48K),
            ("highshelf", 8000.0, 0.8, -12.0, 48000.0, golden::HIGHSHELF_8K_DBM12_48K),
            ("bandpass", 500.0, 2.0, 99.0, 44100.0, golden::BANDPASS_500HZ_Q2_44K1),
            ("allpass", 1200.0, 0.5, 77.0, 48000.0, golden::ALLPASS_1200HZ_Q05_48K),
            ("highpass", 80.0, 0.71, 0.0, 48000.0, golden::HIGHPASS_80HZ_Q071_48K),
        ];
        for (i, (ty, f0, q, gdb, fs, want)) in cases.into_iter().enumerate() {
            let c = design_biquad(ty, f0, q, gdb, fs).expect("合法参数");
            assert_coeffs_close(c, want, &format!("case{}:{}", i, ty));
        }
    }

    #[test]
    fn tdf2_递推序列_命中_ts黄金参考() {
        let mut stage = BiquadStage::new(48000.0, "peaking", 1000.0, 1.2, 4.0).expect("合法参数");
        stage.prepare(8);
        // 输入全部是 2 的幂次，f32 表示精确无舍入。
        let xs = [0.5_f32, -0.25, 0.75, 1.0, -1.0, 0.125, -0.875, 0.0625];
        let mut left = xs.to_vec();
        let mut right = vec![0.0_f32; 8];
        stage.process(&mut left, &mut right);
        for (i, want) in golden::TDF2_PEAKING_SEQ.iter().enumerate() {
            let got = f64::from(left[i]);
            // 写回 f32 的量化误差在 2^-24 相对量级，1e-6 相对容差余量充足；
            // 递推顺序错误（如先更新 s1 再算 y）会产生 O(1) 级偏差必被抓住。
            assert!(
                (got - want).abs() <= 1e-6 * want.abs(),
                "y[{}]：got {}，want {}",
                i,
                got,
                want
            );
            // 右声道喂零：静入零出（GWT-BQ-09 的单测投影）。
            assert_eq!(right[i], 0.0, "右声道零输入必须逐位零输出 @{}", i);
        }
    }

    #[test]
    fn f0_低于下限钳到10hz_与直接按10hz设计逐位一致() {
        let clamped = design_biquad("lowpass", 5.0, 0.707, 0.0, 48000.0).expect("合法参数");
        let direct = design_biquad("lowpass", 10.0, 0.707, 0.0, 48000.0).expect("合法参数");
        assert_eq!(clamped, direct, "f0=5 必须精确等效 f0=10（case2 锚点）");
    }

    #[test]
    fn f0_越过奈奎斯特钳到边界内侧() {
        let boundary = 24000.0 * (1.0 - 1e-9);
        let clamped = design_biquad("peaking", 1.0e18, 1.0, 0.0, 48000.0).expect("合法参数");
        let direct = design_biquad("peaking", boundary, 1.0, 0.0, 48000.0).expect("合法参数");
        assert_eq!(clamped, direct);
    }

    #[test]
    fn q与gain越界时钳制后逐位等效() {
        let q_clamped = design_biquad("peaking", 1000.0, -3.0, 4.0, 48000.0).expect("合法参数");
        let q_direct = design_biquad("peaking", 1000.0, 1e-6, 4.0, 48000.0).expect("合法参数");
        assert_eq!(q_clamped, q_direct, "q=-3 必须精确等效 q=1e-6");

        let g_up = design_biquad("peaking", 1000.0, 1.2, 120.0, 48000.0).expect("合法参数");
        let g_up_direct = design_biquad("peaking", 1000.0, 1.2, 60.0, 48000.0).expect("合法参数");
        assert_eq!(g_up, g_up_direct, "+120 dB 必须精确等效 +60 dB");

        let g_dn = design_biquad("lowshelf", 300.0, 1.0, -120.0, 48000.0).expect("合法参数");
        let g_dn_direct = design_biquad("lowshelf", 300.0, 1.0, -60.0, 48000.0).expect("合法参数");
        assert_eq!(g_dn, g_dn_direct, "-120 dB 必须精确等效 -60 dB");

        // 非 shelf/peaking 类型忽略 gainDb：任意增益与 0 同系数。
        let lp_g = design_biquad("lowpass", 1000.0, 1.0, 77.0, 48000.0).expect("合法参数");
        let lp_g0 = design_biquad("lowpass", 1000.0, 1.0, 0.0, 48000.0).expect("合法参数");
        assert_eq!(lp_g, lp_g0);
    }

    #[test]
    fn 低通高通的直流与奈奎斯特增益行为() {
        // 解析断言：lowpass 直流增益恒为 1、奈奎斯特增益恒为 0（分子和精确抵消）。
        let c = design_biquad("lowpass", 1000.0, 0.707, 0.0, 48000.0).expect("合法参数");
        let dc = (c.b0 + c.b1 + c.b2) / (1.0 + c.a1 + c.a2);
        let nyq = (c.b0 - c.b1 + c.b2) / (1.0 - c.a1 + c.a2);
        assert!((dc - 1.0).abs() < 1e-12, "lowpass 直流增益应为 1，实际 {}", dc);
        assert_eq!(nyq, 0.0, "lowpass 奈奎斯特增益应精确为 0，实际 {}", nyq);

        // 时域断言：直流激励收敛到 1。
        let n = 48000_usize;
        let mut lp = BiquadStage::new(48000.0, "lowpass", 1000.0, 0.707, 0.0).expect("合法参数");
        lp.prepare(n);
        let mut l = vec![1.0_f32; n];
        let mut r = vec![1.0_f32; n];
        lp.process(&mut l, &mut r);
        let tail = f64::from(l[n - 1]);
        assert!((tail - 1.0).abs() < 1e-5, "lowpass 直流稳态应回到 1，实际 {}", tail);

        // highpass 镜像性质：直流增益解析为 0，直流激励稳态衰减到 0。
        let hp = design_biquad("highpass", 80.0, 0.71, 0.0, 48000.0).expect("合法参数");
        let hp_dc = (hp.b0 + hp.b1 + hp.b2) / (1.0 + hp.a1 + hp.a2);
        assert!(hp_dc.abs() < 1e-12, "highpass 直流增益应为 0，实际 {}", hp_dc);
        let mut hp_stage = BiquadStage::new(48000.0, "highpass", 80.0, 0.71, 0.0).expect("合法参数");
        hp_stage.prepare(n);
        let mut hl = vec![1.0_f32; n];
        let mut hr = vec![1.0_f32; n];
        hp_stage.process(&mut hl, &mut hr);
        assert!(
            f64::from(hl[n - 1]).abs() < 1e-5,
            "highpass 直流稳态应衰减到 0，实际 {}",
            hl[n - 1]
        );
    }

    #[test]
    fn 静音输入零输出且_reset_后状态归零() {
        let mut stage = BiquadStage::new(48000.0, "peaking", 1000.0, 1.2, 4.0).expect("合法参数");
        stage.prepare(64);
        // 先污染状态再复位。
        let mut noisy = vec![0.5_f32; 64];
        let mut sink = vec![0.0_f32; 64];
        stage.process(&mut noisy, &mut sink);
        stage.reset();

        let mut l = vec![0.0_f32; 64];
        let mut r = vec![0.0_f32; 64];
        stage.process(&mut l, &mut r);
        for i in 0..64 {
            assert_eq!(l[i], 0.0, "GWT-BQ-09：静音输入必须逐位零输出 @{}", i);
            assert_eq!(r[i], 0.0);
        }
    }

    #[test]
    fn reset_后重放同一输入逐位复现首次输出() {
        let mut stage = BiquadStage::new(48000.0, "notch", 60.0, 8.0, 0.0).expect("合法参数");
        stage.prepare(97);
        let input: Vec<f32> = (0..256)
            .map(|i| (i as f64 * 0.113).sin() as f32 * 0.8)
            .collect();
        let mut l1 = input.clone();
        let mut r1 = input.clone();
        stage.process(&mut l1, &mut r1);

        stage.reset();
        let mut l2 = input.clone();
        let mut r2 = input.clone();
        stage.process(&mut l2, &mut r2);
        assert_eq!(l1, l2, "GWT-BQ-12：reset 后重放必须逐位一致");
        assert_eq!(r1, r2);
    }

    #[test]
    fn 分块处理与整块处理逐位一致_含末尾短块() {
        let n = 97_usize;
        let input: Vec<f32> = (0..n)
            .map(|i| (i as f64 * 0.37).sin() as f32 * 0.707_106_78_f32)
            .collect();

        let mut whole = BiquadStage::new(48000.0, "notch", 60.0, 8.0, 0.0).expect("合法参数");
        whole.prepare(n);
        let mut wl = input.clone();
        let mut wr = input.clone();
        whole.process(&mut wl, &mut wr);

        let mut chunked = BiquadStage::new(48000.0, "notch", 60.0, 8.0, 0.0).expect("合法参数");
        chunked.prepare(30);
        let mut cl = input.clone();
        let mut cr = input.clone();
        let mut off = 0_usize;
        for len in [30_usize, 30, 30, 7] {
            chunked.process(&mut cl[off..off + len], &mut cr[off..off + len]);
            off += len;
        }
        assert_eq!(off, n);
        assert_eq!(wl, cl, "GWT-BQ-11：切块不得改变逐样本运算序列（左）");
        assert_eq!(wr, cr, "GWT-BQ-11：切块不得改变逐样本运算序列（右）");
    }

    #[test]
    fn 左右声道状态互相独立_无串扰() {
        let n = 128_usize;
        let sig_l: Vec<f32> = (0..n).map(|i| (i as f64 * 0.05).sin() as f32).collect();
        let sig_r: Vec<f32> = (0..n).map(|i| (i as f64 * 0.21).cos() as f32).collect();

        let mut stereo = BiquadStage::new(48000.0, "peaking", 1000.0, 1.2, 4.0).expect("合法参数");
        stereo.prepare(n);
        let mut l = sig_l.clone();
        let mut r = sig_r.clone();
        stereo.process(&mut l, &mut r);

        // 各自单独跑同信号，应与立体声运行中对应声道逐位一致。
        let mut mono_l = BiquadStage::new(48000.0, "peaking", 1000.0, 1.2, 4.0).expect("合法参数");
        mono_l.prepare(n);
        let mut ml = sig_l.clone();
        let mut sink = vec![0.0_f32; n];
        mono_l.process(&mut ml, &mut sink);
        assert_eq!(l, ml, "左声道不受右声道输入影响");

        let mut mono_r = BiquadStage::new(48000.0, "peaking", 1000.0, 1.2, 4.0).expect("合法参数");
        mono_r.prepare(n);
        let mut mr = sig_r.clone();
        let mut sink2 = vec![0.0_f32; n];
        mono_r.process(&mut sink2, &mut mr);
        assert_eq!(r, mr, "右声道不受左声道输入影响");
    }

    #[test]
    fn 极端钳制参数下满幅输入有界且有限() {
        // GWT-BQ-08/10 投影：f0 上限 + q 下限 + 增益上限同时越界。
        let mut stage =
            BiquadStage::new(48000.0, "peaking", 1.0e18, 0.0, 120.0).expect("合法参数");
        stage.prepare(512);
        let input: Vec<f32> = (0..512).map(|i| (i as f64 * 0.31).sin() as f32).collect();
        let mut l = input.clone();
        let mut r = input.clone();
        stage.process(&mut l, &mut r);
        for i in 0..512 {
            assert!(l[i].is_finite() && r[i].is_finite(), "输出必须有限 @{}", i);
            assert!(
                l[i].abs() < 1.0e6 && r[i].abs() < 1.0e6,
                "输出应有界 @{}",
                i
            );
        }
    }

    #[test]
    fn nan_参数落入防御直通分支() {
        // TS L109：a0 非有限时兜底直通（b0=1，其余 0）。
        let f0_nan =
            design_biquad("peaking", f64::NAN, 1.2, 4.0, 48000.0).expect("NaN 参数走兜底而非报错");
        assert_eq!(f0_nan, BiquadCoeffs::BYPASS);
        let q_nan =
            design_biquad("peaking", 1000.0, f64::NAN, 4.0, 48000.0).expect("NaN 参数走兜底而非报错");
        assert_eq!(q_nan, BiquadCoeffs::BYPASS);
    }

    #[test]
    fn 未知类型保持_ts_switch_直通语义的全零系数() {
        // TS：switch 无匹配时分子保持初值 0、a0 保持 1，归一化后全零（输出静音）。
        // 枚举外取值属非法域（规格 §三），本测试仅固化移植语义。
        let c = design_biquad("bogus", 1000.0, 1.0, 0.0, 48000.0).expect("未知类型不报错");
        assert_eq!(c.b0, 0.0);
        assert_eq!(c.b1, 0.0);
        assert_eq!(c.b2, 0.0);
        assert_eq!(c.a1, 0.0);
        assert_eq!(c.a2, 0.0);
    }

    #[test]
    fn 非法采样率报错_构造器与设计函数两条路径() {
        // 构造器：骨架锁定的校验口径（正且有限）。
        for bad in [0.0_f64, -44100.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let err = BiquadStage::new(bad, "peaking", 1000.0, 1.0, 0.0)
                .err()
                .expect("非法采样率必须报错");
            assert!(
                err.contains("必须为正有限数"),
                "错误信息应说明原因：{}",
                err
            );
        }
        // design_biquad 对齐 TS 抛 invalid sample rate（fs<=0 与 NaN；+inf 按 TS 源码放行）。
        for bad in [0.0_f64, -44100.0, f64::NAN] {
            let err = design_biquad("peaking", 1000.0, 1.0, 0.0, bad)
                .err()
                .expect("非法采样率必须报错");
            assert!(
                err.contains("invalid sample rate"),
                "应对齐 TS 错误信息：{}",
                err
            );
        }
    }

    #[test]
    fn 构造器立即按参数折算初始系数() {
        let stage = BiquadStage::new(48000.0, "peaking", 1000.0, 1.2, 4.0).expect("合法参数");
        let want = design_biquad("peaking", 1000.0, 1.2, 4.0, 48000.0).expect("合法参数");
        assert_eq!(stage.coeffs(), want);
        // 初始状态为零。
        assert_eq!(stage.s1_l, 0.0);
        assert_eq!(stage.s2_l, 0.0);
        assert_eq!(stage.s1_r, 0.0);
        assert_eq!(stage.s2_r, 0.0);
    }
}
