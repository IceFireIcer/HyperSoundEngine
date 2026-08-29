//! lufs_meter —— ITU-R BS.1770-4 / EBU R128 响度计量（分析型模块，无音频输出）。
//!
//! 行为事实标准：仓库根 src/dsp/LufsMeter.ts；规格：specs/dsp/lufs-meter.md。
//! 分析型语义（规格 §一/§二）：`process_stereo` 为**就地分析**——L/R 均过 K 加权
//! （TDF2 两级串联），z = 左右 K 加权输出之和；**不改写输入缓冲、无音频输出**，
//! 不进入引擎 22 级处理链的音频路径，行为契约由六个 getter 读数承载。
//!
//! 数值精度铁律（规格 §四.3 落盘量化纪律——跨实现对拍的前提）：
//! - 全部滤波运算、z/z²/sumSq、门限统计、峰值/真峰值均为 f64（对齐 TS Number）；
//! - 仅四处落盘量化为 f32：滑动窗环形 zBuf、块历史 blockLoud/blockPower、
//!   短时环形 shortPower、真峰值多相核 tpKernel 与左右历史 hist；
//! - sumSq 的减项 evict² 取 f32 量化值的平方，加项 z² 取 f64 原值（TS 不对称语义）；
//! - tpKernel 归一化和 sum 以**未量化 f64 系数**累加，再除回 f32 落盘值。
//!
//! 与 TS 源码的逐行对应关系（LufsMeter.ts 行号）：
//! - rbjHighPass（L27–L38）→ rbj_high_pass；
//! - shelfCoeffs（L41–L56）→ shelf_coeffs；
//! - 构造器（L112–L151）→ LufsMeter::new（含 44100/48000 精确系数分支、其余 fs
//!   一律按 48000 系数近似、4× 多相核预计算）；
//! - processStereo（L154–L211）→ process_stereo（逐样本运算次序原样保留）；
//! - recordBlock（L345–L360）→ record_block；
//! - 六个 getter（L214–L312）→ get_* 系列（含 NaN/-Infinity 哨兵语义）；
//! - percentile（L363–L371）→ percentile；updateTruePeakInterp（L379–L395）→
//!   update_true_peak_interp（滞后 TAPS 的因果插值位置、rem_euclid 环形取模）；
//! - reset（L314–L340）→ reset。
//!
//! 确定性：同输入同读数；无随机、无时钟、无日志。

use std::f64::consts::PI;

/// 真峰值 4× 过采样倍率（TS TRUE_PEAK_OVS）。
const TRUE_PEAK_OVS: usize = 4;
/// 真峰值每相抽头数（TS TRUE_PEAK_TAPS_PER_PHASE）。
const TRUE_PEAK_TAPS_PER_PHASE: usize = 24;
/// 真峰值核长 = 每相抽头 × 2（TS TRUE_PEAK_HIST）。
const TRUE_PEAK_HIST: usize = 2 * TRUE_PEAK_TAPS_PER_PHASE;
/// 块历史环形容量 = 1 小时 @100ms 步进（TS BLOCK_CAP）。
const BLOCK_CAP: usize = 36_000;
/// 短时环形容量 = 30 块 = 3 s（TS SHORT_CAP）。
const SHORT_CAP: usize = 30;

/// biquad 五系数（f64，对齐 TS 的 BiquadCoeffs 形态）。
#[derive(Debug, Clone, Copy)]
struct BiquadCoeffs {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

/// biquad 系数 + TDF2 状态（每通道每级一份，TS BiquadState）。
#[derive(Debug, Clone, Copy)]
struct BiquadState {
    c: BiquadCoeffs,
    z1: f64,
    z2: f64,
}

/// RLB 高通：二阶高通（f0=38.135822Hz，Q=0.5），RBJ/BLT 公式（TS L27–L38 逐行）。
fn rbj_high_pass(f0: f64, q: f64, fs: f64) -> BiquadCoeffs {
    let w0 = (2.0 * PI * f0) / fs;
    let alpha = w0.sin() / (2.0 * q);
    let cw = w0.cos();
    let b0 = (1.0 + cw) / 2.0;
    let b1 = -(1.0 + cw);
    let b2 = b0;
    let a0 = 1.0 + alpha;
    let a1 = -2.0 * cw;
    let a2 = 1.0 - alpha;
    BiquadCoeffs {
        b0: b0 / a0,
        b1: b1 / a0,
        b2: b2 / a0,
        a1: a1 / a0,
        a2: a2 / a0,
    }
}

/// 高频搁架 +4dB：常数 Q 搁架（48 kHz 下与 BS.1770 公布系数逐位一致；TS L41–L56）。
fn shelf_coeffs(fs: f64) -> BiquadCoeffs {
    let f0 = 1_681.974_450_955_533_f64;
    let g_db = 3.999_843_853_973_347_f64;
    let q = 0.707_175_236_955_419_6_f64;
    // TS：k = Math.tan((Math.PI * f0) / fs)（注意与高通的 2π 形态不同）。
    let k = ((PI * f0) / fs).tan();
    let vh = 10.0_f64.powf(g_db / 20.0);
    let vb = vh.powf(0.499_666_774_154_541_6_f64);
    let a0 = 1.0 + k / q + k * k;
    BiquadCoeffs {
        b0: (vh + (vb * k) / q + k * k) / a0,
        b1: (2.0 * (k * k - vh)) / a0,
        b2: (vh - (vb * k) / q + k * k) / a0,
        a1: (2.0 * (k * k - 1.0)) / a0,
        a2: (1.0 - k / q + k * k) / a0,
    }
}

/// BS.1770-4 / EBU R128 响度测量仪（对齐 TS `LufsMeter` 类）。
///
/// 构造仅依赖 fs（无参数模块，规格 §一）；`process_stereo` 按任意分块逐样本推进
/// （块边界以跨调用累计样本数判定，读数与分块无关——GWT-LUFSMETER-05），全部块
/// 馈入完成后一次性读取六项 getter。
pub struct LufsMeter {
    /// 400ms 块长（样本数）。
    block_len: usize,
    /// 100ms 步进（样本数）。
    hop_len: usize,

