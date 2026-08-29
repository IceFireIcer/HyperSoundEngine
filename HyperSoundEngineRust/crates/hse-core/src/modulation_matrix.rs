//! modulation_matrix —— 参数调制矩阵（LFO / 包络跟随 → 控制率目标值）。
//!
//! 行为事实标准：仓库根 `src/dsp/modulation.ts`（`ModulationMatrix` / `Lfo` /
//! `EnvelopeFollower`）；规格：`specs/dsp/modulation-matrix.md`。
//! 本模块是向量驱动模型的**首个控制率 Stage 形态**（§4.4）：每块先推进矩阵
//! （LFO 相位、包络状态），再把块率 `masterGain` 逐样本乘到 L/R——对应引擎
//! `mod-master-gain` 阶段的逐样本乘法；`stereoWidth` 产物不入向量（引擎
//! `mid-side` 组合行为）。
//!
//! # 移植纪律（specs/dsp/modulation-matrix.md §4）
//!
//! - **LFO 相位每块推进一次且值在推进后采样**（§4.1、§4.5.1）：首块返回值即
//!   `value(推进后相位)·depth`（fs=48000、rate 4Hz、块 256 → 首块
//!   `sin(2π·4·256/48000) ≈ 0.13364`，非相位 0 的值）；`phase = (phase +
//!   rateHz·(n/fs)) % 1`，f64 取模（`%` 语义与 JS 一致，符号随被除数）；
//! - **包络 = 逐样本双声道联合峰值** `e = max(|l|, |r|)`（§4.2、§4.5.7），
//!   一阶 attack/release 平滑；**状态 env 不含 amount**（返回时才乘）；
//!   attack/release 系数 `1 − exp(−1/((max(ms,0.05)/1000)·fs))` 与 limiter/
//!   compressor 同构（平台 `f64::exp` 与 V8 在该值域逐位一致，探针实证）；
//! - **路由求和与钳制**（§4.3）：masterGain 基线 1、钳 [0,4]；stereoWidth
//!   基线 1、钳 [0,2]；两源每块**无条件推进**（无路由也推进，§4.5.4 实证）；
//! - **无 `enabled` 字段**（§一）：`lfo.enabled`/`envelope.enabled` 全仓库无
//!   消费，向量 params 不含；模块自身恒处理；
//! - **输出依赖 blockSize**（§4.4.5、GWT-MM-06）：增益按块常量、LFO 相位按块
//!   推进——对拍必须按冻结向量的 blockSize 回放（harness 已保证）；
//! - **f32 落点**：TS 全部中间量为 Number（f64），唯一 f32 落点是驱动器的
//!   `outL[i] *= g`（f64 乘法后写回 Float32Array）——[`Stage::process`] 以
//!   `f64::from(x) · master_gain → as f32` 复刻同一落点（`x·1` 逐位还原输入
//!   含 ±0，恒等锚点 GWT-MM-05）；
//! - LFO sine 与合成谱三角函数一律用 [`crate::fft::ts_trig`]（V8 fdlibm 逐位
//!   复刻，与 FFT twiddle 同源）。
//!
//! # 与 TS 源码的逐行对应关系（modulation.ts 行号）
//!
//! - clamp（L18–L20）→ [`clamp`]（三目链，NaN 原样返回）；
//! - Lfo（L23–L68）→ [`Lfo`]（setParams/processBlock/reset/value 同序）；
//! - EnvelopeFollower（L71–L107）→ [`EnvelopeFollower`]；
//! - ModulationMatrix（L110–L165）→ [`ModulationMatrixStage`]
//!   （`setRoutes`/`setLfoParams`/`setEnvelopeParams` 等价方法保留）。

use crate::fft::ts_trig;
use crate::Stage;

/// 复刻 JS `Math.max(a, b)` 的 NaN 传播语义（理由同 biquad.rs 的同名助手）。
fn js_max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a > b {
        a
    } else {
        b
    }
}

/// TS `clamp(v, lo, hi)` 的逐字复刻（modulation.ts L18–L20 三目链；NaN 输入
/// 两个比较均为 false → 原样返回，与 JS 一致）。
fn clamp(v: f64, lo: f64, hi: f64) -> f64 {
    if v < lo {
        lo
    } else if v > hi {
        hi
    } else {
        v
    }
}

/// LFO 波形形态（TS `LfoShape`，src/types.ts）。
///
/// [`LfoShape::parse`] 复刻 TS `switch` default：枚举外字符串按 `sine`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LfoShape {
    Sine,
    Triangle,
    Square,
    Saw,
}

