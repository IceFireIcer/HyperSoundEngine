//! mod_effects —— 调制类效果组（Delay → Chorus → Flanger → Phaser → Tremolo 级联）。
//!
//! 行为事实标准：仓库根 `src/dsp/ModEffects.ts`；规格：`specs/dsp/mod-effects.md`。
//! 本文件是该 TS 五个类的逐行移植 + [`ModEffectsStage`] 链路驱动器，关键对应关系：
//!
//! - 级联与门控（规格 §4.1，模块特有行为事实）：级联顺序 = 引擎接线顺序（固定）
//!   Delay → Chorus → Flanger → Phaser → Tremolo；**效果类自身不消费 enabled**——
//!   五级无条件 `set_params`（TS 引擎对五效果无论 enabled 与否都调用 setParams），
//!   仅 enabled 的级参与链路，禁用级整级跳过（逐位旁路、状态不推进）。
//! - 环形延迟线（规格 §4.2）：readDelay 线性插值，**d<1 退化读取区读整环回绕
//!   前值**（`idx0 = pos`——上次写入该槽位的内容，size 样本之前），对 d 连续；
//!   写入发生在读取之后（反馈环含一步延迟）。缓冲长度：Delay = ceil(fs×2)+1、
//!   Chorus = ceil(fs×0.1)+2、Flanger = ceil(fs×0.05)+2。
//! - Chorus / Flanger（规格 §4.4，共享 [`ModulatedDelayCore`]）：chorus 基础延迟
//!   固定 20 ms、反馈恒 0；flanger 固定 1 ms、反馈可配。**LFO 相位按整块步进**
//!   （`advance(n)` 在块末、块内调制量为常量）⇒ 输出依赖驱动分块 blockSize，
//!   对拍必须按冻结向量同一 blockSize 回放。
//! - Phaser（规格 §4.5，模块特有行为事实）：**各级全通并行处理同一输入 in**
//!   （非级联），仅末级输出被采用——stages 通过改变「哪一级是末级」影响输出
//!   （stages=7 ≡ 8 逐位一致）；反馈基准为 mix 前末级输出、逐声道单样本延迟；
//!   allpass 状态（x1/y1）落点为 f32、其余中间量 f64；LFO 逐样本步进（分块
//!   与整块逐位一致）。
//! - Tremolo（规格 §4.6）：逐样本幅度调制，`mix=0` 时乘数精确 1.0（逐位恒等）；
//!   rateHz 钳制上界 30 与 chorus/flanger 的 20 不同。
//!
//! # 数值精度铁律的落点
//!
//! - 一切以 TS Number 参与运算的中间量（延迟样本数、LFO 相位与调制量、allpass
//!   系数 a、干湿混合式）全部 f64 复刻，运算顺序与 TS 逐行一致；f32 落点仅有：
//!   环形缓冲写回、phaser allpass 状态写回、输出样本写回（对齐 Float32Array）；
//! - `Math.min`/`Math.max` 的 NaN 传播语义以 [`js_min`]/[`js_max`] 显式复刻；
//!   `Math.round` 的「ties 向 +∞」语义以 [`js_round`] 复刻（stages 取整）。
//!
//! # 实时安全
//!
//! 全部环形缓冲与全通状态在构造期一次定容分配，`Stage::process` 稳态零分配、
//! 零锁、零系统调用；`prepare` 无需额外预分配。

use crate::Stage;

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

/// 复刻 JS `Math.round` 的「ties 向 +∞」语义（对有限输入等价 floor(x+0.5)，
/// 覆盖 stages 的取值域；NaN 原样返回，经钳制链传播后 stages 为 0——与 TS
/// 循环条件 `s < NaN` 恒假、零级全通的观测行为一致）。
fn js_round(x: f64) -> f64 {
    (x + 0.5).floor()
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

/// 环形延迟线读取（线性插值；逐行复刻 TS `readDelay`）。
///
/// d 下界钳到 0 后落入 [0,1) 退化读取区时 `idx0 = pos`——读到的是整环回绕前值
/// （规格 §4.2 行为事实），两支线必须逐字保持该语义。
fn read_delay(buf: &[f32], pos: usize, delay_samples: f64) -> f64 {
    let size = buf.len();
    let d = clamp(delay_samples, 0.0, (size - 1) as f64);
    let i0 = d.floor();
    let frac = d - i0;
    let i0 = i0 as usize;
    let idx0 = (pos + size - i0) % size;
    let idx1 = (idx0 + size - 1) % size;
    f64::from(buf[idx0]) * (1.0 - frac) + f64::from(buf[idx1]) * frac
}

/// TS `writeDelay(buf, pos, value)`：f64 → f32 写回（Float32Array 量化落点）。
#[inline]
fn write_delay(buf: &mut [f32], pos: usize, value: f64) {
    buf[pos] = value as f32;
}

/// 对齐 TS `DelaySettings`（src/types.ts）的参数快照（字段名蛇形转换）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DelaySettings {
    /// 仅由链路驱动器消费（[`ModEffectsStage`] 门控）；效果类自身不读取。
    pub enabled: bool,
    pub delay_ms: f64,
    pub feedback: f64,
    pub mix: f64,
}

/// 对齐 TS `ChorusSettings`。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChorusSettings {
    pub enabled: bool,
    pub rate_hz: f64,
    pub depth_ms: f64,
    pub mix: f64,
}

/// 对齐 TS `FlangerSettings`。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FlangerSettings {
    pub enabled: bool,
    pub rate_hz: f64,
    pub depth_ms: f64,
    pub feedback: f64,
    pub mix: f64,
}

/// 对齐 TS `PhaserSettings`。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PhaserSettings {
    pub enabled: bool,
    pub rate_hz: f64,
    pub depth: f64,
    pub feedback: f64,
    pub mix: f64,
    /// 全通级数（整数语义，经 round + [2, 8] 钳制；见 [`PhaserEffect::set_params`]）。
    pub stages: f64,
}

/// 对齐 TS `TremoloSettings`。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TremoloSettings {
    pub enabled: bool,
    pub rate_hz: f64,
    pub depth: f64,
    pub mix: f64,
}

/// 对齐 TS `ModEffectsSettings`：五个子对象全部完整给出（含 enabled）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModEffectsSettings {
    pub delay: DelaySettings,
    pub chorus: ChorusSettings,
    pub flanger: FlangerSettings,
    pub phaser: PhaserSettings,
    pub tremolo: TremoloSettings,
}

/// Delay：环形延迟线 + 反馈 + 干湿混合（逐行复刻 TS `DelayEffect`）。
#[derive(Debug, Clone)]
pub struct DelayEffect {
    fs: f64,
    buf_l: Vec<f32>,
    buf_r: Vec<f32>,
    pos: usize,
    delay_samples: f64,
    feedback: f64,
    mix: f64,
}

impl DelayEffect {
    /// 构造：缓冲 `ceil(fs×2)+1`（最大 2 s）。fs ≤ 0 或非有限时报错
    /// （对齐 TS `Error('invalid sample rate')`）。
    pub fn new(sample_rate: f64) -> Result<Self, String> {
        if !(sample_rate.is_finite() && sample_rate > 0.0) {
            return Err("invalid sample rate".into());
        }
        let max_delay = (sample_rate * 2.0).ceil() as usize + 1;
        Ok(Self {
            fs: sample_rate,
            buf_l: vec![0.0; max_delay],
            buf_r: vec![0.0; max_delay],
            pos: 0,
            delay_samples: 0.0,
            feedback: 0.3,
            mix: 0.3,
        })
    }

    /// 对齐 TS `setParams`：ms→样本换算在 setParams 一次性完成（处理循环内常量）。
    pub fn set_params(&mut self, p: DelaySettings) {
        self.delay_samples = (clamp(p.delay_ms, 0.0, 2000.0) / 1000.0) * self.fs;
        self.feedback = clamp(p.feedback, 0.0, 0.98);
        self.mix = clamp(p.mix, 0.0, 1.0);
    }

