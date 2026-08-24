
//! limiter —— 前瞻限幅器 + 真峰值阶段（Phase 1 试点模块）。
//!
//! 行为事实标准：仓库根 `src/dsp/Limiter.ts»；规格：「specs/dsp/limiter.md」。
//! 本文件是该 TS 类的逐行移植，关键对应关系：
//!
//! - 时序模型：流式逐样本，输出 `y[idx] = x[idx−L]·g[idx]»；g 由检测窗峰值经
//!   attack/release 一阶平滑得到。真峰值模式下检测值定位在 `idx−3»（居中 sinc
//!   插值所需），有效检测窗为 `[idx−L−3, idx−3]»（数字峰值模式为 `[idx−L, idx]»）。
//! - 数值精度铁律：TS 中以 Number(f64) 参与运算的中间量（阈值线性值、插值系数
//!   计算、检测峰值 det、增益包络、平滑系数）一律用 f64 复刻；f32 落点只有三处，
//!   且与 TS 完全一致——写回输出样本、队列峰值存储（TS `qVal» 为 Float32Array）、
//!   插值系数表与真峰值历史环（Float32Array）。
//! - Math.round 复刻为 `floor(x + 0.5)»（JS 对 .5 向 +∞ 舍入，非远离零）。
//! - 相位 2（frac=1/2）利用 sinc 偶函数对称 + Blackman 窗对称，tap 对 (t2,t7)、
//!   (t3,t6)、(t4,t5) 系数相同，按 TS 合并乘加——纯优化，加法链顺序与 TS 一致。
//! - 与 TS 的唯一有意偏差：apply_params 重分配延迟线时 TS 不重置 delayW，
//!   新尺寸小于旧写游标时 TS 会越界静默丢写/读到 undefined；此处仅在该越界场景
//!   把写游标归零。由于重分配同时清零整条延迟线且每槽随写随读，环形起始相位在
//!   数值上不可观测，其余场景与 TS 逐位一致。
//! - 实时安全：延迟线 / 检测队列 / 插值历史全部在构造与 configure 中预分配，
//!   process 内零分配、零 panic。

use std::f64::consts::PI;

use crate::Stage;

/// 对齐 TS `LimiterSettings» 的参数快照。
#[derive(Debug, Clone)]
pub struct LimiterSettings {
    pub enabled: bool,
    pub threshold_db: f64,
    pub lookahead_ms: f64,
    pub attack_ms: f64,
    pub release_ms: f64,
    pub true_peak: bool,
}

/// 一个已配置的前瞻限幅器阶段。
///
/// 字段一一对应 TS `Limiter» 的私有域；命名沿用蛇形转换。
pub struct LimiterStage {
    sample_rate: f64,
    // —— 生效参数（apply_params 钳制后的取值）——
    enabled: bool,
    threshold_lin: f64,
    lookahead: usize,
    attack_coef: f64,
    release_coef: f64,
    true_peak: bool,
    // —— 延迟线（尺寸 lookahead+1，读取最旧样本 = 延迟 L）——
    delay_l: Vec<f32>,
    delay_r: Vec<f32>,
    delay_w: usize,
    // —— 单调递减队列（环形），滑动窗峰值检测 ——
    q_idx: Vec<i64>,
    q_val: Vec<f32>,
    q_head: usize,
    q_tail: usize,
    q_len: usize,
    q_cap: usize,
    // —— 真峰值：每通道 8 样本历史（环形）+ 3 相位 × 8 taps 插值系数 ——
    hist_l: [f32; 8],
    hist_r: [f32; 8],
    hist_w: usize,
    interp: [f32; 24],
    // —— 包络与统计 ——
    gain: f64,
    reduction_db: f64,
    sample_index: i64,
}

/// TS `clamp(v, lo, hi)» 的逐字复刻。
fn clamp(v: f64, lo: f64, hi: f64) -> f64 {
    if v < lo {
        lo
    } else if v > hi {
        hi
    } else {
        v
    }
}

/// TS `onePoleCoef(timeMs, fs)» 的逐字复刻：
/// coef = 1 − exp(−1 / ((max(ms, 0.05)/1000) × fs))，ms 下限 0.05 生效。
fn one_pole_coef(time_ms: f64, fs: f64) -> f64 {
    let ms = time_ms.max(0.05);
    1.0 - (-1.0 / ((ms / 1000.0) * fs)).exp()
}

