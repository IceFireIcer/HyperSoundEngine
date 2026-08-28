//! compressor —— 动态压缩器（立体声联合包络 + 软拐点 knee + sidechain 外部驱动）。
//!
//! 行为事实标准：仓库根 `src/dsp/Compressor.ts`；规格：`specs/dsp/compressor.md`。
//! 本文件是该 TS 类的逐行移植，关键对应关系：
//!
//! - 包络（规格 §4.1）：立体声联合峰值 `e = max(|el|,|er|)` + attack/release 双系数
//!   一阶跟随；env 为单标量立体声联动状态——左右声道共用同一 env、同一增益。
//! - dB 域软拐点三区公式（规格 §4.2）：knee=0 退化硬拐点；ratio=1 ⇒ invRatio=0 ⇒
//!   任意电平压缩量恒 0；膝区二次曲线 `(invRatio·x²)/(2·knee)`。
//! - 增益（规格 §4.3）：`g = 10^(−reduction/20) · makeupLin · outputGain`，
//!   左右同增益就地写回。
//! - sidechain（规格 §4.4/§4.5）：`sidechainEnabled` 属参数快照但 TS 模块自身
//!   **不读取**——它是引擎接线层标志。向量驱动器语义按规格 §4.5：
//!   `sideL[n] = sideR[n] = inL[n] + inR[n]`（就地处理前快照），由
//!   `sidechain_from_input` 分支实现；显式外部 sidechain 走
//!   [`CompressorStage::process_with_sidechain`]（服务管线后续接线入口，
//!   不违反 [`Stage`] trait 的两参形态）。
//!
//! # 数值精度铁律的落点
//!
//! - 一切以 TS Number（f64）参与运算的中间量（one-pole 系数、env、levelDb、
//!   reduction、增益 g）全部用 f64 复刻，运算顺序与 TS 逐行一致；
//! - f32 落点与 TS Float32Array 写入点一一对应：输出样本写回，以及 sidechain
//!   快照缓冲——导出工具以 `side[i] = l[i] + r[i]`（f64 加法）写入 `Float32Array`，
//!   因此快照值**经 f32 量化后**才喂给模块（规格 §4.5「双精度派生」的落地形态，
//!   与 scripts/export-vectors.mjs 逐字一致）；
//! - `Math.max` 的 NaN 传播语义以 [`js_max`] 显式复刻（理由同 biquad.rs）。
//!
//! # 实时安全
//!
//! sidechain 快照缓冲在 [`Stage::prepare`] 中按最大块长预分配，`process` 稳态
//! 零分配、零锁、零系统调用；内部状态仅 env 与 reductionDb 两个双精度标量。

use crate::Stage;

/// 对齐 TS `CompressorSettings` 的参数快照（字段名蛇形转换）。
#[derive(Debug, Clone)]
pub struct CompressorSettings {
    pub enabled: bool,
    pub threshold_db: f64,
    pub ratio: f64,
    pub knee_db: f64,
    pub attack_ms: f64,
    pub release_ms: f64,
    pub makeup_db: f64,
    pub output_gain: f64,
    /// TS 可选字段。TS `Compressor` 类自身不读取（规格 §4.4）；此处仅作接线标志
    /// 记录：为 true 时 [`Stage::process`] 按规格 §4.5 从本块输入派生单声道和
    /// sidechain（向量驱动器语义），显式外部 sidechain 则走
    /// [`CompressorStage::process_with_sidechain`]。
    pub sidechain_enabled: bool,
}

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

/// TS `onePoleCoef(timeMs, fs, floorMs)` 的逐字复刻：
/// `coef = 1 − exp(−1 / ((max(ms, floor)/1000) × fs))`，ms 下限 0.05 生效。
fn one_pole_coef(time_ms: f64, fs: f64, floor_ms: f64) -> f64 {
    let ms = js_max(time_ms, floor_ms);
    1.0 - (-1.0 / ((ms / 1000.0) * fs)).exp()
}

