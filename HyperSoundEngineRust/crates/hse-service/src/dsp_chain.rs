//! 引擎子链（Phase 3 全序）：对齐 TS 全链 22 级的引擎相对顺序装配——
//!
//! ```text
//! midSide → biquad(Pre-EQ 前置单节) → eqChain(Pre-EQ 级联) → deesser → compressor
//!   → modEffects(Delay→Chorus→Flanger→Phaser→Tremolo) → reverb 三路路由
//!     (simple | fdn | convolver | off) → bassEnhancer → loudnessComp → dynamicEq
//!   → modMatrix(控制率：推进矩阵 + masterGain 乘 L/R) → limiter
//! ```
//!
//! （交错进、planar 过链、交错出。）全链中的 NightMode / IEQ / 分析取样级是
//! **引擎内部组合**（压缩增强 + shelf、IEQ 校准链、LUFS/分析馈送），不是独立核心
//! 模块，不参与本子链（注释留位，勿误认为遗漏）；loudness-normalization /
//! surround3d / spatial / lufs 采样级同理暂不参与。
//! HseStretch 不入链——引擎语义为 `getStretch()` 离线/过渡外置调用（TS 引擎同样
//! 不内联进主链），本子链不承接。
//!
//! 构造只在控制面线程发生（分配合法）；process_planar 在 DSP 线程稳态零分配零锁
//! 零系统调用。参数热更换经 rtrb 命令环移交整条新链的所有权（见 pipeline）。
//!
//! 直通语义（回归锚点）：全键缺省 + reverbSimple 全干(wet=0,dry=1) + limiter 禁用
//! ⇒ 逐位直通——biquad/eqChain 键缺省不装配级、deesser/modEffects/dynamicEq
//! 缺省 disabled 硬旁路、loudnessComp 缺省 custom+空带（0 目标恒等系数）、
//! modMatrix 缺省无路由（masterGain 基线 1，×1.0 逐位还原）均为直通形态。

use hse_core::bass_enhancer::BassEnhancerStage;
use hse_core::biquad::BiquadStage;
use hse_core::compressor::CompressorStage;
use hse_core::convolver::{build_ir_recipe, ConvolverOptions, ConvolverStage};
use hse_core::deesser::DeesserStage;
use hse_core::dynamic_eq::{DynamicEqBandParam, DynamicEqParams, DynamicEqStage};
use hse_core::eq_chain::{EqBandParam, EqChainStage};
use hse_core::fdn_reverb::FdnReverbStage;
use hse_core::limiter::LimiterStage;
use hse_core::loudness_comp::LoudnessCompStage;
use hse_core::mid_side::MidSideStage;
use hse_core::mod_effects::ModEffectsStage;
use hse_core::modulation_matrix::{
    EnvelopeParams, LfoParams, LfoShape, ModSource, ModTarget, ModulationMatrixStage,
    ModulationRoute,
};
use hse_core::reverb_simple::ReverbSimpleStage;
use hse_core::Stage;

use crate::params::{MidSideParams, PilotParams, ReverbRouteKind};

/// 引擎注入 dynamicEq 各带的固定 crossover 镜像常量（与 params.rs 保持同值；
/// 第 5 带无下交叉，TS 引擎注入 0 且核心忽略末带频率）。
const DYNAMIC_EQ_CROSSOVERS: [f64; 4] = [200.0, 800.0, 2500.0, 8000.0];

/// reverb 级的三路路由持有态（route=off 时不持有任何混响实例）。
enum ReverbRoute {
    Simple(ReverbSimpleStage),
    Fdn(FdnReverbStage),
    Convolver(ConvolverStage),
    Off,
}

/// 已装配就绪的引擎子链（所有权可跨线程移交，rtrb 元素要求 Move）。
pub struct PilotSubchain {
    mid_side: MidSideStage,
    biquad: Option<BiquadStage>,
    /// None = 快照未配置 eqChain 键 → 级不装配（逐位直通；理由见 params.rs）。
    eq_chain: Option<EqChainStage>,
    deesser: DeesserStage,
    compressor: CompressorStage,
    mod_effects: ModEffectsStage,
    reverb: ReverbRoute,
    bass: BassEnhancerStage,
    loudness_comp: LoudnessCompStage,
    dynamic_eq: DynamicEqStage,
    /// 控制率级：每块先推进矩阵（LFO 相位 + 包络状态——包络读取增益前输入），
    /// 再把块率 masterGain 逐样本乘到 L/R（镜像引擎 mod-master-gain 级）。
    /// stereoWidth 产物在本链不接线：引擎把 modStereoWidth 回读进 mid-side 级的
    /// width 快照，而本链的 midSide.width 只来自 setParams.midSide 键——为保持
    /// 既有键语义不变（width 恒等改写会被调制隐式覆盖），width 调制产物暂不
    /// 回灌（additive 扩展留待后续阶段，见 control-plane.md §八）。
    mod_matrix: ModulationMatrixStage,
    limiter: LimiterStage,
}

