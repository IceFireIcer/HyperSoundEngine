//! dynamic_eq —— 自适应动态均衡（全通交叉分带 + 频谱包络驱动增益）。
//!
//! 行为事实标准：仓库根 `src/dsp/DynamicEq.ts`；规格：`specs/dsp/dynamic-eq.md`。
//! 本文件是该 TS 类的逐行移植，关键对应关系：
//!
//! - 分带网络（规格 §4.1）：每通道 8 个一阶 TDF2 单元组成「全通交叉」树——
//!   LP = (1+A)/2、HP = (1−A)/2 共享同一个一阶全通系数 `a1 = −tan(π/4 − wc/2)`，
//!   代数恒等 LP+HP = 1 ⇒ 全带增益为 1 时输出逐位精确重建输入。树内求值顺序与
//!   TS 逐行一致（每层先 HP 残差 r_k、后同层 LP；左声道全树 → 右声道全树）。
//! - 控制节奏（规格 §4.2/§4.5，模块特有行为事实）：内部分析块边界 =
//!   min(params.blockSize, 本次 process 调用的剩余样本数)；sumsq 在每个分析块
//!   开始清零、**不跨调用累积**；块末由本块立体声联合能量算电平（+1e-12 地板）
//!   → dB 域软拐点压缩 → 静态目标曲线经 strength 干湿混合 → 目标增益（钳
//!   [0, 3]，禁用带恒 1）。因此**输出依赖驱动分块**：顶层 blockSize 为
//!   params.blockSize 整数倍时与整块逐位一致，否则控制更新在调用边界提前触发
//!   （对拍必须按冻结向量的同一 blockSize 回放）。
//! - 增益平滑（规格 §4.4）：逐样本一阶——目标低于当前增益走 attack 系数、
//!   否则走 release 系数；attack 下限 0.05 ms、release 下限 1 ms（两者不同）。
//! - 直通语义（规格 §三/§4.8）：`enabled=false` 或 `strength<=0` 时逐样本循环
//!   整体跳过——缓冲逐位不改写，全部状态（树/能量/目标/增益）不推进；
//!   `set_params` 保留全部状态（参数即时生效、不清历史）。
//!
//! # 数值精度铁律的落点
//!
//! - 交叉树 8×2 个单元的系数与 TDF2 状态、sumsq/levelsDb/targetGains/gains 与
//!   全部中间量（TS Number）一律 f64 复刻，运算顺序与 TS 逐行一致；f32 落点
//!   只有输出样本写回（`l[i]`/`r[i]`）——因此不复用 [`crate::biquad::BiquadStage`]
//!   （其对齐 `processBlock` 的逐样本 f32 写回量化），而内联标量递推单元
//!   [`TreeCell`]（理由同 deesser.rs 的 `BiquadCell`）；
//! - `Math.min`/`Math.max` 的 NaN 传播语义以 [`js_min`]/[`js_max`] 显式复刻
//!   （blockSize 与目标增益的钳制；理由同 biquad.rs）。
//!
//! # 实时安全
//!
//! 交叉树与全部带状态为构造期定容数组，`Stage::process` 稳态零分配、零锁、
//! 零系统调用；`prepare` 无需预分配。

use crate::Stage;

/// 固定频带数（5 带：low / low-mid / mid / high-mid / high）。
const BAND_COUNT: usize = 5;
/// 默认交叉频率（4 个：带 i 与带 i+1 的分界；末带 frequency 被完全忽略）。
const DEFAULT_CROSSOVER_HZ: [f64; 4] = [200.0, 800.0, 2500.0, 8000.0];
/// 每带增益钳制范围（线性，防任意参数组合下输出无界；规格 GWT-DY-11）。
const GAIN_MIN: f64 = 0.0;
const GAIN_MAX: f64 = 3.0;

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

/// 一阶全通交叉系数：`wc = 2π·fc/fs`、`a1 = −tan(π/4 − wc/2)`，
/// LP = (1+a1)/2、HP = (1−a1)/2（代数恒等 LP+HP = 1）。
fn crossover_coeffs(fc: f64, fs: f64) -> (f64, f64, f64) {
    let wc = (2.0 * std::f64::consts::PI * fc) / fs;
    let a1 = -(std::f64::consts::PI / 4.0 - wc / 2.0).tan();
    (0.5 * (1.0 + a1), 0.5 * (1.0 - a1), a1)
}

/// 标量 TDF2 递推单元（对齐 TS `Biquad` 的 `process(x): number`——返回 f64、
/// 状态 f64、无中间量化；本模块全链路只有输出写回才是 f32 落点）。
#[derive(Debug, Clone, Copy)]
struct TreeCell {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    s1: f64,
    s2: f64,
}

impl TreeCell {
    /// 直通初值（对齐 TS `Biquad` 字段初值 b0=1、其余 0）。
    fn identity() -> Self {
        Self { b0: 1.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0, s1: 0.0, s2: 0.0 }
    }

    /// 按交叉系数构造（对齐 TS `setCoeffs` 的直接赋值，无归一化）。
    fn with_coeffs(b0: f64, b1: f64, b2: f64, a1: f64, a2: f64) -> Self {
        Self { b0, b1, b2, a1, a2, s1: 0.0, s2: 0.0 }
    }

    /// TDF2 单样本递推（对齐 TS `Biquad.process`，求值顺序即行为）。
    #[inline]
    fn tick(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.s1;
        self.s1 = self.b1 * x - self.a1 * y + self.s2;
        self.s2 = self.b2 * x - self.a2 * y;
        y
    }

    fn reset(&mut self) {
        self.s1 = 0.0;
        self.s2 = 0.0;
    }
}