/// 单次 process 调用内不变的派生量（对齐 TS processStereo 开头的一组局部 const）。
struct LoopConstants {
    attack: f64,
    release: f64,
    thr: f64,
    inv_ratio: f64,
    knee: f64,
    knee_half: f64,
    two_knee: f64,
    gain_scale: f64,
}

/// 一个已配置的动态压缩器阶段（字段一一对应 TS `Compressor` 私有域）。
pub struct CompressorStage {
    fs: f64,
    // —— 生效参数（configure 钳制后的取值）——
    enabled: bool,
    threshold_db: f64,
    ratio: f64,
    knee_db: f64,
    attack_coef: f64,
    release_coef: f64,
    makeup_lin: f64,
    output_gain: f64,
    // —— 状态（规格 §4.6：仅两项）——
    env: f64,
    reduction_db: f64,
    // —— 接线标志与快照缓冲（Rust 侧对规格 §4.5 驱动器语义的落地）——
    sidechain_from_input: bool,
    side_buf: Vec<f32>,
}

impl CompressorStage {
    /// 按 TS 构造函数内置默认创建（enabled=true / thresholdDb=-20 / ratio=4 /
    /// kneeDb=6 / attackMs=10 / releaseMs=150 / makeupDb=0 / outputGain=1）。
    pub fn new(sample_rate: f64) -> Result<Self, String> {
        Self::from_settings(
            sample_rate,
            CompressorSettings {
                enabled: true,
                threshold_db: -20.0,
                ratio: 4.0,
                knee_db: 6.0,
                attack_ms: 10.0,
                release_ms: 150.0,
                makeup_db: 0.0,
                output_gain: 1.0,
                sidechain_enabled: false,
            },
        )
    }

    /// 以显式参数快照构造（对齐 TS `setParams` 整体替换语义；钳制规则见规格参数表）。
    ///
    /// sampleRate ≤ 0 或非有限时报错（GWT-CP-13，对齐 TS `Error('invalid sample rate')`）。
    pub fn from_settings(sample_rate: f64, settings: CompressorSettings) -> Result<Self, String> {
        if !(sample_rate.is_finite() && sample_rate > 0.0) {
            return Err("invalid sample rate".into());
        }
        let mut stage = Self {
            fs: sample_rate,
            enabled: true,
            threshold_db: -20.0,
            ratio: 4.0,
            knee_db: 6.0,
            attack_coef: 0.0,
            release_coef: 0.0,
            makeup_lin: 1.0,
            output_gain: 1.0,
            env: 0.0,
            reduction_db: 0.0,
            sidechain_from_input: false,
            side_buf: Vec::new(),
        };
        stage.configure(settings);
        Ok(stage)
    }

    /// 覆盖参数快照（对齐 TS `applyParams`，逐行同序）。
    ///
    /// 参数即时生效：钳制 + 系数重算；**包络状态保留不清零**（规格 §4.6，
    /// 与 limiter 的管线清空语义不同）。`sidechainEnabled` 不影响任何 DSP 状态，
    /// 仅更新接线标志（见 [`CompressorSettings::sidechain_enabled`]）。
    pub fn configure(&mut self, settings: CompressorSettings) {
        self.enabled = settings.enabled;
        self.threshold_db = clamp(settings.threshold_db, -80.0, 0.0);
        self.ratio = clamp(settings.ratio, 1.0, 100.0);
        self.knee_db = clamp(settings.knee_db, 0.0, 40.0);
        self.attack_coef = one_pole_coef(settings.attack_ms, self.fs, 0.05);
        self.release_coef = one_pole_coef(settings.release_ms, self.fs, 0.05);
        self.makeup_lin = 10.0_f64.powf(clamp(settings.makeup_db, -24.0, 24.0) / 20.0);
        self.output_gain = clamp(settings.output_gain, 0.0, 2.0);
        self.sidechain_from_input = settings.sidechain_enabled;
    }