impl LfoShape {
    /// TS `value()` 的 switch 分派；未知形态走 default 分支 = sine。
    pub fn parse(shape: &str) -> Self {
        match shape {
            "triangle" => LfoShape::Triangle,
            "square" => LfoShape::Square,
            "saw" => LfoShape::Saw,
            // 'sine' 与一切枚举外值（TS switch default）。
            _ => LfoShape::Sine,
        }
    }
}

/// 调制源（TS `ModulationRoute.source`：`'lfo' | 'envelope'`）。
///
/// TS 求值语义为 `route.source === 'lfo' ? lfoVal : envVal`——**任何非
/// `'lfo'` 值（含枚举外字符串）都落 envelope**，[`ModSource::parse`] 保持
/// 同一fallback，不臆造第三形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModSource {
    Lfo,
    Envelope,
}

impl ModSource {
    pub fn parse(source: &str) -> Self {
        if source == "lfo" {
            ModSource::Lfo
        } else {
            ModSource::Envelope
        }
    }
}

/// 调制目标（TS `ModulationRoute.target`：`'masterGain' | 'stereoWidth'`）。
///
/// TS 求值语义为 `route.target === 'masterGain' ? masterGain += v :
/// stereoWidth += v`——非 `'masterGain'` 值全落 stereoWidth。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModTarget {
    MasterGain,
    StereoWidth,
}

impl ModTarget {
    pub fn parse(target: &str) -> Self {
        if target == "masterGain" {
            ModTarget::MasterGain
        } else {
            ModTarget::StereoWidth
        }
    }
}

/// 一条调制路由（TS `ModulationRoute`；`offset` 在 TS 为可选，缺省按 0）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModulationRoute {
    pub source: ModSource,
    pub target: ModTarget,
    /// 路由量不钳制（§三注 2；超界值经目标钳制兜底，case3 固化）。
    pub amount: f64,
    /// TS `route.offset ?? 0`（缺省 0）。
    pub offset: f64,
}

/// LFO 参数快照（TS `setLfoParams(shape, rateHz, depth)` 三元组）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LfoParams {
    pub shape: LfoShape,
    pub rate_hz: f64,
    pub depth: f64,
}

/// 包络跟随器参数快照（TS `setEnvelopeParams(attackMs, releaseMs, amount)`）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EnvelopeParams {
    pub attack_ms: f64,
    pub release_ms: f64,
    pub amount: f64,
}

/// 一次块推进产出的控制率目标值（TS `processBlock` 返回对象）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModulationTargets {
    pub master_gain: f64,
    pub stereo_width: f64,
}

/// LFO 低频振荡器（双极性 -1..1；modulation.ts L23–L68 逐行移植）。
#[derive(Debug)]
pub struct Lfo {
    sample_rate: f64,
    shape: LfoShape,
    rate_hz: f64,
    depth: f64,
    phase: f64,
}

impl Lfo {
    /// 构造（默认 sine / 1Hz / depth 1，modulation.ts L25–L28 类字段初值）。
    /// `sampleRate ≤ 0` 或 NaN 时报错（对齐 TS `Error('invalid sample rate')`）。
    pub fn new(sample_rate: f64) -> Result<Self, String> {
        // TS L31：if (!(sampleRate > 0)) throw——NaN 也落入抛错域。
        if !(sample_rate > 0.0) {
            return Err("invalid sample rate".into());
        }
        Ok(Self {
            sample_rate,
            shape: LfoShape::Sine,
            rate_hz: 1.0,
            depth: 1.0,
            phase: 0.0,
        })
    }

    /// TS `setParams`（L36–L40）：`rateHz = max(0, rateHz)`（负值 → 相位冻结）、
    /// `depth` 钳制 [0,1]、shape 枚举外由 [`LfoShape::parse`] 按 sine。
    pub fn set_params(&mut self, params: LfoParams) {
        self.shape = params.shape;
        self.rate_hz = js_max(0.0, params.rate_hz);
        self.depth = clamp(params.depth, 0.0, 1.0);
    }

    /// TS `processBlock(n)`（L43–L47）：**相位每块推进一次、值在推进后采样**。
    pub fn process_block(&mut self, n: usize) -> f64 {
        let dt = n as f64 / self.sample_rate;
        // TS L45：this.phase = (this.phase + this.rateHz * dt) % 1（f64 取模；
        // 相位恒 ≥ 0，% 符号语义无歧义）。
        self.phase = (self.phase + self.rate_hz * dt) % 1.0;
        self.value() * self.depth
    }