impl PilotSubchain {
    /// 按参数快照构造并对 max_block 完成预分配（控制面线程调用）。
    pub fn build(params: &PilotParams, sample_rate: f64, max_block: usize) -> Result<Self, String> {
        let MidSideParams { width, voice_balance } = params.mid_side;
        let mut mid_side = MidSideStage::new();
        mid_side.set_params(width, voice_balance);
        let biquad = match &params.biquad {
            Some(spec) => Some(BiquadStage::new(
                sample_rate,
                &spec.filter_type,
                spec.f0,
                spec.q,
                spec.gain_db,
            )?),
            None => None, // TS 构造默认即恒等直通
        };
        // Pre-EQ 级联（独立于既有单节 biquad 键）：set_bands → set_q_compensation
        // 的构造顺序与 hse-core 补偿终态一致（两种触发顺序终态逐位相同）。
        let eq_chain = match &params.eq_chain {
            Some(spec) => {
                let mut eq = EqChainStage::new(sample_rate, spec.band_count)?;
                let eq_bands: Vec<EqBandParam> = spec
                    .bands
                    .iter()
                    .map(|b| EqBandParam { frequency: b.frequency, gain: b.gain, q: b.q })
                    .collect();
                eq.set_bands(&eq_bands);
                eq.set_q_compensation(spec.q_compensation);
                Some(eq)
            }
            None => None, // 键未配置：级不装配（逐位直通，与 biquad 键同一先例）
        };
        let deesser = DeesserStage::from_settings(sample_rate, params.deesser.clone())?;
        let compressor = CompressorStage::from_settings(sample_rate, params.compressor.clone())?;
        let mod_effects = ModEffectsStage::from_settings(sample_rate, params.mod_effects)?;
        let reverb = match params.reverb_route {
            ReverbRouteKind::Simple => {
                ReverbRoute::Simple(ReverbSimpleStage::from_params(sample_rate, params.reverb_simple.clone())?)
            }
            ReverbRouteKind::Fdn => {
                ReverbRoute::Fdn(FdnReverbStage::from_params(sample_rate, params.fdn_reverb.clone())?)
            }
            ReverbRouteKind::Convolver => {
                // 卷积路：IR 必须由确定性配方给出（无 IR 即为非法快照）。
                // 装配顺序对齐引擎接线（parity 驱动器同序）：
                // 构造 → loadIR(buildIrRecipe) → setMix → setPreDelayMs。
                let recipe = params
                    .convolver
                    .ir_recipe
                    .ok_or("convolver 路由需要 convolver.irRecipe（delta / expNoise 配方）")?;
                let ir = build_ir_recipe(&recipe).map_err(|e| format!("convolver IR 配方非法：{e}"))?;
                let mut convolver = ConvolverStage::new(sample_rate, ConvolverOptions::default())?;
                convolver.load_ir(&ir, Some("setParams-recipe"))?;
                convolver.set_mix(params.convolver.mix);
                convolver.set_pre_delay_ms(params.convolver.pre_delay_ms);
                ReverbRoute::Convolver(convolver)
            }
            ReverbRouteKind::Off => ReverbRoute::Off,
        };
        let bass = BassEnhancerStage::from_settings(sample_rate, params.bass_enhancer.clone())?;
        let loudness_comp = LoudnessCompStage::from_settings(sample_rate, params.loudness_comp.clone())?;
        // crossover 频率按引擎常量固定注入（镜像 TS DYNAMIC_EQ_CROSSOVERS[i] ?? 0），
        // 协议键只暴露 enabled/targetGainDb；kneeDb/blockSize 保持模块构造默认。
        let dynamic_bands: Vec<DynamicEqBandParam> = (0..params.dynamic_eq.bands.len().min(5))
            .map(|i| DynamicEqBandParam {
                enabled: params.dynamic_eq.bands[i].enabled,
                frequency: DYNAMIC_EQ_CROSSOVERS.get(i).copied().unwrap_or(0.0),
                target_gain_db: params.dynamic_eq.bands[i].target_gain_db,
            })
            .collect();
        let dynamic_eq = DynamicEqStage::from_params(
            sample_rate,
            DynamicEqParams {
                enabled: Some(params.dynamic_eq.enabled),
                strength: Some(params.dynamic_eq.strength),
                threshold_db: Some(params.dynamic_eq.threshold_db),
                ratio: Some(params.dynamic_eq.ratio),
                attack_ms: Some(params.dynamic_eq.attack_ms),
                release_ms: Some(params.dynamic_eq.release_ms),
                knee_db: None,
                block_size: None,
                bands: Some(dynamic_bands),
            },
        )?;
        // modMatrix 构造按引擎接线顺序：setRoutes → setLfoParams → setEnvelopeParams
        // （source/target 枚举外回退由 ModSource::parse/ModTarget::parse 镜像 TS）。
        let routes: Vec<ModulationRoute> = params
            .mod_matrix
            .routes
            .iter()
            .map(|r| ModulationRoute {
                source: ModSource::parse(&r.source),
                target: ModTarget::parse(&r.target),
                amount: r.amount,
                offset: r.offset,
            })
            .collect();
        let mod_matrix = ModulationMatrixStage::from_params(
            sample_rate,
            routes,
            LfoParams {
                shape: LfoShape::parse(&params.mod_matrix.lfo.shape),
                rate_hz: params.mod_matrix.lfo.rate_hz,
                depth: params.mod_matrix.lfo.depth,
            },
            EnvelopeParams {
                attack_ms: params.mod_matrix.envelope.attack_ms,
                release_ms: params.mod_matrix.envelope.release_ms,
                amount: params.mod_matrix.envelope.amount,
            },
        )?;
        let limiter = LimiterStage::from_settings(sample_rate, params.limiter.clone())?;
        let mut chain = Self {
            mid_side,
            biquad,
            eq_chain,
            deesser,
            compressor,
            mod_effects,
            reverb,
            bass,
            loudness_comp,
            dynamic_eq,
            mod_matrix,
            limiter,
        };
        chain.prepare(max_block);
        Ok(chain)
    }

