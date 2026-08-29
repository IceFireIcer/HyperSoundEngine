//! deesser —— 动态齿音抑制阶段（De-esser）。
//!
//! 行为事实标准：仓库根 `src/dsp/Deesser.ts`；规格：`specs/dsp/deesser.md`。
//! 本文件是该 TS 类的逐行移植，关键对应关系：
//!
//! - 信号流（规格 §四）：单声道和 → RBJ 带通（constant-0dB-peak 型，centerHz/q）
//!   → 平方包络 attack/release 一阶跟随（env 为单标量立体声联动状态）→ dB 域阈值
//!   压缩 `reduction = over × (1 − 1/ratio)`、`g = 10^(−reduction/20)` → 分带
//!   （LR-4 交叉：LP2(x) + g·HP2(x)，g=1 时为全通重构——幅度不变、相位旋转，
//!   **非逐位恒等**，规格 §4.8.2）或宽带（x·g）施加 → mix 干湿混合。
//! - 分带交叉（规格 §4.3）：截止 `xo = clamp(centerHz × 0.6, 2500, fs × 0.45)`，
//!   每通道 2 级 Q=0.7071 低通 + 2 级 Q=0.7071 高通（共 8 个 Biquad），左右状态独立。
//! - 参数语义（规格 §三）：centerHz/q/thresholdDb/ratio/mix 双向钳制；
//!   attack 下限 0.05 ms、**release 下限 1 ms（两者不同）**，经 `onePoleCoef` 换算；
//!   `enabled=false` 时逐样本循环整体跳过（逐位直通、全部状态不推进）。
//! - sidechain（规格 §4.5/§4.6）：`sidechainEnabled` 属参数快照但 TS 类自身不读取
//!   ——它是引擎接线层标志；显式提供 sideL/sideR 时检测改用外部声道（音频路径仍
//!   处理 l/r 本体），未提供时退化为内部单声道和检测。向量驱动器的单声道和派生
//!   规则（`sideL = sideR = inL + inR`，f64 加法、就地处理前快照）由调用方承担
//!   （本批冻结向量全部为两参形态）。
//!
//! # 数值精度铁律的落点
//!
//! - 一切以 TS Number（f64）参与运算的中间量——one-pole 系数、9 个 Biquad 的
//!   系数与 TDF2 状态、env、levelDb、reduction、g、混合式——全部用 f64 复刻，
//!   运算顺序与 TS 逐行一致；
//! - **TS 侧链路与音频链路全部走 `Biquad.process`（标量 TDF2，返回 Number）**，
//!   级联中间量不经任何 f32 量化；f32 落点只有输出样本写回（`l[i]`/`r[i]`）。
//!   这与 hse-core 的 `BiquadStage`（对齐 `processBlock` 的逐样本写回量化）不同，
//!   故此处不复用该阶段而内联标量递推单元 [`BiquadCell`]；
//! - 系数设计复用 [`crate::biquad::design_biquad`]（与 TS Deesser 所用
//!   `designBiquad` 为同一事实标准），但**设计采样率恒为 48000**（见
//!   [`BIQUAD_DESIGN_FS`] 的行为事实说明）；`Math.max` 的 NaN 传播语义以
//!   [`js_max`] 显式复刻（理由同 biquad.rs）。
//!
//! # 实时安全
//!
//! 内部状态仅 env 标量 + 9 组 (系数, s1, s2)，全部构造期定容；`process` 稳态
//! 零分配、零锁、零系统调用。

use crate::biquad::{design_biquad, BiquadCoeffs};
use crate::Stage;

/// a0 非法时的防御性兜底直通系数（与 biquad.rs `BYPASS` 同值；正常参数域不可达）。
const BYPASS: BiquadCoeffs = BiquadCoeffs { b0: 1.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0 };

/// **TS 行为事实（规格之外的实证发现，冻结向量 case4 固化）**：Deesser 内部 9 个
/// Biquad 均以无参默认构造（`new Biquad()` → 构造器 `fs ?? 48000`），其系数设计
/// 采样率**恒为 48000**，与 Deesser 自身的 fs 无关。attack/release 系数
/// （`onePoleCoef`）与 centerHz/xo 的钳制上界（`fs × 0.45`）才使用 Deesser 的 fs。
/// 因此 fs=44100 时带通/交叉系数按"Deesser-fs 钳制后的频率 @ 48000 设计"生效
/// （例：case4 的 xo=11907 按 48000 设计，而非按 44100）。
const BIQUAD_DESIGN_FS: f64 = 48000.0;