    // K 加权滤波状态：左/右 × 两级
    rlb_l: BiquadState,
    shelf_l: BiquadState,
    rlb_r: BiquadState,
    shelf_r: BiquadState,

    // 滑动窗口（400ms）内 z 环形缓冲（f32 落盘）与 z² 之和（f64 累加）
    z_buf: Vec<f32>,
    z_pos: usize,
    sum_sq: f64,
    total_samples: usize,

    // 块历史（环形；容量 = 1 小时 @100ms 步进；f32 落盘）
    block_loud: Vec<f32>,
    block_power: Vec<f32>,
    block_write: usize,
    block_count: usize,
    // 短时（3s = 30 块）功率环形（f32 落盘）
    short_power: Vec<f32>,
    short_write: usize,
    short_count: usize,

    // 峰值（f64 与门限常量比较）
    peak: f64,
    true_peak: f64,

    // 真峰值 4× 多相插值（左右通道共用一个写入游标；核与历史 f32 落盘）
    tp_kernel: Vec<f32>,
    hist_l: Vec<f32>,
    hist_r: Vec<f32>,
    hist_pos: usize,
    hist_full: bool,

    // LRA 排序暂存（容量 = 块历史容量）
    sort_scratch: Vec<f32>,
}

impl LufsMeter {
    /// 构造：非法采样率（fs ≤ 0 或非有限）报错（对齐 TS 抛 Error，GWT-LUFSMETER-09）。
    ///
    /// 采样率路径（规格 §四.1）：44100/48000 用本采样率精确 K 加权系数；其余采样率
    /// 一律按 48000 系数近似（blockLen/hopLen 仍按实际 fs 缩放，GWT-LUFSMETER-07）。
    pub fn new(fs: f64) -> Result<Self, String> {
        // TS L113：if (fs <= 0 || !Number.isFinite(fs)) throw new Error('invalid sample rate')
        if !(fs > 0.0) || !fs.is_finite() {
            return Err("invalid sample rate".to_string());
        }
        // TS L117：44100/48000 精确系数；其余按 48k 近似（相等比较为字面恒等）。
        let use_fs = if fs == 44_100.0 || fs == 48_000.0 { fs } else { 48_000.0 };
        let rlb = rbj_high_pass(38.135_822_f64, 0.5, use_fs);
        let shelf = shelf_coeffs(use_fs);
        let state = |c: BiquadCoeffs| BiquadState { c, z1: 0.0, z2: 0.0 };

        // TS L125–L126：blockLen/hopLen = max(1, round(0.4·fs / 0.1·fs))。
        // JS Math.round 对正数 = floor(x + 0.5)（0.4·fs 恒正，语义一致）。
        let js_round = |x: f64| (x + 0.5).floor();
        let block_len = js_round(0.4 * fs).max(1.0) as usize;
        let hop_len = js_round(0.1 * fs).max(1.0) as usize;

        let mut meter = Self {
            block_len,
            hop_len,
            rlb_l: state(rlb),
            shelf_l: state(shelf),
            rlb_r: state(rlb),
            shelf_r: state(shelf),
            z_buf: vec![0.0; block_len],
            z_pos: 0,
            sum_sq: 0.0,
            total_samples: 0,
            block_loud: vec![0.0; BLOCK_CAP],
            block_power: vec![0.0; BLOCK_CAP],
            block_write: 0,
            block_count: 0,
            short_power: vec![0.0; SHORT_CAP],
            short_write: 0,
            short_count: 0,
            peak: 0.0,
            true_peak: 0.0,
            tp_kernel: vec![0.0; TRUE_PEAK_OVS * TRUE_PEAK_HIST],
            hist_l: vec![0.0; TRUE_PEAK_HIST],
            hist_r: vec![0.0; TRUE_PEAK_HIST],
            hist_pos: 0,
            hist_full: false,
            sort_scratch: vec![0.0; BLOCK_CAP],
        };
        meter.build_true_peak_kernel();
        Ok(meter)
    }