    /// 当前增益衰减 dB（≤ 0，不含 makeup/outputGain；对齐 TS `getReductionDb`，
    /// 取最近一个被处理样本的衰减报告）。
    pub fn reduction_db(&self) -> f64 {
        self.reduction_db
    }

    /// 单次处理的派生量快照（对齐 TS processStereo 开头的局部 const 组）。
    fn loop_constants(&self) -> LoopConstants {
        LoopConstants {
            attack: self.attack_coef,
            release: self.release_coef,
            thr: self.threshold_db,
            inv_ratio: 1.0 - 1.0 / self.ratio,
            knee: self.knee_db,
            knee_half: self.knee_db * 0.5,
            two_knee: 2.0 * self.knee_db,
            gain_scale: self.makeup_lin * self.output_gain,
        }
    }

    /// 显式外部 sidechain 接线入口（规格 §4.4：提供了 sideL/sideR 时包络检测改用
    /// 外部信号，音频路径仍处理 left/right 本体）。
    ///
    /// 这是服务管线后续接线的入口（引擎按 `sidechainEnabled` 决定是否以四参形态
    /// 提供外部检测信号），不违反 [`Stage`] trait 的两参形态。
    /// `side_l`/`side_r` 长度不得小于本块帧数。
    pub fn process_with_sidechain(
        &mut self,
        left: &mut [f32],
        right: &mut [f32],
        side_l: &[f32],
        side_r: &[f32],
    ) {
        debug_assert_eq!(left.len(), right.len(), "左右声道块长必须一致（Stage 契约）");
        debug_assert!(side_l.len() >= left.len() && side_r.len() >= left.len());
        if !self.enabled {
            self.reduction_db = 0.0;
            return;
        }
        let c = self.loop_constants();
        let mut env = self.env;
        let mut reduction_db = self.reduction_db;
        run_loop(left, right, Some((side_l, side_r)), &c, &mut env, &mut reduction_db);
        self.env = env;
        self.reduction_db = reduction_db;
    }

    /// 切换「从本块输入派生 sidechain」的接线标志（规格 §4.5 向量驱动器语义）。
    pub fn set_sidechain_from_input(&mut self, flag: bool) {
        self.sidechain_from_input = flag;
    }

    /// 规格 §4.5 快照：`side[i] = (f64(l[i]) + f64(r[i])) as f32`——f64 加法后
    /// 写入 f32 缓冲（对齐导出工具 `new Float32Array` 的存储量化），且必须在
    /// 就地处理**之前**完成（本函数在写回任何输出样本前调用）。
    fn snapshot_side_from_input(&mut self, left: &[f32], right: &[f32]) {
        let n = left.len();
        if self.side_buf.len() < n {
            // 仅当调用方未按 prepare 预分配到位时发生（非稳态兜底路径）。
            self.side_buf.resize(n, 0.0);
        }
        for i in 0..n {
            self.side_buf[i] = (f64::from(left[i]) + f64::from(right[i])) as f32;
        }
    }
}

