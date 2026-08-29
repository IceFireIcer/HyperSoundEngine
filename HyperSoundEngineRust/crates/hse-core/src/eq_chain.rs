//! eq_chain —— 多段参数 EQ 级联 + 级联 Q 补偿。
//!
//! 行为事实标准：仓库根 `src/dsp/EqChain.ts`；规格：`specs/dsp/eq-chain.md`。
//! 本文件是该 TS 类的逐行移植，关键对应关系：
//!
//! - 级联结构（规格 §4.1）：bandCount 段 RBJ peaking biquad 依段序串联；系数设计
//!   复用 [`crate::biquad::design_biquad`]（与 TS 共用 `designBiquad` 事实标准）。
//! - Q 补偿（规格 §4.3，自研）：在控制频点测量整条级联幅频响应（含填充段），
//!   以用户目标增益为固定基准做 dB 误差修正——0.8 阻尼、Gauss-Seidel 式逐段
//!   立即更新系数、至多 5 轮、单轮最大误差 <0.05 dB 提前终止；增益全程按 ±24 再钳制。
//! - 立体声共享状态（规格 §4.4，模块特有）：左右声道**共用同一条级联滤波状态**；
//!   每次 [`Stage::process`] 先整条跑完 L 块、再整条跑完 R 块（TS `processStereo`
//!   对同一组 biquad 先 `processBlock(l, l)` 后 `processBlock(r, r)`）。输出因此
//!   依赖块长，对拍必须按冻结向量的 blockSize 回放同一声道排列。
//! - f32 落点（规格 §4.4）：每段输出写回 f32 后才进入下一段（级联段间信号经 f32
//!   量化）；段内 TDF2 状态 s1/s2 与全部系数为 f64。
//!
//! # 数值精度铁律的落点
//!
//! - 一切以 TS Number（f64）参与运算的中间量（频率/增益/Q、补偿迭代、级联幅度
//!   测量、TDF2 状态）全部用 f64 复刻，运算顺序与 TS 逐行一致；
//! - `Math.min`/`Math.max` 的 NaN 传播语义以 [`js_min`]/[`js_max`] 显式复刻
//!   （理由同 biquad.rs）；`Math.hypot` 以 [`f64::hypot`] 对应（跨库 1 ulp 级差异
//!   远小于对拍容差）。
//!
//! # 实时安全
//!
//! `process` 稳态零分配、零锁、零系统调用；全部段系数/状态在构造与 `set_bands`
//! （非实时路径）中定容分配，[`Stage::prepare`] 无需额外预分配。

use crate::biquad::{design_biquad, BiquadCoeffs};
use crate::Stage;

/// 对齐 TS `EqBandParam` 的单段参数（字段名蛇形转换）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EqBandParam {
    pub frequency: f64,
    pub gain: f64,
    pub q: f64,
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

/// TS biquad.ts L109 的防御性兜底直通系数（a0 非法时返回；与 biquad.rs 的
/// `BYPASS` 同值，该常量未公开，故按同字段本地定义）。
const BYPASS: BiquadCoeffs = BiquadCoeffs { b0: 1.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0 };

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

/// 在给定频率处求单段 |H(e^{jw})|（线性幅度）——逐行复刻 TS `Biquad.magnitudeAt`
/// （biquad.ts L178–L192；hse-core 的 BiquadStage 未暴露该分析接口，按规格在此
/// 为级联补偿复刻，公式与钳制顺序一字不差）。
fn magnitude_at(coeffs: &BiquadCoeffs, freq_hz: f64, fs: f64) -> f64 {
    // TS L180：分析频率钳制下限 1e-6、上限 nyq×(1−1e−9)。
    let f = js_min(js_max(freq_hz, 1e-6), (fs / 2.0) * (1.0 - 1e-9));
    let w = (2.0 * std::f64::consts::PI * f) / fs;
    let cw = w.cos();
    let sw = w.sin();
    let c2w = (2.0 * w).cos();
    let s2w = (2.0 * w).sin();
    // TS L187–L190：H(e^{jw}) = (b0 + b1·e^{-jw} + b2·e^{-j2w}) / (1 + a1·e^{-jw} + a2·e^{-j2w})。
    let br = coeffs.b0 + coeffs.b1 * cw + coeffs.b2 * c2w;
    let bi = -(coeffs.b1 * sw + coeffs.b2 * s2w);
    let ar = 1.0 + coeffs.a1 * cw + coeffs.a2 * c2w;
    let ai = -(coeffs.a1 * sw + coeffs.a2 * s2w);
    // TS L191：Math.hypot(br, bi) / Math.hypot(ar, ai)。
    br.hypot(bi) / ar.hypot(ai)
}