    fn prepare(&mut self, max_block: usize) {
        Stage::prepare(&mut self.mid_side, max_block);
        if let Some(b) = self.biquad.as_mut() {
            Stage::prepare(b, max_block);
        }
        if let Some(eq) = self.eq_chain.as_mut() {
            Stage::prepare(eq, max_block);
        }
        Stage::prepare(&mut self.deesser, max_block);
        Stage::prepare(&mut self.compressor, max_block);
        Stage::prepare(&mut self.mod_effects, max_block);
        match &mut self.reverb {
            ReverbRoute::Simple(s) => Stage::prepare(s, max_block),
            ReverbRoute::Fdn(f) => Stage::prepare(f, max_block),
            ReverbRoute::Convolver(c) => Stage::prepare(c, max_block),
            ReverbRoute::Off => {}
        }
        Stage::prepare(&mut self.bass, max_block);
        Stage::prepare(&mut self.loudness_comp, max_block);
        Stage::prepare(&mut self.dynamic_eq, max_block);
        Stage::prepare(&mut self.mod_matrix, max_block);
        Stage::prepare(&mut self.limiter, max_block);
    }

    /// planar 就地过链：左右声道长度恒等（Stage 契约）。
    pub fn process_planar(&mut self, left: &mut [f32], right: &mut [f32]) {
        self.mid_side.process(left, right);
        if let Some(b) = self.biquad.as_mut() {
            b.process(left, right);
        }
        if let Some(eq) = self.eq_chain.as_mut() {
            eq.process(left, right);
        }
        self.deesser.process(left, right);
        self.compressor.process(left, right);
        self.mod_effects.process(left, right);
        match &mut self.reverb {
            ReverbRoute::Simple(s) => s.process(left, right),
            ReverbRoute::Fdn(f) => f.process(left, right),
            ReverbRoute::Convolver(c) => c.process(left, right),
            ReverbRoute::Off => {} // 路由关闭：整级直通
        }
        self.bass.process(left, right);
        self.loudness_comp.process(left, right);
        self.dynamic_eq.process(left, right);
        self.mod_matrix.process(left, right);
        self.limiter.process(left, right);
    }

    /// 复位全部阶段状态（换链/重启时在非实时侧调用）。
    pub fn reset(&mut self) {
        self.mid_side.reset();
        if let Some(b) = self.biquad.as_mut() {
            b.reset();
        }
        if let Some(eq) = self.eq_chain.as_mut() {
            eq.reset();
        }
        self.deesser.reset();
        self.compressor.reset();
        self.mod_effects.reset();
        match &mut self.reverb {
            ReverbRoute::Simple(s) => s.reset(),
            ReverbRoute::Fdn(f) => f.reset(),
            ReverbRoute::Convolver(c) => c.reset(),
            ReverbRoute::Off => {}
        }
        self.bass.reset();
        self.loudness_comp.reset();
        self.dynamic_eq.reset();
        self.mod_matrix.reset();
        self.limiter.reset();
    }
}