/// 取真峰值历史环中自 oldest 槽位起的连续 8 个样本（对齐 TS 把 i0..i7 一次性
/// 取入局部变量的展开），返回 f64 供插值运算（TS 中历史样本即 Number 参与运算）。
#[inline]
fn hist_window(hist: &[f32; 8], oldest: usize) -> [f64; 8] {
    [
        f64::from(hist[oldest]),
        f64::from(hist[(oldest + 1) & 7]),
        f64::from(hist[(oldest + 2) & 7]),
        f64::from(hist[(oldest + 3) & 7]),
        f64::from(hist[(oldest + 4) & 7]),
        f64::from(hist[(oldest + 5) & 7]),
        f64::from(hist[(oldest + 6) & 7]),
        f64::from(hist[(oldest + 7) & 7]),
    ]
}

impl LimiterStage {
    /// 按 TS 构造函数默认参数创建（thresholdDb=-1 / lookaheadMs=5 / attackMs=0.5 /
    /// releaseMs=150 / truePeak=true / enabled=true），随后可用 configure 覆盖。
    pub fn new(sample_rate: f64) -> Result<Self, String> {
        Self::from_settings(
            sample_rate,
            LimiterSettings { enabled: true, threshold_db: -1.0, lookahead_ms: 5.0, attack_ms: 0.5, release_ms: 150.0, true_peak: true },
        )
    }

    /// 以显式参数快照构造（对齐 TS `setParams» 整体替换语义；钳制规则见规格参数表）。
    ///
    /// sampleRate ≤ 0 或非有限时报错（GWT-LM-11，对齐 TS Error('invalid sample rate')）。
    pub fn from_settings(sample_rate: f64, settings: LimiterSettings) -> Result<Self, String> {
        if !(sample_rate.is_finite() && sample_rate > 0.0) {
            return Err("sampleRate 必须为正有限数".into());
        }
        let mut stage = Self {
            sample_rate,
            // TS 字段初始化器的默认域（构造体内随即被 applyParams 覆写）。
            enabled: true,
            threshold_lin: 10_f64.powf(-1.0 / 20.0),
            lookahead: 0,
            attack_coef: 0.0,
            release_coef: 0.0,
            true_peak: false,
            delay_l: vec![0.0; 1],
            delay_r: vec![0.0; 1],
            delay_w: 0,
            q_idx: vec![0; 8],
            q_val: vec![0.0; 8],
            q_head: 0,
            q_tail: 0,
            q_len: 0,
            q_cap: 8,
            hist_l: [0.0; 8],
            hist_r: [0.0; 8],
            hist_w: 0,
            interp: [0.0; 24],
            gain: 1.0,
            reduction_db: 0.0,
            sample_index: 0,
        };
        stage.apply_params(&settings);
        Ok(stage)
    }

    /// 覆盖参数快照（对齐 TS setParams；缓冲尺寸变化或禁用切回启用时清空管线）。
    pub fn configure(&mut self, settings: LimiterSettings) {
        self.apply_params(&settings);
    }

    /// 当前增益衰减 dB（恒 ≤ 0，对齐 TS getReductionDb）。
    pub fn reduction_db(&self) -> f64 {
        self.reduction_db
    }

    /// 引入的延迟样本数 = lookahead（对齐 TS getLatencySamples，两种模式下均成立）。
    pub fn latency_samples(&self) -> usize {
        self.lookahead
    }