    /// 就地处理（对齐 TS `processStereo` 循环体：读 wet → 写反馈 → 干湿混合 →
    /// pos 前进；纯逐样本递推，分块与整块逐位一致）。
    pub fn process_stereo(&mut self, left: &mut [f32], right: &mut [f32]) {
        let n = left.len();
        let size = self.buf_l.len();
        let d = self.delay_samples;
        let fb = self.feedback;
        let mix = self.mix;
        let mut pos = self.pos;
        for i in 0..n {
            let xl = f64::from(left[i]);
            let xr = f64::from(right[i]);
            let wet_l = read_delay(&self.buf_l, pos, d);
            let wet_r = read_delay(&self.buf_r, pos, d);
            write_delay(&mut self.buf_l, pos, xl + wet_l * fb);
            write_delay(&mut self.buf_r, pos, xr + wet_r * fb);
            left[i] = (xl * (1.0 - mix) + wet_l * mix) as f32;
            right[i] = (xr * (1.0 - mix) + wet_r * mix) as f32;
            pos = (pos + 1) % size;
        }
        self.pos = pos;
    }

    /// 清零缓冲与 pos（对齐 TS `reset`；参数保留）。
    pub fn reset(&mut self) {
        self.buf_l.fill(0.0);
        self.buf_r.fill(0.0);
        self.pos = 0;
    }
}

/// Chorus / Flanger 共享的调制延迟核（逐行复刻 TS `ModulatedDelay`）。
#[derive(Debug, Clone)]
struct ModulatedDelayCore {
    fs: f64,
    buf_l: Vec<f32>,
    buf_r: Vec<f32>,
    pos: usize,
    phase: f64,
    base_delay: f64,
    depth_samples: f64,
    rate_hz: f64,
}

impl ModulatedDelayCore {
    /// 缓冲 `ceil(fs × max_delay_sec) + 2`（Chorus 0.1 s、Flanger 0.05 s）。
    fn new(sample_rate: f64, max_delay_sec: f64) -> Result<Self, String> {
        if !(sample_rate.is_finite() && sample_rate > 0.0) {
            return Err("invalid sample rate".into());
        }
        let len = (sample_rate * max_delay_sec).ceil() as usize + 2;
        Ok(Self {
            fs: sample_rate,
            buf_l: vec![0.0; len],
            buf_r: vec![0.0; len],
            pos: 0,
            phase: 0.0,
            base_delay: 0.0,
            depth_samples: 0.0,
            rate_hz: 1.0,
        })
    }

    /// 对齐 TS `setCommon(baseMs, depthMs, rateHz)`。
    fn set_common(&mut self, base_ms: f64, depth_ms: f64, rate_hz: f64) {
        self.base_delay = (clamp(base_ms, 0.0, 100.0) / 1000.0) * self.fs;
        self.depth_samples = (clamp(depth_ms, 0.0, 50.0) / 1000.0) * self.fs;
        self.rate_hz = clamp(rate_hz, 0.01, 20.0);
    }

    /// 对齐 TS `advance(n)`：LFO 相位按**整块**步进（块内调制量常量——输出依赖
    /// 驱动分块 blockSize 的行为事实来源）。
    fn advance(&mut self, n: usize) {
        self.phase = (self.phase + (self.rate_hz * n as f64) / self.fs) % 1.0;
    }

    /// 对齐 TS `processCore(l, r, feedback, mix)`。
    fn process_core(&mut self, left: &mut [f32], right: &mut [f32], feedback: f64, mix: f64) {
        let n = left.len();
        let size = self.buf_l.len();
        let mut pos = self.pos;
        // TS 循环内 lfoValue() 读取的 phase 在块内不变（advance 仅在块末调用），
        // 因此调制量为本块常量——块外计算一次与 TS 逐样本重算逐位一致。
        let lfo = (2.0 * std::f64::consts::PI * self.phase).sin();
        let d = self.base_delay + self.depth_samples * lfo;
        for i in 0..n {
            let xl = f64::from(left[i]);
            let xr = f64::from(right[i]);
            let wet_l = read_delay(&self.buf_l, pos, d);
            let wet_r = read_delay(&self.buf_r, pos, d);
            write_delay(&mut self.buf_l, pos, xl + wet_l * feedback);
            write_delay(&mut self.buf_r, pos, xr + wet_r * feedback);
            left[i] = (xl * (1.0 - mix) + wet_l * mix) as f32;
            right[i] = (xr * (1.0 - mix) + wet_r * mix) as f32;
            pos = (pos + 1) % size;
        }
        self.pos = pos;
        self.advance(n);
    }

    fn reset(&mut self) {
        self.buf_l.fill(0.0);
        self.buf_r.fill(0.0);
        self.pos = 0;
        self.phase = 0.0;
    }
}

/// Chorus：LFO 调制分数延迟（基础延迟固定 20 ms、反馈恒 0；逐行复刻 TS
/// `ChorusEffect`）。
#[derive(Debug, Clone)]
pub struct ChorusEffect {
    core: ModulatedDelayCore,
    mix: f64,
}

impl ChorusEffect {
    /// 构造：缓冲 `ceil(fs×0.1)+2`。
    pub fn new(sample_rate: f64) -> Result<Self, String> {
        Ok(Self { core: ModulatedDelayCore::new(sample_rate, 0.1)?, mix: 0.4 })
    }

    /// 对齐 TS `setParams`：`setCommon(20, depthMs, rateHz)` + mix 钳制。
    pub fn set_params(&mut self, p: ChorusSettings) {
        self.core.set_common(20.0, p.depth_ms, p.rate_hz);
        self.mix = clamp(p.mix, 0.0, 1.0);
    }

    pub fn process_stereo(&mut self, left: &mut [f32], right: &mut [f32]) {
        self.core.process_core(left, right, 0.0, self.mix);
    }

    pub fn reset(&mut self) {
        self.core.reset();
    }
}

/// Flanger：LFO 调制分数延迟（基础延迟固定 1 ms、反馈可配；逐行复刻 TS
/// `FlangerEffect`）。
#[derive(Debug, Clone)]
pub struct FlangerEffect {
    core: ModulatedDelayCore,
    feedback: f64,
    mix: f64,
}

impl FlangerEffect {
    /// 构造：缓冲 `ceil(fs×0.05)+2`。
    pub fn new(sample_rate: f64) -> Result<Self, String> {
        Ok(Self { core: ModulatedDelayCore::new(sample_rate, 0.05)?, feedback: 0.4, mix: 0.5 })
    }

    /// 对齐 TS `setParams`：`setCommon(1, depthMs, rateHz)` + feedback/mix 钳制。
    pub fn set_params(&mut self, p: FlangerSettings) {
        self.core.set_common(1.0, p.depth_ms, p.rate_hz);
        self.feedback = clamp(p.feedback, 0.0, 0.98);
        self.mix = clamp(p.mix, 0.0, 1.0);
    }

    pub fn process_stereo(&mut self, left: &mut [f32], right: &mut [f32]) {
        self.core.process_core(left, right, self.feedback, self.mix);
    }

    pub fn reset(&mut self) {
        self.core.reset();
    }
}

/// Phaser：多级一阶全通（并级）+ LFO 调制中心频率 + 输出反馈（逐行复刻 TS
/// `PhaserEffect`）。
#[derive(Debug, Clone)]
pub struct PhaserEffect {
    fs: f64,
    rate_hz: f64,
    depth: f64,
    feedback: f64,
    mix: f64,
    /// 生效级数 ∈ [2, 8]（钳制后整数）。
    stages: usize,
    phase: f64,
    /// 每通道每级状态 (x1, y1)——f32 落点（对齐 TS `Float32Array(8×2)`）。
    state_l: Vec<f32>,
    state_r: Vec<f32>,
    /// 上一样本 mix 前的全通末级输出（f64，逐声道单样本反馈基准）。
    last_out_l: f64,
    last_out_r: f64,
}

/// 一阶全通单样本递推（逐行复刻 TS `allpass`；状态 f32 写回）。
#[inline]
fn allpass_tick(state: &mut [f32], base: usize, x: f64, a: f64) -> f64 {
    let x1 = f64::from(state[base]);
    let y1 = f64::from(state[base + 1]);
    let y = -a * x + x1 + a * y1;
    state[base] = x as f32;
    state[base + 1] = y as f32;
    y
}