/// 交错立体声 → planar 双声道（src 长度必须为偶数）。
pub fn deinterleave(src: &[f32], left: &mut [f32], right: &mut [f32]) {
    debug_assert_eq!(src.len(), left.len() + right.len());
    for (f, pair) in src.chunks_exact(2).enumerate() {
        left[f] = pair[0];
        right[f] = pair[1];
    }
}

/// planar 双声道 → 交错立体声。
pub fn interleave(left: &[f32], right: &[f32], dst: &mut [f32]) {
    debug_assert_eq!(dst.len(), left.len() + right.len());
    for f in 0..left.len() {
        dst[f * 2] = left[f];
        dst[f * 2 + 1] = right[f];
    }
}

#[cfg(test)]
#[allow(non_snake_case)] // 测试名引用协议键原文（eqChain/irRecipe 等 camelCase）
mod tests {
    use super::*;
    use crate::params::{parse_pilot_params, ReverbRouteKind};
    use serde_json::{json, Value};

    #[test]
    fn 交错往返无损() {
        let src: Vec<f32> = (0..16).map(|i| i as f32 * 0.25).collect();
        let mut l = vec![0.0; 8];
        let mut r = vec![0.0; 8];
        deinterleave(&src, &mut l, &mut r);
        assert_eq!(l, [0.0, 0.5, 1.0, 1.5, 2.0, 2.5, 3.0, 3.5]);
        assert_eq!(r, [0.25, 0.75, 1.25, 1.75, 2.25, 2.75, 3.25, 3.75]);
        let mut dst = vec![0.0; 16];
        interleave(&l, &r, &mut dst);
        assert_eq!(dst, src);
    }

    /// 固定种子 LCG 伪噪声（无随机依赖），[-amp, amp)。
    fn lcg_noise(n: usize, seed: u32, amp: f64) -> Vec<f32> {
        let mut u = seed;
        (0..n)
            .map(|_| {
                u = u.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((f64::from(u) / 4294967296.0 * 2.0 - 1.0) * amp) as f32
            })
            .collect()
    }

    #[test]
    fn 直通链逐位等于输入() {
        // biquad 关闭 + reverb 全干(wet=0,dry=1) + limiter 禁用 + 全部新键缺省
        // （eqChain 10×0dB / deesser 关 / modEffects 五级关 / loudnessComp custom
        // 空带 / dynamicEq 关 / modMatrix 无路由）⇒ 逐位直通。
        let mut p = PilotParams::default();
        p.reverb_simple.wet = 0.0;
        p.reverb_simple.dry = 1.0;
        p.reverb_simple.pre_delay_ms = 0.0;
        p.limiter.enabled = false;
        let mut chain = PilotSubchain::build(&p, 48000.0, 64).unwrap();
        let mut left: Vec<f32> = (-32..32).map(|i| i as f32 * 0.01).collect();
        let mut right: Vec<f32> = left.clone();
        let want_l = left.clone();
        chain.process_planar(&mut left, &mut right);
        assert_eq!(left, want_l);
        assert_eq!(right, want_l);
    }

    #[test]
    fn 静音输入不产生非有限值() {
        let mut chain = PilotSubchain::build(&PilotParams::default(), 48000.0, 128).unwrap();
        let mut l = vec![0.0_f32; 128];
        let mut r = vec![0.0_f32; 128];
        for _ in 0..8 {
            chain.process_planar(&mut l, &mut r);
        }
        assert!(l.iter().all(|x| x.is_finite()));
        assert!(r.iter().all(|x| x.is_finite()));
        assert!(l.iter().all(|x| *x == 0.0));
    }

    #[test]
    fn 正弦输入经默认链能量受限且有限() {
        let mut chain = PilotSubchain::build(&PilotParams::default(), 48000.0, 256).unwrap();
        let n = 256 * 40;
        let mut l = Vec::with_capacity(256);
        let mut r = Vec::with_capacity(256);
        let mut peak: f32 = 0.0;
        for blk in 0..(n / 256) {
            l.clear();
            r.clear();
            for i in 0..256 {
                let t = ((blk * 256 + i) as f64) / 48000.0;
                let s = (2.0 * std::f64::consts::PI * 440.0 * t).sin() as f32 * 0.5;
                l.push(s);
                r.push(s);
            }
            chain.process_planar(&mut l, &mut r);
            peak = peak.max(l.iter().fold(0.0_f32, |m, x| m.max(x.abs())));
            assert!(l.iter().all(|x| x.is_finite()));
        }
        assert!(peak <= 1.0, "限幅后峰值应不超过 1.0，实际 {}", peak);
    }