/// 对齐 TS `DeesserSettings`（src/types.ts）的参数快照（字段名蛇形转换）。
#[derive(Debug, Clone)]
pub struct DeesserSettings {
    pub enabled: bool,
    pub center_hz: f64,
    pub q: f64,
    pub threshold_db: f64,
    pub ratio: f64,
    pub attack_ms: f64,
    pub release_ms: f64,
    pub split_band: bool,
    pub mix: f64,
    /// TS 可选字段。TS `Deesser` 类自身不读取（规格 §4.5）；此处仅作快照形状
    /// 对齐与接线标志记录，不影响任何 DSP 状态。
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

/// TS `clamp(v, lo, hi)` 的逐字复刻（三目链；NaN 输入原样返回）。
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
/// `coef = 1 − exp(−1 / ((max(ms, floor)/1000) × fs))`。
/// attack 的 floor 为 0.05 ms、release 的 floor 为 1 ms（规格 §三，两者不同）。
fn one_pole_coef(time_ms: f64, fs: f64, floor_ms: f64) -> f64 {
    let ms = js_max(time_ms, floor_ms);
    1.0 - (-1.0 / ((ms / 1000.0) * fs)).exp()
}

/// 标量 TDF2 递推单元（对齐 TS `Biquad` 的 `process(x): number`——返回 f64、
/// 状态 f64、无中间量化；Deesser 全链路只有输出写回才是 f32 落点）。
#[derive(Debug, Clone, Copy)]
struct BiquadCell {
    c: BiquadCoeffs,
    s1: f64,
    s2: f64,
}

impl BiquadCell {
    /// 直通初值（对齐 TS `Biquad` 字段初值 b0=1、其余 0）。
    fn identity() -> Self {
        Self { c: BYPASS, s1: 0.0, s2: 0.0 }
    }

    /// 对齐 TS `Biquad.setParams`：按 RBJ 公式重算系数（状态保留）。
    ///
    /// 设计采样率恒为 [`BIQUAD_DESIGN_FS`]（TS `Biquad` 默认构造的 `fs ?? 48000`），
    /// 与 Deesser 自身 fs 无关——见该常量文档的行为事实说明。
    fn set_design(&mut self, filter_type: &str, f0: f64, q: f64) {
        self.c = design_biquad(filter_type, f0, q, 0.0, BIQUAD_DESIGN_FS).unwrap_or(BYPASS);
    }

    /// TDF2 单样本递推（对齐 TS `Biquad.process`，求值顺序即行为）。
    #[inline]
    fn tick(&mut self, x: f64) -> f64 {
        let y = self.c.b0 * x + self.s1;
        self.s1 = self.c.b1 * x - self.c.a1 * y + self.s2;
        self.s2 = self.c.b2 * x - self.c.a2 * y;
        y
    }

    fn reset(&mut self) {
        self.s1 = 0.0;
        self.s2 = 0.0;
    }
}

/// 一个已配置的动态齿音抑制阶段（字段一一对应 TS `Deesser` 私有域）。
pub struct DeesserStage {
    fs: f64,
    // —— 生效参数（apply_params 钳制后的取值）——
    enabled: bool,
    center_hz: f64,
    q: f64,
    threshold_db: f64,
    ratio: f64,
    split_band: bool,
    mix: f64,
    attack_coef: f64,
    release_coef: f64,
    // —— 状态（规格 §4.7：env 标量 + 9 个 Biquad 的 TDF2 状态）——
    env: f64,
    /// 侧链带通（单声道和 → 齿音频段）。
    bp: BiquadCell,
    /// Linkwitz-Riley 4 阶交叉：每通道 2 级 LP + 2 级 HP（Q=0.7071，左右状态独立）。
    lp_l1: BiquadCell,
    lp_l2: BiquadCell,
    lp_r1: BiquadCell,
    lp_r2: BiquadCell,
    hp_l1: BiquadCell,
    hp_l2: BiquadCell,
    hp_r1: BiquadCell,
    hp_r2: BiquadCell,
}

impl DeesserStage {
    /// 以显式参数快照构造（对齐 TS「构造内置默认 applyParams + `setParams(p)`」
    /// 组合语义：系数按给定参数生效、全部状态为零）。
    ///
    /// fs ≤ 0 或非有限时报错（对齐 TS `Error('invalid sample rate')`）。
    pub fn from_settings(sample_rate: f64, settings: DeesserSettings) -> Result<Self, String> {
        if !(sample_rate.is_finite() && sample_rate > 0.0) {
            return Err("invalid sample rate".into());
        }
        let mut stage = Self {
            fs: sample_rate,
            enabled: true,
            center_hz: 6000.0,
            q: 0.7,
            threshold_db: -30.0,
            ratio: 8.0,
            split_band: true,
            mix: 1.0,
            attack_coef: 0.0,
            release_coef: 0.0,
            env: 0.0,
            bp: BiquadCell::identity(),
            lp_l1: BiquadCell::identity(),
            lp_l2: BiquadCell::identity(),
            lp_r1: BiquadCell::identity(),
            lp_r2: BiquadCell::identity(),
            hp_l1: BiquadCell::identity(),
            hp_l2: BiquadCell::identity(),
            hp_r1: BiquadCell::identity(),
            hp_r2: BiquadCell::identity(),
        };
        stage.apply_params(&settings);
        Ok(stage)
    }

    /// 覆盖参数快照（对齐 TS `applyParams`，逐行同序）。
    ///
    /// 参数即时生效：钳制 + 全部系数重算；**包络与滤波器状态保留不清零**
    /// （规格 §4.7，避免改参爆音）。`sidechainEnabled` 不影响任何 DSP 状态。
    pub fn configure(&mut self, settings: DeesserSettings) {
        self.apply_params(&settings);
    }