    /// TS `reset`（L49–L51）：phase = 0。
    pub fn reset(&mut self) {
        self.phase = 0.0;
    }

    /// TS `value()`（L53–L67）：四形态波形，输出前不乘 depth（乘法在
    /// `processBlock` 中）。sine 用 [`ts_trig::sin`]（与 V8 `Math.sin` 逐位一致）。
    fn value(&self) -> f64 {
        let p = self.phase;
        match self.shape {
            LfoShape::Sine => ts_trig::sin(2.0 * std::f64::consts::PI * p),
            // TS L59：4 * Math.abs(p - 0.5) - 1。
            LfoShape::Triangle => 4.0 * (p - 0.5).abs() - 1.0,
            LfoShape::Square => {
                if p < 0.5 {
                    1.0
                } else {
                    -1.0
                }
            }
            // TS L63：2 * p - 1。
            LfoShape::Saw => 2.0 * p - 1.0,
        }
    }
}

/// 包络跟随器（峰值检测 + 一阶平滑，输出 0..1；modulation.ts L71–L107 逐行移植）。
#[derive(Debug)]
pub struct EnvelopeFollower {
    sample_rate: f64,
    attack_coef: f64,
    release_coef: f64,
    amount: f64,
    env: f64,
}

impl EnvelopeFollower {
    /// 构造（默认 attack 10ms / release 200ms / amount 1，TS L73–L76 类字段初值）。
    pub fn new(sample_rate: f64) -> Result<Self, String> {
        if !(sample_rate > 0.0) {
            return Err("invalid sample rate".into());
        }
        Ok(Self {
            sample_rate,
            attack_coef: 0.0,
            release_coef: 0.0,
            amount: 1.0,
            env: 0.0,
        })
    }

    /// TS `setParams`（L84–L90）：`max(ms, 0.05)` 下限钳制、amount 钳 [0,1]。
    /// 系数表达式 `1 − exp(−1 / ((ms/1000)·fs))` 与 limiter/compressor 同构
    /// （modulation.ts L87 的结合序逐字保持）。
    pub fn set_params(&mut self, params: EnvelopeParams) {
        let a = js_max(params.attack_ms, 0.05);
        let r = js_max(params.release_ms, 0.05);
        self.attack_coef = 1.0 - (-1.0 / ((a / 1000.0) * self.sample_rate)).exp();
        self.release_coef = 1.0 - (-1.0 / ((r / 1000.0) * self.sample_rate)).exp();
        self.amount = clamp(params.amount, 0.0, 1.0);
    }

    /// TS `processBlock`（L93–L102）：逐样本双声道联合峰值
    /// `e = max(|l|, |r|)`，`e > env` 走 attack 否则 release（差值为 0 时
    /// 包络精确保持——静音段 env 精确 0 的来源，§4.5.5）。
    /// 状态 `env` 不含 amount，返回时才乘（§4.2）。
    pub fn process_block(&mut self, left: &[f32], right: &[f32]) -> f64 {
        let attack = self.attack_coef;
        let release = self.release_coef;
        for i in 0..left.len() {
            // TS L97：Math.abs(l[i]) > Math.abs(r[i]) ? Math.abs(l[i]) : Math.abs(r[i])
            // ——f32 值宽化为 f64 后取绝对值与比较（与 JS Number 读数一致）。
            let al = f64::from(left[i]).abs();
            let ar = f64::from(right[i]).abs();
            let e = if al > ar { al } else { ar };
            if e > self.env {
                self.env += attack * (e - self.env);
            } else {
                self.env += release * (e - self.env);
            }
        }
        self.env * self.amount
    }

    /// TS `reset`（L104–L106）：env = 0。
    pub fn reset(&mut self) {
        self.env = 0.0;
    }
}

/// modulation-matrix Stage（控制率形态，specs/dsp/modulation-matrix.md §4.4）。
///
/// [`Stage::process`] 的每块音频可观测语义 = **先推进矩阵**（LFO 相位、包络
/// 状态——包络跟踪增益前输入，因为乘法发生在推进之后），**再把块率
/// masterGain 逐样本乘到 L/R**（f64 乘法、f32 写回，对齐引擎
/// `mod-master-gain` 阶段与导出驱动器 `outL[i] *= g` 的落点）。
#[derive(Debug)]
pub struct ModulationMatrixStage {
    lfo: Lfo,
    env: EnvelopeFollower,
    /// 路由表按 TS `setRoutes(routes.slice())` 语义持有拷贝，求和按表序。
    routes: Vec<ModulationRoute>,
}