    /// TS applyParams 的逐行移植：钳制 + 系数重算 + 条件性管线清理 + 插值系数刷新。
    fn apply_params(&mut self, p: &LimiterSettings) {
        let was_enabled = self.enabled;
        self.enabled = p.enabled;
        self.threshold_lin = 10_f64.powf(clamp(p.threshold_db, -60.0, 0.0) / 20.0);
        // TS Math.round((lookaheadMs * fs) / 1000)：乘除次序保持，再钳到 [0, floor(fs*0.1)]。
        let rounded = (((p.lookahead_ms * self.sample_rate) / 1000.0) + 0.5).floor();
        let upper = (self.sample_rate * 0.1).floor();
        self.lookahead = clamp(rounded, 0.0, upper.max(0.0)) as usize;
        self.attack_coef = one_pole_coef(p.attack_ms, self.sample_rate);
        self.release_coef = one_pole_coef(p.release_ms, self.sample_rate);
        self.true_peak = p.true_peak;

        let size = (self.lookahead + 1).max(1);
        let cap = (self.lookahead + 8).max(8);
        if size != self.delay_l.len() || cap != self.q_cap {
            self.delay_l = vec![0.0; size];
            self.delay_r = vec![0.0; size];
            self.q_idx = vec![0; cap];
            self.q_val = vec![0.0; cap];
            self.q_cap = cap;
            self.q_head = 0;
            self.q_tail = 0;
            self.q_len = 0;
            self.hist_l = [0.0; 8];
            self.hist_r = [0.0; 8];
            self.hist_w = 0;
            self.gain = 1.0;
            self.sample_index = 0;
            self.reduction_db = 0.0;
            // 见模块注释「唯一有意偏差」：TS 不重置 delayW，这里仅防越界归零。
            if self.delay_w >= size {
                self.delay_w = 0;
            }
        }
        if self.enabled && !was_enabled {
            // 禁用期间延迟线未更新，恢复时清空避免陈旧样本（GWT-LM-01/§4.4）。
            self.delay_l.fill(0.0);
            self.delay_r.fill(0.0);
            self.q_head = 0;
            self.q_tail = 0;
            self.q_len = 0;
            self.hist_l.fill(0.0);
            self.hist_r.fill(0.0);
            self.hist_w = 0;
            self.gain = 1.0;
            self.sample_index = 0;
            self.reduction_db = 0.0;
        }
        // 4× 过采样 sinc 插值系数（Blackman 窗，3 相位 × 8 taps，窗支撑 [-5, 5]）；
        // 仅 truePeak 时计算，系数按 Float32Array 语义落点 f32。
        if self.true_peak {
            for ph in 0..3_usize {
                let frac = (ph as f64 + 1.0) / 4.0;
                for k in -4_i64..=3 {
                    let x = frac - k as f64;
                    let sx = if x == 0.0 { 1.0 } else { (PI * x).sin() / (PI * x) };
                    let u = (x + 5.0) / 10.0;
                    let w = 0.42 - 0.5 * (2.0 * PI * u).cos() + 0.08 * (4.0 * PI * u).cos();
                    self.interp[ph * 8 + (k + 4) as usize] = (sx * w) as f32;
                }
            }
        }
    }

    /// 单调递减队列的弹出过期队首 + 弹出 ≤det 队尾 + 入队（两种检测模式共用骨架）。
    ///
    /// key/oldest 用 i64 复刻 TS 的 Int32Array 索引比较（允许启动期为负）；
    /// det 入队时落点 f32（TS qVal 为 Float32Array），比较时升回 f64。
    #[inline]
    fn queue_push(&mut self, key: i64, oldest: i64, det: f64, q_head: &mut usize, q_tail: &mut usize, q_len: &mut usize) {
        while *q_len > 0 && self.q_idx[*q_head] < oldest {
            *q_head = (*q_head + 1) % self.q_cap;
            *q_len -= 1;
        }
        while *q_len > 0 {
            let t = (*q_tail + self.q_cap - 1) % self.q_cap;
            if f64::from(self.q_val[t]) > det {
                break;
            }
            *q_tail = t;
            *q_len -= 1;
        }
        self.q_idx[*q_tail] = key;
        self.q_val[*q_tail] = det as f32;
        *q_tail = (*q_tail + 1) % self.q_cap;
        *q_len += 1;
    }
}

impl Stage for LimiterStage {
    fn prepare(&mut self, _max_block_size: usize) {
        // 全部工作缓冲与块长无关（延迟线/队列/历史尺寸只由参数决定），
        // 已在 from_settings/configure 预分配，这里无事可做。
    }

    /// 就地处理一个立体声块；状态跨块保持，运算序列与分块方式无关（GWT-LM-07）。
    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        if !self.enabled {
            // GWT-LM-01：禁用即直通——不改写缓冲（逐位一致），衰减报告归零。
            self.reduction_db = 0.0;
            return;
        }
        let n = left.len();
        let thr = self.threshold_lin;
        let dsize = self.delay_l.len();
        let lookahead = self.lookahead as i64;
        let tp = self.true_peak;
        let attack = self.attack_coef;
        let release = self.release_coef;