/// 一个已配置的多段 EQ 级联阶段（字段一一对应 TS `EqChain` 私有域）。
pub struct EqChainStage {
    fs: f64,
    band_count: usize,
    coeffs: Vec<BiquadCoeffs>,
    /// 每段一组 TDF2 状态 (s1, s2)，**左右声道共享**（规格 §4.4）。
    states: Vec<(f64, f64)>,
    /// 当前频段参数（gains 为补偿后的实际增益；user_gains 为用户目标，补偿基准不变）。
    freqs: Vec<f64>,
    gains: Vec<f64>,
    user_gains: Vec<f64>,
    qs: Vec<f64>,
    /// 用户实际设置的段数（<= bandCount）；超出部分为直通填充段，不参与补偿。
    active_count: usize,
    q_compensation_enabled: bool,
}

impl EqChainStage {
    /// 按 TS 构造函数语义创建：`bandCount 生效值 = max(1, floor(bandCount))`；
    /// 初始每段为 peaking 1000Hz / Q1 / 0dB 的零增益直通段，`active_count = 0`、
    /// 补偿关闭。fs ≤ 0 或非有限时报错（对齐 TS `Error('invalid sample rate')`）。
    pub fn new(sample_rate: f64, band_count: f64) -> Result<Self, String> {
        if !(sample_rate.is_finite() && sample_rate > 0.0) {
            return Err("invalid sample rate".into());
        }
        // TS L48：Math.max(1, Math.floor(bandCount ?? 20))。向量域为 ≥1 的整数；
        // NaN 按复刻语义落入 0 段（空级联 = 直通，与 TS 循环不执行的路径一致）。
        let count = js_max(1.0, band_count.floor());
        let count = count as usize;
        // TS L50：每段 new Biquad('peaking', 1000, 1, 0, fs)——同参构造产生同系数。
        let coeffs = design_biquad("peaking", 1000.0, 1.0, 0.0, sample_rate)
            .unwrap_or(BYPASS);
        Ok(Self {
            fs: sample_rate,
            band_count: count,
            coeffs: vec![coeffs; count],
            states: vec![(0.0, 0.0); count],
            freqs: vec![1000.0; count],
            gains: vec![0.0; count],
            user_gains: vec![0.0; count],
            qs: vec![1.0; count],
            active_count: 0,
            q_compensation_enabled: false,
        })
    }

    /// 对齐 TS `setBands`（L64–L84）：钳制写入、填充段回退、按需补偿、重算系数。
    ///
    /// `bands` 短于 bandCount 时尾部为填充段（freq=1000 / gain=0 / q=1）；
    /// 长于 bandCount 时多出段被忽略。`active_count = min(bands.len(), band_count)`。
    pub fn set_bands(&mut self, bands: &[EqBandParam]) {
        let n = self.band_count;
        // TS L66：频率上限 = min(20000, fs/2 × 0.999)。
        let fmax = js_min(20000.0, (self.fs / 2.0) * 0.999);
        for i in 0..n {
            match bands.get(i) {
                Some(b) => {
                    // TS L70–L73：clamp 顺序 = 先 max 后 min，增益补偿从用户目标出发。
                    self.freqs[i] = js_min(js_max(b.frequency, 20.0), fmax);
                    self.user_gains[i] = js_min(js_max(b.gain, -24.0), 24.0);
                    self.gains[i] = self.user_gains[i];
                    self.qs[i] = js_min(js_max(b.q, 0.1), 18.0);
                }
                None => {
                    // TS L74–L79：填充段回退值。
                    self.freqs[i] = 1000.0;
                    self.user_gains[i] = 0.0;
                    self.gains[i] = 0.0;
                    self.qs[i] = 1.0;
                }
            }
        }
        self.active_count = bands.len().min(n);
        if self.q_compensation_enabled {
            self.compensate();
        }
        self.update_coeffs();
    }