    /// 预计算 4× 多相插值核（Blackman 窗 sinc，截止 = 原 Nyquist = 4× 率的 1/4，
    /// 逐相归一化；TS L130–L150 逐行）。
    ///
    /// 落盘纪律：归一化和 sum 以未量化 f64 系数累加；落盘值为 f32（先存 c 再除以
    /// sum 写回，两步各自 f32 量化——与 TS 对 Float32Array 的两次写入一致）。
    fn build_true_peak_kernel(&mut self) {
        for phi in 0..TRUE_PEAK_OVS {
            let mut sum = 0.0_f64;
            let base = phi * TRUE_PEAK_HIST;
            for j in 0..TRUE_PEAK_HIST {
                // TS：u = j - (TAPS - 1) + phi / OVS
                let u = j as f64 - (TRUE_PEAK_TAPS_PER_PHASE as f64 - 1.0)
                    + phi as f64 / TRUE_PEAK_OVS as f64;
                let mut c = if u.abs() < 1e-9 {
                    1.0
                } else {
                    let t = (PI * u) / TRUE_PEAK_OVS as f64;
                    t.sin() / t
                };
                let xw = u / TRUE_PEAK_TAPS_PER_PHASE as f64;
                if xw.abs() <= 1.0 {
                    c *= 0.42 + 0.5 * (PI * xw).cos() + 0.08 * (2.0 * PI * xw).cos();
                } else {
                    c = 0.0;
                }
                self.tp_kernel[base + j] = c as f32;
                sum += c;
            }
            if sum != 0.0 {
                for j in 0..TRUE_PEAK_HIST {
                    let v = f64::from(self.tp_kernel[base + j]);
                    self.tp_kernel[base + j] = (v / sum) as f32;
                }
            }
        }
    }

    /// 就地分析立体声（L/R 均过 K 加权；z = L' + R'）。
    ///
    /// 逐样本运算次序（TS L154–L211）：K 加权两级 TDF2 → 滑动窗/sumSq/totalSamples →
    /// 样本峰值 → 真峰值历史推进与插值 → 块边界判定。输入缓冲不被改写（分析型语义）。
    pub fn process_stereo(&mut self, l: &[f32], r: &[f32]) {
        let count = l.len().min(r.len());
        for i in 0..count {
            let xl = f64::from(l[i]);
            let xr = f64::from(r[i]);

            // K 加权（TDF2，两级串联；求值顺序即行为——先 y 后状态，先减后加）
            let y1l = {
                let rl = &mut self.rlb_l;
                let y1 = rl.c.b0 * xl + rl.z1;
                rl.z1 = rl.c.b1 * xl - rl.c.a1 * y1 + rl.z2;
                rl.z2 = rl.c.b2 * xl - rl.c.a2 * y1;
                y1
            };
            let yl = {
                let sl = &mut self.shelf_l;
                let y = sl.c.b0 * y1l + sl.z1;
                sl.z1 = sl.c.b1 * y1l - sl.c.a1 * y + sl.z2;
                sl.z2 = sl.c.b2 * y1l - sl.c.a2 * y;
                y
            };
            let y1r = {
                let rr = &mut self.rlb_r;
                let y1 = rr.c.b0 * xr + rr.z1;
                rr.z1 = rr.c.b1 * xr - rr.c.a1 * y1 + rr.z2;
                rr.z2 = rr.c.b2 * xr - rr.c.a2 * y1;
                y1
            };
            let yr = {
                let sr = &mut self.shelf_r;
                let y = sr.c.b0 * y1r + sr.z1;
                sr.z1 = sr.c.b1 * y1r - sr.c.a1 * y + sr.z2;
                sr.z2 = sr.c.b2 * y1r - sr.c.a2 * y;
                y
            };

            // 块功率：z = yL + yR（BS.1770 通道求和）；加项用 f64 原值、减项（evict²）
            // 用 f32 量化值的平方，sumSq 本身 f64 累加（TS 不对称落盘语义）。
            let z = yl + yr;
            let zsq = z * z;
            let evict = f64::from(self.z_buf[self.z_pos]);
            self.z_buf[self.z_pos] = z as f32;
            self.z_pos += 1;
            if self.z_pos >= self.block_len {
                self.z_pos = 0;
            }
            self.sum_sq += zsq - evict * evict;
            self.total_samples += 1;

            // 样本峰值（f32 输入取绝对值后 f64 比较）
            let a_l = if xl < 0.0 { -xl } else { xl };
            let a_r = if xr < 0.0 { -xr } else { xr };
            if a_l > self.peak {
                self.peak = a_l;
            }
            if a_r > self.peak {
                self.peak = a_r;
            }

            // 真峰值（4× 插值）：左右共用一个写入游标，每样本推进一次
            self.hist_l[self.hist_pos] = l[i];
            self.hist_r[self.hist_pos] = r[i];
            self.hist_pos += 1;
            if self.hist_pos >= TRUE_PEAK_HIST {
                self.hist_pos = 0;
                self.hist_full = true;
            }
            // 当前样本索引 n = totalSamples - 1；可插值位置 t = n - TAPS（滞后保证核窗口因果可用）
            let t = self.total_samples as i64 - 1 - TRUE_PEAK_TAPS_PER_PHASE as i64;
            if self.hist_full && t >= 0 {
                Self::update_true_peak_interp(&self.tp_kernel, &self.hist_l, t, &mut self.true_peak);
                Self::update_true_peak_interp(&self.tp_kernel, &self.hist_r, t, &mut self.true_peak);
            }

            // 块边界（400ms 窗 / 100ms 步进，跨调用累计样本数判定——与分块无关）
            if self.total_samples >= self.block_len
                && (self.total_samples - self.block_len) % self.hop_len == 0
            {
                self.record_block();
            }
        }
    }