impl ModulationMatrixStage {
    /// TS `new ModulationMatrix(sampleRate)`：全新零状态 + 构造缺省
    /// （lfo sine/1/0.5、envelope 10/200/0.5，TS L121–L127）。
    pub fn new(sample_rate: f64) -> Result<Self, String> {
        Ok(Self {
            lfo: Lfo::new(sample_rate)?,
            env: EnvelopeFollower::new(sample_rate)?,
            routes: Vec::new(),
        })
    }

    /// 引擎接线顺序的完整构造（specs/dsp/modulation-matrix.md §4.4.1）：
    /// `new ModulationMatrix(fs)` → `setRoutes` → `setLfoParams` →
    /// `setEnvelopeParams`——与导出驱动器（export-vectors.mjs modulation-matrix
    /// 分支）逐字同序。实例为全新零状态，不额外调用 reset（总纲 §3.4）。
    pub fn from_params(
        sample_rate: f64,
        routes: Vec<ModulationRoute>,
        lfo: LfoParams,
        envelope: EnvelopeParams,
    ) -> Result<Self, String> {
        let mut stage = Self::new(sample_rate)?;
        stage.set_routes(routes);
        stage.set_lfo_params(lfo);
        stage.set_envelope_params(envelope);
        Ok(stage)
    }

    /// TS `setRoutes`（L131–L133）：整体替换并持有拷贝（`routes.slice()`）。
    pub fn set_routes(&mut self, routes: Vec<ModulationRoute>) {
        self.routes = routes;
    }

    /// TS `setLfoParams`（L135–L137）。
    pub fn set_lfo_params(&mut self, params: LfoParams) {
        self.lfo.set_params(params);
    }

    /// TS `setEnvelopeParams`（L139–L141）。
    pub fn set_envelope_params(&mut self, params: EnvelopeParams) {
        self.env.set_params(params);
    }

    /// TS `ModulationMatrix.processBlock`（L144–L159）：两源每块**无条件推进**
    /// （与路由表内容无关，§4.3），随后按表序求和、钳制
    /// （masterGain [0,4] / stereoWidth [0,2]，基线均为 1）。
    pub fn process_block(&mut self, left: &[f32], right: &[f32]) -> ModulationTargets {
        let lfo_val = self.lfo.process_block(left.len());
        let env_val = self.env.process_block(left, right);

        let mut master_gain = 1.0_f64;
        let mut stereo_width = 1.0_f64;
        for route in &self.routes {
            // TS L151–L152：src = source === 'lfo' ? lfoVal : envVal；
            // v = src * amount + (offset ?? 0)。
            let src = match route.source {
                ModSource::Lfo => lfo_val,
                ModSource::Envelope => env_val,
            };
            let v = src * route.amount + route.offset;
            match route.target {
                ModTarget::MasterGain => master_gain += v,
                ModTarget::StereoWidth => stereo_width += v,
            }
        }
        // TS L156–L157：钳制域 [0,4] / [0,2]。
        ModulationTargets {
            master_gain: clamp(master_gain, 0.0, 4.0),
            stereo_width: clamp(stereo_width, 0.0, 2.0),
        }
    }
}

impl Stage for ModulationMatrixStage {
    /// 无工作缓冲（控制率状态全是标量），无需预分配。
    fn prepare(&mut self, _max_block_size: usize) {}