    fn apply_params(&mut self, p: &DeesserSettings) {
        self.enabled = p.enabled;
        self.center_hz = clamp(p.center_hz, 100.0, self.fs * 0.45);
        self.q = clamp(p.q, 0.1, 20.0);
        self.threshold_db = clamp(p.threshold_db, -80.0, 0.0);
        self.ratio = clamp(p.ratio, 1.0, 100.0);
        self.split_band = p.split_band;
        self.mix = clamp(p.mix, 0.0, 1.0);
        self.attack_coef = one_pole_coef(p.attack_ms, self.fs, 0.05);
        // release 的下限 floor 为 1 ms，与 attack 的 0.05 ms 不同（规格 §三）。
        self.release_coef = one_pole_coef(p.release_ms, self.fs, 1.0);
        // 侧链带通：齿音频段（RBJ constant-0dB-peak 型；设计采样率恒 48000，
        // 见 BIQUAD_DESIGN_FS 行为事实）。
        self.bp.set_design("bandpass", self.center_hz, self.q);
        // 分带交叉：截止取 centerHz·0.6（下限 2.5kHz、上限 fs·0.45——此处钳制用
        // Deesser 的 fs），LR-4（设计采样率同上恒 48000）。
        let xo = clamp(self.center_hz * 0.6, 2500.0, self.fs * 0.45);
        self.lp_l1.set_design("lowpass", xo, 0.7071);
        self.lp_l2.set_design("lowpass", xo, 0.7071);
        self.lp_r1.set_design("lowpass", xo, 0.7071);
        self.lp_r2.set_design("lowpass", xo, 0.7071);
        self.hp_l1.set_design("highpass", xo, 0.7071);
        self.hp_l2.set_design("highpass", xo, 0.7071);
        self.hp_r1.set_design("highpass", xo, 0.7071);
        self.hp_r2.set_design("highpass", xo, 0.7071);
    }

    /// 逐样本主循环（对齐 TS `processStereo` 循环体，逐行同序）。
    ///
    /// `side` 为 `Some((side_l, side_r))` 时检测信号改用外部声道（规格 §4.5），
    /// 音频路径仍处理 left/right 本体；`None` 时退化为内部单声道和检测。
    fn run(&mut self, left: &mut [f32], right: &mut [f32], side: Option<(&[f32], &[f32])>) {
        if !self.enabled {
            return; // 恒等直通：缓冲不被改写，全部状态不推进（规格 §4.7）
        }
        let n = left.len();
        // 局部化派生量（对齐 TS processStereo 开头的一组局部 const）。
        let attack = self.attack_coef;
        let release = self.release_coef;
        let threshold_db = self.threshold_db;
        let inv_ratio = 1.0 - 1.0 / self.ratio;
        let mix = self.mix;
        let split = self.split_band;
        let mut env = self.env;
        for i in 0..n {
            let xl = f64::from(left[i]);
            let xr = f64::from(right[i]);
            // 1) 检测信号：外部 sidechain（若提供）或主信号单声道和 → 带通。
            let (dl, dr) = match side {
                Some((sl, sr)) => (f64::from(sl[i]), f64::from(sr[i])),
                None => (xl, xr),
            };
            let s = self.bp.tick(0.5 * (dl + dr));
            // 2) 平方包络 + attack/release 一阶分段跟随。
            let p = s * s;
            if p > env {
                env += attack * (p - env);
            } else {
                env += release * (p - env);
            }
            // 3) dB 域阈值压缩（+1e-12 静音地板防 log(0)）。
            let level_db = 10.0 * (env + 1e-12).log10();
            let over = level_db - threshold_db;
            let reduction = if over > 0.0 { over * inv_ratio } else { 0.0 };
            let g = 10.0_f64.powf(-reduction / 20.0);
            if split {
                // 分带式：LP2 + g·HP2；g=1 时 LR-4 交叉重建全通（幅度不变、相位旋转）。
                // 级联求值顺序与 TS 一致：内层 1 级先跑、外层 2 级后跑；四条链按
                // lowL → lowR → highL → highR 的 TS 求值顺序推进。
                let low_l = self.lp_l2.tick(self.lp_l1.tick(xl));
                let low_r = self.lp_r2.tick(self.lp_r1.tick(xr));
                let high_l = self.hp_l2.tick(self.hp_l1.tick(xl));
                let high_r = self.hp_r2.tick(self.hp_r1.tick(xr));
                let out_l = low_l + g * high_l;
                let out_r = low_r + g * high_r;
                left[i] = (xl + mix * (out_l - xl)) as f32;
                right[i] = (xr + mix * (out_r - xr)) as f32;
            } else {
                // 宽带式：整体增益。
                left[i] = (xl + mix * (xl * g - xl)) as f32;
                right[i] = (xr + mix * (xr * g - xr)) as f32;
            }
        }
        self.env = env;
    }