    // ---------- Phase 3 全序链新增锚点 ----------

    /// 经 parse_pilot_params 装配（显式覆盖新键）+ 直通基准，返回 (chain, 输入)。
    fn build_parsed(base: serde_json::Value, extra: serde_json::Value) -> (PilotSubchain, Vec<f32>, Vec<f32>) {
        let mut obj = base.as_object().cloned().unwrap();
        for (k, v) in extra.as_object().unwrap() {
            obj.insert(k.clone(), v.clone());
        }
        let (p, warnings) = parse_pilot_params(&serde_json::Value::Object(obj)).unwrap();
        assert!(warnings.is_empty(), "锚点快照不应产生 warnings：{warnings:?}");
        let chain = PilotSubchain::build(&p, 48000.0, 256).unwrap();
        let in_l = lcg_noise(256, 42, 0.9);
        let in_r = lcg_noise(256, 43, 0.8);
        (chain, in_l, in_r)
    }

    fn bypass_json() -> serde_json::Value {
        json!({
            "reverbSimple": {"wet": 0.0, "dry": 1.0, "preDelayMs": 0.0},
            "limiter": {"enabled": false}
        })
    }

    #[test]
    fn 全新键显式缺省快照_逐位直通() {
        // 全部新键以「缺省直通形态」显式出现在快照中：解析 + 装配 + 过链全程
        // 不得破坏直通回归锚（新增键的缺省形态即关闭/恒等）。eqChain 采用
        // hse-core GWT-EQ-01 零增益锚点同配置（该 (f,q) 集合下 0dB 级联逐位
        // 直通——design_biquad 以 b0×(1/a0) 归一化，部分 (f,q) 组合存在 ±1 ulp
        // 偏差，任意 (f,q) 的 0dB 不作逐位承诺，见 params.rs eq_chain 文档）。
        let five_bands: Vec<Value> =
            (0..5).map(|_| json!({"enabled": true, "targetGainDb": 0})).collect();
        let eq_bands: Vec<Value> = [40.0, 80.0, 160.0, 320.0, 640.0, 1280.0, 2560.0, 5120.0, 10240.0, 16000.0]
            .iter()
            .zip([0.5, 0.8, 1.0, 1.2, 1.4, 2.0, 3.0, 4.0, 0.707, 6.0])
            .map(|(&f, q)| json!({"frequency": f, "gain": 0, "q": q}))
            .collect();
        let extra = json!({
            "eqChain": {"bands": eq_bands, "bandCount": 10, "qCompensation": false},
            "deesser": {"enabled": false, "centerHz": 6000, "q": 0.7, "thresholdDb": -30,
                        "ratio": 8, "attackMs": 1, "releaseMs": 80, "splitBand": true, "mix": 1},
            "modEffects": {"delay": {"enabled": false}, "chorus": {"enabled": false},
                           "flanger": {"enabled": false}, "phaser": {"enabled": false},
                           "tremolo": {"enabled": false}},
            "reverbRoute": "simple",
            "loudnessComp": {"mode": "custom", "bands": [], "volumePercent": 80,
                             "maxBoostDb": 12, "smoothingSeconds": 0.2},
            "dynamicEq": {"enabled": false, "strength": 0.5, "thresholdDb": -20, "ratio": 2,
                          "attackMs": 20, "releaseMs": 200, "bands": five_bands},
            "modMatrix": {"routes": [], "lfo": {"shape": "sine", "rateHz": 1, "depth": 0.5},
                          "envelope": {"attackMs": 10, "releaseMs": 200, "amount": 0.5}},
            "fdnReverb": {"roomSize": 0.5, "damping": 0.5, "wet": 0.3, "dry": 0.7,
                          "preDelayMs": 0, "width": 1, "type": "hall", "lines": 8},
            "convolver": {"mix": 0.3, "preDelayMs": 0}
        });
        let (mut chain, in_l, in_r) = build_parsed(bypass_json(), extra);
        let mut left = in_l.clone();
        let mut right = in_r.clone();
        chain.process_planar(&mut left, &mut right);
        assert_eq!(left, in_l);
        assert_eq!(right, in_r);
    }