        // 热路径游标/包络取局部变量，块尾写回（对齐 TS processStereo 的缓存模式）。
        let mut q_head = self.q_head;
        let mut q_tail = self.q_tail;
        let mut q_len = self.q_len;
        let mut delay_w = self.delay_w;
        let mut gain = self.gain;

        for i in 0..n {
            let xl = left[i];
            let xr = right[i];
            let idx = self.sample_index;

            // 1) 4× 过采样历史写入（位置 idx−7..idx）
            self.hist_l[self.hist_w] = xl;
            self.hist_r[self.hist_w] = xr;
            self.hist_w = (self.hist_w + 1) & 7;

            // 2) 检测值：数字峰值 或 真峰值（4× sinc 插值，位置 p = idx−3）
            let abs_xl = xl.abs();
            let abs_xr = xr.abs();
            let mut det = if abs_xl > abs_xr { f64::from(abs_xl) } else { f64::from(abs_xr) };

            if tp {
                if idx >= 7 {
                    let w = self.hist_w; // 自增后 = 最旧样本 x[idx−7] 的槽位
                    let hl = hist_window(&self.hist_l, w);
                    let hr = hist_window(&self.hist_r, w);
                    let mut vl = 0.0_f64;
                    let mut vr = 0.0_f64;
                    // 相位 1（frac=1/4）：8 taps 顺序展开（左折叠加法链与 TS 一致）
                    {
                        let s_l = f64::from(self.interp[0]) * hl[0]
                            + f64::from(self.interp[1]) * hl[1]
                            + f64::from(self.interp[2]) * hl[2]
                            + f64::from(self.interp[3]) * hl[3]
                            + f64::from(self.interp[4]) * hl[4]
                            + f64::from(self.interp[5]) * hl[5]
                            + f64::from(self.interp[6]) * hl[6]
                            + f64::from(self.interp[7]) * hl[7];
                        let s_r = f64::from(self.interp[0]) * hr[0]
                            + f64::from(self.interp[1]) * hr[1]
                            + f64::from(self.interp[2]) * hr[2]
                            + f64::from(self.interp[3]) * hr[3]
                            + f64::from(self.interp[4]) * hr[4]
                            + f64::from(self.interp[5]) * hr[5]
                            + f64::from(self.interp[6]) * hr[6]
                            + f64::from(self.interp[7]) * hr[7];
                        let a_l = s_l.abs();
                        let a_r = s_r.abs();
                        if a_l > vl {
                            vl = a_l;
                        }
                        if a_r > vr {
                            vr = a_r;
                        }
                    }
                    // 相位 2（frac=1/2）：sinc 偶对称 → tap 对合并（hL2+hL7 / hL3+hL6 / hL4+hL5）
                    {
                        let s_l = f64::from(self.interp[8]) * hl[0]
                            + f64::from(self.interp[9]) * hl[1]
                            + f64::from(self.interp[10]) * (hl[2] + hl[7])
                            + f64::from(self.interp[11]) * (hl[3] + hl[6])
                            + f64::from(self.interp[12]) * (hl[4] + hl[5]);
                        let s_r = f64::from(self.interp[8]) * hr[0]
                            + f64::from(self.interp[9]) * hr[1]
                            + f64::from(self.interp[10]) * (hr[2] + hr[7])
                            + f64::from(self.interp[11]) * (hr[3] + hr[6])
                            + f64::from(self.interp[12]) * (hr[4] + hr[5]);
                        let a_l = s_l.abs();
                        let a_r = s_r.abs();
                        if a_l > vl {
                            vl = a_l;
                        }
                        if a_r > vr {
                            vr = a_r;
                        }
                    }
                    // 相位 3（frac=3/4）：8 taps 顺序展开
                    {
                        let s_l = f64::from(self.interp[16]) * hl[0]
                            + f64::from(self.interp[17]) * hl[1]
                            + f64::from(self.interp[18]) * hl[2]
                            + f64::from(self.interp[19]) * hl[3]
                            + f64::from(self.interp[20]) * hl[4]
                            + f64::from(self.interp[21]) * hl[5]
                            + f64::from(self.interp[22]) * hl[6]
                            + f64::from(self.interp[23]) * hl[7];
                        let s_r = f64::from(self.interp[16]) * hr[0]
                            + f64::from(self.interp[17]) * hr[1]
                            + f64::from(self.interp[18]) * hr[2]
                            + f64::from(self.interp[19]) * hr[3]
                            + f64::from(self.interp[20]) * hr[4]
                            + f64::from(self.interp[21]) * hr[5]
                            + f64::from(self.interp[22]) * hr[6]
                            + f64::from(self.interp[23]) * hr[7];
                        let a_l = s_l.abs();
                        let a_r = s_r.abs();
                        if a_l > vl {
                            vl = a_l;
                        }
                        if a_r > vr {
                            vr = a_r;
                        }
                    }
                    if vl > det {
                        det = vl;
                    }
                    if vr > det {
                        det = vr;
                    }
                }
                // 弹出窗口外（索引 < oldest）的队首过期项；单调递减入队（相等值保留最新）。
                let oldest = idx - 3 - lookahead;
                self.queue_push(idx - 3, oldest, det, &mut q_head, &mut q_tail, &mut q_len);
            } else {
                let oldest = idx - lookahead;
                self.queue_push(idx, oldest, det, &mut q_head, &mut q_tail, &mut q_len);
            }

            // 3) 延迟线写入（写后游标自增，环回用比较代替取模）
            self.delay_l[delay_w] = xl;
            self.delay_r[delay_w] = xr;
            delay_w += 1;
            if delay_w >= dsize {
                delay_w = 0;
            }

            // 4) 目标增益 = min(1, 阈值/峰值)，attack/release 一阶平滑
            //    （peak 从 Float32Array 读回升 f64；Math.max(peak, 1e-12) 防 0 除）
            let peak = if q_len > 0 { f64::from(self.q_val[q_head]) } else { 0.0 };
            let target = (thr / peak.max(1e-12)).min(1.0);
            if target < gain {
                gain += attack * (target - gain);
            } else {
                gain += release * (target - gain);
            }

            // 5) 输出 = 延迟 L 样本 × 平滑增益（f64 运算，样本落点 f32）
            left[i] = (f64::from(self.delay_l[delay_w]) * gain) as f32;
            right[i] = (f64::from(self.delay_r[delay_w]) * gain) as f32;
            self.sample_index = idx + 1;
        }