/// 压缩器逐样本主循环（对齐 TS processStereo 循环体，逐行同序）。
///
/// `side` 为 `Some((side_l, side_r))` 时包络检测改用外部信号（规格 §4.4），
/// 音频路径仍处理 left/right 本体；`None` 时退化为内部联合包络。
/// env 与衰减报告通过 `&mut` 写回调用方状态，循环内不触碰 `self`。
fn run_loop(
    left: &mut [f32],
    right: &mut [f32],
    side: Option<(&[f32], &[f32])>,
    c: &LoopConstants,
    env: &mut f64,
    reduction_db: &mut f64,
) {
    let n = left.len();
    for i in 0..n {
        // TS L83–L93：先读本体样本，再按 useSide 选检测信号。
        let xl = f64::from(left[i]);
        let xr = f64::from(right[i]);
        let (el, er) = match side {
            Some((sl, sr)) => (f64::from(sl[i]), f64::from(sr[i])),
            None => (xl, xr),
        };
        // 1) 立体声联合包络（峰值检测）；TS L94 的三目链逐字复刻。
        let e = if el.abs() > er.abs() { el.abs() } else { er.abs() };
        // TS L95–L96：attack/release 双系数一阶跟随。
        if e > *env {
            *env += c.attack * (e - *env);
        } else {
            *env += c.release * (e - *env);
        }
        // 2) dB 域软拐点三区公式（TS L98–L109，分支顺序即行为）。
        let level_db = 20.0 * (*env + 1e-12).log10();
        let reduction: f64;
        if c.knee <= 0.0 {
            reduction = if level_db > c.thr {
                (level_db - c.thr) * c.inv_ratio
            } else {
                0.0
            };
        } else if level_db < c.thr - c.knee_half {
            reduction = 0.0;
        } else if level_db > c.thr + c.knee_half {
            reduction = (level_db - c.thr) * c.inv_ratio;
        } else {
            let x = level_db - (c.thr - c.knee_half);
            reduction = (c.inv_ratio * x * x) / c.two_knee;
        }
        *reduction_db = -reduction;
        // 3) makeup + outputGain 补偿，左右同增益写回（TS L112–L114）。
        let g = 10.0_f64.powf(-reduction / 20.0) * c.gain_scale;
        left[i] = (xl * g) as f32;
        right[i] = (xr * g) as f32;
    }
}

impl Stage for CompressorStage {
    /// 预分配 sidechain 快照缓冲（仅接线标志开启时需要）；无其他工作缓冲。
    fn prepare(&mut self, max_block_size: usize) {
        if self.sidechain_from_input && self.side_buf.len() < max_block_size {
            self.side_buf.resize(max_block_size, 0.0);
        }
    }