    /// §4.4.2 每块语义：推进矩阵（包络读取**增益前**输入）→ masterGain 逐样本
    /// 乘到 L/R（f64 乘法、f32 写回；`g = 1` 时逐位还原输入含 ±0）。
    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let targets = self.process_block(left, right);
        let g = targets.master_gain;
        for i in 0..left.len() {
            left[i] = (f64::from(left[i]) * g) as f32;
        }
        for i in 0..right.len() {
            right[i] = (f64::from(right[i]) * g) as f32;
        }
    }

    /// TS `reset`（L161–L164）：lfo.reset() + env.reset()。
    fn reset(&mut self) {
        self.lfo.reset();
        self.env.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试激励：与导出工具同族的固定种子 LCG 伪噪声（禁随机源）。
    fn lcg_noise(n: usize, seed: u32, amp: f64) -> Vec<f32> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((f64::from(s) / 4294967296.0) * 2.0 - 1.0) * amp
            })
            .map(|v| v as f32)
            .collect()
    }

    /// 黄金参考：node 直跑 src/dsp/modulation.ts（esbuild bundle，与导出工具
    /// 同一加载策略）冻结的 f64 位型——位级对拍锚点。
    mod golden {
        // LFO 四形态（route lfo→masterGain amount 1、rate 4Hz、depth 1、
        // fs 48000、块 256；masterGain = 1 + LFO 值）。
        pub const LFO_SINE_B1: u64 = 0x3ff2_2363_f7d4_58a3; // 1.1336402588690724
        pub const LFO_SINE_B2: u64 = 0x3ff4_3cf5_f178_f783; // 1.2648829872627807
        pub const LFO_TRIANGLE_B1: u64 = 0x3ffe_a279_83c1_31d6; // 1.9146666666666667
        pub const LFO_TRIANGLE_B2: u64 = 0x3ffd_44f3_0782_63ab; // 1.8293333333333333
        pub const LFO_SQUARE_B1: u64 = 0x4000_0000_0000_0000; // 2
        pub const LFO_SQUARE_B2: u64 = 0x4000_0000_0000_0000;
        pub const LFO_SAW_B1: u64 = 0x3fa5_d867_c3ec_e2a0; // 0.04266666666666663
        pub const LFO_SAW_B2: u64 = 0x3fb5_d867_c3ec_e2a8; // 0.08533333333333337
        // case1 参数（amount 0.5）前 8 块 masterGain。
        pub const MM1_GAINS: [u64; 8] = [
            0x3ff1_11b1_fbea_2c51,
            0x3ff2_1e7a_f8bc_7bc2,
            0x3ff3_2188_845c_a6ad,
            0x3ff4_1634_df1a_06de,
            0x3ff4_f81c_53d2_3a08,
            0x3ff5_c331_60c7_41f8,
            0x3ff6_73cf_5491_080c,
            0x3ff7_06cb_09ae_5fec,
        ];
        // 包络 golden（route env→masterGain amount 1、fs 48000、attack 10ms/
        // release 200ms/amount 1；输入 [0.3,-0.9,0.5,0]/[0.9,0.1,-0.7,0]）。
        pub const ENV_BLOCK1: u64 = 0x3ff0_1542_d3cd_1706; // 1.0051906846505871
        // 三块无路由推进后（相位 3·4·256/48000）再挂路由的 LFO 块值。
        pub const ADVANCE_AFTER_3: u64 = 0x3ff8_2c69_be34_0dbc; // 1.5108430318658597
    }

    fn bits(x: f64) -> u64 {
        x.to_bits()
    }

    fn route_master(source: ModSource, amount: f64) -> ModulationRoute {
        ModulationRoute { source, target: ModTarget::MasterGain, amount, offset: 0.0 }
    }

    #[test]
    fn lfo_四形态_推进后采样_命中_node_黄金位型() {
        // GWT-MM-01/07：相位每块推进一次、值在推进后采样（首块即
        // sin(2π·4·256/48000)，非相位 0 的值）；四形态公式逐位命中 TS。
        for (shape, b1, b2) in [
            (LfoShape::Sine, golden::LFO_SINE_B1, golden::LFO_SINE_B2),
            (LfoShape::Triangle, golden::LFO_TRIANGLE_B1, golden::LFO_TRIANGLE_B2),
            (LfoShape::Square, golden::LFO_SQUARE_B1, golden::LFO_SQUARE_B2),
            (LfoShape::Saw, golden::LFO_SAW_B1, golden::LFO_SAW_B2),
        ] {
            let mut stage = ModulationMatrixStage::new(48000.0).unwrap();
            stage.set_routes(vec![route_master(ModSource::Lfo, 1.0)]);
            stage.set_lfo_params(LfoParams { shape, rate_hz: 4.0, depth: 1.0 });
            let z = vec![0.0_f32; 256];
            let g1 = stage.process_block(&z, &z);
            let g2 = stage.process_block(&z, &z);
            assert_eq!(bits(g1.master_gain), b1, "{shape:?} 块 1");
            assert_eq!(bits(g2.master_gain), b2, "{shape:?} 块 2");
        }
    }

    #[test]
    fn lfo_相位推进与采样次序_黄金位型() {
        // §4.5.1：首块值 = 推进后相位采样；连续两块相位逐块累进。
        let mut lfo = Lfo::new(48000.0).unwrap();
        lfo.set_params(LfoParams { shape: LfoShape::Sine, rate_hz: 4.0, depth: 1.0 });
        let v1 = lfo.process_block(256);
        assert!((v1 - 0.1336402588690724).abs() < 1e-15, "v1={v1}");
        assert_eq!(bits(1.0 + v1), golden::LFO_SINE_B1);
        // 枚举外形态按 sine（TS switch default）。
        let mut stage = ModulationMatrixStage::new(48000.0).unwrap();
        stage.set_routes(vec![route_master(ModSource::Lfo, 1.0)]);
        stage.set_lfo_params(LfoParams {
            shape: LfoShape::parse("unknown-shape"),
            rate_hz: 4.0,
            depth: 1.0,
        });
        let z = vec![0.0_f32; 256];
        assert_eq!(bits(stage.process_block(&z, &z).master_gain), golden::LFO_SINE_B1);
    }

    #[test]
    fn 无路由时状态照常推进() {
        // §4.3/§4.5.4：两源推进与路由表内容无关——3 块空路由后第 4 块的
        // LFO 值接续（黄金位型），且空路由块 masterGain 恒精确 1。
        let mut stage = ModulationMatrixStage::new(48000.0).unwrap();
        stage.set_lfo_params(LfoParams { shape: LfoShape::Sine, rate_hz: 4.0, depth: 1.0 });
        let z = vec![0.0_f32; 256];
        for b in 0..3 {
            let t = stage.process_block(&z, &z);
            assert_eq!(bits(t.master_gain), 0x3ff0_0000_0000_0000, "空路由块 {b} 恒 1");
        }
        stage.set_routes(vec![route_master(ModSource::Lfo, 1.0)]);
        let t = stage.process_block(&z, &z);
        assert_eq!(bits(t.master_gain), golden::ADVANCE_AFTER_3);
    }

    #[test]
    fn 包络_联合峰值与静音保持_命中_node_黄金位型() {
        // GWT-MM-02：联合峰值 max(|L|,|R|)——整块交换左右声道内容（峰值在
        // L 侧或 R 侧）包络轨迹必须逐位一致（立体声联动）。
        let env_params = EnvelopeParams { attack_ms: 10.0, release_ms: 200.0, amount: 1.0 };
        let left = [0.3_f32, -0.9, 0.5, 0.0];
        let right = [0.9_f32, 0.1, -0.7, 0.0];
        let run = |swap: bool| {
            let mut stage = ModulationMatrixStage::new(48000.0).unwrap();
            stage.set_routes(vec![route_master(ModSource::Envelope, 1.0)]);
            stage.set_envelope_params(env_params);
            let (l, r) = if swap {
                (right.to_vec(), left.to_vec())
            } else {
                (left.to_vec(), right.to_vec())
            };
            stage.process_block(&l, &r).master_gain
        };
        assert_eq!(bits(run(false)), bits(run(true)), "联合峰值：峰值在 L 或 R 侧包络一致");
        assert_eq!(bits(run(false)), golden::ENV_BLOCK1, "attack 轨迹块尾包络黄金位型");
        // 静音输入（e=0、env=0）走 release 路径但差值为 0：包络精确保持 0
        // → masterGain 恒精确 1（§4.5.5）。
        let mut stage = ModulationMatrixStage::new(48000.0).unwrap();
        stage.set_routes(vec![route_master(ModSource::Envelope, 1.0)]);
        stage.set_envelope_params(env_params);
        let z = vec![0.0_f32; 8];
        for b in 0..4 {
            let t = stage.process_block(&z, &z);
            assert_eq!(bits(t.master_gain), 0x3ff0_0000_0000_0000, "静音块 {b} 包络精确 0");
        }
    }

    #[test]
    fn masterGain_钳制_0_4_双向可达() {
        // GWT-MM-03：saw 相位接近 1（saw→+1）触上界 4；相位 0（saw→−1）触下界 0。
        let rate_up = 0.999_999 * 48000.0 / 256.0; // 首块推进后相位 ≈ 0.999999
        let mut up = ModulationMatrixStage::new(48000.0).unwrap();
        up.set_routes(vec![route_master(ModSource::Lfo, 5.0)]);
        up.set_lfo_params(LfoParams { shape: LfoShape::Saw, rate_hz: rate_up, depth: 1.0 });
        let z = vec![0.0_f32; 256];
        assert_eq!(up.process_block(&z, &z).master_gain, 4.0, "上界钳到 4");
        assert_eq!(bits(up.process_block(&z, &z).master_gain), 0x4010_0000_0000_0000);

        let rate_zero = 48000.0 / 256.0; // 每块推进恰好 1.0 → 相位 % 1 = 0 → saw = −1
        let mut down = ModulationMatrixStage::new(48000.0).unwrap();
        down.set_routes(vec![route_master(ModSource::Lfo, 5.0)]);
        down.set_lfo_params(LfoParams { shape: LfoShape::Saw, rate_hz: rate_zero, depth: 1.0 });
        assert_eq!(down.process_block(&z, &z).master_gain, 0.0, "下界钳到 0");
    }

    #[test]
    fn case1_参数_块增益轨迹_命中_node_黄金位型() {
        // 向量 modulation-matrix.case1 的参数与前 8 块 masterGain 轨迹（f64 位型）。
        let mut stage = ModulationMatrixStage::new(48000.0).unwrap();
        stage.set_routes(vec![route_master(ModSource::Lfo, 0.5)]);
        stage.set_lfo_params(LfoParams { shape: LfoShape::Sine, rate_hz: 4.0, depth: 1.0 });
        stage.set_envelope_params(EnvelopeParams {
            attack_ms: 10.0,
            release_ms: 200.0,
            amount: 0.5,
        });
        let left = lcg_noise(8 * 256, 87001, 0.6);
        let right = lcg_noise(8 * 256, 31337, 0.5);
        for (b, want) in golden::MM1_GAINS.iter().enumerate() {
            let blk_l = &left[b * 256..(b + 1) * 256];
            let blk_r = &right[b * 256..(b + 1) * 256];
            let t = stage.process_block(blk_l, blk_r);
            assert_eq!(bits(t.master_gain), *want, "case1 块 {b}");
        }
    }

    #[test]
    fn 无_masterGain_路由_逐位恒等锚点() {
        // GWT-MM-05：仅 stereoWidth 路由（非零 amount/offset）→ masterGain 恒
        // 精确 1，输出与输入逐位一致（含负值与 ±0 语义）。
        let mut stage = ModulationMatrixStage::new(48000.0).unwrap();
        stage.set_routes(vec![ModulationRoute {
            source: ModSource::Lfo,
            target: ModTarget::StereoWidth,
            amount: 1.5,
            offset: 0.5,
        }]);
        stage.set_lfo_params(LfoParams { shape: LfoShape::Square, rate_hz: 2.0, depth: 1.0 });
        stage.set_envelope_params(EnvelopeParams {
            attack_ms: 10.0,
            release_ms: 200.0,
            amount: 0.5,
        });
        let mut left = lcg_noise(512, 87001, 0.6);
        let mut right = lcg_noise(512, 12345, 0.6);
        left[0] = 0.0;
        left[1] = -0.0;
        right[0] = -0.0;
        let want_l = left.clone();
        let want_r = right.clone();
        stage.process(&mut left, &mut right);
        assert_eq!(left, want_l, "左声道逐位一致");
        assert_eq!(right, want_r, "右声道逐位一致");
        // ±0 位型保持（x·1 的 IEEE 语义）。
        assert_eq!(left[0].to_bits(), 0.0_f32.to_bits());
        assert_eq!(left[1].to_bits(), (-0.0_f32).to_bits());
        assert_eq!(right[0].to_bits(), (-0.0_f32).to_bits());
    }

    #[test]
    fn 输出依赖_blockSize_增益按块常量() {
        // GWT-MM-06：增益按块采样、LFO 相位按块推进——不同块长产生不同轨迹。
        let build = || {
            let mut stage = ModulationMatrixStage::new(48000.0).unwrap();
            stage.set_routes(vec![route_master(ModSource::Lfo, 0.5)]);
            stage.set_lfo_params(LfoParams { shape: LfoShape::Sine, rate_hz: 4.0, depth: 1.0 });
            stage
        };
        let frames = 6000_usize;
        let left = lcg_noise(frames, 87001, 0.6);
        let right = lcg_noise(frames, 31337, 0.5);
        let mut out256 = (left.clone(), right.clone());
        let mut stage = build();
        stage.prepare(256);
        let mut off = 0;
        while off < frames {
            let end = (off + 256).min(frames);
            stage.process(&mut out256.0[off..end], &mut out256.1[off..end]);
            off = end;
        }
        let mut out384 = (left.clone(), right.clone());
        let mut stage = build();
        stage.prepare(384);
        let mut off = 0;
        while off < frames {
            let end = (off + 384).min(frames);
            stage.process(&mut out384.0[off..end], &mut out384.1[off..end]);
            off = end;
        }
        assert_ne!(out256, out384, "不同 blockSize 输出必须可区分");
    }

    #[test]
    fn 极值钳制_无数值事故() {
        // GWT-MM-08：rateHz 负值 → 相位冻结 0（sine 值恒 0 → gain 恒 1）；
        // depth 3 → 1 封顶；amount 越界经目标钳制兜底；全程有界。
        let mut stage = ModulationMatrixStage::new(48000.0).unwrap();
        stage.set_routes(vec![route_master(ModSource::Lfo, 100.0)]);
        stage.set_lfo_params(LfoParams { shape: LfoShape::Sine, rate_hz: -5.0, depth: 3.0 });
        let z = vec![0.0_f32; 256];
        for b in 0..4 {
            let t = stage.process_block(&z, &z);
            assert_eq!(bits(t.master_gain), 0x3ff0_0000_0000_0000, "负 rate 冻结块 {b}");
            assert!(t.stereo_width.is_finite());
        }
        // depth 3 → 1：sine 值与 depth=1 完全一致（黄金位型复用）。
        let mut stage = ModulationMatrixStage::new(48000.0).unwrap();
        stage.set_routes(vec![route_master(ModSource::Lfo, 1.0)]);
        stage.set_lfo_params(LfoParams { shape: LfoShape::Sine, rate_hz: 4.0, depth: 3.0 });
        let t = stage.process_block(&z, &z);
        assert_eq!(bits(t.master_gain), golden::LFO_SINE_B1);
    }

    #[test]
    fn reset_复现性与抛错路径() {
        // GWT-MM-09：reset 后同参数同输入重放逐位一致；fs ≤ 0 / NaN 抛
        // Error('invalid sample rate')。
        let mut stage = ModulationMatrixStage::new(48000.0).unwrap();
        stage.set_routes(vec![
            route_master(ModSource::Lfo, 1.0),
            route_master(ModSource::Envelope, 1.0),
        ]);
        stage.set_lfo_params(LfoParams { shape: LfoShape::Saw, rate_hz: 4.0, depth: 1.0 });
        stage.set_envelope_params(EnvelopeParams {
            attack_ms: 5.0,
            release_ms: 100.0,
            amount: 1.0,
        });
        let left = lcg_noise(256, 42, 0.5);
        let right = lcg_noise(256, 43, 0.5);
        // 每轮重放使用输入的新拷贝（process 是就地乘法，会改写工作缓冲）。
        let mut run = |stage: &mut ModulationMatrixStage| {
            let mut l = left.clone();
            let mut r = right.clone();
            let mut gains = Vec::new();
            for _b in 0..4 {
                gains.push(stage.process_block(&l, &r).master_gain.to_bits());
                stage.process(&mut l, &mut r);
            }
            gains
        };
        let first = run(&mut stage);
        stage.reset();
        let second = run(&mut stage);
        assert_eq!(first, second, "reset 后重放逐位一致");
        for fs in [0.0_f64, -1.0, f64::NAN, f64::NEG_INFINITY] {
            let err = ModulationMatrixStage::new(fs).unwrap_err();
            assert_eq!(err, "invalid sample rate");
            assert!(Lfo::new(fs).is_err());
            assert!(EnvelopeFollower::new(fs).is_err());
        }
    }

    #[test]
    fn 包络不放大极小差值_且处理不改写输入() {
        // §4.5.6：processBlock 不改写输入缓冲（Lfo 不触碰 l/r，Envelope 只读）。
        let mut stage = ModulationMatrixStage::new(48000.0).unwrap();
        stage.set_routes(vec![route_master(ModSource::Envelope, 1.0)]);
        stage.set_envelope_params(EnvelopeParams {
            attack_ms: 10.0,
            release_ms: 200.0,
            amount: 1.0,
        });
        let left = lcg_noise(256, 7, 0.5);
        let right = lcg_noise(256, 8, 0.5);
        let want_l = left.clone();
        let want_r = right.clone();
        let _ = stage.process_block(&left, &right);
        assert_eq!(left, want_l);
        assert_eq!(right, want_r);
    }
}