    /// 整合响度 LUFS（绝对 -70 + 相对 -10 双门限）；未测到返回 NaN。
    pub fn get_integrated_lufs(&self) -> f64 {
        if self.block_count == 0 {
            return f64::NAN;
        }
        // TS：(blockWrite - blockCount + cap) % cap（非负，等价 rem_euclid）。
        let start = (self.block_write as i64 - self.block_count as i64).rem_euclid(BLOCK_CAP as i64) as usize;
        // TS 同一循环里的 sumP1 累加实际未被后续消费（TS 源码内的死代码，
        // 对读数无可观测影响），忠实保留并以 _ 前缀消警。
        let mut _sum_p1 = 0.0_f64;
        let mut sum_l1 = 0.0_f64;
        let mut n1 = 0_usize;
        for k in 0..self.block_count {
            let idx = (start + k) % BLOCK_CAP;
            // 门限判定消费 f32 落盘的块响度（NaN 块不过门：NaN >= -70 为 false）。
            let lk = f64::from(self.block_loud[idx]);
            if lk >= -70.0 {
                _sum_p1 += f64::from(self.block_power[idx]);
                sum_l1 += lk;
                n1 += 1;
            }
        }
        if n1 == 0 {
            return f64::NAN;
        }
        let gate = sum_l1 / n1 as f64 - 10.0;
        let mut sum_p2 = 0.0_f64;
        let mut n2 = 0_usize;
        for k in 0..self.block_count {
            let idx = (start + k) % BLOCK_CAP;
            let lk = f64::from(self.block_loud[idx]);
            if lk >= -70.0 && lk >= gate {
                sum_p2 += f64::from(self.block_power[idx]);
                n2 += 1;
            }
        }
        if n2 == 0 {
            return f64::NAN;
        }
        -0.691 + 10.0 * (sum_p2 / n2 as f64).log10()
    }

    /// 瞬时响度（最新一个完整 400ms 块的块响度）；未测到/静音末块返回 NaN。
    pub fn get_momentary_lufs(&self) -> f64 {
        if self.block_count == 0 {
            return f64::NAN;
        }
        // TS：(blockWrite - 1 + cap) % cap
        let last = (self.block_write + BLOCK_CAP - 1) % BLOCK_CAP;
        let v = f64::from(self.block_loud[last]);
        if v.is_nan() {
            f64::NAN
        } else {
            v
        }
    }

    /// 短时响度（最近 3s = 30 块的块功率均值）；块数不足 30 或 30 块功率和 ≤ 1e-30
    /// （全静音）返回 NaN。
    pub fn get_short_term_lufs(&self) -> f64 {
        if self.short_count < SHORT_CAP {
            return f64::NAN;
        }
        // TS：(shortWrite - cap + k + 2*cap) % cap ≡ (shortWrite + k) % cap（两者非负同余）。
        let mut sum = 0.0_f64;
        for k in 0..SHORT_CAP {
            let idx = (self.short_write + k) % SHORT_CAP;
            sum += f64::from(self.short_power[idx]);
        }
        if sum <= 1e-30 {
            return f64::NAN;
        }
        -0.691 + 10.0 * (sum / SHORT_CAP as f64).log10()
    }

    /// LRA（EBU Tech 3342，LU）：绝对 -70 + 相对 -20 双门限后 10/95 百分位差；
    /// 总块数/过门 1 块数/过门 2 块数任一 < 2 返回 NaN。
    ///
    /// 取 `&mut self`：升序排序复用预分配暂存（对齐 TS 的 sortScratch 原地排序，
    /// 读取期零分配；读数在全部块馈入完成后一次性读取，不在音频回调内发生）。
    pub fn get_lra(&mut self) -> f64 {
        if self.block_count < 2 {
            return f64::NAN;
        }
        let start = (self.block_write as i64 - self.block_count as i64).rem_euclid(BLOCK_CAP as i64) as usize;
        let mut sum_l = 0.0_f64;
        let mut n1 = 0_usize;
        for k in 0..self.block_count {
            let idx = (start + k) % BLOCK_CAP;
            let lk = f64::from(self.block_loud[idx]);
            if lk >= -70.0 {
                sum_l += lk;
                n1 += 1;
            }
        }
        if n1 < 2 {
            return f64::NAN;
        }
        let gate = sum_l / n1 as f64 - 20.0;
        let mut m = 0_usize;
        for k in 0..self.block_count {
            let idx = (start + k) % BLOCK_CAP;
            let lk_stored = self.block_loud[idx];
            let lk = f64::from(lk_stored);
            if lk >= -70.0 && lk >= gate {
                self.sort_scratch[m] = lk_stored;
                m += 1;
            }
        }
        if m < 2 {
            return f64::NAN;
        }
        // 升序排序后线性插值百分位（对齐 TS Float32Array.subarray(...).sort()）。
        // 过门值必为有限数（静音块响度为 NaN，NaN >= -70 恒 false），partial_cmp 安全。
        self.sort_scratch[..m]
            .sort_by(|a, b| a.partial_cmp(b).expect("过门块响度不可能为 NaN"));
        let p10 = percentile(&self.sort_scratch[..m], 0.1);
        let p95 = percentile(&self.sort_scratch[..m], 0.95);
        p95 - p10
    }