    /// 对齐 TS `setQCompensation`（L86–L93）：开启时补偿迭代 + 重算系数；
    /// 关闭时**仅翻标志**（已补偿的增益与系数保留到下一次 setBands，规格 §4.3
    /// 关闭语义）；状态不变时为空操作。
    pub fn set_q_compensation(&mut self, enabled: bool) {
        if self.q_compensation_enabled == enabled {
            return;
        }
        self.q_compensation_enabled = enabled;
        if enabled {
            self.compensate();
            self.update_coeffs();
        }
    }

    /// 级联 Q 补偿——逐行复刻 TS `compensate`（L107–L131），语义见规格 §4.3。
    fn compensate(&mut self) {
        let n = self.band_count;
        let m0 = self.active_count; // 只补偿用户实际设置的段；填充段保持 0dB 直通
        // TS L111：先用当前（用户初始）增益同步各段系数，保证首轮测量基于正确状态。
        self.update_coeffs();
        for _iter in 0..5 {
            let mut max_err_db = 0.0_f64;
            for i in 0..m0 {
                // ① 在当前系数下测量段 i 控制频点处的整条级联响应（含填充段）。
                let mut mag = 1.0_f64;
                for j in 0..n {
                    mag *= magnitude_at(&self.coeffs[j], self.freqs[i], self.fs);
                }
                // ②③ 以用户目标为基准做 dB 误差修正（0.8 阻尼）并立即更新该段系数
                //     （Gauss-Seidel：后面的段直接看到前面的修正）。
                let target = 10.0_f64.powf(self.user_gains[i] / 20.0);
                let m = js_max(mag, 1e-12);
                let err_db = 20.0 * (target / m).log10();
                self.gains[i] = js_min(js_max(self.gains[i] + 0.8 * err_db, -24.0), 24.0);
                // fs 在构造时已校验为正，design_biquad 在此不可能失败；
                // BYPASS 兜底仅为不 panic 的防御分支（对齐 TS 同路径不可达）。
                self.coeffs[i] = design_biquad("peaking", self.freqs[i], self.qs[i], self.gains[i], self.fs)
                    .unwrap_or(BYPASS);
                let a = err_db.abs();
                if a > max_err_db {
                    max_err_db = a;
                }
            }
            // TS L129：单轮最大误差 <0.05 dB 提前终止。
            if max_err_db < 0.05 {
                break;
            }
        }
    }

    /// 对齐 TS `updateCoeffs`（L133–L138）：按当前参数重算全部段系数。
    fn update_coeffs(&mut self) {
        for i in 0..self.band_count {
            self.coeffs[i] = design_biquad("peaking", self.freqs[i], self.qs[i], self.gains[i], self.fs)
                .unwrap_or(BYPASS);
        }
    }

    /// 单声道级联：band0 → … → band(n−1) 依次跑完整个缓冲（band-major），
    /// 共享状态按段推进；每段输出写回 f32 后才进入下一段（规格 §4.4 f32 落点）。
    fn process_mono(&mut self, buf: &mut [f32]) {
        for b in 0..self.band_count {
            let coeffs = self.coeffs[b];
            let (mut s1, mut s2) = self.states[b];
            for sample in buf.iter_mut() {
                // TDF2 三行的求值顺序即行为（对齐 TS Biquad.processBlock L161–L167）。
                let x = f64::from(*sample);
                let y = coeffs.b0 * x + s1;
                s1 = coeffs.b1 * x - coeffs.a1 * y + s2;
                s2 = coeffs.b2 * x - coeffs.a2 * y;
                *sample = y as f32;
            }
            self.states[b] = (s1, s2);
        }
    }

    /// 当前内部快照（诊断/测试用途）：每段 (freq, gain, user_gain, q)。
    pub fn bands_snapshot(&self) -> Vec<(f64, f64, f64, f64)> {
        (0..self.band_count)
            .map(|i| (self.freqs[i], self.gains[i], self.user_gains[i], self.qs[i]))
            .collect()
    }