    #[test]
    fn eqChain_全零增益锚点逐位直通_与hse_core同配置() {
        // 规格 §4.3 锚点（GWT-EQ-01 投影）：hse-core 零增益直通单测同款 (f,q) 集合
        // —— 0dB peaking 级联逐位直通（该集合下归一化精确还原 b0=1，级联状态恒零）。
        let eq_bands: Vec<Value> = [40.0, 80.0, 160.0, 320.0, 640.0, 1280.0, 2560.0, 5120.0, 10240.0, 16000.0]
            .iter()
            .zip([0.5, 0.8, 1.0, 1.2, 1.4, 2.0, 3.0, 4.0, 0.707, 6.0])
            .map(|(&f, q)| json!({"frequency": f, "gain": 0, "q": q}))
            .collect();
        let extra = json!({"eqChain": {"bands": eq_bands, "bandCount": 10, "qCompensation": false}});
        let (mut chain, in_l, in_r) = build_parsed(bypass_json(), extra);
        let mut left = in_l.clone();
        let mut right = in_r.clone();
        chain.process_planar(&mut left, &mut right);
        assert_eq!(left, in_l, "0dB 级联必须逐位直通（左）");
        assert_eq!(right, in_r, "0dB 级联必须逐位直通（右，共享状态续跑）");
    }

    #[test]
    fn eqChain_非零增益改变输出且有限() {
        let extra = json!({"eqChain": {"bands": [{"frequency": 1000, "gain": 6, "q": 1.0}], "bandCount": 1, "qCompensation": true}});
        let (mut chain, in_l, in_r) = build_parsed(bypass_json(), extra);
        let mut left = in_l.clone();
        let mut right = in_r.clone();
        chain.process_planar(&mut left, &mut right);
        assert!(left.iter().all(|x| x.is_finite()) && right.iter().all(|x| x.is_finite()));
        assert_ne!(left, in_l, "非零增益 EQ 不得恒等直通");
    }

    #[test]
    fn modEffects_五级全禁用逐位直通_单级开启改变输出且有限() {
        // 全禁用（显式）→ 逐位。
        let extra = json!({
            "modEffects": {
                "delay": {"enabled": false, "delayMs": 120},
                "chorus": {"enabled": false},
                "flanger": {"enabled": false},
                "phaser": {"enabled": false},
                "tremolo": {"enabled": false}
            }
        });
        let (mut chain, in_l, in_r) = build_parsed(bypass_json(), extra);
        let mut left = in_l.clone();
        let mut right = in_r.clone();
        chain.process_planar(&mut left, &mut right);
        assert_eq!(left, in_l);
        assert_eq!(right, in_r);
        // 逐级开启 → 输出有限且非恒等（引擎接线顺序级联的激活烟测）。
        for effect in ["delay", "chorus", "flanger", "phaser", "tremolo"] {
            let extra = json!({"modEffects": {effect: {"enabled": true}}});
            let (mut chain, in_l, in_r) = build_parsed(bypass_json(), extra);
            let mut left = in_l.clone();
            let mut right = in_r.clone();
            for _ in 0..4 {
                chain.process_planar(&mut left, &mut right);
            }
            assert!(
                left.iter().all(|x| x.is_finite()) && right.iter().all(|x| x.is_finite()),
                "{effect} 开启后输出必须有限"
            );
            assert_ne!(left, in_l, "{effect} 开启后不得恒等直通");
        }
    }

    #[test]
    fn modMatrix_无路由恒等_矩阵仍逐块推进() {
        // 无路由但 LFO 全深度推进：masterGain 基线 1 → ×1.0 逐位还原；
        // 同时固化「矩阵推进与路由表无关」的引擎语义（状态推进可观测于后续路由）。
        let extra = json!({
            "modMatrix": {
                "routes": [],
                "lfo": {"shape": "saw", "rateHz": 20, "depth": 1},
                "envelope": {"attackMs": 1, "releaseMs": 1, "amount": 1}
            }
        });
        let (mut chain, in_l, in_r) = build_parsed(bypass_json(), extra);
        let mut left = in_l.clone();
        let mut right = in_r.clone();
        for _ in 0..4 {
            chain.process_planar(&mut left, &mut right);
        }
        assert_eq!(left, in_l, "无路由时 masterGain=1 必须逐位恒等");
        assert_eq!(right, in_r);
    }