/// 对齐 TS `DynamicEqBandParam` 的单带参数（字段名蛇形转换）。
///
/// `frequency` 为必填——TS 侧 bands 项缺 `frequency` 会经 clamp 落成 NaN 级联
/// （规格 §三：未定义行为，禁止进入向量），故此处不提供缺省形态。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DynamicEqBandParam {
    pub enabled: bool,
    pub frequency: f64,
    /// TS 可选字段（`targetGainDb?: number`）；缺省保持该带当前/默认静态偏移。
    pub target_gain_db: Option<f64>,
}

/// 对齐 TS `DynamicEqParams`（Partial 形态，全部字段可选）的参数快照。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DynamicEqParams {
    pub enabled: Option<bool>,
    pub strength: Option<f64>,
    pub threshold_db: Option<f64>,
    pub ratio: Option<f64>,
    pub knee_db: Option<f64>,
    pub attack_ms: Option<f64>,
    pub release_ms: Option<f64>,
    /// 内部分析块长（样本）；与向量顶层驱动分块是两个独立参数（规格 §4.5）。
    pub block_size: Option<f64>,
    /// 固定 5 带；短于 5 项时缺项带保持当前/默认配置，超出忽略。
    pub bands: Option<Vec<DynamicEqBandParam>>,
}

/// 一个已配置的自适应动态均衡阶段（字段一一对应 TS `DynamicEq` 私有域）。
#[derive(Debug, Clone)]
pub struct DynamicEqStage {
    fs: f64,
    // —— 生效参数（apply_params 钳制后的取值）——
    enabled: bool,
    strength: f64,
    threshold_db: f64,
    ratio: f64,
    knee_db: f64,
    attack_coef: f64,
    release_coef: f64,
    /// 内部分析块长（TS 存 Number；钳制后为 [16, 2048] 的整数值）。
    block_size: f64,
    // —— 带配置 ——
    band_enabled: [bool; BAND_COUNT],
    cross_freqs: [f64; 4],
    static_db: [f64; BAND_COUNT],
    // —— 状态（规格 §4.8）——
    sumsq: [f64; BAND_COUNT],
    levels_db: [f64; BAND_COUNT],
    target_gains: [f64; BAND_COUNT],
    gains: [f64; BAND_COUNT],
    /// 交叉树：每通道 LP1,HP1,LP2,HP2,LP3,HP3,LP4,HP4（共 8 个单元）。
    tree_l: [TreeCell; 8],
    tree_r: [TreeCell; 8],
}

impl DynamicEqStage {
    /// 以显式参数快照构造（对齐 TS「构造内置默认 applyParams + params 直传」
    /// 组合语义：系数按给定参数生效、全部状态为零）。
    ///
    /// fs ≤ 0 或非有限时报错（对齐 TS `Error('invalid sample rate')`）。
    pub fn from_params(sample_rate: f64, params: DynamicEqParams) -> Result<Self, String> {
        if !(sample_rate.is_finite() && sample_rate > 0.0) {
            return Err("invalid sample rate".into());
        }
        let mut stage = Self {
            fs: sample_rate,
            enabled: true,
            strength: 1.0,
            threshold_db: -20.0,
            ratio: 2.0,
            knee_db: 6.0,
            attack_coef: 0.0,
            release_coef: 0.0,
            block_size: 128.0,
            band_enabled: [true; BAND_COUNT],
            cross_freqs: DEFAULT_CROSSOVER_HZ,
            static_db: [0.0; BAND_COUNT],
            sumsq: [0.0; BAND_COUNT],
            levels_db: [0.0; BAND_COUNT],
            target_gains: [1.0; BAND_COUNT],
            gains: [1.0; BAND_COUNT],
            tree_l: [TreeCell::identity(); 8],
            tree_r: [TreeCell::identity(); 8],
        };
        stage.apply_params(&params);
        Ok(stage)
    }

    /// 覆盖参数快照（对齐 TS `setParams` → `applyParams`，逐行同序）。
    ///
    /// 参数即时生效：钳制 + 交叉树系数与 attack/release 系数重算；**增益/目标/
    /// 电平/滤波器状态保留**（规格 §4.8，避免改参爆音）。
    pub fn set_params(&mut self, params: DynamicEqParams) {
        self.apply_params(&params);
    }

    /// 对齐 TS `applyParams`（L138–L162），赋值顺序逐行一致。
    fn apply_params(&mut self, p: &DynamicEqParams) {
        let fs = self.fs;
        let nyq = fs / 2.0;
        if let Some(v) = p.enabled {
            self.enabled = v;
        }
        self.strength = clamp(p.strength.unwrap_or(self.strength), 0.0, 1.0);
        self.threshold_db = clamp(p.threshold_db.unwrap_or(self.threshold_db), -80.0, 0.0);
        self.ratio = clamp(p.ratio.unwrap_or(self.ratio), 1.0, 100.0);
        self.knee_db = clamp(p.knee_db.unwrap_or(self.knee_db), 0.0, 40.0);
        // TS：p.attackMs ?? this.currentAttackMs()（未指定时反解当前毫秒保持平滑时间）。
        let attack_ms = match p.attack_ms {
            Some(v) => v,
            None => self.current_attack_ms(),
        };
        self.attack_coef = one_pole_coef(attack_ms, fs, 0.05);
        let release_ms = match p.release_ms {
            Some(v) => v,
            None => self.current_release_ms(),
        };
        self.release_coef = one_pole_coef(release_ms, fs, 1.0);
        self.block_size =
            js_max(16.0, js_min(2048.0, p.block_size.unwrap_or(self.block_size).floor()));
        if let Some(bands) = &p.bands {
            for i in 0..BAND_COUNT {
                if let Some(b) = bands.get(i) {
                    self.band_enabled[i] = b.enabled;
                    self.static_db[i] =
                        clamp(b.target_gain_db.unwrap_or(self.static_db[i]), -12.0, 12.0);
                    // 仅 i < 4 的带读取交叉频率；末带完全忽略（不读取、不钳制）。
                    if i < BAND_COUNT - 1 {
                        self.cross_freqs[i] = clamp(b.frequency, 30.0, nyq * 0.9);
                    }
                }
            }
        }
        // 无论是否改频带都重算交叉树（构造期也必须生效，否则树保持直通系数）。
        self.update_crossover();
    }