    /// 活动段数（对齐 TS activeCount）。
    pub fn active_count(&self) -> usize {
        self.active_count
    }
}

impl Stage for EqChainStage {
    /// 段系数与状态为构造期定容缓冲，无需按块长预分配（保留形参以符合 Stage 契约）。
    fn prepare(&mut self, _max_block_size: usize) {}

    /// 就地处理一个立体声块：**先整条跑完 L 块、再整条跑完 R 块**，两声道共享
    /// 同一条级联状态（规格 §4.4）。输出依赖块长，对拍按冻结向量 blockSize 回放。
    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        debug_assert_eq!(left.len(), right.len(), "左右声道块长必须一致（Stage 契约）");
        self.process_mono(left);
        self.process_mono(right);
    }

    /// 清零全部段 TDF2 状态；系数与参数全部保留（规格 §4.5，对齐 TS reset L180–L182）。
    fn reset(&mut self) {
        for s in self.states.iter_mut() {
            *s = (0.0, 0.0);
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
                u = u.wrapping_mul(1664525).wrapping_add(1013904223);
                let unit = f64::from(u) / 4294967296.0 * 2.0 - 1.0;
                (unit * amp) as f32
            })
            .collect()
    }

    /// 与 Node 直接执行 TS 源码取得的黄金参考比对。
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

    /// 黄金参考：node 直跑仓库根 src/dsp/EqChain.ts 导出（JSON 最短往返表示）。
    mod golden {
        /// case2 配置（40Hz+6/Q1.4、1kHz−4/Q1.0、8kHz+3/Q0.8）补偿收敛后的实际增益。
        pub const COMPENSATED_GAINS: [f64; 3] =
            [6.006334987836898, -4.065246490926785, 3.0526569316961742];
        /// 补偿收敛后三段系数（b0, b1, b2, a1, a2）。
        pub const COMPENSATED_COEFFS: [[f64; 5]; 3] = [
            [1.0013172803549246, -1.9973293851174263, 0.9960394840348286, -1.9973293851174263, 0.9973567643897532],
            [0.9715238801021812, -1.831818650472443, 0.8761014612951771, -1.831818650472443, 0.8476253413973581],
            [1.1315015206493513, -0.6877389345365638, 0.243976348423776, -0.6877389345365638, 0.37547786907312736],
        ];
        /// 未补偿（qCompensation=false）三段系数。
        pub const UNCOMPENSATED_COEFFS: [[f64; 5]; 3] = [
            [1.0013158355220269, -1.9973284223175285, 0.9960399660546305, -1.9973284223175285, 0.9973558015766572],
            [0.9719810269927781, -1.8323419939499637, 0.8761721737923382, -1.8323419939499637, 0.8481532007851165],
            [1.1290881103752763, -0.687087606793895, 0.24508710321251345, -0.687087606793895, 0.3741752135877899],
        ];
        /// 补偿后级联对固定输入序列的输出——左声道喂入 xs，右声道**零输入**，
        /// 从左声道结束时的共享状态继续演化（规格 §4.4 块内顺序）。
        pub const CASCADE_OUT_L: [f64; 8] = [
            0.5503644347190857,
            -0.2592966854572296,
            0.7346709370613098,
            1.0927033424377441,
            -1.1837440729141235,
            -0.15303008258342743,
            -0.9731066226959229,
            0.11683536320924759,
        ];
        pub const CASCADE_OUT_R: [f64; 8] = [
            0.20071928203105927,
            0.12744180858135223,
            0.028588389977812767,
            -0.013167464174330235,
            -0.006085437256842852,
            0.013137813657522202,
            0.02238798327744007,
            0.020253479480743408,
        ];
        /// 补偿后 band0 系数在 40Hz / 1kHz 处的级联单段幅度（Biquad.magnitudeAt）。
        pub const MAG_AT_40HZ: f64 = 1.9967180752390874;
        pub const MAG_AT_1000HZ: f64 = 1.000610341181525;
    }

    fn case2_bands() -> Vec<EqBandParam> {
        vec![
            EqBandParam { frequency: 40.0, gain: 6.0, q: 1.4 },
            EqBandParam { frequency: 1000.0, gain: -4.0, q: 1.0 },
            EqBandParam { frequency: 8000.0, gain: 3.0, q: 0.8 },
        ]
    }

    #[test]
    fn 补偿开启_增益与系数命中_ts黄金参考() {
        let mut eq = EqChainStage::new(48000.0, 5.0).expect("合法参数");
        eq.set_bands(&case2_bands());
        eq.set_q_compensation(true);
        let snap = eq.bands_snapshot();
        for i in 0..3 {
            let (_, gain, user_gain, _) = snap[i];
            assert_eq!(user_gain, [6.0, -4.0, 3.0][i], "用户目标增益不受补偿影响 @{}", i);
            // 补偿经 log10/powf/hypot 等跨库函数，1e-9 相对容差远宽于 ulp 差异、
            // 远严于任何迭代轮数/阻尼/钳制错误。
            assert!(
                (gain - golden::COMPENSATED_GAINS[i]).abs() <= 1e-9 * golden::COMPENSATED_GAINS[i].abs(),
                "补偿增益 @{}：got {}，want {}",
                i,
                gain,
                golden::COMPENSATED_GAINS[i]
            );
            assert_coeffs_close(eq.coeffs[i], golden::COMPENSATED_COEFFS[i], &format!("补偿系数band{}", i));
        }
        // 填充段（band3/4）保持 0dB 直通且不参与补偿。
        for i in 3..5 {
            let (f, g, ug, q) = snap[i];
            assert_eq!((f, g, ug, q), (1000.0, 0.0, 0.0, 1.0), "填充段回退值 @{}", i);
        }
        assert_eq!(eq.active_count(), 3);
    }

    #[test]
    fn 补偿关闭_增益恰为用户目标_系数命中_ts黄金参考() {
        let mut eq = EqChainStage::new(48000.0, 5.0).expect("合法参数");
        eq.set_bands(&case2_bands());
        eq.set_q_compensation(false); // 初值即 false：空操作
        let snap = eq.bands_snapshot();
        for i in 0..3 {
            assert_eq!(snap[i].1, [6.0, -4.0, 3.0][i], "未补偿增益=用户目标 @{}", i);
            assert_coeffs_close(eq.coeffs[i], golden::UNCOMPENSATED_COEFFS[i], &format!("未补偿系数band{}", i));
        }
    }

    #[test]
    fn 级联输出序列_命中_ts黄金参考_含共享状态续跑右声道() {
        let mut eq = EqChainStage::new(48000.0, 5.0).expect("合法参数");
        eq.set_bands(&case2_bands());
        eq.set_q_compensation(true);
        eq.prepare(8);
        // 输入全部是 2 的幂次，f32 表示精确无舍入；右声道零输入（黄金参考同款）。
        let xs = [0.5_f32, -0.25, 0.75, 1.0, -1.0, 0.125, -0.875, 0.0625];
        let mut left = xs.to_vec();
        let mut right = vec![0.0_f32; 8];
        eq.process(&mut left, &mut right);
        for i in 0..8 {
            let got_l = f64::from(left[i]);
            let got_r = f64::from(right[i]);
            // 写回 f32 的量化误差在 2^-24 相对量级；状态共享/块内顺序错误会产生 O(1) 级偏差。
            assert!(
                (got_l - golden::CASCADE_OUT_L[i]).abs() <= 1e-6 * golden::CASCADE_OUT_L[i].abs(),
                "L[{}]：got {}，want {}",
                i,
                got_l,
                golden::CASCADE_OUT_L[i]
            );
            assert!(
                (got_r - golden::CASCADE_OUT_R[i]).abs() <= 1e-6 * golden::CASCADE_OUT_R[i].abs(),
                "R[{}]（共享状态续跑）：got {}，want {}",
                i,
                got_r,
                golden::CASCADE_OUT_R[i]
            );
        }
    }

    #[test]
    fn magnitude_at_命中_ts黄金参考() {
        let coeffs = design_biquad("peaking", 40.0, 1.4, 6.006334987836898, 48000.0).expect("合法参数");
        let m40 = magnitude_at(&coeffs, 40.0, 48000.0);
        let m1k = magnitude_at(&coeffs, 1000.0, 48000.0);
        assert!(
            (m40 - golden::MAG_AT_40HZ).abs() <= 1e-12 * golden::MAG_AT_40HZ,
            "magnitudeAt(40Hz)：got {}，want {}",
            m40,
            golden::MAG_AT_40HZ
        );
        assert!(
            (m1k - golden::MAG_AT_1000HZ).abs() <= 1e-12 * golden::MAG_AT_1000HZ,
            "magnitudeAt(1kHz)：got {}，want {}",
            m1k,
            golden::MAG_AT_1000HZ
        );
    }

    #[test]
    fn 零增益全直通_输出与输入逐位一致_含右声道() {
        // GWT-EQ-01 投影：10 段全 0 增益（规格锚点 case1 同配置）。
        let freqs = [40.0, 80.0, 160.0, 320.0, 640.0, 1280.0, 2560.0, 5120.0, 10240.0, 16000.0];
        let qs = [0.5, 0.8, 1.0, 1.2, 1.4, 2.0, 3.0, 4.0, 0.707, 6.0];
        let bands: Vec<EqBandParam> = freqs
            .iter()
            .zip(qs.iter())
            .map(|(&f, &q)| EqBandParam { frequency: f, gain: 0.0, q })
            .collect();
        let mut eq = EqChainStage::new(48000.0, 10.0).expect("合法参数");
        eq.set_bands(&bands);
        eq.prepare(512);
        let in_l = lcg_noise(512, 42, 0.9);
        let in_r = lcg_noise(512, 43, 0.8);
        let mut left = in_l.clone();
        let mut right = in_r.clone();
        eq.process(&mut left, &mut right);
        assert_eq!(left, in_l, "零增益级联必须逐位直通（左）");
        assert_eq!(right, in_r, "零增益级联必须逐位直通（右，含共享状态续跑）");
    }

    #[test]
    fn 越界参数钳制生效值与边界值逐位等效() {
        // GWT-EQ-05 投影：frequency/gain/q 三参数双向钳制（case4 首末段同款）。
        let mut eq = EqChainStage::new(48000.0, 20.0).expect("合法参数");
        let mut bands = vec![
            EqBandParam { frequency: 5.0, gain: 30.0, q: 0.05 }, // → 20 / +24 / 0.1
            EqBandParam { frequency: 30000.0, gain: -40.0, q: 50.0 }, // → 20000 / −24 / 18
        ];
        eq.set_bands(&bands);
        let snap = eq.bands_snapshot();
        assert_eq!((snap[0].0, snap[0].1, snap[0].3), (20.0, 24.0, 0.1));
        assert_eq!((snap[1].0, snap[1].1, snap[1].3), (20000.0, -24.0, 18.0));
        // 钳制生效值与直接按边界值配置逐位等效（同一系数设计路径）。
        let mut direct = EqChainStage::new(48000.0, 20.0).expect("合法参数");
        bands[0] = EqBandParam { frequency: 20.0, gain: 24.0, q: 0.1 };
        bands[1] = EqBandParam { frequency: 20000.0, gain: -24.0, q: 18.0 };
        direct.set_bands(&bands);
        assert_eq!(eq.coeffs[0], direct.coeffs[0]);
        assert_eq!(eq.coeffs[1], direct.coeffs[1]);
    }

    #[test]
    fn bands_长度与_bandCount_不等时的填充与截断语义() {
        // GWT-EQ-04 投影：短于 → 尾部填充；长于 → 多出段忽略。
        let mut eq = EqChainStage::new(48000.0, 5.0).expect("合法参数");
        eq.set_bands(&case2_bands()); // 3 段进 5 段
        assert_eq!(eq.active_count(), 3);
        let snap = eq.bands_snapshot();
        for i in 3..5 {
            assert_eq!((snap[i].0, snap[i].1, snap[i].3), (1000.0, 0.0, 1.0));
        }
        // 长于：第 4 段被忽略，active_count 封顶 bandCount。
        let mut eq2 = EqChainStage::new(48000.0, 2.0).expect("合法参数");
        eq2.set_bands(&case2_bands());
        assert_eq!(eq2.active_count(), 2);
        let snap2 = eq2.bands_snapshot();
        assert_eq!((snap2[0].0, snap2[1].0), (40.0, 1000.0));
    }

    #[test]
    fn 关闭补偿仅翻标志_已补偿系数保留() {
        // 规格 §4.3 关闭语义（行为事实）：true→false 不重算系数。
        let mut eq = EqChainStage::new(48000.0, 5.0).expect("合法参数");
        eq.set_bands(&case2_bands());
        eq.set_q_compensation(true);
        let before = eq.coeffs.clone();
        eq.set_q_compensation(false);
        assert_eq!(eq.coeffs, before, "关闭补偿不得改动系数");
        // 再开启：补偿从**保留的（已补偿）gains** 出发继续迭代（而非重置为用户目标），
        // 因此系数会发生微小变化；该变化必须远小于收敛阈值对应的量级。
        eq.set_q_compensation(true);
        for i in 0..eq.band_count {
            for (got, want) in [eq.coeffs[i].b0, eq.coeffs[i].b1, eq.coeffs[i].b2, eq.coeffs[i].a1, eq.coeffs[i].a2]
                .iter()
                .zip([before[i].b0, before[i].b1, before[i].b2, before[i].a1, before[i].a2].iter())
            {
                assert!(
                    ((got - want) / want).abs() < 1e-3,
                    "再开启补偿只应做微小修正 @band{}：got {}，want {}",
                    i,
                    got,
                    want
                );
            }
        }
    }

    #[test]
    fn 补偿两种触发顺序终态逐位一致() {
        // 规格 §4.3：setBands→setQCompensation(true) 与反向顺序终态一致（实证事实）。
        let mut a = EqChainStage::new(48000.0, 5.0).expect("合法参数");
        a.set_bands(&case2_bands());
        a.set_q_compensation(true);
        let mut b = EqChainStage::new(48000.0, 5.0).expect("合法参数");
        b.set_q_compensation(true);
        b.set_bands(&case2_bands());
        assert_eq!(a.coeffs, b.coeffs, "两种触发顺序的终态系数必须逐位一致");
        assert_eq!(a.gains, b.gains);
    }

    #[test]
    fn reset_清状态保系数_重放逐位复现() {
        let mut eq = EqChainStage::new(48000.0, 5.0).expect("合法参数");
        eq.set_bands(&case2_bands());
        eq.set_q_compensation(true);
        eq.prepare(97);
        let coeffs_before = eq.coeffs.clone();
        let input: Vec<f32> = (0..97).map(|i| (f64::from(i) * 0.113).sin() as f32 * 0.8).collect();
        let mut l1 = input.clone();
        let mut r1 = input.clone();
        eq.process(&mut l1, &mut r1);
        eq.reset();
        assert_eq!(eq.coeffs, coeffs_before, "reset 不改动系数");
        assert!(eq.states.iter().all(|&s| s == (0.0, 0.0)), "reset 清零全部段状态");
        let mut l2 = input.clone();
        let mut r2 = input.clone();
        eq.process(&mut l2, &mut r2);
        assert_eq!(l1, l2, "GWT-EQ-09：reset 后重放必须逐位一致");
        assert_eq!(r1, r2);
    }

    #[test]
    fn 同一块长调度下逐块回放确定可复现() {
        // GWT-EQ-07 的对拍判定形态：同一 blockSize、同一声道排列下输出确定。
        let input_l = lcg_noise(600, 7, 0.7);
        let input_r = lcg_noise(600, 8, 0.7);
        let run = |schedule: &[usize]| -> (Vec<f32>, Vec<f32>) {
            let mut eq = EqChainStage::new(48000.0, 5.0).expect("合法参数");
            eq.set_bands(&case2_bands());
            eq.set_q_compensation(true);
            eq.prepare(*schedule.iter().max().unwrap_or(&0));
            let mut left = input_l.clone();
            let mut right = input_r.clone();
            let mut off = 0_usize;
            for &len in schedule {
                eq.process(&mut left[off..off + len], &mut right[off..off + len]);
                off += len;
            }
            (left, right)
        };
        let (wl, wr) = run(&[600]);
        let (cl, cr) = run(&[256, 256, 88]);
        // 块长不同 → 共享状态交错历史不同 → 输出**允许**不同（规格 §4.4 行为事实）；
        // 此处固化该事实：整块与分块不要求一致，但同一调度必须可复现。
        let (cl_again, cr_again) = run(&[256, 256, 88]);
        assert_eq!(cl, cl_again, "同一块长调度必须逐位可复现");
        assert_eq!(cr, cr_again);
        // 固化块长敏感事实：立体声入口分块 ≠ 整块（文档化，防止误加"优化"破坏语义）。
        assert!(wl != cl || wr != cr, "规格 §4.4：立体声输出依赖 blockSize（若此断言失败，说明实现已被改动，须复核规格）");
    }

    #[test]
    fn 左声道历史经共享状态影响右声道输出() {
        // 规格 §4.4 共享状态语义投影：L 先整条处理，R 从 L 结束时的状态继续。
        let mut eq = EqChainStage::new(48000.0, 5.0).expect("合法参数");
        eq.set_bands(&case2_bands());
        eq.set_q_compensation(true);
        eq.prepare(64);
        let loud = vec![0.9_f32; 64];
        let mut left = loud.clone();
        let mut right = vec![0.0_f32; 64];
        eq.process(&mut left, &mut right);
        assert!(
            right.iter().any(|&s| s != 0.0),
            "右声道零输入但共享状态非零：输出不得恒为零（否则左右状态被错误隔离）"
        );
    }

    #[test]
    fn 静音输入零输出且状态保持零() {
        let mut eq = EqChainStage::new(48000.0, 5.0).expect("合法参数");
        eq.set_bands(&case2_bands());
        eq.prepare(64);
        let mut left = vec![0.0_f32; 64];
        let mut right = vec![0.0_f32; 64];
        eq.process(&mut left, &mut right);
        assert!(left.iter().all(|&s| s == 0.0) && right.iter().all(|&s| s == 0.0));
        assert!(eq.states.iter().all(|&s| s == (0.0, 0.0)));
    }

    #[test]
    fn 满幅输入极值参数下有界不发散() {
        // GWT-EQ-11 投影：钳制后仍在域内的大增益 + 低 q 组合。
        let mut eq = EqChainStage::new(48000.0, 5.0).expect("合法参数");
        eq.set_bands(&[
            EqBandParam { frequency: 100.0, gain: 24.0, q: 0.1 },
            EqBandParam { frequency: 120.0, gain: 24.0, q: 0.1 },
            EqBandParam { frequency: 140.0, gain: -24.0, q: 0.1 },
        ]);
        eq.set_q_compensation(true);
        eq.prepare(512);
        let input = lcg_noise(512, 99, 1.0);
        let mut left = input.clone();
        let mut right = input.clone();
        eq.process(&mut left, &mut right);
        for i in 0..512 {
            assert!(left[i].is_finite() && right[i].is_finite(), "输出必须有限 @{}", i);
        }
    }

    #[test]
    fn bandCount_取整与下限语义() {
        // TS L48：max(1, floor(bandCount))。
        let eq = EqChainStage::new(48000.0, 5.9).expect("合法参数");
        assert_eq!(eq.band_count, 5);
        let eq1 = EqChainStage::new(48000.0, 0.5).expect("合法参数");
        assert_eq!(eq1.band_count, 1);
        // 段数为 0/1 的退化级联：单段行为与裸 biquad 一致（此处仅验证可构造可处理）。
        let mut eq_one = EqChainStage::new(48000.0, 1.0).expect("合法参数");
        eq_one.prepare(4);
        let mut l = vec![0.5_f32; 4];
        let mut r = vec![-0.5_f32; 4];
        eq_one.process(&mut l, &mut r);
        assert!(l.iter().all(|s| s.is_finite()) && r.iter().all(|s| s.is_finite()));
    }

    #[test]
    fn 非法采样率报错() {
        for bad in [0.0_f64, -44100.0, f64::NAN, f64::NEG_INFINITY] {
            let err = EqChainStage::new(bad, 10.0).err().expect("非法采样率必须报错");
            assert!(err.contains("invalid sample rate"), "应对齐 TS 错误信息：{}", err);
        }
    }
}