    #[test]
    fn modMatrix_有路由masterGain调制_输出有限且有界非恒等() {
        let extra = json!({
            "modMatrix": {
                "routes": [{"source": "lfo", "target": "masterGain", "amount": 0.5, "offset": 0}],
                "lfo": {"shape": "square", "rateHz": 30, "depth": 1}
            }
        });
        let (mut chain, in_l, in_r) = build_parsed(bypass_json(), extra);
        let mut left = in_l.clone();
        let mut right = in_r.clone();
        for _ in 0..8 {
            chain.process_planar(&mut left, &mut right);
        }
        assert!(left.iter().all(|x| x.is_finite()) && right.iter().all(|x| x.is_finite()));
        // masterGain 钳制域 [0,4] ⇒ 幅度界 = 输入界 × 4 + 余量。
        assert!(left.iter().all(|x| x.abs() <= 4.0), "masterGain 调制输出越界");
        assert_ne!(left, in_l, "调制路由生效时不得恒等直通");
    }

    #[test]
    fn reverbRoute_三路切换烟测_关闭路逐位直通() {
        // off：整级直通（逐位）。
        let extra = json!({"reverbRoute": "off"});
        let (mut chain, in_l, in_r) = build_parsed(bypass_json(), extra);
        let mut left = in_l.clone();
        let mut right = in_r.clone();
        for _ in 0..4 {
            chain.process_planar(&mut left, &mut right);
        }
        assert_eq!(left, in_l, "reverbRoute=off 必须逐位直通");
        assert_eq!(right, in_r);
        // fdn：湿声有限、非恒等。
        let extra = json!({
            "reverbRoute": "fdn",
            "fdnReverb": {"roomSize": 0.6, "damping": 0.4, "wet": 0.3, "dry": 0.7, "type": "hall", "lines": 8}
        });
        let (mut chain, in_l, in_r) = build_parsed(bypass_json(), extra);
        let mut left = in_l.clone();
        let mut right = in_r.clone();
        for _ in 0..4 {
            chain.process_planar(&mut left, &mut right);
        }
        assert!(left.iter().all(|x| x.is_finite()) && right.iter().all(|x| x.is_finite()));
        assert_ne!(left, in_l, "FDN 湿声路由不得恒等直通");
        // convolver（expNoise 配方）：湿声有限、非恒等。
        let extra = json!({
            "reverbRoute": "convolver",
            "convolver": {"irRecipe": {"kind": "expNoise", "length": 2048, "seed": 42, "decay": 4.0, "amp": 0.4}, "mix": 0.5}
        });
        let (mut chain, in_l, in_r) = build_parsed(bypass_json(), extra);
        let mut left = in_l.clone();
        let mut right = in_r.clone();
        for _ in 0..4 {
            chain.process_planar(&mut left, &mut right);
        }
        assert!(left.iter().all(|x| x.is_finite()) && right.iter().all(|x| x.is_finite()));
        assert_ne!(left, in_l, "卷积湿声路由不得恒等直通");
        // 简单路（既有行为回归）：route 显式 simple 与缺省等价。
        let extra = json!({"reverbRoute": "simple"});
        let (mut chain, in_l, in_r) = build_parsed(bypass_json(), extra);
        let mut left = in_l.clone();
        let mut right = in_r.clone();
        chain.process_planar(&mut left, &mut right);
        assert_eq!(left, in_l, "simple 路在 bypass 配置下仍逐位直通");
    }

    #[test]
    fn convolver_delta_IR_湿路延迟等于分区长_mix0干路逐位() {
        // delta(delay=0) 配方 + mix=1：输出 = 输入延迟 partitionSize（模块默认 512）。
        let extra = json!({
            "reverbRoute": "convolver",
            "convolver": {"irRecipe": {"kind": "delta", "delay": 0}, "mix": 1.0, "preDelayMs": 0}
        });
        let (mut chain, _in_l, _in_r) = build_parsed(bypass_json(), extra);
        let n = 1024;
        let mut left = vec![0.0_f32; n];
        let mut right = vec![0.0_f32; n];
        left[0] = 1.0;
        right[0] = 1.0;
        chain.process_planar(&mut left, &mut right);
        let ls = 512; // ConvolverOptions::default().partition_size
        for i in 0..n {
            let want = if i == ls { 1.0 } else { 0.0 };
            assert!(
                (f64::from(left[i]) - want).abs() < 1e-4,
                "delta IR 冲激应出现在湿路延迟 {ls} 处 @i={i}：{}",
                left[i]
            );
            assert!((f64::from(right[i]) - want).abs() < 1e-4, "右声道同延迟 @i={i}");
        }
        // mix=0：干路逐位还原（除 -0.0 符号位）。
        let extra = json!({
            "reverbRoute": "convolver",
            "convolver": {"irRecipe": {"kind": "delta", "delay": 0}, "mix": 0.0, "preDelayMs": 0}
        });
        let (mut chain, in_l, in_r) = build_parsed(bypass_json(), extra);
        let mut left = in_l.clone();
        let mut right = in_r.clone();
        chain.process_planar(&mut left, &mut right);
        assert_eq!(left, in_l, "mix=0 卷积路干路必须逐位直通");
        assert_eq!(right, in_r);
    }