impl PhaserEffect {
    /// 构造：每通道 8×2 状态槽（TS 固定容量，与钳制后 stages 上界一致）。
    pub fn new(sample_rate: f64) -> Result<Self, String> {
        if !(sample_rate.is_finite() && sample_rate > 0.0) {
            return Err("invalid sample rate".into());
        }
        Ok(Self {
            fs: sample_rate,
            rate_hz: 0.5,
            depth: 0.5,
            feedback: 0.4,
            mix: 0.5,
            stages: 4,
            phase: 0.0,
            state_l: vec![0.0; 8 * 2],
            state_r: vec![0.0; 8 * 2],
            last_out_l: 0.0,
            last_out_r: 0.0,
        })
    }

    /// 对齐 TS `setParams`：`stages = max(2, min(8, round(stages)))`。
    pub fn set_params(&mut self, p: PhaserSettings) {
        self.rate_hz = clamp(p.rate_hz, 0.01, 20.0);
        self.depth = clamp(p.depth, 0.0, 1.0);
        self.feedback = clamp(p.feedback, 0.0, 0.98);
        self.mix = clamp(p.mix, 0.0, 1.0);
        self.stages = js_max(2.0, js_min(8.0, js_round(p.stages))) as usize;
    }

    /// 就地处理（对齐 TS `processStereo` 循环体）。
    pub fn process_stereo(&mut self, left: &mut [f32], right: &mut [f32]) {
        let n = left.len();
        let stages = self.stages;
        let fs = self.fs;
        let depth = self.depth;
        let fb = self.feedback;
        let mix = self.mix;
        let rate_hz = self.rate_hz;
        let mut phase = self.phase;
        let mut last_out_l = self.last_out_l;
        let mut last_out_r = self.last_out_r;
        for i in 0..n {
            let xl = f64::from(left[i]);
            let xr = f64::from(right[i]);
            // LFO 调制中心频率 200..2000Hz（逐样本步进）。
            let lfo = 0.5 + 0.5 * (2.0 * std::f64::consts::PI * phase).sin();
            let fc = 200.0 + 1800.0 * (0.2 + 0.8 * lfo * depth);
            // TS 源码对同一实参调用两次 Math.tan（同值）；此处计算一次逐位等效。
            let t = (std::f64::consts::PI * fc / fs).tan();
            let a = (1.0 - t) / (1.0 + t);

            // 简单反馈：用上一个样本的（mix 前）末级全通输出叠加，逐声道独立。
            let in_l = xl + fb * last_out_l;
            let in_r = xr + fb * last_out_r;
            let mut yl = in_l;
            let mut yr = in_r;
            for s in 0..stages {
                let base = s * 2;
                // 并级结构：各级全通并行处理同一输入 in（非级联），仅末级输出
                // 被采用——循环体逐行保持 TS 的 yl 先、yr 后求值顺序。
                yl = allpass_tick(&mut self.state_l, base, in_l, a);
                yr = allpass_tick(&mut self.state_r, base, in_r, a);
            }
            last_out_l = yl;
            last_out_r = yr;
            left[i] = (xl * (1.0 - mix) + yl * mix) as f32;
            right[i] = (xr * (1.0 - mix) + yr * mix) as f32;
            phase = (phase + rate_hz / fs) % 1.0;
        }
        self.phase = phase;
        self.last_out_l = last_out_l;
        self.last_out_r = last_out_r;
    }

    /// 清零全通状态、相位与反馈记忆（对齐 TS `reset`；参数保留）。
    pub fn reset(&mut self) {
        self.state_l.fill(0.0);
        self.state_r.fill(0.0);
        self.phase = 0.0;
        self.last_out_l = 0.0;
        self.last_out_r = 0.0;
    }
}

/// Tremolo：逐样本 LFO 幅度调制（逐行复刻 TS `TremoloEffect`）。
#[derive(Debug, Clone)]
pub struct TremoloEffect {
    fs: f64,
    rate_hz: f64,
    depth: f64,
    mix: f64,
    phase: f64,
}

impl TremoloEffect {
    pub fn new(sample_rate: f64) -> Result<Self, String> {
        if !(sample_rate.is_finite() && sample_rate > 0.0) {
            return Err("invalid sample rate".into());
        }
        Ok(Self { fs: sample_rate, rate_hz: 5.0, depth: 0.5, mix: 1.0, phase: 0.0 })
    }

    /// 对齐 TS `setParams`（rateHz 上界 30 与 chorus/flanger 的 20 不同）。
    pub fn set_params(&mut self, p: TremoloSettings) {
        self.rate_hz = clamp(p.rate_hz, 0.01, 30.0);
        self.depth = clamp(p.depth, 0.0, 1.0);
        self.mix = clamp(p.mix, 0.0, 1.0);
    }

    /// 就地处理（对齐 TS `processStereo`：`mix=0` 时乘数精确 1.0 → 逐位恒等）。
    pub fn process_stereo(&mut self, left: &mut [f32], right: &mut [f32]) {
        let n = left.len();
        let fs = self.fs;
        let depth = self.depth;
        let mix = self.mix;
        let rate_hz = self.rate_hz;
        let mut phase = self.phase;
        for i in 0..n {
            let g = 1.0 - depth * (0.5 + 0.5 * (2.0 * std::f64::consts::PI * phase).sin());
            let wet = g;
            left[i] = (f64::from(left[i]) * (1.0 - mix + mix * wet)) as f32;
            right[i] = (f64::from(right[i]) * (1.0 - mix + mix * wet)) as f32;
            phase = (phase + rate_hz / fs) % 1.0;
        }
        self.phase = phase;
    }

    /// 清零相位（对齐 TS `reset`）。
    pub fn reset(&mut self) {
        self.phase = 0.0;
    }
}

/// mod-effects 向量模块的链路驱动器：五效果按引擎接线顺序级联 + enabled 门控
/// （规格 §4.1）。禁用级跳过但其 `set_params` 仍被无条件调用（引擎接线语义）。
#[derive(Debug, Clone)]
pub struct ModEffectsStage {
    delay_enabled: bool,
    chorus_enabled: bool,
    flanger_enabled: bool,
    phaser_enabled: bool,
    tremolo_enabled: bool,
    delay: DelayEffect,
    chorus: ChorusEffect,
    flanger: FlangerEffect,
    phaser: PhaserEffect,
    tremolo: TremoloEffect,
}

impl ModEffectsStage {
    /// 以显式参数快照构造（对齐向量驱动器语义：五个效果全新零状态实例 →
    /// 五级无条件 setParams → 仅 enabled 的级参与链路；与首次 enable 等价）。
    ///
    /// fs ≤ 0 或非有限时报错（对齐 TS 各效果构造 `Error('invalid sample rate')`）。
    pub fn from_settings(sample_rate: f64, settings: ModEffectsSettings) -> Result<Self, String> {
        let mut stage = Self {
            delay_enabled: false,
            chorus_enabled: false,
            flanger_enabled: false,
            phaser_enabled: false,
            tremolo_enabled: false,
            delay: DelayEffect::new(sample_rate)?,
            chorus: ChorusEffect::new(sample_rate)?,
            flanger: FlangerEffect::new(sample_rate)?,
            phaser: PhaserEffect::new(sample_rate)?,
            tremolo: TremoloEffect::new(sample_rate)?,
        };
        stage.configure(settings);
        Ok(stage)
    }

    /// 覆盖参数快照：五级**无条件** setParams（enabled 字段被效果类自身忽略，
    /// 仅作为驱动器门控标志；规格 §4.1 引擎接线事实）。
    pub fn configure(&mut self, settings: ModEffectsSettings) {
        self.delay_enabled = settings.delay.enabled;
        self.chorus_enabled = settings.chorus.enabled;
        self.flanger_enabled = settings.flanger.enabled;
        self.phaser_enabled = settings.phaser.enabled;
        self.tremolo_enabled = settings.tremolo.enabled;
        self.delay.set_params(settings.delay);
        self.chorus.set_params(settings.chorus);
        self.flanger.set_params(settings.flanger);
        self.phaser.set_params(settings.phaser);
        self.tremolo.set_params(settings.tremolo);
    }
}