    /// 显式外部 sidechain 接线入口（规格 §4.5：提供 sideL/sideR 时包络检测改用
    /// 外部信号，音频路径仍处理 left/right 本体；§4.6 的单声道和派生由调用方
    /// 在就地处理前完成）。这是服务管线后续接线的入口，不违反 [`Stage`] trait
    /// 的两参形态。`side_l`/`side_r` 长度不得小于本块帧数。
    pub fn process_with_sidechain(
        &mut self,
        left: &mut [f32],
        right: &mut [f32],
        side_l: &[f32],
        side_r: &[f32],
    ) {
        debug_assert_eq!(left.len(), right.len(), "左右声道块长必须一致（Stage 契约）");
        debug_assert!(side_l.len() >= left.len() && side_r.len() >= left.len());
        self.run(left, right, Some((side_l, side_r)));
    }
}

impl Stage for DeesserStage {
    /// 内部状态为标量与定容滤波单元，无需按块长预分配（保留形参以符合 Stage 契约）。
    fn prepare(&mut self, _max_block_size: usize) {}

    /// 就地处理一个立体声块（两参形态 = 内部单声道和检测）；状态跨块保持
    /// （GWT-DE-07：切块不改变逐样本运算序列）。
    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        debug_assert_eq!(left.len(), right.len(), "左右声道块长必须一致（Stage 契约）");
        self.run(left, right, None);
    }