    #[test]
    fn 逐级激活烟测_每新级单独开启输出有限有界() {
        // deesser / loudnessComp(auto) / dynamicEq 三级的单独激活
        // （eqChain/modEffects/modMatrix/reverb 路由已有各自的激活烟测）。
        let five_bands: Vec<Value> =
            (0..5).map(|_| json!({"enabled": true, "targetGainDb": 0})).collect();
        let cases: Vec<(&str, serde_json::Value)> = vec![
            (
                "deesser",
                json!({"deesser": {"enabled": true, "centerHz": 6000, "q": 0.7,
                                   "thresholdDb": -30, "ratio": 8, "attackMs": 1,
                                   "releaseMs": 80, "splitBand": true, "mix": 1}}),
            ),
            (
                "loudnessComp",
                json!({"loudnessComp": {"mode": "auto", "volumePercent": 40, "maxBoostDb": 12,
                                        "smoothingSeconds": 0.1}}),
            ),
            (
                "loudnessComp-preset",
                json!({"loudnessComp": {"mode": "preset", "preset": "night"}}),
            ),
            (
                "dynamicEq",
                json!({"dynamicEq": {"enabled": true, "strength": 0.8, "thresholdDb": -30,
                                     "ratio": 3, "attackMs": 5, "releaseMs": 100,
                                     "bands": five_bands}}),
            ),
        ];
        for (name, extra) in cases {
            let (mut chain, in_l, in_r) = build_parsed(bypass_json(), extra);
            let mut left = in_l.clone();
            let mut right = in_r.clone();
            for _ in 0..8 {
                chain.process_planar(&mut left, &mut right);
            }
            assert!(
                left.iter().all(|x| x.is_finite()) && right.iter().all(|x| x.is_finite()),
                "{name} 激活后输出必须有限"
            );
            assert!(
                left.iter().all(|x| x.abs() <= 4.0),
                "{name} 激活后输出应受界（无爆音发散）"
            );
        }
    }

    #[test]
    fn loudnessComp_custom曲线激活_非恒等且有限() {
        let extra = json!({
            "loudnessComp": {"mode": "custom",
                             "bands": [{"frequency": 100, "gain": 6}, {"frequency": 1000, "gain": -3}]}
        });
        let (mut chain, in_l, in_r) = build_parsed(bypass_json(), extra);
        let mut left = in_l.clone();
        let mut right = in_r.clone();
        for _ in 0..8 {
            chain.process_planar(&mut left, &mut right);
        }
        assert!(left.iter().all(|x| x.is_finite()));
        assert_ne!(left, in_l, "custom 曲线激活后不得恒等直通");
    }

    #[test]
    fn dynamicEq_参数缺省形状带交叉注入构建成功() {
        // bands 提供不足 5 项：缺项带保持构造默认（enabled=true、静态偏移 0），
        // crossover 由服务侧注入 [200,800,2500,8000]——构建成功且默认 disabled
        // 下为硬直通。
        let extra = json!({"dynamicEq": {"enabled": false, "bands": [{"enabled": true, "targetGainDb": 2}]}});
        let (mut chain, in_l, in_r) = build_parsed(bypass_json(), extra);
        let mut left = in_l.clone();
        let mut right = in_r.clone();
        for _ in 0..4 {
            chain.process_planar(&mut left, &mut right);
        }
        assert_eq!(left, in_l, "dynamicEq disabled 为硬直通");
        assert_eq!(right, in_r);
    }

    #[test]
    fn convolver路由缺IR配方_构建拒绝() {
        // route=convolver 但无 irRecipe → build 报错（调用方映射 -32602）。
        let extra = json!({"reverbRoute": "convolver"});
        let (p, _) = parse_pilot_params(
            &{
                let mut obj = bypass_json().as_object().cloned().unwrap();
                for (k, v) in extra.as_object().unwrap() {
                    obj.insert(k.clone(), v.clone());
                }
                serde_json::Value::Object(obj)
            },
        )
        .unwrap();
        assert!(PilotSubchain::build(&p, 48000.0, 256).is_err(), "缺 IR 配方必须构建失败");
        assert_eq!(p.reverb_route, ReverbRouteKind::Convolver);
    }
}