    /// 反解当前 attack 毫秒（TS `currentAttackMs`：coef === 0 时取默认 20）。
    fn current_attack_ms(&self) -> f64 {
        if self.attack_coef == 0.0 {
            20.0
        } else {
            -1000.0 / (self.fs * (1.0 - self.attack_coef).ln())
        }
    }

    /// 反解当前 release 毫秒（TS `currentReleaseMs`：coef === 0 时取默认 200）。
    fn current_release_ms(&self) -> f64 {
        if self.release_coef == 0.0 {
            200.0
        } else {
            -1000.0 / (self.fs * (1.0 - self.release_coef).ln())
        }
    }

    /// 按当前交叉频率重算交叉树系数（对齐 TS `updateCrossover`）。
    fn update_crossover(&mut self) {
        let fs = self.fs;
        for i in 0..BAND_COUNT - 1 {
            let (lp, hp, a1) = crossover_coeffs(self.cross_freqs[i], fs);
            self.tree_l[2 * i] = TreeCell::with_coeffs(lp, lp, 0.0, a1, 0.0);
            self.tree_l[2 * i + 1] = TreeCell::with_coeffs(hp, -hp, 0.0, a1, 0.0);
            self.tree_r[2 * i] = TreeCell::with_coeffs(lp, lp, 0.0, a1, 0.0);
            self.tree_r[2 * i + 1] = TreeCell::with_coeffs(hp, -hp, 0.0, a1, 0.0);
        }
    }

    /// 逐样本主循环（对齐 TS `processStereo` 循环体，逐行同序）。
    fn run(&mut self, left: &mut [f32], right: &mut [f32]) {
        if !self.enabled || self.strength <= 0.0 {
            return; // 硬直通：输出逐样本等于输入，全部状态不推进（规格 §4.8）
        }
        let n = left.len();
        let block_size = self.block_size;
        if !block_size.is_finite() {
            // NaN blockSize：TS 循环条件 pos<n 恒假 → 零样本处理（防御域，向量不可达）。
            return;
        }
        // [16, 2048] 整数语义（apply_params 已 floor+钳制；向量域必为有限整数）。
        let block = block_size as usize;
        // 局部化派生量（对齐 TS processStereo 开头的一组局部 const）。
        let attack = self.attack_coef;
        let release = self.release_coef;
        let inv_ratio = 1.0 - 1.0 / self.ratio;
        let knee = self.knee_db;
        let knee_half = knee * 0.5;
        let two_knee = 2.0 * knee;
        let thr = self.threshold_db;
        let strength = self.strength;
        let mut pos = 0;
        while pos < n {
            // 内部分析块边界 = min(params.blockSize, 本次调用剩余样本数)；
            // sumsq 不跨调用累积（规格 §4.5 的分块耦合行为事实）。
            let end = (pos + block).min(n);
            let len = end - pos;
            for b in 0..BAND_COUNT {
                self.sumsq[b] = 0.0;
            }
            let inv_n = 1.0 / (2.0 * len as f64);
            for i in pos..end {
                let xl = f64::from(left[i]);
                let xr = f64::from(right[i]);
                // —— 交叉树（逐样本）：band0=LP1(x); band1=HP1→LP2; …; band4=HP4（链式残差）
                let r1l = self.tree_l[1].tick(xl);
                let b0l = self.tree_l[0].tick(xl);
                let r2l = self.tree_l[3].tick(r1l);
                let b1l = self.tree_l[2].tick(r1l);
                let r3l = self.tree_l[5].tick(r2l);
                let b2l = self.tree_l[4].tick(r2l);
                let r4l = self.tree_l[7].tick(r3l);
                let b3l = self.tree_l[6].tick(r3l);
                let b4l = r4l;
                let r1r = self.tree_r[1].tick(xr);
                let b0r = self.tree_r[0].tick(xr);
                let r2r = self.tree_r[3].tick(r1r);
                let b1r = self.tree_r[2].tick(r1r);
                let r3r = self.tree_r[5].tick(r2r);
                let b2r = self.tree_r[4].tick(r2r);
                let r4r = self.tree_r[7].tick(r3r);
                let b3r = self.tree_r[6].tick(r3r);
                let b4r = r4r;
                // —— 能量累加（块内分析，立体声联合：L² + R²）
                self.sumsq[0] += b0l * b0l + b0r * b0r;
                self.sumsq[1] += b1l * b1l + b1r * b1r;
                self.sumsq[2] += b2l * b2l + b2r * b2r;
                self.sumsq[3] += b3l * b3l + b3r * b3r;
                self.sumsq[4] += b4l * b4l + b4r * b4r;
                // —— 增益平滑（逐样本一阶：下降用 attack，恢复用 release）
                for b in 0..BAND_COUNT {
                    let t = self.target_gains[b];
                    let g = self.gains[b];
                    let coef = if t < g { attack } else { release };
                    self.gains[b] = g + coef * (t - g);
                }
                // —— 输出：Σ gain_b·band_b（单位增益时精确重建输入）
                left[i] = (self.gains[0] * b0l
                    + self.gains[1] * b1l
                    + self.gains[2] * b2l
                    + self.gains[3] * b3l
                    + self.gains[4] * b4l) as f32;
                right[i] = (self.gains[0] * b0r
                    + self.gains[1] * b1r
                    + self.gains[2] * b2r
                    + self.gains[3] * b3r
                    + self.gains[4] * b4r) as f32;
            }
            // —— 块末控制：由本块能量计算下一块的目标增益（软拐点压缩 + 静态曲线 + strength）
            for b in 0..BAND_COUNT {
                let level_db = 10.0 * (self.sumsq[b] * inv_n + 1e-12).log10();
                self.levels_db[b] = level_db;
                let over = level_db - thr;
                let reduction = if knee <= 0.0 {
                    // 硬拐点分支（kneeDb=0）。
                    if over > 0.0 {
                        over * inv_ratio
                    } else {
                        0.0
                    }
                } else if over < -knee_half {
                    0.0
                } else if over > knee_half {
                    over * inv_ratio
                } else {
                    // 软拐点二次插值。
                    let x = over + knee_half;
                    (inv_ratio * x * x) / two_knee
                };
                let target_db = self.static_db[b] - reduction;
                let target_lin = 10.0_f64.powf(target_db / 20.0);
                let mixed = 1.0 + strength * (target_lin - 1.0);
                self.target_gains[b] = if self.band_enabled[b] {
                    js_min(js_max(mixed, GAIN_MIN), GAIN_MAX)
                } else {
                    1.0
                };
            }
            pos = end;
        }
    }