    /// 就地处理一个立体声块；状态跨块保持（GWT-CP-09：切块不改变逐样本运算序列）。
    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        debug_assert_eq!(left.len(), right.len(), "左右声道块长必须一致（Stage 契约）");
        // TS L66–L69：禁用即逐位直通，缓冲不被改写，衰减报告归 0。
        if !self.enabled {
            self.reduction_db = 0.0;
            return;
        }
        let n = left.len();
        if self.sidechain_from_input {
            self.snapshot_side_from_input(left, right);
        }
        // 局部化派生量与状态（对齐 TS L71–L79 的一组局部 const）；
        // 循环内不触碰 self，从而允许 side 快照切片与 env 可变写回并存。
        let c = self.loop_constants();
        let mut env = self.env;
        let mut reduction_db = self.reduction_db;
        if self.sidechain_from_input {
            // 同一派生数组传入两参（sideL 与 sideR 内容相同，规格 §4.5）。
            let side = &self.side_buf[..n];
            run_loop(left, right, Some((side, side)), &c, &mut env, &mut reduction_db);
        } else {
            run_loop(left, right, None, &c, &mut env, &mut reduction_db);
        }
        self.env = env;
        self.reduction_db = reduction_db;
    }

    /// reset()：env 与 reductionDb 归零（规格 §4.6；参数保留）。
    fn reset(&mut self) {
        self.env = 0.0;
        self.reduction_db = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 确定性伪噪声（LCG，无随机依赖），幅度 [-1, 1)，可整体缩放。
    fn lcg_noise(n: usize, seed: u32, amp: f32) -> Vec<f32> {
        let mut u = seed;
        (0..n)
            .map(|_| {
                u = u.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (((f64::from(u) / f64::from(u32::MAX)) * 2.0 - 1.0) * f64::from(amp)) as f32
            })
            .collect()
    }

    fn sine(n: usize, freq: f64, fs: f64, amp: f64, phase: f64) -> Vec<f32> {
        (0..n)
            .map(|i| (2.0 * std::f64::consts::PI * freq * i as f64 / fs + phase).sin() as f32 * amp as f32)
            .collect()
    }

    fn settings() -> CompressorSettings {
        CompressorSettings {
            enabled: true,
            threshold_db: -20.0,
            ratio: 4.0,
            knee_db: 6.0,
            attack_ms: 10.0,
            release_ms: 150.0,
            makeup_db: 0.0,
            output_gain: 1.0,
            sidechain_enabled: false,
        }
    }

    fn drive_in_chunks(stage: &mut CompressorStage, l: &[f32], r: &[f32], block: usize) -> (Vec<f32>, Vec<f32>) {
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
            let err = CompressorStage::new(bad).err().expect("非法采样率必须报错");
            assert!(err.contains("invalid sample rate"), "错误信息应与 TS 一致：{err}");
        }
    }

    #[test]
    fn 阈下直通_输出与输入逐位一致且衰减恒零() {
        // GWT-CP-01：输入峰值约 0.02（≈ -34 dBFS），低于 -20-3=-23 dBFS 下膝点，
        // 压缩量恒 0、增益恰为 1 → xl·1.0 精确 → 逐位一致。
        let n = 4800;
        let input_l = lcg_noise(n, 7, 0.02);
        let input_r = lcg_noise(n, 91, 0.02);
        let mut stage = CompressorStage::from_settings(48000.0, settings()).unwrap();
        stage.prepare(256);
        let (out_l, out_r) = drive_in_chunks(&mut stage, &input_l, &input_r, 256);
        assert_eq!(out_l, input_l, "左声道必须逐位一致");
        assert_eq!(out_r, input_r, "右声道必须逐位一致");
        assert_eq!(stage.reduction_db(), 0.0);
    }

    #[test]
    fn 重压缩稳态_输出收敛于阈值加makeup附近() {
        // GWT-CP-02：硬拐点 knee=0、ratio=20、makeup +6 dB，0.9 幅度正弦稳态：
        // levelDb ≈ 20·log10(0.9) ≈ -0.915 → reduction ≈ 18.13 dB → 输出 ≈ 0.22。
        let n = 9600;
        let input = sine(n, 220.0, 48000.0, 0.9, 0.0);
        let mut p = settings();
        p.threshold_db = -20.0;
        p.ratio = 20.0;
        p.knee_db = 0.0;
        p.attack_ms = 5.0;
        p.release_ms = 100.0;
        p.makeup_db = 6.0;
        let mut stage = CompressorStage::from_settings(48000.0, p).unwrap();
        stage.prepare(384);
        let (out_l, _) = drive_in_chunks(&mut stage, &input, &input, 384);
        let tail = &out_l[n - 480..];
        for (i, &x) in tail.iter().enumerate() {
            assert!(x.is_finite(), "稳态输出必须有限 @{i}");
        }
        // 稳态峰值电平（正弦瞬时值过零，只能对峰值断言）。
        let tail_peak = tail.iter().map(|&x| f64::from(x).abs()).fold(0.0_f64, f64::max);
        assert!(
            tail_peak > 0.12 && tail_peak < 0.32,
            "稳态峰值应收敛于阈值+makeup 附近，实际 {tail_peak}"
        );
        assert!(stage.reduction_db() < -10.0, "衰减报告应为显著负值");
    }

    #[test]
    fn 立体声联动_同输入两声道逐位一致() {
        // GWT-CP-04：联合包络对两声道施加同一增益标量。
        let n = 2400;
        let input = sine(n, 440.0, 48000.0, 0.8, 0.3);
        let mut stage = CompressorStage::from_settings(48000.0, settings()).unwrap();
        stage.prepare(128);
        let (out_l, out_r) = drive_in_chunks(&mut stage, &input, &input, 128);
        assert_eq!(out_l, out_r, "左右输出必须逐位一致");
    }

    #[test]
    fn ratio为1_恒不压缩_输出为常数缩放() {
        // GWT-CP-08：invRatio=0 → 任意电平 reduction 恒 0，g = makeup·outputGain。
        let n = 2400;
        let input = sine(n, 1000.0, 48000.0, 1.0, 0.0); // 满幅
        let mut p = settings();
        p.ratio = 1.0;
        p.makeup_db = 3.0;
        p.output_gain = 0.5;
        let mut stage = CompressorStage::from_settings(48000.0, p).unwrap();
        stage.prepare(64);
        let (out_l, _) = drive_in_chunks(&mut stage, &input, &input, 64);
        let g = 10.0_f64.powf(3.0 / 20.0) * 0.5;
        for (i, (&x, &y)) in input.iter().zip(out_l.iter()).enumerate() {
            let want = (f64::from(x) * g) as f32;
            assert_eq!(y.to_bits(), want.to_bits(), "输出应为常数缩放 @{i}");
        }
        assert_eq!(stage.reduction_db(), 0.0, "ratio=1 时压缩量恒为 0");
    }

    #[test]
    fn 静音输入静音输出_包络地板不产生nan() {
        // GWT-CP-06：全零输入 → 逐位全零；levelDb 地板由 env + 1e-12 保证。
        let n = 4800;
        let mut stage = CompressorStage::from_settings(48000.0, settings()).unwrap();
        stage.prepare(256);
        let (out_l, out_r) = drive_in_chunks(&mut stage, &vec![0.0_f32; n], &vec![0.0_f32; n], 256);
        assert!(out_l.iter().all(|&x| x.to_bits() == 0_u32));
        assert!(out_r.iter().all(|&x| x.to_bits() == 0_u32));
        assert!(stage.reduction_db().is_finite());
    }

    #[test]
    fn sidechain_输入派生与显式api逐位一致() {
        // 规格 §4.5 派生语义的两条落地路径必须逐位一致：
        // ① sidechain_from_input 标志；② 显式 process_with_sidechain 传同派生缓冲。
        let n = 6000;
        let input_l = sine(n, 800.0, 48000.0, 0.8, 0.0);
        let input_r = sine(n, 500.0, 48000.0, 0.8, 1.0);
        let mut p = settings();
        p.threshold_db = -12.0;
        p.ratio = 8.0;
        p.attack_ms = 5.0;
        p.release_ms = 120.0;
        p.sidechain_enabled = true;

        let mut a = CompressorStage::from_settings(48000.0, p.clone()).unwrap();
        a.prepare(256);
        let (a_l, a_r) = drive_in_chunks(&mut a, &input_l, &input_r, 256);

        let mut b = CompressorStage::from_settings(48000.0, p).unwrap();
        b.set_sidechain_from_input(false);
        b.prepare(256);
        let mut bl = input_l.clone();
        let mut br = input_r.clone();
        let mut off = 0;
        while off < n {
            let end = (off + 256).min(n);
            let side: Vec<f32> = input_l[off..end]
                .iter()
                .zip(input_r[off..end].iter())
                .map(|(&x, &y)| (f64::from(x) + f64::from(y)) as f32)
                .collect();
            b.process_with_sidechain(&mut bl[off..end], &mut br[off..end], &side, &side);
            off = end;
        }
        assert_eq!(a_l, bl, "两条 sidechain 路径必须逐位一致（左）");
        assert_eq!(a_r, br, "两条 sidechain 路径必须逐位一致（右）");
    }

    #[test]
    fn sidechain_单声道和驱动与内部包络显著可区分() {
        // GWT-CP-05：去相关双正弦下，单声道和包络（峰值 ~1.6）与内部联合峰值
        // 包络（~0.8）产生可观测差异。
        let n = 6000;
        let input_l = sine(n, 800.0, 48000.0, 0.8, 0.0);
        let input_r = sine(n, 500.0, 48000.0, 0.8, 1.0);
        let mut p = settings();
        p.threshold_db = -12.0;
        p.ratio = 8.0;
        p.attack_ms = 5.0;
        p.release_ms = 120.0;

        let mut with_side = CompressorStage::from_settings(48000.0, {
            let mut q = p.clone();
            q.sidechain_enabled = true;
            q
        })
        .unwrap();
        with_side.prepare(256);
        let (s_l, _) = drive_in_chunks(&mut with_side, &input_l, &input_r, 256);

        let mut internal = CompressorStage::from_settings(48000.0, p).unwrap();
        internal.prepare(256);
        let (i_l, _) = drive_in_chunks(&mut internal, &input_l, &input_r, 256);

        let max_diff = s_l
            .iter()
            .zip(i_l.iter())
            .map(|(&x, &y)| (f64::from(x) - f64::from(y)).abs())
            .fold(0.0_f64, f64::max);
        assert!(
            max_diff > 1.0e-3,
            "单声道和 sidechain 与内部包络应显著可区分，实际 maxDiff={max_diff}"
        );
    }

    #[test]
    fn 跨块状态连续性_分块与整块逐位一致() {
        // GWT-CP-09：blockSize=97 不整除 1000（末块 10 帧）。
        let n = 1000;
        let input_l = lcg_noise(n, 1234, 0.7);
        let input_r = lcg_noise(n, 4321, 0.5);
        let mut whole = CompressorStage::from_settings(48000.0, settings()).unwrap();
        whole.prepare(n);
        let (w_l, w_r) = drive_in_chunks(&mut whole, &input_l, &input_r, n);
        let mut chunked = CompressorStage::from_settings(48000.0, settings()).unwrap();
        chunked.prepare(97);
        let (c_l, c_r) = drive_in_chunks(&mut chunked, &input_l, &input_r, 97);
        assert_eq!(w_l, c_l, "GWT-CP-09：切块不得改变逐样本运算序列（左）");
        assert_eq!(w_r, c_r, "GWT-CP-09：切块不得改变逐样本运算序列（右）");
    }

    #[test]
    fn reset_后重放与首次从零状态逐位一致() {
        // GWT-CP-10。
        let n = 1024;
        let input_l = lcg_noise(n, 3, 0.9);
        let input_r = lcg_noise(n, 55, 0.6);
        let mut stage = CompressorStage::from_settings(48000.0, settings()).unwrap();
        stage.prepare(128);
        let (first_l, first_r) = drive_in_chunks(&mut stage, &input_l, &input_r, 128);
        stage.reset();
        let (again_l, again_r) = drive_in_chunks(&mut stage, &input_l, &input_r, 128);
        assert_eq!(first_l, again_l, "reset 后重放必须逐位一致（左）");
        assert_eq!(first_r, again_r, "reset 后重放必须逐位一致（右）");
    }

    #[test]
    fn 禁用即直通_缓冲不被改写且衰减归零() {
        // GWT-CP-11。
        let input_l = lcg_noise(256, 11, 0.9);
        let input_r = lcg_noise(256, 22, 0.9);
        let mut p = settings();
        p.enabled = false;
        p.makeup_db = 12.0; // 禁用态下 makeup 也不得生效
        let mut stage = CompressorStage::from_settings(48000.0, p).unwrap();
        stage.prepare(64);
        let (out_l, out_r) = drive_in_chunks(&mut stage, &input_l, &input_r, 64);
        assert_eq!(out_l, input_l);
        assert_eq!(out_r, input_r);
        assert_eq!(stage.reduction_db(), 0.0);
    }

    #[test]
    fn 改参保留包络状态() {
        // GWT-CP-12：流中 configure 不清 env（无增益跳变）。
        let n = 2048;
        let input = sine(n, 220.0, 48000.0, 0.9, 0.0);
        let mut stage = CompressorStage::from_settings(48000.0, settings()).unwrap();
        stage.prepare(128);
        let _ = drive_in_chunks(&mut stage, &input, &input, 128);
        let env_before = stage.env;
        assert!(env_before > 0.0, "预处理后包络应非零");
        stage.configure(settings()); // 同参数重设
        assert_eq!(stage.env, env_before, "configure 必须保留包络状态");
    }

    #[test]
    fn 极值参数全程有限有界() {
        // GWT-CP-07：thresholdDb/ratio/kneeDb/makeupDb/outputGain 端值与越界钳制、
        // attack/release 下限 0.05 ms 同时生效。
        let n = 4800;
        let input = sine(n, 997.0, 48000.0, 1.0, 0.13);
        let mut p = CompressorSettings {
            enabled: true,
            threshold_db: -80.0,
            ratio: 1.0e9,   // 越上界按 100 生效
            knee_db: 99.0,  // 越上界按 40 生效
            attack_ms: 0.0, // 按 0.05 ms 下限生效
            release_ms: 0.0,
            makeup_db: 99.0, // 越上界按 +24 生效
            output_gain: 7.0,
            sidechain_enabled: false,
        };
        let mut stage = CompressorStage::from_settings(48000.0, p.clone()).unwrap();
        assert_eq!(stage.ratio, 100.0);
        assert_eq!(stage.knee_db, 40.0);
        assert_eq!(stage.makeup_lin, 10.0_f64.powf(24.0 / 20.0));
        assert_eq!(stage.output_gain, 2.0);
        let (out_l, out_r) = drive_in_chunks(&mut stage, &input, &input, 256);
        for (i, (&x, &y)) in out_l.iter().zip(out_r.iter()).enumerate() {
            assert!(x.is_finite() && y.is_finite(), "输出必须有限 @{i}");
            assert!(x.abs() < 1.0e5 && y.abs() < 1.0e5, "输出应有界 @{i}");
        }

        // 反方向端值：threshold 0、makeup -24、outputGain 0。
        p.threshold_db = 5.0; // 越上界按 0 生效
        p.makeup_db = -99.0; // 按 -24 生效
        p.output_gain = 0.0;
        p.knee_db = 0.0;
        p.ratio = 0.5; // 越下界按 1 生效
        let mut stage2 = CompressorStage::from_settings(48000.0, p).unwrap();
        assert_eq!(stage2.threshold_db, 0.0);
        assert_eq!(stage2.output_gain, 0.0);
        // outputGain=0 → g 恒为 0；负样本与 0 相乘得 -0（与 TS Float32Array 行为
        // 一致，±0 数值相等但位型不同，故按数值零断言）。
        let (out_l, _) = drive_in_chunks(&mut stage2, &input, &input, 256);
        assert!(out_l.iter().all(|&x| x.is_finite()));
        assert!(out_l.iter().all(|&x| x == 0.0), "outputGain=0 → 数值零输出");
    }

    #[test]
    fn 参数钳制生效值核对() {
        let p = CompressorSettings {
            enabled: true,
            threshold_db: -120.0, // → -80
            ratio: 0.2,           // → 1
            knee_db: -3.0,        // → 0
            attack_ms: 0.001,     // 按 0.05 ms 生效
            release_ms: 1.0e9,
            makeup_db: -99.0, // → -24
            output_gain: -1.0, // → 0
            sidechain_enabled: false,
        };
        let stage = CompressorStage::from_settings(48000.0, p).unwrap();
        assert_eq!(stage.threshold_db, -80.0);
        assert_eq!(stage.ratio, 1.0);
        assert_eq!(stage.knee_db, 0.0);
        let want = 1.0_f64 - (-1.0_f64 / ((0.05_f64 / 1000.0) * 48000.0)).exp();
        assert_eq!(stage.attack_coef, want, "attack 下限 0.05 ms 生效");
        let want_r = 1.0_f64 - (-1.0_f64 / ((1.0e9_f64 / 1000.0) * 48000.0)).exp();
        assert_eq!(stage.release_coef, want_r);
        assert_eq!(stage.makeup_lin, 10.0_f64.powf(-24.0 / 20.0));
    }
}