    /// 样本峰值 dBFS（20·log10）；全静音（peak = 0）返回 -Infinity。
    pub fn get_peak_db(&self) -> f64 {
        if self.peak <= 0.0 {
            return f64::NEG_INFINITY;
        }
        20.0 * self.peak.log10()
    }

    /// 真峰值 dBFS（4× 过采样）；全静音（truePeak = 0）返回 -Infinity。
    pub fn get_true_peak_db(&self) -> f64 {
        if self.true_peak <= 0.0 {
            return f64::NEG_INFINITY;
        }
        20.0 * self.true_peak.log10()
    }

    /// 全部状态归零，回到未测量状态（TS reset L314–L340；tpKernel 为预计算常量不清）。
    pub fn reset(&mut self) {
        self.z_buf.fill(0.0);
        self.z_pos = 0;
        self.sum_sq = 0.0;
        self.total_samples = 0;
        self.block_loud.fill(0.0);
        self.block_power.fill(0.0);
        self.block_write = 0;
        self.block_count = 0;
        self.short_power.fill(0.0);
        self.short_write = 0;
        self.short_count = 0;
        self.peak = 0.0;
        self.true_peak = 0.0;
        self.hist_l.fill(0.0);
        self.hist_r.fill(0.0);
        self.hist_pos = 0;
        self.hist_full = false;
        self.rlb_l.z1 = 0.0;
        self.rlb_l.z2 = 0.0;
        self.shelf_l.z1 = 0.0;
        self.shelf_l.z2 = 0.0;
        self.rlb_r.z1 = 0.0;
        self.rlb_r.z2 = 0.0;
        self.shelf_r.z1 = 0.0;
        self.shelf_r.z2 = 0.0;
    }

    // ---------------------------------------------------------------- 内部

    /// 记录一个完整 400ms 块（静音块 p ≤ 1e-30 响度记 NaN，防 -Infinity 泄漏进门限
    /// 统计；p 照常落盘；TS recordBlock L345–L360）。
    fn record_block(&mut self) {
        let p = self.sum_sq / self.block_len as f64;
        let lk = if p > 1e-30 { -0.691 + 10.0 * p.log10() } else { f64::NAN };
        self.block_loud[self.block_write] = lk as f32;
        self.block_power[self.block_write] = p as f32;
        self.block_write += 1;
        if self.block_write >= BLOCK_CAP {
            self.block_write = 0;
        }
        if self.block_count < BLOCK_CAP {
            self.block_count += 1;
        }

        self.short_power[self.short_write] = p as f32;
        self.short_write += 1;
        if self.short_write >= SHORT_CAP {
            self.short_write = 0;
        }
        if self.short_count < SHORT_CAP {
            self.short_count += 1;
        }
    }