    /// 当前每带平滑增益（线性，5 项；调试/UI 用，不进向量；对齐 TS `getBandGains`）。
    pub fn get_band_gains(&self) -> [f64; BAND_COUNT] {
        self.gains
    }

    /// 最近一次分析的各带电平 dB（5 项，调试/UI 用；对齐 TS `getBandLevelsDb`）。
    pub fn get_band_levels_db(&self) -> [f64; BAND_COUNT] {
        self.levels_db
    }
}

impl Stage for DynamicEqStage {
    /// 交叉树与全部带状态为构造期定容数组，无需按块长预分配（保留形参以符合
    /// Stage 契约）。
    fn prepare(&mut self, _max_block_size: usize) {}

    /// 就地处理一个立体声块；内部分析块边界与控制节奏见模块文档（输出依赖
    /// 驱动分块，对拍按冻结向量同一 blockSize 回放）。
    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        debug_assert_eq!(left.len(), right.len(), "左右声道块长必须一致（Stage 契约）");
        self.run(left, right);
    }

    /// 复位：清空全部交叉树状态并将 sumsq/levelsDb 归零、targetGains/gains 归 1
    /// （重放与首次一致；参数保留；对齐 TS `reset`）。
    fn reset(&mut self) {
        for cell in self.tree_l.iter_mut() {
            cell.reset();
        }
        for cell in self.tree_r.iter_mut() {
            cell.reset();
        }
        self.sumsq = [0.0; BAND_COUNT];
        self.levels_db = [0.0; BAND_COUNT];
        self.target_gains = [1.0; BAND_COUNT];
        self.gains = [1.0; BAND_COUNT];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 确定性伪噪声（LCG，无随机依赖），幅度 [-amp, amp)；与导出工具同款。
    fn lcg_noise(n: usize, seed: u32, amp: f64) -> Vec<f32> {
        let mut u = seed;
        (0..n)
            .map(|_| {
                u = u.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (((f64::from(u) / 4_294_967_296.0) * 2.0 - 1.0) * amp) as f32
            })
            .collect()
    }

    /// 正弦叠加（f64 域计算后写 f32，对齐 TS 浮点行为）。
    fn sine(n: usize, freq: f64, fs: f64, amp: f64, phase: f64) -> Vec<f32> {
        (0..n)
            .map(|i| {
                (2.0 * std::f64::consts::PI * freq * i as f64 / fs + phase).sin() * amp
            })
            .map(|v| v as f32)
            .collect()
    }

    fn drive_in_chunks(
        stage: &mut DynamicEqStage,
        l: &[f32],
        r: &[f32],
        block: usize,
    ) -> (Vec<f32>, Vec<f32>) {
        let mut blocks = Vec::new();
        let mut off = 0_usize;
        while off < l.len() {
            blocks.push((off + block).min(l.len()) - off);
            off += block;
        }
        drive_schedule(stage, l, r, &blocks)
    }

    /// 按显式块长调度驱动（与黄金参考的驱动分块逐块对应，含末块短块）。
    fn drive_schedule(
        stage: &mut DynamicEqStage,
        l: &[f32],
        r: &[f32],
        blocks: &[usize],
    ) -> (Vec<f32>, Vec<f32>) {
        let mut out_l = l.to_vec();
        let mut out_r = r.to_vec();
        let mut off = 0_usize;
        for &len in blocks {
            stage.process(&mut out_l[off..off + len], &mut out_r[off..off + len]);
            off += len;
        }
        assert_eq!(off, l.len(), "块长调度必须覆盖全部帧");
        (out_l, out_r)
    }

    /// f32 输出落点比对（对拍口径同容差：相对 1e-6、地板 1e-9）。
    fn assert_f32_close(got: f32, want: f64, label: &str) {
        let diff = (f64::from(got) - want).abs();
        assert!(
            diff <= 1e-6 * want.abs().max(1e-9),
            "{label}：got {got}，want {want}"
        );
    }

    /// f64 状态量比对（相对 1e-9：远宽于跨库 libm 1~2 ulp 差异——log10/powf/exp
    /// 在非整周角上可差末位——远严于任何公式/钳制/块边界错误）。
    fn assert_state_close(got: f64, want: f64, label: &str) {
        assert!(
            (got - want).abs() <= 1e-9 * want.abs().max(1e-12),
            "{label}：got {got}，want {want}"
        );
    }

    /// 黄金参考：node 直跑仓库根 src/dsp/DynamicEq.ts 导出（JSON 最短往返表示）。
    mod golden {
        /// case2 形态（release 爬升 + 静态提升曲线）：48 帧、块 [11,11,11,15]。
        pub const DY_A_OUT_L: [f64; 48] = [
            0.0, 0.007853658869862556, 0.015705378726124763, 0.02355322614312172,
            0.031395260244607925, 0.03922954946756363, 0.04705415666103363,
            0.054867155849933624, 0.06266661733388901, 0.07045061886310577,
            0.07821723073720932, 0.08596697449684143, 0.09369605779647827,
            0.10140258818864822, 0.10908468067646027, 0.11674045026302338,
            0.12436801195144653, 0.1319654881954193, 0.13953103125095367,
            0.1470627784729004, 0.15455885231494904, 0.16201741993427277,
            0.16943667829036713, 0.1768147498369217, 0.184149831533432,
            0.1914401352405548, 0.19868387281894684, 0.20587922632694244,
            0.21302443742752075, 0.2201177477836609, 0.22715741395950317,
            0.23414167761802673, 0.24106884002685547, 0.2479371875524521,
            0.2547450065612793, 0.2614906430244446, 0.26817241311073303,
            0.27478864789009094, 0.28133776783943176, 0.2878181040287018,
            0.29422807693481445, 0.3005661368370056, 0.30683064460754395,
            0.31302008032798767, 0.3191329538822174, 0.3251676857471466,
            0.33112284541130066, 0.3369969129562378,
        ];
        pub const DY_A_OUT_R: [f64; 48] = [
            -0.19812385737895966, -0.3294999599456787, 0.06182495877146721,
            -0.22195658087730408, -0.09947184473276138, -0.3794688880443573,
            -0.04217497259378433, -0.3052319884300232, 0.2990509569644928,
            0.3957074284553528, 0.28256216645240784, -0.00025568518321961164,
            0.113605797290802, 0.2891800105571747, 0.07719218730926514,
            -0.32740887999534607, -0.2878621220588684, 0.3600817918777466,
            0.33969178795814514, 0.3116411566734314, 0.04045436903834343,
            -0.2555927038192749, 0.040068481117486954, -0.19284963607788086,
            0.35454317927360535, 0.2572886645793915, -0.27576249837875366,
            0.263563871383667, -0.026423687115311623, -0.35149484872817993,
            -0.38216936588287354, 0.029759157449007034, 0.2640082836151123,
            0.2763754427433014, 0.14484386146068573, -0.09535977244377136,
            0.06856243312358856, 0.15720108151435852, 0.16920961439609528,
            0.1750340759754181, 0.3924654722213745, -0.04005347564816475,
            -0.32170188426971436, -0.24684922397136688, -0.04217355325818062,
            -0.29855218529701233, 0.21501187980175018, 0.07503638416528702,
        ];
        /// 48 帧驱动结束时的每带平滑增益（release 爬升轨迹终值）。
        pub const DY_A_GAINS: [f64; 5] = [
            1.0019142621072363, 1.00112496862211, 1.0007934641728178, 1.0004980105213908,
            1.0002346871773808,
        ];
        /// case4 形态（极值钳制 + 部分带禁用）@44100：32 帧、块 [7,7,7,11]。
        pub const DY_B_OUT_L: [f64; 32] = [
            0.0, 0.03845676779747009, 0.07684329152107239, 0.11508944630622864,
            0.15312537550926208, 0.19088158011436462, 0.22828912734985352,
            0.2630476653575897, 0.29788938164711, 0.331705778837204,
            0.3655411899089813, 0.3982667922973633, 0.43062129616737366,
            0.46187108755111694, 0.49175143241882324, 0.5205922722816467,
            0.5485726594924927, 0.5754156708717346, 0.6011930704116821,
            0.6257354021072388, 0.6490610837936401, 0.6706020832061768,
            0.6909129023551941, 0.7098919749259949, 0.7275004982948303,
            0.743672788143158, 0.7583779692649841, 0.7715719938278198,
            0.7832310199737549, 0.7933252453804016, 0.8018367886543274,
            0.8087462782859802,
        ];
        pub const DY_B_OUT_R: [f64; 32] = [
            -0.4701944887638092, 0.7442878484725952, 0.20248499512672424,
            0.7685666084289551, -0.811185896396637, 0.6404104232788086,
            -0.2502267062664032, -0.9277328848838806, -0.6437969207763672,
            0.6535174250602722, 1.101788878440857, -0.08811628818511963,
            -0.022474080324172974, 0.00488591194152832, 0.24433466792106628,
            0.43011415004730225, 0.5552448034286499, -0.14521285891532898,
            -0.10544665902853012, 0.6078943610191345, -0.1294371485710144,
            -0.19616486132144928, -0.5550784468650818, 0.1743348091840744,
            0.8351243734359741, -0.3584550619125366, -0.5378078818321228,
            0.6779839992523193, -0.3458307087421417, -0.46547192335128784,
            0.2162037491798401, -0.867618978023529,
        ];
        /// 32 帧驱动结束时的每带平滑增益（禁用带恰为 1.0，启用带深压缩下探）。
        pub const DY_B_GAINS: [f64; 5] = [
            0.02461357612260793, 1.0, 0.015926621498209028, 1.0, 0.002481740329322963,
        ];
    }

    fn case_a_params() -> DynamicEqParams {
        DynamicEqParams {
            enabled: Some(true),
            strength: Some(0.5),
            threshold_db: Some(-10.0),
            ratio: Some(2.0),
            knee_db: Some(6.0),
            attack_ms: Some(20.0),
            release_ms: Some(200.0),
            block_size: Some(128.0),
            bands: Some(vec![
                DynamicEqBandParam { enabled: true, frequency: 200.0, target_gain_db: Some(6.0) },
                DynamicEqBandParam { enabled: true, frequency: 800.0, target_gain_db: Some(4.0) },
                DynamicEqBandParam { enabled: true, frequency: 2500.0, target_gain_db: Some(3.0) },
                DynamicEqBandParam { enabled: true, frequency: 8000.0, target_gain_db: Some(2.0) },
                DynamicEqBandParam { enabled: true, frequency: 0.0, target_gain_db: Some(1.0) },
            ]),
        }
    }

    fn case_b_params() -> DynamicEqParams {
        DynamicEqParams {
            enabled: Some(true),
            strength: Some(1.5),      // → 1
            threshold_db: Some(-90.0), // → −80
            ratio: Some(120.0),        // → 100
            knee_db: Some(45.0),       // → 40
            attack_ms: Some(0.0),      // → 0.05 ms 下限
            release_ms: Some(0.0),     // → 1 ms 下限
            block_size: Some(8.0),     // → 16
            bands: Some(vec![
                DynamicEqBandParam { enabled: true, frequency: 10.0, target_gain_db: Some(20.0) },
                DynamicEqBandParam { enabled: false, frequency: 30000.0, target_gain_db: Some(-20.0) },
                DynamicEqBandParam { enabled: true, frequency: 2500.0, target_gain_db: Some(3.0) },
                DynamicEqBandParam { enabled: false, frequency: 8000.0, target_gain_db: Some(0.0) },
                DynamicEqBandParam { enabled: true, frequency: 99999.0, target_gain_db: Some(1.0) },
            ]),
        }
    }

    #[test]
    fn 静态提升与release爬升_输出与增益命中ts黄金参考() {
        let fs = 48000.0;
        let mut stage = DynamicEqStage::from_params(fs, case_a_params()).expect("合法参数");
        stage.prepare(48);
        let in_l = sine(48, 120.0, fs, 0.5, 0.0);
        let in_r = lcg_noise(48, 42, 0.4);
        let (out_l, out_r) = drive_schedule(&mut stage, &in_l, &in_r, &[11, 11, 11, 15]);
        for (i, want) in golden::DY_A_OUT_L.iter().enumerate() {
            assert_f32_close(out_l[i], *want, &format!("DY-A L[{i}]"));
        }
        for (i, want) in golden::DY_A_OUT_R.iter().enumerate() {
            assert_f32_close(out_r[i], *want, &format!("DY-A R[{i}]"));
        }
        let gains = stage.get_band_gains();
        for b in 0..5 {
            assert_state_close(gains[b], golden::DY_A_GAINS[b], &format!("DY-A gain[{b}]"));
        }
    }

    #[test]
    fn 极值钳制与部分带禁用_输出与增益命中ts黄金参考_case4形态() {
        let fs = 44100.0;
        let mut stage = DynamicEqStage::from_params(fs, case_b_params()).expect("合法参数");
        stage.prepare(32);
        let in_l = sine(32, 300.0, fs, 0.9, 0.0);
        let in_r = lcg_noise(32, 7, 0.9);
        let (out_l, out_r) = drive_schedule(&mut stage, &in_l, &in_r, &[7, 7, 7, 11]);
        for (i, want) in golden::DY_B_OUT_L.iter().enumerate() {
            assert_f32_close(out_l[i], *want, &format!("DY-B L[{i}]"));
        }
        for (i, want) in golden::DY_B_OUT_R.iter().enumerate() {
            assert_f32_close(out_r[i], *want, &format!("DY-B R[{i}]"));
        }
        // 禁用带（bands[1]/bands[3]）目标增益恒 1，启用带深压缩下探。
        let gains = stage.get_band_gains();
        for b in 0..5 {
            assert_state_close(gains[b], golden::DY_B_GAINS[b], &format!("DY-B gain[{b}]"));
        }
        assert_eq!(gains[1], 1.0, "禁用带增益必须恰为 1");
        assert_eq!(gains[3], 1.0, "禁用带增益必须恰为 1");
    }

    #[test]
    fn enabled_false_逐位直通且全部状态不推进() {
        // GWT-DY-01：缓冲零改写，增益/目标/电平/交叉树状态不推进。
        let fs = 48000.0;
        let mut p = case_a_params();
        p.enabled = Some(false);
        let mut stage = DynamicEqStage::from_params(fs, p).expect("合法参数");
        stage.prepare(64);
        let in_l = lcg_noise(256, 11, 0.9);
        let in_r = sine(256, 6000.0, fs, 0.4, 0.3);
        let (out_l, out_r) = drive_in_chunks(&mut stage, &in_l, &in_r, 64);
        assert_eq!(out_l, in_l, "左声道必须逐位一致");
        assert_eq!(out_r, in_r, "右声道必须逐位一致");
        assert_eq!(stage.get_band_gains(), [1.0; 5], "禁用态下增益不得推进");
        assert_eq!(stage.get_band_levels_db(), [0.0; 5], "禁用态下电平不得推进");
    }

    #[test]
    fn strength_0_与_enabled_false_同路径硬直通() {
        // GWT-DY-01/规格 §4.8：strength≤0 与 enabled=false 同首行联合判定。
        let fs = 48000.0;
        let mut p = case_a_params();
        p.strength = Some(0.0);
        let mut stage = DynamicEqStage::from_params(fs, p).expect("合法参数");
        stage.prepare(33);
        let in_l = lcg_noise(200, 12, 0.8);
        let in_r = lcg_noise(200, 13, 0.8);
        let (out_l, out_r) = drive_in_chunks(&mut stage, &in_l, &in_r, 33);
        assert_eq!(out_l, in_l);
        assert_eq!(out_r, in_r);
        assert_eq!(stage.get_band_gains(), [1.0; 5]);
    }

    #[test]
    fn 单位增益精确重建_全带静态0_阈下输出逐位一致() {
        // GWT-DY-07：reduction 恒 0、目标恒 1 → 增益恒 1 → LP+HP=1 逐位重建输入。
        let fs = 48000.0;
        let params = DynamicEqParams {
            enabled: Some(true),
            strength: Some(1.0),
            threshold_db: Some(0.0), // 低电平激励 → levelDb < 0 → 恒阈下
            ratio: Some(4.0),
            knee_db: Some(6.0),
            attack_ms: Some(10.0),
            release_ms: Some(100.0),
            block_size: Some(128.0),
            bands: Some((0..5)
                .map(|i| DynamicEqBandParam {
                    enabled: true,
                    frequency: [200.0, 800.0, 2500.0, 8000.0, 0.0][i],
                    target_gain_db: Some(0.0),
                })
                .collect()),
        };
        let mut stage = DynamicEqStage::from_params(fs, params).expect("合法参数");
        stage.prepare(97);
        let in_l = lcg_noise(400, 21, 0.01);
        let in_r = sine(400, 300.0, fs, 0.01, 0.5);
        let (out_l, out_r) = drive_in_chunks(&mut stage, &in_l, &in_r, 97);
        assert_eq!(out_l, in_l, "单位增益必须逐位重建输入（左）");
        assert_eq!(out_r, in_r, "单位增益必须逐位重建输入（右）");
        assert_eq!(stage.get_band_gains(), [1.0; 5], "增益全程保持 1");
    }

    #[test]
    fn 控制节奏与分块耦合_整数倍逐位一致_非整数倍发散() {
        // GWT-DY-06/规格 §4.5 行为事实：512 = 4×128。
        let fs = 48000.0;
        let in_l = sine(512, 120.0, fs, 0.5, 0.0);
        let in_r = lcg_noise(512, 42, 0.4);
        let run = |block: usize| {
            let mut stage = DynamicEqStage::from_params(fs, case_a_params()).expect("合法参数");
            stage.prepare(block);
            drive_in_chunks(&mut stage, &in_l, &in_r, block)
        };
        let (whole_l, whole_r) = run(512);
        let (multiple_l, multiple_r) = run(128);
        let (uneven_l, uneven_r) = run(333);
        assert_eq!(whole_l, multiple_l, "顶层分块为 params.blockSize 整数倍时必须逐位一致（左）");
        assert_eq!(whole_r, multiple_r, "顶层分块为 params.blockSize 整数倍时必须逐位一致（右）");
        assert!(
            whole_l != uneven_l || whole_r != uneven_r,
            "非整数倍分块必须在调用边界提前触发控制更新（若相等说明块耦合语义被破坏）"
        );
    }

    #[test]
    fn 末带frequency_被完全忽略() {
        // 规格 §三/§4.9.6：0 / 8000 / 99999 输出逐位一致。
        let fs = 48000.0;
        let in_l = lcg_noise(300, 31, 0.7);
        let in_r = sine(300, 1000.0, fs, 0.4, 0.0);
        let run = |last_freq: f64| {
            let mut p = case_a_params();
            if let Some(bands) = &mut p.bands {
                bands[4].frequency = last_freq;
            }
            let mut stage = DynamicEqStage::from_params(fs, p).expect("合法参数");
            stage.prepare(64);
            drive_in_chunks(&mut stage, &in_l, &in_r, 64)
        };
        let (a_l, a_r) = run(0.0);
        let (b_l, b_r) = run(8000.0);
        let (c_l, c_r) = run(99999.0);
        assert_eq!(a_l, b_l);
        assert_eq!(a_l, c_l);
        assert_eq!(a_r, b_r);
        assert_eq!(a_r, c_r);
    }

    #[test]
    fn 越界参数钳制与直接按生效值配置逐位等效() {
        // GWT-DY-05：全维度钳制（case4 形态）与边界值直接配置等效。
        let fs = 44100.0;
        let direct = DynamicEqParams {
            enabled: Some(true),
            strength: Some(1.0),
            threshold_db: Some(-80.0),
            ratio: Some(100.0),
            knee_db: Some(40.0),
            attack_ms: Some(0.05),
            release_ms: Some(1.0),
            block_size: Some(16.0),
            bands: Some(vec![
                DynamicEqBandParam { enabled: true, frequency: 30.0, target_gain_db: Some(12.0) },
                DynamicEqBandParam { enabled: false, frequency: 19845.0, target_gain_db: Some(-12.0) },
                DynamicEqBandParam { enabled: true, frequency: 2500.0, target_gain_db: Some(3.0) },
                DynamicEqBandParam { enabled: false, frequency: 8000.0, target_gain_db: Some(0.0) },
                DynamicEqBandParam { enabled: true, frequency: 99999.0, target_gain_db: Some(1.0) },
            ]),
        };
        let mut clamped = DynamicEqStage::from_params(fs, case_b_params()).expect("合法参数");
        let mut direct = DynamicEqStage::from_params(fs, direct).expect("合法参数");
        clamped.prepare(64);
        direct.prepare(64);
        let in_l = lcg_noise(256, 51, 0.8);
        let in_r = lcg_noise(256, 52, 0.6);
        let (c_l, c_r) = drive_in_chunks(&mut clamped, &in_l, &in_r, 64);
        let (d_l, d_r) = drive_in_chunks(&mut direct, &in_l, &in_r, 64);
        assert_eq!(c_l, d_l, "钳制生效值必须与边界值直接配置逐位一致（左）");
        assert_eq!(c_r, d_r, "钳制生效值必须与边界值直接配置逐位一致（右）");
    }

    #[test]
    fn bands_短于5项时缺项保持当前默认_超出忽略() {
        // 规格 §三补充说明：短项带保持 enabled=true / 交叉默认 / targetGainDb=0；
        // 超出 5 项的部分被忽略。
        let fs = 48000.0;
        let in_l = lcg_noise(240, 61, 0.6);
        let in_r = sine(240, 500.0, fs, 0.3, 0.0);
        // 短于：3 项 + 缺项带按默认补齐的显式 5 项（band3={true,8000,0}、
        // band4={true,任意,0}——末带 frequency 被忽略）必须逐位一致。
        let mut short = case_a_params();
        short.bands.as_mut().unwrap().truncate(3);
        let mut explicit_default = case_a_params();
        {
            let bands = explicit_default.bands.as_mut().unwrap();
            bands[3] = DynamicEqBandParam { enabled: true, frequency: 8000.0, target_gain_db: Some(0.0) };
            bands[4] = DynamicEqBandParam { enabled: true, frequency: 0.0, target_gain_db: Some(0.0) };
        }
        let mut stage_short = DynamicEqStage::from_params(fs, short).expect("合法参数");
        stage_short.prepare(60);
        let (s_l, s_r) = drive_in_chunks(&mut stage_short, &in_l, &in_r, 60);
        let mut stage_default = DynamicEqStage::from_params(fs, explicit_default).expect("合法参数");
        stage_default.prepare(60);
        let (d_l, d_r) = drive_in_chunks(&mut stage_default, &in_l, &in_r, 60);
        assert_eq!(s_l, d_l, "缺项带必须保持默认配置（左）");
        assert_eq!(s_r, d_r, "缺项带必须保持默认配置（右）");
        // 长于：7 项（多出 2 项为 band0 的复制）与恰 5 项逐位一致。
        let mut long = case_a_params();
        let extra = long.bands.as_ref().unwrap()[0];
        long.bands.as_mut().unwrap().extend_from_slice(&[extra; 2]);
        let mut stage_long = DynamicEqStage::from_params(fs, long).expect("合法参数");
        stage_long.prepare(60);
        let (l_l, l_r) = drive_in_chunks(&mut stage_long, &in_l, &in_r, 60);
        let mut stage_plain = DynamicEqStage::from_params(fs, case_a_params()).expect("合法参数");
        stage_plain.prepare(60);
        let (p_l, p_r) = drive_in_chunks(&mut stage_plain, &in_l, &in_r, 60);
        assert_eq!(l_l, p_l, "超出的带项必须被忽略（左）");
        assert_eq!(l_r, p_r, "超出的带项必须被忽略（右）");
    }

    #[test]
    fn set_params_保留增益与电平状态() {
        // 规格 §4.8：参数即时生效、不清历史。
        let fs = 48000.0;
        let mut stage = DynamicEqStage::from_params(fs, case_a_params()).expect("合法参数");
        stage.prepare(128);
        let in_l = lcg_noise(512, 71, 0.8);
        let in_r = sine(512, 2000.0, fs, 0.5, 0.0);
        let _ = drive_in_chunks(&mut stage, &in_l, &in_r, 128);
        let gains_before = stage.get_band_gains();
        let levels_before = stage.get_band_levels_db();
        assert!(gains_before.iter().any(|&g| g != 1.0), "预处理后增益应已推进");
        stage.set_params(case_a_params()); // 同参数重设
        assert_eq!(stage.get_band_gains(), gains_before, "set_params 必须保留增益状态");
        assert_eq!(stage.get_band_levels_db(), levels_before, "set_params 必须保留电平状态");
    }

    #[test]
    fn reset_后重放与首次从零状态逐位一致() {
        // GWT-DY-09。
        let fs = 48000.0;
        let in_l = lcg_noise(600, 81, 0.8);
        let in_r = sine(600, 1500.0, fs, 0.5, 0.2);
        let mut stage = DynamicEqStage::from_params(fs, case_a_params()).expect("合法参数");
        stage.prepare(97);
        let (first_l, first_r) = drive_in_chunks(&mut stage, &in_l, &in_r, 97);
        stage.reset();
        assert_eq!(stage.get_band_gains(), [1.0; 5], "reset 后增益归 1");
        let (again_l, again_r) = drive_in_chunks(&mut stage, &in_l, &in_r, 97);
        assert_eq!(first_l, again_l, "reset 后重放必须逐位一致（左）");
        assert_eq!(first_r, again_r, "reset 后重放必须逐位一致（右）");
    }

    #[test]
    fn 静音输入_输出逐位零_电平落在地板无nan() {
        // GWT-DY-08。
        let fs = 48000.0;
        let mut stage = DynamicEqStage::from_params(fs, case_a_params()).expect("合法参数");
        stage.prepare(128);
        let n = 960;
        let (out_l, out_r) = drive_in_chunks(&mut stage, &vec![0.0_f32; n], &vec![0.0_f32; n], 128);
        assert!(out_l.iter().all(|&x| x.to_bits() == 0_u32));
        assert!(out_r.iter().all(|&x| x.to_bits() == 0_u32));
        for b in 0..5 {
            let level = stage.get_band_levels_db()[b];
            assert!(level.is_finite(), "电平不得为 NaN @band{b}");
            assert_state_close(level, 10.0 * 1e-12_f64.log10(), &format!("地板电平 band{b}"));
        }
    }

    #[test]
    fn 满幅输入极值参数下有界不发散() {
        // GWT-DY-11。
        let fs = 44100.0;
        let mut stage = DynamicEqStage::from_params(fs, case_b_params()).expect("合法参数");
        stage.prepare(128);
        let in_l = lcg_noise(2048, 91, 1.0);
        let in_r = lcg_noise(2048, 92, 1.0);
        let (out_l, out_r) = drive_in_chunks(&mut stage, &in_l, &in_r, 128);
        for i in 0..2048 {
            assert!(out_l[i].is_finite() && out_r[i].is_finite(), "输出必须有限 @{i}");
        }
        assert!(stage.get_band_gains().iter().all(|&g| (0.0..=3.0).contains(&g) || g == 1.0));
    }

    #[test]
    fn 非法采样率报错_对齐ts错误信息() {
        for bad in [0.0_f64, -44100.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let err = DynamicEqStage::from_params(bad, case_a_params())
                .err()
                .expect("非法采样率必须报错");
            assert!(err.contains("invalid sample rate"), "应对齐 TS 错误信息：{err}");
        }
    }
}