        self.delay_w = delay_w;
        self.q_head = q_head;
        self.q_tail = q_tail;
        self.q_len = q_len;
        self.gain = gain;
        // TS 在 processStereo 尾部统一刷新衰减报告（含空块调用）。
        self.reduction_db = 20.0 * gain.log10();
    }

    /// 清空全部管线状态回刚构造取值（对齐 TS reset）。
    fn reset(&mut self) {
        self.delay_l.fill(0.0);
        self.delay_r.fill(0.0);
        self.delay_w = 0;
        self.q_head = 0;
        self.q_tail = 0;
        self.q_len = 0;
        self.hist_l.fill(0.0);
        self.hist_r.fill(0.0);
        self.hist_w = 0;
        self.gain = 1.0;
        self.reduction_db = 0.0;
        self.sample_index = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 确定性伪随机源（LCG；测试专用，不进实时路径）。
    struct Lcg(u64);

    impl Lcg {
        fn next_f32(&mut self) -> f32 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((self.0 >> 33) as f32 / (1_u32 << 31) as f32) * 2.0 - 1.0
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn settings(enabled: bool, threshold_db: f64, lookahead_ms: f64, attack_ms: f64, release_ms: f64, true_peak: bool) -> LimiterSettings {
        LimiterSettings { enabled, threshold_db, lookahead_ms, attack_ms, release_ms, true_peak }
    }

    fn noise_stereo(seed: u64, frames: usize) -> (Vec<f32>, Vec<f32>) {
        let mut rng = Lcg(seed);
        let l = (0..frames).map(|_| rng.next_f32()).collect::<Vec<f32>>();
        let r = (0..frames).map(|_| rng.next_f32()).collect::<Vec<f32>>();
        (l, r)
    }

    // ---- GWT-LM-11：非法采样率抛错 ----

    #[test]
    fn 非法采样率一律报错() {
        for fs in [0.0, -48_000.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(LimiterStage::new(fs).is_err(), "fs={fs} 必须报错");
        }
    }

    // ---- 构造默认值与 TS 一致 ----

    #[test]
    fn 构造默认值与ts构造函数一致() {
        let st = LimiterStage::new(48_000.0).unwrap();
        assert_eq!(st.latency_samples(), 240); // round(5*48000/1000)
        assert!(st.enabled);
        assert!(st.true_peak);
        assert_eq!(st.reduction_db(), 0.0);
        assert!((st.threshold_lin - 10_f64.powf(-1.0 / 20.0)).abs() < 1e-15);
        // attack 0.5ms@48k：1-exp(-1/24)；release 150ms@48k：1-exp(-1/7200)
        let expect_attack = 1.0 - (-1.0_f64 / ((0.5 / 1000.0) * 48_000.0)).exp();
        let expect_release = 1.0 - (-1.0_f64 / ((150.0 / 1000.0) * 48_000.0)).exp();
        assert!((st.attack_coef - expect_attack).abs() < 1e-15);
        assert!((st.release_coef - expect_release).abs() < 1e-15);
        // 延迟线尺寸 = lookahead+1，队列容量 = lookahead+8
        assert_eq!(st.delay_l.len(), 241);
        assert_eq!(st.delay_r.len(), 241);
        assert_eq!(st.q_cap, 248);
        // 默认参数下插值系数表已填充
        assert_ne!(st.interp[0], 0.0);
        assert_ne!(st.interp[23], 0.0);
    }

    // ---- lookahead 取整与上限钳制 ----

    #[test]
    fn lookahead取整与上限钳制() {
        // round 半值向上语义：220.5 → 221
        let st = LimiterStage::new(44_100.0).unwrap();
        assert_eq!(st.lookahead, 221);
        let cases: [(f64, f64, usize); 6] = [
            (48_000.0, 5.0, 240),     // 典型值
            (48_000.0, 0.4, 19),      // round(19.2)=19
            (48_000.0, 200.0, 4_800), // 9600 → 钳到 floor(48000*0.1)=4800
            (48_000.0, -5.0, 0),      // 负值钳 0
            (48_000.0, 0.0, 0),       // 无前瞻
            // 0.0625*8000/1000 = 0.5（二的幂精确表示）→ 半值向上 → 1
            (8_000.0, 0.0625, 1),
        ];
        for (fs, ms, want) in cases {
            let st = LimiterStage::from_settings(fs, settings(true, -1.0, ms, 0.5, 150.0, false)).unwrap();
            assert_eq!(st.lookahead, want, "fs={fs} lookaheadMs={ms}");
            assert_eq!(st.delay_l.len(), want + 1);
            assert_eq!(st.q_idx.len(), want + 8);
        }
        // 上限钳制的另一档：150ms@8000 = 1200 → 钳到 floor(800)
        let st = LimiterStage::from_settings(8_000.0, settings(true, -1.0, 150.0, 0.5, 150.0, false)).unwrap();
        assert_eq!(st.lookahead, 800);
        assert_eq!(st.latency_samples(), 800);
    }

    // ---- onePole 系数边界 ----

    #[test]
    fn one_pole系数ms下限生效() {
        let fs = 48_000.0;
        let floor = one_pole_coef(0.05, fs);
        assert_eq!(one_pole_coef(0.0, fs), floor, "ms≤0.05 按 0.05 生效");
        assert_eq!(one_pole_coef(-10.0, fs), floor);
        // (0.05/1000)*48000 = 2.4 → 1-exp(-1/2.4)
        assert!((floor - (1.0 - (-1.0_f64 / 2.4).exp())).abs() < 1e-15);
        // 正常值不受下限影响
        let normal = one_pole_coef(50.0, fs);
        let expected = 1.0 - (-1.0_f64 / ((50.0 / 1000.0) * fs)).exp();
        assert!((normal - expected).abs() < 1e-15);
    }

    // ---- GWT-LM-02：静音输入静音输出 ----

    #[test]
    fn 静音输入恒静音且增益保持一() {
        for tp in [true, false] {
            let mut st = LimiterStage::from_settings(48_000.0, settings(true, -6.0, 10.0, 0.5, 150.0, tp)).unwrap();
            for _ in 0..8 {
                let mut l = vec![0.0_f32; 512];
                let mut r = vec![0.0_f32; 512];
                st.process(&mut l, &mut r);
                assert!(l.iter().all(|&s| s.to_bits() == 0), "tp={tp} 左声道必须全零位型");
                assert!(r.iter().all(|&s| s.to_bits() == 0), "tp={tp} 右声道必须全零位型");
            }
            assert_eq!(st.reduction_db(), 0.0);
            assert_eq!(st.gain, 1.0);
        }
    }

    // ---- GWT-LM-01：禁用即逐位直通 ----

    #[test]
    fn 禁用时输出与输入逐位一致() {
        let mut st = LimiterStage::from_settings(48_000.0, settings(false, -12.0, 5.0, 0.5, 150.0, true)).unwrap();
        let (mut l, mut r) = noise_stereo(42, 777); // 故意非整块长
        let before_l = l.clone();
        let before_r = r.clone();
        st.process(&mut l, &mut r);
        assert_eq!(l, before_l);
        assert_eq!(r, before_r);
        assert_eq!(st.reduction_db(), 0.0);
    }

    // ---- GWT-LM-07：跨块状态连续性（逐位一致）----

    #[test]
    fn 分块处理与整块处理逐位一致() {
        for params in [
            settings(true, -1.0, 5.0, 0.5, 150.0, true),  // case1 同款
            settings(true, -12.0, 0.0, 1.0, 50.0, false), // case2 同款（无前瞻）
        ] {
            let frames = 1000;
            let (src_l, src_r) = noise_stereo(7, frames);
            let mut whole_l = src_l.clone();
            let mut whole_r = src_r.clone();
            let mut whole = LimiterStage::from_settings(48_000.0, params.clone()).unwrap();
            whole.process(&mut whole_l, &mut whole_r);

            let mut chunk_l = src_l.clone();
            let mut chunk_r = src_r.clone();
            let mut chunked = LimiterStage::from_settings(48_000.0, params.clone()).unwrap();
            let mut off = 0;
            while off < frames {
                let end = (off + 333).min(frames); // 333 不整除 1000，末块短
                chunked.process(&mut chunk_l[off..end], &mut chunk_r[off..end]);
                off = end;
            }
            assert_eq!(chunk_l, whole_l, "params={params:?} 分块必须逐位一致");
            assert_eq!(chunk_r, whole_r);
        }
    }

    // ---- GWT-LM-09：reset 后行为可复现 ----

    #[test]
    fn reset后重放同一输入输出一致() {
        let (src_l, src_r) = noise_stereo(99, 600);
        let mut st = LimiterStage::from_settings(48_000.0, settings(true, -3.0, 2.5, 0.5, 80.0, true)).unwrap();
        let mut first_l = src_l.clone();
        let mut first_r = src_r.clone();
        st.process(&mut first_l, &mut first_r);
        st.reset();
        let mut again_l = src_l.clone();
        let mut again_r = src_r.clone();
        st.process(&mut again_l, &mut again_r);
        assert_eq!(first_l, again_l);
        assert_eq!(first_r, again_r);
    }

    // ---- GWT-LM-10 / §4.4：改前瞻、禁用→启用均等价新实例 ----

    #[test]
    fn 改前瞻后管线等价于同参新实例() {
        let (tail_src_l, tail_src_r) = noise_stereo(2024, 300);
        let (head_l, head_r) = noise_stereo(123, 500);

        let mut tuned = LimiterStage::from_settings(48_000.0, settings(true, -6.0, 5.0, 0.5, 150.0, true)).unwrap();
        let mut hl = head_l.clone();
        let mut hr = head_r.clone();
        tuned.process(&mut hl, &mut hr); // 先流过一段旧参数
        tuned.configure(settings(true, -6.0, 2.0, 0.5, 150.0, true)); // lookahead 变更 → 清空

        let mut fresh = LimiterStage::from_settings(48_000.0, settings(true, -6.0, 2.0, 0.5, 150.0, true)).unwrap();
        let mut fl = tail_src_l.clone();
        let mut fr = tail_src_r.clone();
        fresh.process(&mut fl, &mut fr);

        let mut al = tail_src_l.clone();
        let mut ar = tail_src_r.clone();
        tuned.process(&mut al, &mut ar);
        assert_eq!(al, fl, "改前瞻后必须与同参新实例逐位一致（陈旧内容不残留）");
        assert_eq!(ar, fr);
    }

    #[test]
    fn 禁用再启用后管线等价于同参新实例() {
        let (head_l, head_r) = noise_stereo(55, 400);
        let (tail_src_l, tail_src_r) = noise_stereo(66, 250);
        let mut st = LimiterStage::from_settings(48_000.0, settings(true, -1.0, 5.0, 0.5, 150.0, true)).unwrap();
        let mut hl = head_l.clone();
        let mut hr = head_r.clone();
        st.process(&mut hl, &mut hr);
        st.configure(settings(false, -1.0, 5.0, 0.5, 150.0, true));
        // 禁用期间送噪声：必须原样直通且不留状态
        let (mut gap_l, mut gap_r) = noise_stereo(77, 120);
        let (gap_before_l, gap_before_r) = (gap_l.clone(), gap_r.clone());
        st.process(&mut gap_l, &mut gap_r);
        assert_eq!(gap_l, gap_before_l);
        assert_eq!(gap_r, gap_before_r);
        st.configure(settings(true, -1.0, 5.0, 0.5, 150.0, true)); // 切回启用 → 清空

        let mut fresh = LimiterStage::from_settings(48_000.0, settings(true, -1.0, 5.0, 0.5, 150.0, true)).unwrap();
        let mut fl = tail_src_l.clone();
        let mut fr = tail_src_r.clone();
        fresh.process(&mut fl, &mut fr);
        let mut tl = tail_src_l.clone();
        let mut tr = tail_src_r.clone();
        st.process(&mut tl, &mut tr);
        assert_eq!(tl, fl, "切回启用后必须与同参新实例逐位一致");
        assert_eq!(tr, fr);
    }

    // ---- GWT-LM-06：极值参数无数值事故 ----

    #[test]
    fn 极值参数全程有限且有界() {
        let combos = [
            settings(true, -60.0, 0.0, 0.0, 0.0, true),
            settings(true, 0.0, 20.0, 0.0, 0.0, true),
            settings(true, -60.0, 20.0, 0.001, 0.001, false),
            settings(true, 0.0, 0.0, 0.05, 1000.0, false),
        ];
        for params in combos {
            let mut st = LimiterStage::from_settings(48_000.0, params.clone()).unwrap();
            // 满幅 ±1 方波串激励（限幅器增益恒 ≤1，输出不得超输入幅值）
            let mut l: Vec<f32> = (0..2048).map(|i| if (i / 16) % 2 == 0 { 1.0 } else { -1.0 }).collect();
            let mut r: Vec<f32> = l.iter().map(|s| -s).collect();
            st.process(&mut l, &mut r);
            for (i, (&a, &b)) in l.iter().zip(r.iter()).enumerate() {
                assert!(a.is_finite() && b.is_finite(), "params={params:?} @{i} 出现非有限值");
            }
            let max_out = l.iter().chain(r.iter()).fold(0.0_f32, |m, s| m.max(s.abs()));
            assert!(max_out <= 1.0 + 1e-6, "限幅器不得增益：{max_out}");
        }
    }

    // ---- 行为冒烟：brickwall 方向正确（非冻结断言）----

    #[test]
    fn 满幅正弦稳态输出贴近阈值() {
        // 3kHz 满幅正弦、阈值 -12dBFS：稳态输出峰值应显著低于输入并接近阈值
        let mut st = LimiterStage::from_settings(48_000.0, settings(true, -12.0, 5.0, 0.5, 150.0, true)).unwrap();
        let frames = 24_000;
        let mut l: Vec<f32> = (0..frames)
            .map(|i| (2.0 * PI * 3_000.0 * i as f64 / 48_000.0).sin() as f32)
            .collect();
        let mut r = l.clone();
        st.process(&mut l, &mut r);
        let peak_out = l.iter().skip(4_000).fold(0.0_f32, |m, s| m.max(s.abs()));
        let thr_lin = 10_f64.powf(-12.0 / 20.0) as f32;
        assert!(peak_out < 0.35, "稳态输出峰值应被压低，实际 {peak_out}");
        assert!(peak_out >= thr_lin * 0.85, "不应过度压限，实际 {peak_out} vs 阈值 {thr_lin}");
        assert!(st.reduction_db() <= 0.0);
    }
}