impl Stage for ModEffectsStage {
    /// 各效果缓冲为构造期定容分配，无需按块长预分配（保留形参以符合 Stage 契约）。
    fn prepare(&mut self, _max_block_size: usize) {}

    /// 就地处理一个立体声块：级联顺序 = 引擎接线顺序（固定）
    /// Delay → Chorus → Flanger → Phaser → Tremolo；禁用级整级跳过（逐位旁路、
    /// 状态不推进）。
    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        debug_assert_eq!(left.len(), right.len(), "左右声道块长必须一致（Stage 契约）");
        if self.delay_enabled {
            self.delay.process_stereo(left, right);
        }
        if self.chorus_enabled {
            self.chorus.process_stereo(left, right);
        }
        if self.flanger_enabled {
            self.flanger.process_stereo(left, right);
        }
        if self.phaser_enabled {
            self.phaser.process_stereo(left, right);
        }
        if self.tremolo_enabled {
            self.tremolo.process_stereo(left, right);
        }
    }

    /// 复位全部五级状态（缓冲/相位/全通状态/反馈记忆；参数保留）。
    fn reset(&mut self) {
        self.delay.reset();
        self.chorus.reset();
        self.flanger.reset();
        self.phaser.reset();
        self.tremolo.reset();
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

    /// 正弦（f64 域计算后写 f32，对齐 TS 浮点行为）。
    fn sine(n: usize, freq: f64, fs: f64, amp: f64, phase: f64) -> Vec<f32> {
        (0..n)
            .map(|i| {
                (2.0 * std::f64::consts::PI * freq * i as f64 / fs + phase).sin() * amp
            })
            .map(|v| v as f32)
            .collect()
    }

    /// 按块长调度驱动（复制输入 → 逐块就地处理），对齐导出工具 l.slice() 语义。
    fn drive_sched<F>(mut step: F, in_l: &[f32], in_r: &[f32], blocks: &[usize]) -> (Vec<f32>, Vec<f32>)
    where
        F: FnMut(&mut [f32], &mut [f32]),
    {
        let mut out_l = in_l.to_vec();
        let mut out_r = in_r.to_vec();
        let mut off = 0_usize;
        for &len in blocks {
            step(&mut out_l[off..off + len], &mut out_r[off..off + len]);
            off += len;
        }
        assert_eq!(off, in_l.len(), "块长调度必须覆盖全部帧");
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

    /// 黄金参考：node 直跑仓库根 src/dsp/ModEffects.ts 导出（JSON 最短往返表示）。
    mod golden {
        /// delay 小数延迟（0.05ms→2.4 样本、反馈 0.55、mix 0.4）：16 帧、单块。
        pub const ME_D1_OUT_L: [f64; 16] = [
            0.30000001192092896, -0.15000000596046448, 0.5699999928474426,
            -0.5799999833106995, 0.25460001826286316, -0.6119999885559082,
            -0.028832003474235535, 0.12080199271440506, -0.466029554605484,
            0.20627161860466003, -0.5842133164405823, 0.5620431303977966,
            -0.24691063165664673, 0.6029472947120667, 0.02741897664964199,
            0.4029022753238678,
        ];
        pub const ME_D1_OUT_R: [f64; 16] = [
            -0.18863585591316223, 0.16710172593593597, -0.26044026017189026,
            0.2706608772277832, 0.1014804020524025, -0.05936231091618538,
            0.11501599103212357, -0.1980540007352829, -0.30287644267082214,
            0.21154627203941345, -0.4884592890739441, -0.23692640662193298,
            -0.12550051510334015, -0.1134779155254364, -0.35816681385040283,
            0.19071073830127716,
        ];
        /// case2 形态 delay（40ms/0.55/0.4）：1024 帧 LCG、块 [333,333,333,25]、
        /// 取尾段 24 帧（反馈回声族衰减尾）。
        pub const ME_D2_OUT_L: [f64; 24] = [
            0.4103703498840332, 0.26343536376953125, -0.40743938088417053,
            0.16802529990673065, -0.0015768613666296005, 0.14158298075199127,
            0.1590256541967392, -0.32872459292411804, 0.15775945782661438,
            0.21154583990573883, 0.38452088832855225, 0.3762860596179962,
            0.19551299512386322, 0.33657002449035645, 0.3478469252586365,
            0.14328519999980927, -0.3796895742416382, -0.017565421760082245,
            -0.09864216297864914, -0.44317924976348877, -0.47743576765060425,
            -0.18873992562294006, -0.19952717423439026, 0.019413091242313385,
        ];
        pub const ME_D2_OUT_R: [f64; 24] = [
            0.1892370581626892, 0.05661116540431976, 0.14628104865550995,
            -0.23843352496623993, 0.018482087180018425, -0.09427079558372498,
            0.15674032270908356, -0.13203175365924835, -0.07235574722290039,
            0.2263646125793457, 0.07096786051988602, -0.30002927780151367,
            0.32839930057525635, -0.02154366672039032, -0.20082467794418335,
            -0.24159836769104004, 0.3014979660511017, 0.28663715720176697,
            -0.3400610387325287, 0.28829681873321533, 0.30348965525627136,
            -0.08485899865627289, -0.1485065519809723, -0.12677860260009766,
        ];
        /// chorus（case3 形态 4Hz/5ms/0.5）：1024 帧 LCG、块 [333,333,333,25]、尾段 24。
        pub const ME_C_OUT_L: [f64; 24] = [
            -0.23837582767009735, 0.04457053542137146, 0.13746382296085358,
            0.3389595150947571, -0.2570742964744568, 0.04772516340017319,
            -0.3055547773838043, -0.20623546838760376, 0.28397253155708313,
            -0.33697471022605896, 0.3495441675186157, -0.12210501730442047,
            -0.29321542382240295, 0.26345980167388916, 0.29786840081214905,
            0.27278295159339905, -0.07585795223712921, 0.11303585022687912,
            0.26249003410339355, 0.30484312772750854, -0.3076128363609314,
            -0.09367643296718597, -0.20084857940673828, -0.11522676050662994,
        ];
        pub const ME_C_OUT_R: [f64; 24] = [
            0.2474116086959839, -0.0660565048456192, -0.0880194827914238,
            -0.01097807940095663, -0.16996844112873077, -0.1051175594329834,
            -0.19223183393478394, -0.06778904050588608, 0.07042410969734192,
            -0.19367869198322296, 0.0986863449215889, 0.008446132764220238,
            -0.08321420103311539, -0.0020721983164548874, -0.10786870867013931,
            -0.04755937308073044, -0.14705567061901093, -0.2110578715801239,
            0.0027155079878866673, 0.14877374470233917, 0.23969772458076477,
            -0.02753954380750656, -0.14267241954803467, -0.1804565191268921,
        ];
        /// flanger（case3 形态 2.5Hz/4ms/0.6/0.5，负半周进入 d<1 退化读取区）：
        /// 同上驱动，尾段 24。
        pub const ME_F_OUT_L: [f64; 24] = [
            0.292854905128479, -0.48447492718696594, 0.16558320820331573,
            0.1449132114648819, -0.15623921155929565, -0.3886083662509918,
            -0.14093433320522308, 0.1684814691543579, 0.11484427005052567,
            -0.02683592215180397, -0.04478192329406738, 0.1803666204214096,
            -0.0141361178830266, -0.3441191613674164, 0.3053171932697296,
            0.602102518081665, -0.24859002232551575, 0.11380144953727722,
            -0.010258194990456104, 0.17416276037693024, 0.3343742787837982,
            0.0015066860942170024, -0.4242366850376129, 0.04591110348701477,
        ];
        pub const ME_F_OUT_R: [f64; 24] = [
            0.18415868282318115, 0.3031318187713623, 0.27898964285850525,
            0.12546387314796448, -0.231533482670784, 0.11266771703958511,
            -0.13082940876483917, -0.12806947529315948, -0.3459152281284332,
            -0.16608625650405884, -0.2227737456560135, 0.2478945404291153,
            0.12198732048273087, -0.15030014514923096, -0.5117760300636292,
            -0.15288002789020538, 0.024406548589468002, 0.21658381819725037,
            0.1736924648284912, -0.033406611531972885, 0.09547226876020432,
            0.06197492405772209, -0.1379055231809616, -0.03811086341738701,
        ];
        /// phaser（case4 形态 @44100 1.5Hz/0.8/0.5/0.5/stages=6）：1024 帧 LCG、
        /// 块 [480,480,64]、尾段 24。
        pub const ME_P_OUT_L: [f64; 24] = [
            0.046784114092588425, 0.11452823877334595, 0.15141203999519348,
            -0.044229064136743546, 0.31744933128356934, -0.16085132956504822,
            0.13695305585861206, -0.06098278984427452, 0.3022209107875824,
            0.10521163791418076, 0.0645601749420166, 0.18796363472938538,
            -0.20536772906780243, -0.03220929950475693, -0.04849185049533844,
            0.009816343896090984, 0.22956952452659607, -0.016290323808789253,
            -0.026494348421692848, -0.014234787784516811, -0.0031954117584973574,
            0.14941464364528656, 0.019616853445768356, -0.08914408087730408,
        ];
        pub const ME_P_OUT_R: [f64; 24] = [
            0.01237054355442524, -0.024365903809666634, 0.08919791877269745,
            0.09738210588693619, -0.03094521351158619, -0.04646267369389534,
            -0.08607671409845352, -0.07074613869190216, -0.16722755134105682,
            0.04248812794685364, -0.09039793163537979, -0.03473351523280144,
            -0.1334325671195984, -0.025126686319708824, 0.11805597692728043,
            -0.020716166123747826, -0.01749025098979473, 0.00023678669822402298,
            0.03250772878527641, -0.20590901374816895, -0.012766147963702679,
            0.02182750776410103, 0.016694771125912666, -0.1404484212398529,
        ];
        /// tremolo（case4 形态 @44100 8Hz/0.7/mix1）：16 帧、单块。
        pub const ME_T_OUT_L: [f64; 16] = [
            0.0, 0.022205831483006477, 0.044343847781419754, 0.06637369096279144,
            0.08825524151325226, 0.10994870215654373, 0.13141466677188873,
            0.15261420607566833, 0.1735088974237442, 0.19406098127365112,
            0.21423329412937164, 0.2339894324541092, 0.25329387187957764,
            0.27211183309555054, 0.2904095947742462, 0.3081543445587158,
        ];
        pub const ME_T_OUT_R: [f64; 16] = [
            -0.16928863525390625, -0.2631528675556183, 0.3128558397293091,
            -0.3237435817718506, -0.07633424550294876, -0.09641976654529572,
            -0.1793486326932907, 0.2494417130947113, -0.2676401436328888,
            -0.2593176066875458, 0.24564968049526215, 0.25880974531173706,
            -0.060920942574739456, -0.03128983452916145, -0.2824540436267853,
            0.13499081134796143,
        ];
        /// chorus+flanger 级联（case3 形态、块 [333,179]）：512 帧 LCG、尾段 16。
        pub const ME_CASCADE_OUT_L: [f64; 16] = [
            0.08105660229921341, -0.01264991145581007, 0.07429368048906326,
            -0.1516377031803131, -0.03544649854302406, 0.032064467668533325,
            0.05409117043018341, -0.061065223067998886, 0.2497253119945526,
            0.3567200303077698, 0.13128510117530823, -0.16145102679729462,
            -0.09197580814361572, -0.22368156909942627, 0.08032740652561188,
            -0.11385971307754517,
        ];
        pub const ME_CASCADE_OUT_R: [f64; 16] = [
            -0.02782311849296093, -0.01807199791073799, 0.1790475845336914,
            0.20364119112491608, 0.15838900208473206, 0.19480469822883606,
            -0.0810767412185669, -0.013769886456429958, 0.10638542473316193,
            0.08991134166717529, 0.052626654505729675, -0.07515635341405869,
            -0.08289845287799835, -0.04437994956970215, 0.07895573228597641,
            -0.02068980410695076,
        ];
    }

    #[test]
    fn delay_小数延迟插值_命中ts黄金参考() {
        // 0.05ms → 2.4 样本：分数插值 + 反馈写入路径整段冻结。
        let fs = 48000.0;
        let s = DelaySettings { enabled: true, delay_ms: 0.05, feedback: 0.55, mix: 0.4 };
        let mut fx = DelayEffect::new(fs).expect("合法参数");
        fx.set_params(s);
        let in_l = vec![0.5_f32, -0.25, 0.75, -1.0, 0.125, -0.875, 0.0625, 0.5, -0.5, 0.25, -0.75, 1.0, -0.125, 0.875, -0.0625, 0.375];
        let in_r = lcg_noise(16, 5, 0.6);
        let (out_l, out_r) = drive_sched(|l, r| fx.process_stereo(l, r), &in_l, &in_r, &[16]);
        for (i, want) in golden::ME_D1_OUT_L.iter().enumerate() {
            assert_f32_close(out_l[i], *want, &format!("ME-D1 L[{i}]"));
        }
        for (i, want) in golden::ME_D1_OUT_R.iter().enumerate() {
            assert_f32_close(out_r[i], *want, &format!("ME-D1 R[{i}]"));
        }
    }

    #[test]
    fn delay_反馈回声衰减尾_命中ts黄金参考_case2形态() {
        let fs = 48000.0;
        let s = DelaySettings { enabled: true, delay_ms: 40.0, feedback: 0.55, mix: 0.4 };
        let n = 1024;
        let in_l = lcg_noise(n, 91, 0.8);
        let in_r = lcg_noise(n, 92, 0.6);
        let mut fx = DelayEffect::new(fs).expect("合法参数");
        fx.set_params(s);
        let (out_l, out_r) = drive_sched(|l, r| fx.process_stereo(l, r), &in_l, &in_r, &[333, 333, 333, 25]);
        for (k, want) in golden::ME_D2_OUT_L.iter().enumerate() {
            assert_f32_close(out_l[1000 + k], *want, &format!("ME-D2 L[{}]", 1000 + k));
        }
        for (k, want) in golden::ME_D2_OUT_R.iter().enumerate() {
            assert_f32_close(out_r[1000 + k], *want, &format!("ME-D2 R[{}]", 1000 + k));
        }
    }

    #[test]
    fn chorus_块级步进lfo_命中ts黄金参考_且输出依赖块长() {
        // GWT-ME-03/07：LFO 相位按整块步进 → 同参数不同块长输出不同。
        let fs = 48000.0;
        let s = ChorusSettings { enabled: true, rate_hz: 4.0, depth_ms: 5.0, mix: 0.5 };
        let n = 1024;
        let in_l = lcg_noise(n, 41, 0.7);
        let in_r = lcg_noise(n, 42, 0.5);
        let mut fx = ChorusEffect::new(fs).expect("合法参数");
        fx.set_params(s);
        let (out_l, out_r) = drive_sched(|l, r| fx.process_stereo(l, r), &in_l, &in_r, &[333, 333, 333, 25]);
        for (k, want) in golden::ME_C_OUT_L.iter().enumerate() {
            assert_f32_close(out_l[1000 + k], *want, &format!("ME-C L[{}]", 1000 + k));
        }
        for (k, want) in golden::ME_C_OUT_R.iter().enumerate() {
            assert_f32_close(out_r[1000 + k], *want, &format!("ME-C R[{}]", 1000 + k));
        }
        // 整块重放：输出必须不同（若相同说明 LFO 被误改为逐样本步进）。
        let mut whole = ChorusEffect::new(fs).expect("合法参数");
        whole.set_params(s);
        let (w_l, _) = drive_sched(|l, r| whole.process_stereo(l, r), &in_l, &in_r, &[n]);
        assert!(w_l != out_l, "规格 §4.4：chorus 输出必须依赖驱动分块 blockSize");
    }

    #[test]
    fn flanger_负半周进入d小于1退化读取区_命中ts黄金参考() {
        // GWT-ME-03：基础延迟 1ms − depth 4ms < 0 → readDelay 下界钳制 +
        // d<1 区读整环回绕前值；输出连续、有限。
        let fs = 48000.0;
        let s = FlangerSettings { enabled: true, rate_hz: 2.5, depth_ms: 4.0, feedback: 0.6, mix: 0.5 };
        let n = 1024;
        let in_l = lcg_noise(n, 43, 0.7);
        let in_r = lcg_noise(n, 44, 0.5);
        let mut fx = FlangerEffect::new(fs).expect("合法参数");
        fx.set_params(s);
        let (out_l, out_r) = drive_sched(|l, r| fx.process_stereo(l, r), &in_l, &in_r, &[333, 333, 333, 25]);
        for (k, want) in golden::ME_F_OUT_L.iter().enumerate() {
            assert_f32_close(out_l[1000 + k], *want, &format!("ME-F L[{}]", 1000 + k));
        }
        for (k, want) in golden::ME_F_OUT_R.iter().enumerate() {
            assert_f32_close(out_r[1000 + k], *want, &format!("ME-F R[{}]", 1000 + k));
        }
        assert!(out_l.iter().all(|v| v.is_finite()) && out_r.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn phaser_并级全通stages6_命中ts黄金参考_且分块逐位一致() {
        // GWT-ME-04：并级结构（各级处理同一输入、仅末级被采用）+ 逐样本 LFO
        //（分块与整块逐位一致，规格 §4.5）。
        let fs = 44100.0;
        let s = PhaserSettings { enabled: true, rate_hz: 1.5, depth: 0.8, feedback: 0.5, mix: 0.5, stages: 6.0 };
        let n = 1024;
        let in_l = lcg_noise(n, 45, 0.7);
        let in_r = lcg_noise(n, 46, 0.5);
        let mut fx = PhaserEffect::new(fs).expect("合法参数");
        fx.set_params(s);
        let (out_l, out_r) = drive_sched(|l, r| fx.process_stereo(l, r), &in_l, &in_r, &[480, 480, 64]);
        for (k, want) in golden::ME_P_OUT_L.iter().enumerate() {
            assert_f32_close(out_l[1000 + k], *want, &format!("ME-P L[{}]", 1000 + k));
        }
        for (k, want) in golden::ME_P_OUT_R.iter().enumerate() {
            assert_f32_close(out_r[1000 + k], *want, &format!("ME-P R[{}]", 1000 + k));
        }
        // 分块与整块逐位一致。
        let mut whole = PhaserEffect::new(fs).expect("合法参数");
        whole.set_params(s);
        let (w_l, w_r) = drive_sched(|l, r| whole.process_stereo(l, r), &in_l, &in_r, &[n]);
        assert_eq!(w_l, out_l, "phaser 为纯逐样本递推，分块不得改变输出（左）");
        assert_eq!(w_r, out_r, "phaser 为纯逐样本递推，分块不得改变输出（右）");
    }

    #[test]
    fn phaser_stages_7_与_8_输出逐位一致() {
        // 规格 §4.5 行为事实：并级结构下 stages=7 ≡ stages=8（末级状态槽独立
        // 演化、其余级输出被丢弃）。
        let fs = 44100.0;
        let base = PhaserSettings { enabled: true, rate_hz: 1.5, depth: 0.8, feedback: 0.5, mix: 0.5, stages: 6.0 };
        let n = 1024;
        let in_l = lcg_noise(n, 45, 0.7);
        let in_r = lcg_noise(n, 46, 0.5);
        let run = |stages: f64| {
            let mut fx = PhaserEffect::new(fs).expect("合法参数");
            fx.set_params(PhaserSettings { stages, ..base });
            drive_sched(|l, r| fx.process_stereo(l, r), &in_l, &in_r, &[n])
        };
        let (s7_l, s7_r) = run(7.0);
        let (s8_l, s8_r) = run(8.0);
        assert_eq!(s7_l, s8_l, "stages=7 与 8 必须逐位一致（左）");
        assert_eq!(s7_r, s8_r, "stages=7 与 8 必须逐位一致（右）");
    }

    #[test]
    fn tremolo_命中ts黄金参考_case4形态() {
        let fs = 44100.0;
        let s = TremoloSettings { enabled: true, rate_hz: 8.0, depth: 0.7, mix: 1.0 };
        let mut fx = TremoloEffect::new(fs).expect("合法参数");
        fx.set_params(s);
        let in_l = sine(16, 300.0, fs, 0.8, 0.0);
        let in_r = lcg_noise(16, 9, 0.5);
        let (out_l, out_r) = drive_sched(|l, r| fx.process_stereo(l, r), &in_l, &in_r, &[16]);
        for (i, want) in golden::ME_T_OUT_L.iter().enumerate() {
            assert_f32_close(out_l[i], *want, &format!("ME-T L[{i}]"));
        }
        for (i, want) in golden::ME_T_OUT_R.iter().enumerate() {
            assert_f32_close(out_r[i], *want, &format!("ME-T R[{i}]"));
        }
    }

    #[test]
    fn chorus_flanger_级联_命中ts黄金参考() {
        // 链路驱动器级联顺序（chorus → flanger）与逐块状态连续性。
        let fs = 48000.0;
        let settings = ModEffectsSettings {
            delay: DelaySettings { enabled: false, delay_ms: 300.0, feedback: 0.5, mix: 0.3 },
            chorus: ChorusSettings { enabled: true, rate_hz: 4.0, depth_ms: 5.0, mix: 0.5 },
            flanger: FlangerSettings { enabled: true, rate_hz: 2.5, depth_ms: 4.0, feedback: 0.6, mix: 0.5 },
            phaser: PhaserSettings { enabled: false, rate_hz: 0.5, depth: 0.5, feedback: 0.4, mix: 0.5, stages: 4.0 },
            tremolo: TremoloSettings { enabled: false, rate_hz: 5.0, depth: 0.5, mix: 1.0 },
        };
        let mut stage = ModEffectsStage::from_settings(fs, settings).expect("合法参数");
        stage.prepare(333);
        let n = 512;
        let in_l = lcg_noise(n, 51, 0.7);
        let in_r = lcg_noise(n, 52, 0.5);
        // 333 + 179（末块短块），状态跨块保持。
        let mut out_l = in_l.clone();
        let mut out_r = in_r.clone();
        stage.process(&mut out_l[..333], &mut out_r[..333]);
        stage.process(&mut out_l[333..], &mut out_r[333..]);
        for (k, want) in golden::ME_CASCADE_OUT_L.iter().enumerate() {
            assert_f32_close(out_l[496 + k], *want, &format!("级联 L[{}]", 496 + k));
        }
        for (k, want) in golden::ME_CASCADE_OUT_R.iter().enumerate() {
            assert_f32_close(out_r[496 + k], *want, &format!("级联 R[{}]", 496 + k));
        }
    }

    #[test]
    fn 全五级禁用_输出与输入逐位一致_状态不推进() {
        // GWT-ME-01：链路驱动器跳过语义的最强锚点；五级 setParams 仍被无条件调用。
        let fs = 48000.0;
        let settings = ModEffectsSettings {
            delay: DelaySettings { enabled: false, delay_ms: 1500.0, feedback: 0.9, mix: 0.8 },
            chorus: ChorusSettings { enabled: false, rate_hz: 15.0, depth_ms: 30.0, mix: 1.0 },
            flanger: FlangerSettings { enabled: false, rate_hz: 15.0, depth_ms: 30.0, feedback: 0.9, mix: 1.0 },
            phaser: PhaserSettings { enabled: false, rate_hz: 15.0, depth: 1.0, feedback: 0.9, mix: 1.0, stages: 9.0 },
            tremolo: TremoloSettings { enabled: false, rate_hz: 25.0, depth: 1.0, mix: 1.0 },
        };
        let mut stage = ModEffectsStage::from_settings(fs, settings).expect("合法参数");
        stage.prepare(97);
        let in_l = lcg_noise(400, 11, 0.9);
        let in_r = sine(400, 3300.0, fs, 0.6, 0.0);
        let mut out_l = in_l.clone();
        let mut out_r = in_r.clone();
        let mut off = 0;
        while off < 400 {
            let end = (off + 97).min(400);
            stage.process(&mut out_l[off..end], &mut out_r[off..end]);
            off = end;
        }
        assert_eq!(out_l, in_l, "全禁用必须逐位直通（左）");
        assert_eq!(out_r, in_r, "全禁用必须逐位直通（右）");
        // reset 后重放仍逐位一致（状态从未推进）。
        stage.reset();
        let mut again = in_l.clone();
        let mut again_r = in_r.clone();
        stage.process(&mut again, &mut again_r);
        assert_eq!(again, in_l);
        assert_eq!(again_r, in_r);
    }

    #[test]
    fn delay_phaser_tremolo_分块与整块逐位一致() {
        // GWT-ME-08：三者均为纯逐样本递推（含末块短块）。
        let n = 600;
        let in_l = lcg_noise(n, 21, 0.6);
        let in_r = sine(n, 900.0, 48000.0, 0.4, 0.7);
        let ds = DelaySettings { enabled: true, delay_ms: 3.0, feedback: 0.7, mix: 0.5 };
        let ps = PhaserSettings { enabled: true, rate_hz: 3.0, depth: 0.9, feedback: 0.8, mix: 0.6, stages: 2.0 };
        let ts = TremoloSettings { enabled: true, rate_hz: 12.0, depth: 0.9, mix: 0.8 };

        let run_delay = |block: usize| {
            let mut fx = DelayEffect::new(48000.0).expect("合法参数");
            fx.set_params(ds);
            drive_sched(|l, r| fx.process_stereo(l, r), &in_l, &in_r, &block_schedule(n, block))
        };
        let run_phaser = |block: usize| {
            let mut fx = PhaserEffect::new(44100.0).expect("合法参数");
            fx.set_params(ps);
            drive_sched(|l, r| fx.process_stereo(l, r), &in_l, &in_r, &block_schedule(n, block))
        };
        let run_tremolo = |block: usize| {
            let mut fx = TremoloEffect::new(48000.0).expect("合法参数");
            fx.set_params(ts);
            drive_sched(|l, r| fx.process_stereo(l, r), &in_l, &in_r, &block_schedule(n, block))
        };

        let (dw_l, dw_r) = run_delay(n);
        let (dc_l, dc_r) = run_delay(97);
        assert_eq!(dw_l, dc_l, "delay 分块与整块必须逐位一致（左）");
        assert_eq!(dw_r, dc_r);
        let (pw_l, pw_r) = run_phaser(n);
        let (pc_l, pc_r) = run_phaser(111);
        assert_eq!(pw_l, pc_l, "phaser 分块与整块必须逐位一致（左）");
        assert_eq!(pw_r, pc_r);
        let (tw_l, tw_r) = run_tremolo(n);
        let (tc_l, tc_r) = run_tremolo(83);
        assert_eq!(tw_l, tc_l, "tremolo 分块与整块必须逐位一致（左）");
        assert_eq!(tw_r, tc_r);
    }

    /// 生成覆盖 n 帧的块长调度（等长块 + 末块短块）。
    fn block_schedule(n: usize, block: usize) -> Vec<usize> {
        let mut blocks = Vec::new();
        let mut off = 0;
        while off < n {
            blocks.push((off + block).min(n) - off);
            off += block;
        }
        blocks
    }

    #[test]
    fn tremolo_mix_0_逐位恒等() {
        // GWT-ME-09：乘数精确 1.0。
        let fs = 48000.0;
        let s = TremoloSettings { enabled: true, rate_hz: 25.0, depth: 1.0, mix: 0.0 };
        let mut fx = TremoloEffect::new(fs).expect("合法参数");
        fx.set_params(s);
        let in_l = lcg_noise(256, 31, 0.9);
        let in_r = lcg_noise(256, 32, 0.9);
        let (out_l, out_r) = drive_sched(|l, r| fx.process_stereo(l, r), &in_l, &in_r, &[64, 64, 64, 64]);
        assert_eq!(out_l, in_l, "mix=0 必须逐位恒等（左）");
        assert_eq!(out_r, in_r, "mix=0 必须逐位恒等（右）");
    }

    #[test]
    fn 极值参数钳制与直接按生效值配置逐位等效() {
        // GWT-ME-05：delayMs/feedback/mix/rateHz/depth/stages 全维度钳制。
        let fs = 48000.0;
        let n = 256;
        let in_l = lcg_noise(n, 61, 0.8);
        let in_r = lcg_noise(n, 62, 0.6);
        // delay：delayMs 5000→2000、feedback 2→0.98、mix −1→0。
        let mut clamped = DelayEffect::new(fs).expect("合法参数");
        clamped.set_params(DelaySettings { enabled: true, delay_ms: 5000.0, feedback: 2.0, mix: -1.0 });
        let mut direct = DelayEffect::new(fs).expect("合法参数");
        direct.set_params(DelaySettings { enabled: true, delay_ms: 2000.0, feedback: 0.98, mix: 0.0 });
        let (a_l, _) = drive_sched(|l, r| clamped.process_stereo(l, r), &in_l, &in_r, &[64, 64, 64, 64]);
        let (b_l, _) = drive_sched(|l, r| direct.process_stereo(l, r), &in_l, &in_r, &[64, 64, 64, 64]);
        assert_eq!(a_l, b_l, "delay 钳制生效值必须与边界值直接配置逐位一致");
        // chorus：rateHz 100→20（上界 20）。
        let mut c1 = ChorusEffect::new(fs).expect("合法参数");
        c1.set_params(ChorusSettings { enabled: true, rate_hz: 100.0, depth_ms: 80.0, mix: 2.0 });
        let mut c2 = ChorusEffect::new(fs).expect("合法参数");
        c2.set_params(ChorusSettings { enabled: true, rate_hz: 20.0, depth_ms: 50.0, mix: 1.0 });
        let (a_l, _) = drive_sched(|l, r| c1.process_stereo(l, r), &in_l, &in_r, &[64, 64, 64, 64]);
        let (b_l, _) = drive_sched(|l, r| c2.process_stereo(l, r), &in_l, &in_r, &[64, 64, 64, 64]);
        assert_eq!(a_l, b_l, "chorus 钳制生效值必须与边界值直接配置逐位一致");
        // tremolo：rateHz 100→30（上界 30，与 chorus 的 20 不同）。
        let mut t1 = TremoloEffect::new(fs).expect("合法参数");
        t1.set_params(TremoloSettings { enabled: true, rate_hz: 100.0, depth: -3.0, mix: 7.0 });
        let mut t2 = TremoloEffect::new(fs).expect("合法参数");
        t2.set_params(TremoloSettings { enabled: true, rate_hz: 30.0, depth: 0.0, mix: 1.0 });
        let (a_l, _) = drive_sched(|l, r| t1.process_stereo(l, r), &in_l, &in_r, &[64, 64, 64, 64]);
        let (b_l, _) = drive_sched(|l, r| t2.process_stereo(l, r), &in_l, &in_r, &[64, 64, 64, 64]);
        assert_eq!(a_l, b_l, "tremolo 钳制生效值必须与边界值直接配置逐位一致");
        // phaser：stages 9→8、1→2（round + [2,8] 钳制）。
        let mut p1 = PhaserEffect::new(fs).expect("合法参数");
        p1.set_params(PhaserSettings { enabled: true, rate_hz: 0.001, depth: 5.0, feedback: 1.5, mix: -2.0, stages: 9.0 });
        let mut p2 = PhaserEffect::new(fs).expect("合法参数");
        p2.set_params(PhaserSettings { enabled: true, rate_hz: 0.01, depth: 1.0, feedback: 0.98, mix: 0.0, stages: 8.0 });
        let (a_l, _) = drive_sched(|l, r| p1.process_stereo(l, r), &in_l, &in_r, &[64, 64, 64, 64]);
        let (b_l, _) = drive_sched(|l, r| p2.process_stereo(l, r), &in_l, &in_r, &[64, 64, 64, 64]);
        assert_eq!(a_l, b_l, "phaser 钳制生效值必须与边界值直接配置逐位一致");
        let mut p3 = PhaserEffect::new(fs).expect("合法参数");
        p3.set_params(PhaserSettings { enabled: true, rate_hz: 0.01, depth: 1.0, feedback: 0.98, mix: 0.0, stages: 1.0 });
        let mut p4 = PhaserEffect::new(fs).expect("合法参数");
        p4.set_params(PhaserSettings { enabled: true, rate_hz: 0.01, depth: 1.0, feedback: 0.98, mix: 0.0, stages: 2.0 });
        let (a_l, _) = drive_sched(|l, r| p3.process_stereo(l, r), &in_l, &in_r, &[64, 64, 64, 64]);
        let (b_l, _) = drive_sched(|l, r| p4.process_stereo(l, r), &in_l, &in_r, &[64, 64, 64, 64]);
        assert_eq!(a_l, b_l, "stages 下界钳制必须与直接配置逐位一致");
    }

    #[test]
    fn 静音输入_输出逐位零_无数值泄漏() {
        // GWT-ME-10：Delay 反馈环、Phaser 反馈、各 LFO 相位均无泄漏。
        let fs = 48000.0;
        let settings = ModEffectsSettings {
            delay: DelaySettings { enabled: true, delay_ms: 20.0, feedback: 0.9, mix: 0.7 },
            chorus: ChorusSettings { enabled: true, rate_hz: 4.0, depth_ms: 5.0, mix: 0.5 },
            flanger: FlangerSettings { enabled: true, rate_hz: 2.5, depth_ms: 4.0, feedback: 0.6, mix: 0.5 },
            phaser: PhaserSettings { enabled: true, rate_hz: 1.5, depth: 0.8, feedback: 0.5, mix: 0.5, stages: 4.0 },
            tremolo: TremoloSettings { enabled: true, rate_hz: 8.0, depth: 0.7, mix: 1.0 },
        };
        let mut stage = ModEffectsStage::from_settings(fs, settings).expect("合法参数");
        stage.prepare(128);
        let n = 960;
        let (out_l, out_r) =
            drive_sched(|l, r| stage.process(l, r), &vec![0.0_f32; n], &vec![0.0_f32; n], &block_schedule(n, 128));
        assert!(out_l.iter().all(|&x| x.to_bits() == 0_u32), "静音输入必须逐位零输出（左）");
        assert!(out_r.iter().all(|&x| x.to_bits() == 0_u32), "静音输入必须逐位零输出（右）");
    }

    #[test]
    fn reset_后重放与首次从零状态逐位一致() {
        // GWT-ME-11：五效果各自 reset 语义。
        let fs = 48000.0;
        let n = 600;
        let in_l = lcg_noise(n, 71, 0.7);
        let in_r = lcg_noise(n, 72, 0.5);
        // 级联（chorus+flanger+phaser+tremolo）。
        let settings = ModEffectsSettings {
            delay: DelaySettings { enabled: false, delay_ms: 10.0, feedback: 0.5, mix: 0.3 },
            chorus: ChorusSettings { enabled: true, rate_hz: 4.0, depth_ms: 5.0, mix: 0.5 },
            flanger: FlangerSettings { enabled: true, rate_hz: 2.5, depth_ms: 4.0, feedback: 0.6, mix: 0.5 },
            phaser: PhaserSettings { enabled: true, rate_hz: 1.5, depth: 0.8, feedback: 0.5, mix: 0.5, stages: 6.0 },
            tremolo: TremoloSettings { enabled: true, rate_hz: 8.0, depth: 0.7, mix: 1.0 },
        };
        let mut stage = ModEffectsStage::from_settings(fs, settings).expect("合法参数");
        stage.prepare(97);
        let (first_l, first_r) = drive_sched(|l, r| stage.process(l, r), &in_l, &in_r, &block_schedule(n, 97));
        stage.reset();
        let (again_l, again_r) = drive_sched(|l, r| stage.process(l, r), &in_l, &in_r, &block_schedule(n, 97));
        assert_eq!(first_l, again_l, "reset 后重放必须逐位一致（左）");
        assert_eq!(first_r, again_r, "reset 后重放必须逐位一致（右）");
        // 单效果（delay）。
        let mut fx = DelayEffect::new(fs).expect("合法参数");
        fx.set_params(DelaySettings { enabled: true, delay_ms: 2.0, feedback: 0.6, mix: 0.5 });
        let (d1_l, _) = drive_sched(|l, r| fx.process_stereo(l, r), &in_l, &in_r, &block_schedule(n, 97));
        fx.reset();
        let (d2_l, _) = drive_sched(|l, r| fx.process_stereo(l, r), &in_l, &in_r, &block_schedule(n, 97));
        assert_eq!(d1_l, d2_l, "delay reset 后重放必须逐位一致");
    }

    #[test]
    fn 非法采样率报错_对齐ts错误信息() {
        for bad in [0.0_f64, -44100.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let err = DelayEffect::new(bad).err().expect("非法采样率必须报错");
            assert!(err.contains("invalid sample rate"), "应对齐 TS 错误信息：{err}");
            let err = ChorusEffect::new(bad).err().expect("非法采样率必须报错");
            assert!(err.contains("invalid sample rate"));
            let err = FlangerEffect::new(bad).err().expect("非法采样率必须报错");
            assert!(err.contains("invalid sample rate"));
            let err = PhaserEffect::new(bad).err().expect("非法采样率必须报错");
            assert!(err.contains("invalid sample rate"));
            let err = TremoloEffect::new(bad).err().expect("非法采样率必须报错");
            assert!(err.contains("invalid sample rate"));
            let err = ModEffectsStage::from_settings(
                bad,
                ModEffectsSettings {
                    delay: DelaySettings { enabled: false, delay_ms: 0.0, feedback: 0.0, mix: 0.0 },
                    chorus: ChorusSettings { enabled: false, rate_hz: 1.0, depth_ms: 0.0, mix: 0.0 },
                    flanger: FlangerSettings { enabled: false, rate_hz: 1.0, depth_ms: 0.0, feedback: 0.0, mix: 0.0 },
                    phaser: PhaserSettings { enabled: false, rate_hz: 1.0, depth: 0.0, feedback: 0.0, mix: 0.0, stages: 4.0 },
                    tremolo: TremoloSettings { enabled: false, rate_hz: 5.0, depth: 0.0, mix: 0.0 },
                },
            )
            .err()
            .expect("非法采样率必须报错");
            assert!(err.contains("invalid sample rate"));
        }
    }

    #[test]
    fn 满幅输入极值参数下有界不发散() {
        let fs = 44100.0;
        let settings = ModEffectsSettings {
            delay: DelaySettings { enabled: true, delay_ms: 2000.0, feedback: 0.98, mix: 1.0 },
            chorus: ChorusSettings { enabled: true, rate_hz: 20.0, depth_ms: 50.0, mix: 1.0 },
            flanger: FlangerSettings { enabled: true, rate_hz: 20.0, depth_ms: 50.0, feedback: 0.98, mix: 1.0 },
            phaser: PhaserSettings { enabled: true, rate_hz: 20.0, depth: 1.0, feedback: 0.98, mix: 1.0, stages: 8.0 },
            tremolo: TremoloSettings { enabled: true, rate_hz: 30.0, depth: 1.0, mix: 1.0 },
        };
        let mut stage = ModEffectsStage::from_settings(fs, settings).expect("合法参数");
        stage.prepare(128);
        let n = 2048;
        let (out_l, out_r) =
            drive_sched(|l, r| stage.process(l, r), &lcg_noise(n, 99, 1.0), &lcg_noise(n, 98, 1.0), &block_schedule(n, 128));
        for i in 0..n {
            assert!(out_l[i].is_finite() && out_r[i].is_finite(), "输出必须有限 @{i}");
            assert!(out_l[i].abs() < 1.0e3 && out_r[i].abs() < 1.0e3, "输出应有界 @{i}");
        }
    }
}