    /// 真峰值插值：历史满后对滞后 TAPS 的因果位置做 4× 插值取峰（TS
    /// updateTruePeakInterp L379–L395；只读历史，写 true_peak）。
    #[inline]
    fn update_true_peak_interp(kernel: &[f32], hist: &[f32], t: i64, true_peak: &mut f64) {
        for phi in 0..TRUE_PEAK_OVS {
            let base = phi * TRUE_PEAK_HIST;
            let mut y = 0.0_f64;
            for (j, tap) in kernel[base..base + TRUE_PEAK_HIST].iter().enumerate() {
                // TS：idx = t - j + TAPS - 1；环形取模需容忍负数（t < TAPS 时）。
                let idx = t - j as i64 + TRUE_PEAK_TAPS_PER_PHASE as i64 - 1;
                let ring_idx = idx.rem_euclid(TRUE_PEAK_HIST as i64) as usize;
                y += f64::from(*tap) * f64::from(hist[ring_idx]);
            }
            if y < 0.0 {
                y = -y;
            }
            if y > *true_peak {
                *true_peak = y;
            }
        }
    }
}

/// 线性插值百分位（arr 必须已升序；p ∈ [0,1]；TS percentile L363–L371）。
fn percentile(arr: &[f32], p: f64) -> f64 {
    let n = arr.len();
    if n == 1 {
        return f64::from(arr[0]);
    }
    let rank = p * (n - 1) as f64;
    let lo = rank.floor();
    let hi = (lo + 1.0).min((n - 1) as f64);
    let frac = rank - lo;
    f64::from(arr[lo as usize]) + frac * (f64::from(arr[hi as usize]) - f64::from(arr[lo as usize]))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------- 确定性信号（与 scripts/export-vectors.mjs 同族参数，逐位复刻） ----------------

    /// 固定种子 LCG 伪噪声（整数运算跨语言逐位一致；f32 落盘一次量化）。
    fn lcg_noise(frames: usize, seed: u32, amp: f64) -> Vec<f32> {
        let mut s = seed;
        let mut x = Vec::with_capacity(frames);
        for _ in 0..frames {
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            x.push((((f64::from(s) / 4_294_967_296.0) * 2.0 - 1.0) * amp) as f32);
        }
        x
    }

    /// 多频正弦叠加（f64 双精度计算后一次 f32 落盘）。
    fn sine_sum(frames: usize, sample_rate: f64, comps: &[(f64, f64, f64)]) -> Vec<f32> {
        let mut x = Vec::with_capacity(frames);
        for i in 0..frames {
            let mut acc = 0.0_f64;
            for &(freq, amp, phase) in comps {
                acc += amp * ((2.0 * PI * freq * i as f64) / sample_rate + phase).sin();
            }
            x.push(acc as f32);
        }
        x
    }

    /// TS `x[i] *= k` 语义：f64 乘法后一次 f32 落盘（不能写成 f32 乘法——0.1 的
    /// f32 近似会改变舍入结果）。
    fn scale_in_place(x: &mut [f32], k: f64) {
        for v in x.iter_mut() {
            *v = (f64::from(*v) * k) as f32;
        }
    }

    /// 按 blockSize 分块馈入（末块可短），返回块数。
    fn feed(meter: &mut LufsMeter, l: &[f32], r: &[f32], block_size: usize) -> usize {
        let mut blocks = 0;
        let mut offset = 0;
        while offset < l.len() {
            let len = (l.len() - offset).min(block_size);
            meter.process_stereo(&l[offset..offset + len], &r[offset..offset + len]);
            offset += len;
            blocks += 1;
        }
        blocks
    }

    /// 六项读数快照。
    #[derive(Debug, Clone, Copy, PartialEq)]
    struct Readings {
        integrated: f64,
        momentary: f64,
        short_term: f64,
        lra: f64,
        peak_db: f64,
        true_peak_db: f64,
    }

    fn read(m: &mut LufsMeter) -> Readings {
        Readings {
            integrated: m.get_integrated_lufs(),
            momentary: m.get_momentary_lufs(),
            short_term: m.get_short_term_lufs(),
            lra: m.get_lra(),
            peak_db: m.get_peak_db(),
            true_peak_db: m.get_true_peak_db(),
        }
    }

    /// 黄金参考断言：哨兵按等值（NaN 位型 / 同号无穷大），有限值走绝对容差。
    /// 黄金值来自 node 直跑仓库根 src/dsp/LufsMeter.ts（输入信号按同一确定性公式
    /// 生成）；容差 1e-6 远宽于跨库 libm 1 ulp 级噪声、远严于任何结构/次序错误。
    fn assert_readings_close(got: Readings, want: Readings, tol: f64, label: &str) {
        let pairs = [
            ("integratedLufs", got.integrated, want.integrated),
            ("momentaryLufs", got.momentary, want.momentary),
            ("shortTermLufs", got.short_term, want.short_term),
            ("lra", got.lra, want.lra),
            ("peakDb", got.peak_db, want.peak_db),
            ("truePeakDb", got.true_peak_db, want.true_peak_db),
        ];
        for (name, g, w) in pairs {
            if w.is_nan() {
                assert!(g.is_nan(), "{label}.{name}：got {g}，want NaN（哨兵等值）");
            } else if w == f64::NEG_INFINITY {
                assert!(
                    g == f64::NEG_INFINITY,
                    "{label}.{name}：got {g}，want -Infinity（哨兵等值）"
                );
            } else {
                assert!(
                    (g - w).abs() <= tol,
                    "{label}.{name}：got {g}，want {w}，|dev| = {} 超出容差 {tol}",
                    (g - w).abs()
                );
            }
        }
    }

    // ---------------- 黄金参考（node 直跑 TS 源码捕获，JSON 最短往返表示） ----------------

    /// A：48 kHz 稳态 997 Hz 纯音（幅度 0.1/声道，96000 帧，块 777 非整除）。
    /// 块数 19 < 30 → shortTerm = NaN。
    mod golden {
        use super::Readings;

        pub const A: Readings = Readings {
            integrated: -17.032962523071564,
            momentary: -17.033370971679688,
            short_term: f64::NAN,
            lra: 0.0030612945556640625,
            peak_db: -19.99999987057016,
            true_peak_db: -20.002594520013957,
        };
        /// B：44.1 kHz（精确系数路径）1 kHz 正弦 0.88 + LCG 噪声 0.1，头 4410 帧静音，
        /// 块 500。块数 5 < 30 → shortTerm = NaN。
        pub const B: Readings = Readings {
            integrated: 0.6618665335557837,
            momentary: 0.6695103645324707,
            short_term: f64::NAN,
            lra: 0.013005790114402838,
            peak_db: -0.1777729621775159,
            true_peak_db: -0.2919734853418469,
        };
        /// C：48 kHz 两电平节目（响段 + LCG、0.1× 静段，120000 帧，块 1024）。
        /// 块数 24 < 30 → shortTerm = NaN；LRA 全路径 ≈ 20 LU。
        pub const C: Readings = Readings {
            integrated: -7.66528352693089,
            momentary: -23.582473754882812,
            short_term: f64::NAN,
            lra: 20.002248728275298,
            peak_db: -4.437331017974574,
            true_peak_db: -4.580558433466363,
        };
        /// D：48 kHz 4 s 稳态（192000 帧 → 39 块，短时环形滚动越过容量），块 321。
        /// 六项读数全部有限。
        pub const D: Readings = Readings {
            integrated: -17.468860488857576,
            momentary: -17.47011947631836,
            short_term: -17.468869797825054,
            lra: 0.003063201904296875,
            peak_db: -19.99999987057016,
            true_peak_db: -20.002594520013957,
        };
        /// E：32 kHz（非 44.1k/48k → 按 48k 系数近似，blockLen/hop 仍随 fs 缩放），
        /// 48000 帧 997 Hz 纯音，块 999。近似路径读数有限无 NaN（GWT-LUFSMETER-07）。
        pub const E: Readings = Readings {
            integrated: -15.70042039019309,
            momentary: -15.70079231262207,
            short_term: f64::NAN,
            lra: 0.002963542938232422,
            peak_db: -19.99999987057016,
            true_peak_db: -20.00111416353803,
        };
    }

    /// 生成黄金用例 B 的输入（44.1 kHz 正弦 + LCG，头 4410 帧静音）。
    fn signal_b(frames: usize) -> (Vec<f32>, Vec<f32>) {
        let fs = 44_100.0;
        let mut l = sine_sum(frames, fs, &[(1000.0, 0.88, 0.0)]);
        let mut r = sine_sum(frames, fs, &[(1000.0, 0.88, PI / 3.0)]);
        let nl = lcg_noise(frames, 53001, 0.1);
        let nr = lcg_noise(frames, 53002, 0.1);
        for i in 4410..frames {
            l[i] += nl[i];
            r[i] += nr[i];
        }
        (l, r)
    }

    /// 生成黄金用例 C 的输入（48 kHz 两电平节目）。
    fn signal_c(frames: usize) -> (Vec<f32>, Vec<f32>) {
        let fs = 48_000.0;
        let mut l = sine_sum(frames, fs, &[(1000.0, 0.5, 0.0)]);
        let mut r = sine_sum(frames, fs, &[(1000.0, 0.5, PI / 4.0)]);
        let nl = lcg_noise(frames, 54001, 0.1);
        let nr = lcg_noise(frames, 54002, 0.1);
        for i in 0..frames {
            l[i] += nl[i];
            r[i] += nr[i];
        }
        for v in l.iter_mut().skip(48_000) {
            *v = (f64::from(*v) * 0.1) as f32;
        }
        for v in r.iter_mut().skip(48_000) {
            *v = (f64::from(*v) * 0.1) as f32;
        }
        (l, r)
    }

    // ---------------- 单元测试 ----------------

    #[test]
    fn 非法采样率构造报错() {
        for bad in [0.0_f64, -44_100.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let err = LufsMeter::new(bad).err().expect("非法采样率必须报错");
            assert!(err.contains("invalid sample rate"), "应对齐 TS 错误信息：{err}");
        }
        assert!(LufsMeter::new(48_000.0).is_ok());
    }

    #[test]
    fn 静音输入读数全哨兵() {
        // 0.5 s 全零（2 个完整分析块，全为静音块）：GWT-LUFSMETER-02 的单测投影。
        let mut meter = LufsMeter::new(48_000.0).expect("合法采样率");
        let zeros = vec![0.0_f32; 24_000];
        feed(&mut meter, &zeros, &zeros, 256);
        let got = read(&mut meter);
        assert!(got.integrated.is_nan(), "integrated 应为 NaN，实际 {}", got.integrated);
        assert!(got.momentary.is_nan(), "momentary 应为 NaN（静音块透传）");
        assert!(got.short_term.is_nan(), "shortTerm 应为 NaN（块数 < 30）");
        assert!(got.lra.is_nan(), "lra 应为 NaN（有效块 < 2）");
        assert_eq!(got.peak_db, f64::NEG_INFINITY, "peakDb 应为 -Infinity");
        assert_eq!(got.true_peak_db, f64::NEG_INFINITY, "truePeakDb 应为 -Infinity");
    }

    #[test]
    fn 稳态纯音_48k_读数命中_ts黄金参考() {
        let fs = 48_000.0;
        let l = sine_sum(96_000, fs, &[(997.0, 0.1, 0.0)]);
        let r = sine_sum(96_000, fs, &[(997.0, 0.1, 0.0)]);
        let mut meter = LufsMeter::new(fs).expect("合法采样率");
        feed(&mut meter, &l, &r, 777);
        assert_readings_close(read(&mut meter), golden::A, 1e-6, "A");
    }

    #[test]
    fn 正弦加lcg_44k1精确系数路径_读数命中_ts黄金参考() {
        let (l, r) = signal_b(22_050);
        let mut meter = LufsMeter::new(44_100.0).expect("合法采样率");
        feed(&mut meter, &l, &r, 500);
        assert_readings_close(read(&mut meter), golden::B, 1e-6, "B");
    }

    #[test]
    fn 两电平节目_lra全路径_读数命中_ts黄金参考() {
        let (l, r) = signal_c(120_000);
        let mut meter = LufsMeter::new(48_000.0).expect("合法采样率");
        feed(&mut meter, &l, &r, 1024);
        assert_readings_close(read(&mut meter), golden::C, 1e-6, "C");
    }

    #[test]
    fn 短时环形滚动_读数命中_ts黄金参考() {
        // 4 s → 39 块 > 30，短时环形滚动越过容量后读数仍稳定有限。
        let fs = 48_000.0;
        let l = sine_sum(192_000, fs, &[(997.0, 0.1, 0.0)]);
        let r = sine_sum(192_000, fs, &[(997.0, 0.1, PI / 5.0)]);
        let mut meter = LufsMeter::new(fs).expect("合法采样率");
        feed(&mut meter, &l, &r, 321);
        assert_readings_close(read(&mut meter), golden::D, 1e-6, "D");
    }

    #[test]
    fn 采样率32k_按48k系数近似_读数有限且命中_ts黄金参考() {
        // 非 44100/48000 → 滤波系数一律按 48k 近似；blockLen/hopLen 仍按实际 fs 缩放
        // （32000 → blockLen 12800 / hop 3200），近似路径不抛错、读数有限（GWT-07）。
        let fs = 32_000.0;
        let l = sine_sum(48_000, fs, &[(997.0, 0.1, 0.0)]);
        let r = sine_sum(48_000, fs, &[(997.0, 0.1, 0.0)]);
        let mut meter = LufsMeter::new(fs).expect("合法采样率");
        feed(&mut meter, &l, &r, 999);
        let got = read(&mut meter);
        assert_readings_close(got, golden::E, 1e-6, "E");
        assert!(got.integrated.is_finite() && got.lra.is_finite());
    }

    #[test]
    fn 分块不变性_任意分块读数逐位一致() {
        // GWT-LUFSMETER-05：块边界以跨调用累计样本数判定、逐样本运算次序与分块无关。
        let (l, r) = signal_b(22_050);
        let reference = {
            let mut meter = LufsMeter::new(44_100.0).expect("合法采样率");
            feed(&mut meter, &l, &r, l.len());
            read(&mut meter)
        };
        for block_size in [1_usize, 7, 500, 4096] {
            let mut meter = LufsMeter::new(44_100.0).expect("合法采样率");
            feed(&mut meter, &l, &r, block_size);
            let got = read(&mut meter);
            assert_eq!(
                got.integrated.to_bits(),
                reference.integrated.to_bits(),
                "blockSize={block_size} 的 integrated 必须与整段逐位一致"
            );
            assert_eq!(got.momentary.to_bits(), reference.momentary.to_bits());
            assert_eq!(got.short_term.to_bits(), reference.short_term.to_bits());
            assert_eq!(got.lra.to_bits(), reference.lra.to_bits());
            assert_eq!(got.peak_db.to_bits(), reference.peak_db.to_bits());
            assert_eq!(got.true_peak_db.to_bits(), reference.true_peak_db.to_bits());
        }
    }

    #[test]
    fn reset_回到未测量状态且二次读数与首次一致() {
        // GWT-LUFSMETER-08：reset 后 integrated = NaN、peakDb = -Infinity，重放同输入
        // 读数逐位复现。
        let (l, r) = signal_c(120_000);
        let mut meter = LufsMeter::new(48_000.0).expect("合法采样率");
        feed(&mut meter, &l, &r, 1024);
        let first = read(&mut meter);
        assert!(first.integrated.is_finite(), "首测 integrated 应有限");

        meter.reset();
        let cleared = read(&mut meter);
        assert!(cleared.integrated.is_nan());
        assert!(cleared.momentary.is_nan());
        assert!(cleared.short_term.is_nan());
        assert!(cleared.lra.is_nan());
        assert_eq!(cleared.peak_db, f64::NEG_INFINITY);
        assert_eq!(cleared.true_peak_db, f64::NEG_INFINITY);

        feed(&mut meter, &l, &r, 1024);
        let second = read(&mut meter);
        assert_eq!(second.integrated.to_bits(), first.integrated.to_bits());
        assert_eq!(second.momentary.to_bits(), first.momentary.to_bits());
        assert_eq!(second.short_term.to_bits(), first.short_term.to_bits());
        assert_eq!(second.lra.to_bits(), first.lra.to_bits());
        assert_eq!(second.peak_db.to_bits(), first.peak_db.to_bits());
        assert_eq!(second.true_peak_db.to_bits(), first.true_peak_db.to_bits());
    }

    #[test]
    fn 就地分析不改写输入缓冲() {
        let fs = 48_000.0;
        let l = sine_sum(2048, fs, &[(440.0, 0.3, 0.0)]);
        let r = lcg_noise(2048, 12345, 0.2);
        let l_snapshot = l.clone();
        let r_snapshot = r.clone();
        let mut meter = LufsMeter::new(fs).expect("合法采样率");
        meter.process_stereo(&l, &r);
        assert_eq!(l, l_snapshot, "分析型语义：左声道输入不得被改写");
        assert_eq!(r, r_snapshot, "分析型语义：右声道输入不得被改写");
    }

    #[test]
    fn scale_in_place_复刻_ts的f64乘后落盘语义() {
        // f32 直接乘 0.1 与 f64 乘 0.1 后落盘可能差 1 ulp：固化 helper 的语义。
        let mut a = vec![0.7_f32];
        scale_in_place(&mut a, 0.1);
        let b = (f64::from(0.7_f32) * 0.1) as f32;
        assert_eq!(a[0], b);
        // 反例证明 f32 直乘可能不同（0.7·f32(0.1) 与 0.7·f64(0.1) 的舍入不一致）。
        let c = 0.7_f32 * 0.1_f32;
        // 不强求 c != a[0] 恒成立，但两者都必须落在 1e-7 相对量级内。
        assert!((f64::from(a[0]) - f64::from(c)).abs() < 1e-7);
    }
}