    /// reset()：env 归零并 reset 全部 9 个 Biquad（规格 §4.7；参数保留）。
    fn reset(&mut self) {
        self.env = 0.0;
        self.bp.reset();
        self.lp_l1.reset();
        self.lp_l2.reset();
        self.lp_r1.reset();
        self.lp_r2.reset();
        self.hp_l1.reset();
        self.hp_l2.reset();
        self.hp_r1.reset();
        self.hp_r2.reset();
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

    fn sine(n: usize, freq: f64, fs: f64, amp: f64, phase: f64) -> Vec<f32> {
        (0..n)
            .map(|i| {
                (2.0 * std::f64::consts::PI * freq * i as f64 / fs + phase).sin() as f32
                    * amp as f32
            })
            .collect()
    }

    fn settings() -> DeesserSettings {
        DeesserSettings {
            enabled: true,
            center_hz: 8000.0,
            q: 0.7,
            threshold_db: -30.0,
            ratio: 8.0,
            attack_ms: 1.0,
            release_ms: 80.0,
            split_band: true,
            mix: 1.0,
            sidechain_enabled: false,
        }
    }

    fn drive_in_chunks(
        stage: &mut DeesserStage,
        l: &[f32],
        r: &[f32],
        block: usize,
    ) -> (Vec<f32>, Vec<f32>) {
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

    /// 与 Node 直跑 TS 源取得的黄金参考比对（f32 写回落点，容差同对拍口径）。
    fn assert_f32_close(got: f32, want: f64, label: &str) {
        let diff = (f64::from(got) - want).abs();
        assert!(
            diff <= 1e-6 * want.abs().max(1e-9),
            "{label}：got {got}，want {want}"
        );
    }

    /// f64 系数黄金参考比对（相对容差 1e-12：远宽于跨库 libm 1~2 ulp 差异——
    /// V8 与 Rust 的 sin/cos/exp 在非整周角上可差末位 1 ulp，case4 的
    /// 11907/48000 即属此类——远严于任何公式/钳制/量化口径错误）。
    fn assert_coeff_close(got: f64, want: f64, label: &str) {
        assert!(
            (got - want).abs() <= 1e-12 * want.abs().max(1e-300),
            "{label}：got {got}，want {want}"
        );
    }

    #[test]
    fn 非法采样率报错_对齐ts错误信息() {
        for bad in [0.0_f64, -44100.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let err = DeesserStage::from_settings(bad, settings()).err().expect("非法采样率必须报错");
            assert!(err.contains("invalid sample rate"), "错误信息应与 TS 一致：{err}");
        }
    }

    #[test]
    fn 参数钳制与系数命中ts黄金参考_case2() {
        // 黄金参考：node 直跑 src/dsp/Deesser.ts（fs=48000，case2 参数形态）。
        let s = DeesserStage::from_settings(48000.0, settings()).unwrap();
        assert_eq!(s.center_hz, 8000.0);
        assert_eq!(s.q, 0.7);
        assert_eq!(s.threshold_db, -30.0);
        assert_eq!(s.ratio, 8.0);
        assert_eq!(s.mix, 1.0);
        assert_coeff_close(s.attack_coef, 0.02061781866875978, "attack 1ms @48k");
        assert_coeff_close(s.release_coef, 0.0002603827611897813, "release 80ms @48k");
        assert_coeff_close(s.bp.c.b0, 0.3821781531390199, "bp.b0");
        assert_coeff_close(s.bp.c.b1, 0.0, "bp.b1");
        assert_coeff_close(s.bp.c.b2, -0.3821781531390199, "bp.b2");
        assert_coeff_close(s.bp.c.a1, -0.6178218468609802, "bp.a1");
        assert_coeff_close(s.bp.c.a2, 0.23564369372196017, "bp.a2");
        // 交叉截止 xo = 8000×0.6 = 4800（未触钳制边界）。
        assert_coeff_close(s.lp_l1.c.b0, 0.06745508395870334, "lp.b0");
        assert_coeff_close(s.lp_l1.c.a1, -1.142977284308092, "lp.a1");
        assert_coeff_close(s.lp_l1.c.a2, 0.41279762014290533, "lp.a2");
        assert_coeff_close(s.hp_l1.c.b0, 0.6389437261127493, "hp.b0");
        assert_coeff_close(s.hp_l1.c.a1, -1.142977284308092, "hp.a1");
        // 左右交叉滤波器同系数（状态独立）。
        assert_eq!(s.lp_r1.c, s.lp_l1.c);
        assert_eq!(s.hp_r2.c, s.hp_l1.c);
    }

    #[test]
    fn 极值钳制生效值命中ts黄金参考_case4() {
        // case4 形态 @44100：centerHz=30000→19845（fs×0.45）、q=0.05→0.1、
        // attackMs=0→0.05ms 下限、releaseMs=0→1ms 下限（与 attack 不同）。
        let mut p = settings();
        p.center_hz = 30000.0;
        p.q = 0.05;
        p.threshold_db = -20.0;
        p.attack_ms = 0.0;
        p.release_ms = 0.0;
        let s = DeesserStage::from_settings(44100.0, p).unwrap();
        assert_eq!(s.center_hz, 19845.0);
        assert_eq!(s.q, 0.1);
        assert_eq!(s.threshold_db, -20.0);
        assert_coeff_close(s.attack_coef, 0.3646090112311966, "attack 下限 0.05ms @44.1k");
        assert_coeff_close(s.release_coef, 0.022420574740847354, "release 下限 1ms @44.1k");
        // 系数按"Deesser-fs 钳制后的频率 @ 48000 设计"生效（BIQUAD_DESIGN_FS 行为事实）。
        assert_coeff_close(s.bp.c.b0, 0.7212415592657756, "bp.b0");
        assert_coeff_close(s.bp.c.a1, 0.4770689376100315, "bp.a1");
        assert_coeff_close(s.bp.c.a2, -0.4424831185315512, "bp.a2");
        // xo = clamp(19845×0.6=11907, 2500, 19845) = 11907（按 48000 设计）。
        assert_coeff_close(s.lp_l1.c.b0, 0.2893354522061511, "lp.b0");
        assert_coeff_close(s.hp_l1.c.b0, 0.296466638297796, "hp.b0");
        // 生效值与直接按钳制后参数配置逐位等效。
        let mut direct = settings();
        direct.center_hz = 19845.0;
        direct.q = 0.1;
        direct.attack_ms = 0.05;
        direct.release_ms = 1.0;
        let d = DeesserStage::from_settings(44100.0, direct).unwrap();
        assert_eq!(s.bp.c, d.bp.c);
        assert_eq!(s.lp_l1.c, d.lp_l1.c);
        assert_eq!(s.hp_r2.c, d.hp_r2.c);
        assert_eq!(s.attack_coef, d.attack_coef);
        assert_eq!(s.release_coef, d.release_coef);
    }

    #[test]
    fn enabled_false_逐位直通且状态不推进() {
        // GWT-DE-01：禁用即直通，缓冲零改写。
        let mut p = settings();
        p.enabled = false;
        p.threshold_db = 0.0; // 禁用态下激进参数也不得生效
        p.mix = 0.0;
        let mut stage = DeesserStage::from_settings(48000.0, p).unwrap();
        stage.prepare(64);
        let in_l = lcg_noise(512, 11, 0.5);
        let in_r = sine(512, 200.0, 48000.0, 0.4, 0.0);
        let (out_l, out_r) = drive_in_chunks(&mut stage, &in_l, &in_r, 64);
        assert_eq!(out_l, in_l, "左声道必须逐位一致");
        assert_eq!(out_r, in_r, "右声道必须逐位一致");
        // 全部状态不推进：env 与滤波器状态保持构造初值。
        assert_eq!(stage.env, 0.0, "禁用态下包络不得推进");
        assert_eq!(stage.bp.s1, 0.0, "禁用态下带通状态不得推进");
        assert_eq!(stage.bp.s2, 0.0, "禁用态下带通状态不得推进");
    }

    #[test]
    fn 分带输出命中ts黄金参考_含左右声道() {
        // 黄金参考：node 直跑（fs=48000，case2 参数，输入 l=±0.5 交替、r=0.25 恒值，
        // 单块 16 帧）。检测信号为单声道和 → 带通。
        let mut stage = DeesserStage::from_settings(48000.0, settings()).unwrap();
        stage.prepare(16);
        let mut l: Vec<f32> = (0..16).map(|i| if i % 2 == 0 { 0.5 } else { -0.5 }).collect();
        let mut r = vec![0.25_f32; 16];
        stage.process(&mut l, &mut r);
        let want_l = [
            0.20639963448047638, -0.5419772267341614, 0.5732174515724182, -0.3989863991737366,
            0.5852321982383728, -0.4442795515060425, 0.5285037159919739, -0.4904220402240753,
            0.49918121099472046, -0.5048896074295044, 0.4947493374347687, -0.5039830207824707,
            0.4976149797439575, -0.5010818839073181, 0.49974799156188965, -0.49984145164489746,
        ];
        let want_r = [
            0.10319981724023819, -0.06458897888660431, -0.04896886646747589, 0.038146670907735825,
            0.13126958906650543, 0.2017459273338318, 0.2438579946756363, 0.26289883255958557,
            0.26727840304374695, 0.264424204826355, 0.2593540847301483, 0.2547372579574585,
            0.2515532374382019, 0.249819815158844, 0.24915288388729095, 0.24910613894462585,
        ];
        for i in 0..16 {
            assert_f32_close(l[i], want_l[i], &format!("L[{i}]"));
            assert_f32_close(r[i], want_r[i], &format!("R[{i}]"));
        }
    }

    #[test]
    fn 极值钳制分带输出命中ts黄金参考_case4形态() {
        // case4 形态 @44100（全部钳制生效 + LR-4 全通重构），LCG 输入、2 块 441 帧。
        let mut p = settings();
        p.center_hz = 30000.0;
        p.q = 0.05;
        p.threshold_db = -20.0;
        p.attack_ms = 0.0;
        p.release_ms = 0.0;
        let mut stage = DeesserStage::from_settings(44100.0, p).unwrap();
        stage.prepare(441);
        let n = 882;
        let mut l = lcg_noise(n, 91, 0.008);
        let mut r = lcg_noise(n, 92, 0.006);
        let mut off = 0;
        while off < n {
            let end = (off + 441).min(n);
            stage.process(&mut l[off..end], &mut r[off..end]);
            off = end;
        }
        let want_l = [
            -0.001486802939325571, 0.0014813632005825639, 0.006884575355798006,
            0.0016693592770025134, -0.00901285745203495, -0.0011495668441057205,
            0.000831213837955147, -0.002607028465718031, 0.0010468466207385063,
            -0.001889064209535718, 0.00199502008035779, 0.007336150389164686,
        ];
        let want_r = [
            0.0013131959130987525, -0.002610238967463374, -0.0009178545442409813,
            0.004534080624580383, 0.0037694601342082024, -0.0040595754981040955,
            0.004875213373452425, 0.005733225494623184, -0.0025438431184738874,
            0.000843722140416503, 0.0014975740341469646, -0.0025818960275501013,
        ];
        for (k, w) in want_l.iter().enumerate() {
            assert_f32_close(l[429 + k], *w, &format!("case4 L[{}]", 429 + k));
        }
        for (k, w) in want_r.iter().enumerate() {
            assert_f32_close(r[870 + k], *w, &format!("case4 R[{}]", 870 + k));
        }
    }

    #[test]
    fn 阈下宽带g1_输出与输入逐位一致() {
        // 黄金参考 wideOut16L：宽带 + g=1 时 `x + mix·(x·1 − x) = x` 精确成立 →
        // 宽带形态的逐位锚点（低电平输入使包络稳态低于阈值）。
        let mut p = settings();
        p.split_band = false;
        let mut stage = DeesserStage::from_settings(48000.0, p).unwrap();
        stage.prepare(16);
        let mut l: Vec<f32> = (0..16).map(|i| if i % 2 == 0 { 0.5 } else { -0.5 }).collect();
        let mut r = vec![0.25_f32; 16];
        stage.process(&mut l, &mut r);
        for i in 0..16 {
            let want = if i % 2 == 0 { 0.5_f32 } else { -0.5_f32 };
            assert_eq!(l[i].to_bits(), want.to_bits(), "宽带 g=1 逐位恒等 L[{i}]");
            assert_eq!(r[i].to_bits(), 0.25_f32.to_bits(), "宽带 g=1 逐位恒等 R[{i}]");
        }
    }

    #[test]
    fn 阈下分带_幅度不变但相位旋转_非逐位() {
        // GWT-DE-04：g 恒 1 的分带输出 = LP2+HP2 全通重构——RMS 变化 ≈0（<0.001dB，
        // 规格 §4.8.2）但逐样本波形不同（相位旋转）。
        let mut stage = DeesserStage::from_settings(48000.0, settings()).unwrap();
        stage.prepare(256);
        let n = 9600;
        let in_l = lcg_noise(n, 33, 0.008);
        let in_r = sine(n, 200.0, 48000.0, 0.006, 0.3);
        let (out_l, _) = drive_in_chunks(&mut stage, &in_l, &in_r, 256);
        let rms = |a: &[f32]| -> f64 {
            a.iter().map(|&x| f64::from(x) * f64::from(x)).sum::<f64>() / a.len() as f64
        };
        let rms_in = rms(&in_l);
        let rms_out = rms(&out_l);
        let ratio_db = 10.0 * (rms_out / rms_in).log10();
        assert!(
            ratio_db.abs() < 0.001,
            "阈下分带应幅度不变（RMS 变化 <0.001dB），实际 {ratio_db} dB"
        );
        let bit_equal = out_l.iter().zip(in_l.iter()).all(|(a, b)| a.to_bits() == b.to_bits());
        assert!(!bit_equal, "LR-4 全通重构必须非逐位一致（相位旋转），否则交叉未生效");
    }

    #[test]
    fn ratio为1_恒不压缩_宽带逐位恒等() {
        // GWT-DE-06：invRatio=0 → reduction 恒 0、g 恒 1；宽带输出逐位等于输入，
        // 分带输出为纯 LR-4 全通形态（有限）。
        let mut p = settings();
        p.ratio = 1.0;
        p.threshold_db = 0.0; // 任意电平都不得产生压缩
        p.split_band = false;
        let mut wide = DeesserStage::from_settings(48000.0, p.clone()).unwrap();
        wide.prepare(128);
        let in_l = lcg_noise(960, 21, 0.9);
        let in_r = lcg_noise(960, 22, 0.9);
        let (out_l, out_r) = drive_in_chunks(&mut wide, &in_l, &in_r, 128);
        assert_eq!(out_l, in_l, "ratio=1 宽带必须逐位恒等（左）");
        assert_eq!(out_r, in_r, "ratio=1 宽带必须逐位恒等（右）");

        p.split_band = true;
        let mut split = DeesserStage::from_settings(48000.0, p).unwrap();
        split.prepare(128);
        let (s_l, s_r) = drive_in_chunks(&mut split, &in_l, &in_r, 128);
        assert!(s_l.iter().all(|&x| x.is_finite()) && s_r.iter().all(|&x| x.is_finite()));
        let bit_equal = s_l.iter().zip(in_l.iter()).all(|(a, b)| a.to_bits() == b.to_bits());
        assert!(!bit_equal, "ratio=1 分带输出为全通重构，不应与输入逐位一致");
    }

    #[test]
    fn 分带与宽带对照_低频带保持与全带衰减可区分() {
        // GWT-DE-03：同一激励下分带只压高频带、宽带全带同增益衰减。
        // 检测器置于 200Hz（centerHz=200 → 200Hz 正弦直接驱动包络超阈产生稳定 g）；
        // 分带交叉 xo = clamp(200×0.6, 2500, fs×0.45) = 2500 → 200Hz 位于低频带，
        // 分带式经 LP2 原样通过（幅度保持）、宽带式整体乘 g 显著衰减。
        let n = 9600;
        let in_l = sine(n, 200.0, 48000.0, 0.6, 0.0);
        let in_r = sine(n, 200.0, 48000.0, 0.6, 1.0);
        let mut split_p = settings();
        split_p.center_hz = 200.0;
        let mut wide_p = split_p.clone();
        wide_p.split_band = false;
        let mut split = DeesserStage::from_settings(48000.0, split_p).unwrap();
        let mut wide = DeesserStage::from_settings(48000.0, wide_p).unwrap();
        let (s_l, _) = drive_in_chunks(&mut split, &in_l, &in_r, 384);
        let (w_l, _) = drive_in_chunks(&mut wide, &in_l, &in_r, 384);
        // 取后半段（包络已进入稳态压缩）度量 200Hz 成分的幅度变化。
        let rms = |a: &[f32]| -> f64 {
            a.iter().map(|&x| f64::from(x) * f64::from(x)).sum::<f64>() / a.len() as f64
        };
        let rms_in = rms(&in_l[n / 2..]);
        let split_db = 10.0 * (rms(&s_l[n / 2..]) / rms_in).log10();
        let wide_db = 10.0 * (rms(&w_l[n / 2..]) / rms_in).log10();
        assert!(
            split_db.abs() < 0.5,
            "分带模式 200Hz 低频带应基本保持（LR-4 仅相位旋转），实际 {split_db} dB"
        );
        assert!(
            wide_db < -10.0,
            "宽带模式 200Hz 应随全带同增益衰减（g 稳态深压缩），实际 {wide_db} dB"
        );
    }

    #[test]
    fn 检测信号为单声道和_单侧能量驱动两侧共同增益() {
        // 规格 §4.8.5：检测是单声道和的带通（非逐声道）。左声道齿音能量足以
        // 压低右声道；对左声道而言，右声道有无同源齿音会改变单声道和 → g 不同。
        let n = 9600;
        let sib = sine(n, 7500.0, 48000.0, 0.5, 0.0);
        let mut both_p = settings();
        both_p.split_band = false;
        both_p.center_hz = 7500.0;
        let one_p = both_p.clone();
        let mut both = DeesserStage::from_settings(48000.0, both_p).unwrap();
        let mut one = DeesserStage::from_settings(48000.0, one_p).unwrap();
        let in_r_both = sib.clone();
        let in_r_one = vec![0.0_f32; n];
        let (b_l, _) = drive_in_chunks(&mut both, &sib, &in_r_both, 256);
        let (o_l, _) = drive_in_chunks(&mut one, &sib, &in_r_one, 256);
        let max_diff = b_l
            .iter()
            .zip(o_l.iter())
            .map(|(&x, &y)| (f64::from(x) - f64::from(y)).abs())
            .fold(0.0_f64, f64::max);
        assert!(
            max_diff > 1.0e-3,
            "单声道和检测下，右侧有无齿音必须改变左侧输出，实际 maxDiff={max_diff}"
        );
    }

    #[test]
    fn 跨块状态连续性_分块与整块逐位一致() {
        // GWT-DE-07：env 与全部滤波器均为逐样本递推；blockSize=97 不整除 1000。
        let n = 1000;
        let in_l = lcg_noise(n, 1234, 0.5);
        let in_r = sine(n, 200.0, 48000.0, 0.3, 0.5);
        let mut whole = DeesserStage::from_settings(48000.0, settings()).unwrap();
        whole.prepare(n);
        let (w_l, w_r) = drive_in_chunks(&mut whole, &in_l, &in_r, n);
        let mut chunked = DeesserStage::from_settings(48000.0, settings()).unwrap();
        chunked.prepare(97);
        let (c_l, c_r) = drive_in_chunks(&mut chunked, &in_l, &in_r, 97);
        assert_eq!(w_l, c_l, "切块不得改变逐样本运算序列（左）");
        assert_eq!(w_r, c_r, "切块不得改变逐样本运算序列（右）");
    }

    #[test]
    fn reset后重放与首次从零状态逐位一致() {
        // GWT-DE-11：reset 清 env 与全部 9 个 Biquad 状态。
        let in_l = lcg_noise(1024, 3, 0.5);
        let in_r = sine(1024, 200.0, 48000.0, 0.4, 0.0);
        let mut stage = DeesserStage::from_settings(48000.0, settings()).unwrap();
        stage.prepare(128);
        let (first_l, first_r) = drive_in_chunks(&mut stage, &in_l, &in_r, 128);
        stage.reset();
        let (again_l, again_r) = drive_in_chunks(&mut stage, &in_l, &in_r, 128);
        assert_eq!(first_l, again_l, "reset 后重放必须逐位一致（左）");
        assert_eq!(first_r, again_r, "reset 后重放必须逐位一致（右）");
    }

    #[test]
    fn 静音输入零输出_包络地板不产生nan() {
        // GWT-DE-10：全零输入 → 逐位全零；levelDb 地板由 env + 1e-12 保证。
        let n = 4800;
        let mut stage = DeesserStage::from_settings(48000.0, settings()).unwrap();
        stage.prepare(256);
        let (out_l, out_r) = drive_in_chunks(&mut stage, &vec![0.0_f32; n], &vec![0.0_f32; n], 256);
        assert!(out_l.iter().all(|&x| x.to_bits() == 0_u32));
        assert!(out_r.iter().all(|&x| x.to_bits() == 0_u32));
        assert!(stage.env.is_finite());
    }

    #[test]
    fn 改参保留包络与滤波器状态() {
        // 规格 §4.7：setParams 仅重算系数，env 保留（避免改参爆音）。
        let n = 2048;
        let in_l = sine(n, 7500.0, 48000.0, 0.5, 0.0);
        let in_r = vec![0.0_f32; n];
        let mut stage = DeesserStage::from_settings(48000.0, settings()).unwrap();
        stage.prepare(128);
        let _ = drive_in_chunks(&mut stage, &in_l, &in_r, 128);
        let env_before = stage.env;
        assert!(env_before > 0.0, "预处理后包络应非零");
        stage.configure(settings()); // 同参数重设
        assert_eq!(stage.env, env_before, "configure 必须保留包络状态");
        assert_eq!(stage.bp.s1 != 0.0 || stage.bp.s2 != 0.0, true, "滤波器状态同样保留");
    }

    #[test]
    fn 外部sidechain驱动与内部检测共用音频路径() {
        // 规格 §4.5：提供 sideL/sideR 时检测改用外部信号、音频路径仍处理本体；
        // §4.6 派生规则（f64 单声道和、f32 快照、处理前快照）由调用方承担。
        let n = 6000;
        let in_l = lcg_noise(n, 41, 0.5);
        let in_r = sine(n, 200.0, 48000.0, 0.4, 0.0);
        let mut p = settings();
        p.threshold_db = -12.0;
        let mut with_side = DeesserStage::from_settings(48000.0, p.clone()).unwrap();
        with_side.prepare(256);
        let mut l1 = in_l.clone();
        let mut r1 = in_r.clone();
        let mut off = 0;
        while off < n {
            let end = (off + 256).min(n);
            let side: Vec<f32> = in_l[off..end]
                .iter()
                .zip(in_r[off..end].iter())
                .map(|(&x, &y)| (f64::from(x) + f64::from(y)) as f32)
                .collect();
            with_side.process_with_sidechain(&mut l1[off..end], &mut r1[off..end], &side, &side);
            off = end;
        }
        let mut internal = DeesserStage::from_settings(48000.0, p).unwrap();
        internal.prepare(256);
        let (l2, _) = drive_in_chunks(&mut internal, &in_l, &in_r, 256);
        let max_diff = l1
            .iter()
            .zip(l2.iter())
            .map(|(&x, &y)| (f64::from(x) - f64::from(y)).abs())
            .fold(0.0_f64, f64::max);
        assert!(
            max_diff > 1.0e-3,
            "外部 sidechain 与内部检测应显著可区分，实际 maxDiff={max_diff}"
        );
    }
}
