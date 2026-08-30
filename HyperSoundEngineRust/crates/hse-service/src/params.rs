//! setParams 参数快照解析（specs/service/control-plane.md §5.6）。
//!
//! 校验分层：协议层只做「键存在性 + JSON 类型匹配」的结构检查——
//! - 可识别顶层键内**未知的子键**：忽略并记 warnings，元素形如 "顶层键.子键"
//!   （嵌套子对象内为 "顶层键.子对象.子键"，如 "modEffects.delay.foo"）；
//! - **未知的顶层键**：整体忽略并记 warnings，元素为该键名原文；
//! - 子键**类型不符**属结构违规 → 整体拒绝（调用方映射 -32602），不做静默回退；
//! - 数值越界/枚举外取值不在此层判定，交由模块自身 clamp/回退（如 reverbSimple.type
//!   未知值按模块规格回退 hall），不产生 warnings、不算错误。唯一例外是
//!   `convolver.irRecipe.kind`：IR 配方无「模块内回退形态」，判别值未知时无法
//!   成形对象，按结构违规拒绝（与 hse-parity 驱动器行为一致）。
//! warnings 最终按字典序升序排列（确定性输出）。
//! 缺省值对齐 TS 支线 createDefaultParams 与各模块构造默认（快照整体替换语义：
//! 省略的顶层键回落内置缺省，不是增量合并）。

use std::collections::HashSet;

use hse_core::bass_enhancer::BassEnhancerSettings;
use hse_core::compressor::CompressorSettings;
use hse_core::convolver::IrRecipe;
use hse_core::deesser::DeesserSettings;
use hse_core::fdn_reverb::FdnReverbParams;
use hse_core::limiter::LimiterSettings;
use hse_core::loudness_comp::LoudnessCompSettings;
use hse_core::mod_effects::{
    ChorusSettings, DelaySettings, FlangerSettings, ModEffectsSettings, PhaserSettings,
    TremoloSettings,
};
use hse_core::reverb_simple::ReverbSimpleParams;
use serde_json::{json, Map, Value};

/// 单只 biquad 的显式规格（来自 setParams.biquad）。
#[derive(Debug, Clone)]
pub struct BiquadSpec {
    pub filter_type: String,
    pub f0: f64,
    pub q: f64,
    pub gain_db: f64,
}

/// MidSide 的可配置对（对应全链第 3 级；width 即 TS stereoWidth，vb 仅 pitch 启用时非零）。
#[derive(Debug, Clone)]
pub struct MidSideParams {
    pub width: f64,
    pub voice_balance: f64,
}

/// eqChain 单段参数（对齐 TS EqBandParam；越界钳制由 EqChainStage.set_bands 完成）。
#[derive(Debug, Clone, Copy)]
pub struct EqBandSpec {
    pub frequency: f64,
    pub gain: f64,
    pub q: f64,
}

/// eqChain 键快照（对齐 TS createDefaultParams().eq 的处理相关子集；
/// 缺省 = 10 段全 0 增益 peaking + 级联 Q 补偿开启——0 增益级联为逐位直通）。
#[derive(Debug, Clone)]
pub struct EqChainSpec {
    pub bands: Vec<EqBandSpec>,
    pub band_count: f64,
    pub q_compensation: bool,
}

/// dynamicEq 单带协议形态：crossover 频率由服务侧按引擎常量 [200,800,2500,8000]
/// 固定注入（不暴露协议键，镜像 TS 引擎 DYNAMIC_EQ_CROSSOVERS 行为）。
#[derive(Debug, Clone, Copy)]
pub struct DynamicEqBandSpec {
    pub enabled: bool,
    /// None = 保持该带当前/默认静态偏移（TS 可选 targetGainDb 语义）。
    pub target_gain_db: Option<f64>,
}

/// dynamicEq 键快照（对齐 TS createDefaultParams().dynamicEq；
/// 缺省 enabled=false → DynamicEqStage 硬直通）。
#[derive(Debug, Clone)]
pub struct DynamicEqSpec {
    pub enabled: bool,
    pub strength: f64,
    pub threshold_db: f64,
    pub ratio: f64,
    pub attack_ms: f64,
    pub release_ms: f64,
    pub bands: Vec<DynamicEqBandSpec>,
}

/// convolver 键快照：IR 由确定性配方描述（specs/dsp/convolver.md §4.2 的
/// delta / expNoise 两形态，与 hse-parity 冻结向量同源）；分区尺寸等卷积
/// 规划选项使用模块默认（512/4096/100ms/去周期化），不暴露协议键。
#[derive(Debug, Clone)]
pub struct ConvolverSpec {
    /// None = 未配置 IR；route=convolver 时构建失败（-32602）。
    pub ir_recipe: Option<IrRecipe>,
    pub mix: f64,
    pub pre_delay_ms: f64,
}

/// modMatrix 单条路由（source/target 原样下发，枚举外回退由
/// ModSource::parse/ModTarget::parse 完成，镜像 TS 求值语义）。
#[derive(Debug, Clone)]
pub struct ModMatrixRouteSpec {
    pub source: String,
    pub target: String,
    pub amount: f64,
    /// TS `route.offset ?? 0`：缺省 0。
    pub offset: f64,
}

#[derive(Debug, Clone)]
pub struct ModMatrixLfoSpec {
    pub shape: String,
    pub rate_hz: f64,
    pub depth: f64,
}

#[derive(Debug, Clone)]
pub struct ModMatrixEnvelopeSpec {
    pub attack_ms: f64,
    pub release_ms: f64,
    pub amount: f64,
}

/// modMatrix 键快照（对齐 TS createDefaultParams().modulation 的处理相关子集；
/// 缺省 routes 空 → masterGain 基线 1 → 逐位恒等）。
#[derive(Debug, Clone)]
pub struct ModMatrixSpec {
    pub routes: Vec<ModMatrixRouteSpec>,
    pub lfo: ModMatrixLfoSpec,
    pub envelope: ModMatrixEnvelopeSpec,
}

/// reverb 级三路路由（对齐 TS ReverbMode 的处理语义：
/// simple=算法混响、fdn=FDN 网络、convolver=分区卷积、off=整级直通）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReverbRouteKind {
    Simple,
    Fdn,
    Convolver,
    Off,
}

/// 引擎子链的参数快照（控制面可识别键：midSide / biquad / eqChain / deesser /
/// compressor / modEffects / reverbSimple / reverbRoute / fdnReverb / convolver /
/// bassEnhancer / loudnessComp / dynamicEq / modMatrix / limiter——按全链相对顺序入链）。
#[derive(Debug, Clone)]
pub struct PilotParams {
    pub mid_side: MidSideParams,
    /// None = 未配置滤波器，按 TS 构造默认直通。
    pub biquad: Option<BiquadSpec>,
    /// None = 未配置 Pre-EQ 级联 → **不装配该级（逐位直通）**。
    ///
    /// 与 `biquad` 键同一先例（None = 级不存在）。这是对「缺省链逐位直通」
    /// 回归锚的让步：TS createDefaultParams().eq（enabled=true + 10×0dB + Q 补偿）
    /// 在浮点上只是 ~1e-15 级恒等而非逐位——design_biquad 以 `b0 × (1/a0)` 归一化，
    /// 部分 (f, q) 组合下 `b0·inv ≠ 1.0`（±1 ulp），0dB peaking 级联因此不保证逐位
    /// 还原。显式给出 `eqChain` 键时，省略的子键回落 TS 形态（10 段 0dB / Q 补偿开）。
    pub eq_chain: Option<EqChainSpec>,
    pub deesser: DeesserSettings,
    pub compressor: CompressorSettings,
    pub mod_effects: ModEffectsSettings,
    pub reverb_simple: ReverbSimpleParams,
    pub reverb_route: ReverbRouteKind,
    pub fdn_reverb: FdnReverbParams,
    pub convolver: ConvolverSpec,
    pub bass_enhancer: BassEnhancerSettings,
    pub loudness_comp: LoudnessCompSettings,
    pub dynamic_eq: DynamicEqSpec,
    pub mod_matrix: ModMatrixSpec,
    pub limiter: LimiterSettings,
}

/// TS PRO_EQ_DEFAULT_BANDS：专业 10 段（octave）中心频率。
const PRO_EQ_DEFAULT_BANDS: [f64; 10] = [
    31.5, 63.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0,
];

impl Default for PilotParams {
    fn default() -> Self {
        Self {
            // 对齐 TS createDefaultParams().stereoWidth（M/S 恒活跃，width=1 恒等）。
            mid_side: MidSideParams {
                width: 1.0,
                voice_balance: 0.0,
            },
            biquad: None,
            // 对齐 TS createDefaultParams().eq 的处理形态（10 段全 0 增益 + qComp），
            // 但**仅在显式下发 eqChain 键时生效**；缺省 None = 级不装配（逐位直通），
            // 理由见 PilotParams.eq_chain 字段文档。
            eq_chain: None,
            // 对齐 TS createDefaultParams().deesser（enabled=false → 恒等直通）。
            deesser: DeesserSettings {
                enabled: false,
                center_hz: 6000.0,
                q: 0.7,
                threshold_db: -30.0,
                ratio: 8.0,
                attack_ms: 1.0,
                release_ms: 80.0,
                split_band: true,
                mix: 1.0,
                sidechain_enabled: false,
            },
            // 对齐 TS createDefaultParams().compressor。
            compressor: CompressorSettings {
                enabled: false,
                threshold_db: -20.0,
                ratio: 4.0,
                knee_db: 6.0,
                attack_ms: 10.0,
                release_ms: 150.0,
                makeup_db: 0.0,
                output_gain: 1.0,
                sidechain_enabled: false,
            },
            // 对齐 TS createDefaultParams().modEffects（五级全部 disabled → 逐位直通）。
            mod_effects: ModEffectsSettings {
                delay: DelaySettings {
                    enabled: false,
                    delay_ms: 250.0,
                    feedback: 0.3,
                    mix: 0.3,
                },
                chorus: ChorusSettings {
                    enabled: false,
                    rate_hz: 1.0,
                    depth_ms: 3.0,
                    mix: 0.4,
                },
                flanger: FlangerSettings {
                    enabled: false,
                    rate_hz: 0.5,
                    depth_ms: 2.0,
                    feedback: 0.4,
                    mix: 0.5,
                },
                phaser: PhaserSettings {
                    enabled: false,
                    rate_hz: 0.5,
                    depth: 0.5,
                    feedback: 0.4,
                    mix: 0.5,
                    stages: 4.0,
                },
                tremolo: TremoloSettings {
                    enabled: false,
                    rate_hz: 5.0,
                    depth: 0.5,
                    mix: 1.0,
                },
            },
            // 对齐 TS createDefaultParams().reverb.algorithmic。
            reverb_simple: ReverbSimpleParams {
                room_size: 0.5,
                damping: 0.5,
                wet: 0.3,
                dry: 0.7,
                pre_delay_ms: 0.0,
                width: 1.0,
                reverb_type: "hall".to_string(),
            },
            // 路由键为服务链自有形态（TS 由 reverb.enabled+mode 组合表达）；
            // 缺省 simple = 既有六模块链行为。
            reverb_route: ReverbRouteKind::Simple,
            // 对齐 TS createDefaultParams().reverb.algorithmic 同源字段（FDN 以其
            // 为基准参数；lines 缺省 8 由 FdnReverbParams.lines=None 表达）。
            fdn_reverb: FdnReverbParams {
                room_size: 0.5,
                damping: 0.5,
                wet: 0.3,
                dry: 0.7,
                pre_delay_ms: 0.0,
                width: 1.0,
                reverb_type: "hall".to_string(),
                lines: None,
            },
            // 对齐 TS createDefaultParams().reverb.convolution（ir=null → 配方 None）。
            convolver: ConvolverSpec {
                ir_recipe: None,
                mix: 0.3,
                pre_delay_ms: 0.0,
            },
            // 对齐 TS createDefaultParams().bassEnhancer。
            bass_enhancer: BassEnhancerSettings {
                enabled: false,
                cutoff_hz: 90.0,
                q: 0.7,
                harmonic_type: "odd".to_string(),
                harmonic_gain: 0.6,
                mix: 0.5,
                level_db: 0.0,
                low_boost_db: Some(0.0),
            },
            // TS createDefaultParams().loudnessCompensation = {enabled:false, mode:'auto',
            // preset:'flat', bands:[], volumePercent:80, maxBoostDb:12, smoothingSeconds:0.2}。
            // LoudnessCompSettings 无 enabled 字段（引擎层门控在 TS 由
            // loudnessCompensation.enabled 承担；本链以参数形态表达等价「关闭」）：
            // 缺省 mode 取 **custom + 空 bands**——目标曲线全 0、平滑增益恒 0，
            // 构造期恒等系数不参与重算 → 逐位直通（custom+[] 是 LoudnessCompStage
            // 唯一的确定性零目标形态；TS 缺省 mode='auto' 在 volumePercent=80 下
            // 会产生 +2.4dB 低频架的目标，非直通，故不能照抄）。
            loudness_comp: LoudnessCompSettings {
                volume_percent: 80.0,
                max_boost_db: 12.0,
                preset: "flat".to_string(),
                bands: Vec::new(),
                mode: "custom".to_string(),
                smoothing_seconds: 0.2,
            },
            // 对齐 TS createDefaultParams().dynamicEq（enabled=false → 硬直通）。
            // kneeDb/blockSize 不暴露协议键，保持模块构造默认（6/128）。
            dynamic_eq: DynamicEqSpec {
                enabled: false,
                strength: 0.5,
                threshold_db: -20.0,
                ratio: 2.0,
                attack_ms: 20.0,
                release_ms: 200.0,
                bands: (0..5)
                    .map(|_| DynamicEqBandSpec {
                        enabled: true,
                        target_gain_db: Some(0.0),
                    })
                    .collect(),
            },
            // 对齐 TS createDefaultParams().modulation 处理子集（enabled=false 的
            // 等价形态即 routes=[]：masterGain 基线 1 → 逐位恒等；lfo/envelope 的
            // enabled 标志核心模块不读取，仅快照形状对齐 TS 字段值）。
            mod_matrix: ModMatrixSpec {
                routes: Vec::new(),
                lfo: ModMatrixLfoSpec {
                    shape: "sine".to_string(),
                    rate_hz: 1.0,
                    depth: 0.5,
                },
                envelope: ModMatrixEnvelopeSpec {
                    attack_ms: 10.0,
                    release_ms: 200.0,
                    amount: 0.5,
                },
            },
            // 对齐 TS createDefaultParams().limiter（与 LimiterStage::new 一致）。
            limiter: LimiterSettings {
                enabled: true,
                threshold_db: -1.0,
                lookahead_ms: 5.0,
                attack_ms: 0.5,
                release_ms: 150.0,
                true_peak: true,
            },
        }
    }
}

impl PilotParams {
    /// 将旧版服务 wire 快照投影为 hse-core 接受的完整 HyperSoundEngineParams。
    ///
    /// wire 仍保持整体替换语义；未暴露的完整链级使用 core canonical 默认值。
    /// 空间级始终强制 off，HseStretch 不属于该参数树与主链。
    pub fn to_canonical_json(&self, source: &Value, sample_rate: f64) -> Result<Value, String> {
        let mut canonical = hse_core::params::default_params(sample_rate);
        canonical["sampleRate"] = json!(sample_rate);
        canonical["spatial"]["mode"] = json!("off");
        canonical["stereoWidth"] = json!(self.mid_side.width);
        canonical["pitch"]["enabled"] = json!(self.mid_side.voice_balance != 0.0);
        canonical["pitch"]["voiceBalance"] = json!(self.mid_side.voice_balance);

        let mut eq_bands = Vec::new();
        if let Some(biquad) = &self.biquad {
            // canonical Pre-EQ 是 peaking 段数组；旧 biquad 键继续接受并投影到同一位置。
            eq_bands.push(json!({
                "frequency": biquad.f0,
                "gain": biquad.gain_db,
                "q": biquad.q,
            }));
        }
        if let Some(eq) = &self.eq_chain {
            eq_bands.extend(eq.bands.iter().map(|band| {
                json!({
                    "frequency": band.frequency,
                    "gain": band.gain,
                    "q": band.q,
                })
            }));
            canonical["eq"]["qCompensation"] = json!(eq.q_compensation);
        }
        canonical["eq"]["enabled"] = json!(!eq_bands.is_empty());
        canonical["eq"]["mode"] = json!("pro");
        canonical["eq"]["bandCount"] = json!(eq_bands.len().min(20));
        canonical["eq"]["proBands"] = Value::Array(eq_bands);

        canonical["deesser"] = json!({
            "enabled": self.deesser.enabled,
            "centerHz": self.deesser.center_hz,
            "q": self.deesser.q,
            "thresholdDb": self.deesser.threshold_db,
            "ratio": self.deesser.ratio,
            "attackMs": self.deesser.attack_ms,
            "releaseMs": self.deesser.release_ms,
            "splitBand": self.deesser.split_band,
            "mix": self.deesser.mix,
            "sidechainEnabled": self.deesser.sidechain_enabled,
        });
        canonical["compressor"] = json!({
            "enabled": self.compressor.enabled,
            "thresholdDb": self.compressor.threshold_db,
            "ratio": self.compressor.ratio,
            "kneeDb": self.compressor.knee_db,
            "attackMs": self.compressor.attack_ms,
            "releaseMs": self.compressor.release_ms,
            "makeupDb": self.compressor.makeup_db,
            "outputGain": self.compressor.output_gain,
            "sidechainEnabled": self.compressor.sidechain_enabled,
        });
        canonical["modEffects"] = json!({
            "delay": {"enabled":self.mod_effects.delay.enabled,"delayMs":self.mod_effects.delay.delay_ms,"feedback":self.mod_effects.delay.feedback,"mix":self.mod_effects.delay.mix},
            "chorus": {"enabled":self.mod_effects.chorus.enabled,"rateHz":self.mod_effects.chorus.rate_hz,"depthMs":self.mod_effects.chorus.depth_ms,"mix":self.mod_effects.chorus.mix},
            "flanger": {"enabled":self.mod_effects.flanger.enabled,"rateHz":self.mod_effects.flanger.rate_hz,"depthMs":self.mod_effects.flanger.depth_ms,"feedback":self.mod_effects.flanger.feedback,"mix":self.mod_effects.flanger.mix},
            "phaser": {"enabled":self.mod_effects.phaser.enabled,"rateHz":self.mod_effects.phaser.rate_hz,"depth":self.mod_effects.phaser.depth,"feedback":self.mod_effects.phaser.feedback,"mix":self.mod_effects.phaser.mix,"stages":self.mod_effects.phaser.stages},
            "tremolo": {"enabled":self.mod_effects.tremolo.enabled,"rateHz":self.mod_effects.tremolo.rate_hz,"depth":self.mod_effects.tremolo.depth,"mix":self.mod_effects.tremolo.mix},
        });

        let (reverb_enabled, reverb_mode) = match self.reverb_route {
            ReverbRouteKind::Simple => (true, "algorithmic"),
            ReverbRouteKind::Fdn => (true, "fdn"),
            ReverbRouteKind::Convolver => (true, "convolution"),
            ReverbRouteKind::Off => (false, "off"),
        };
        let algorithmic = if self.reverb_route == ReverbRouteKind::Fdn {
            json!({"roomSize":self.fdn_reverb.room_size,"damping":self.fdn_reverb.damping,
                "wet":self.fdn_reverb.wet,"dry":self.fdn_reverb.dry,
                "preDelayMs":self.fdn_reverb.pre_delay_ms,"width":self.fdn_reverb.width,
                "type":self.fdn_reverb.reverb_type})
        } else {
            json!({"roomSize":self.reverb_simple.room_size,"damping":self.reverb_simple.damping,
                "wet":self.reverb_simple.wet,"dry":self.reverb_simple.dry,
                "preDelayMs":self.reverb_simple.pre_delay_ms,"width":self.reverb_simple.width,
                "type":self.reverb_simple.reverb_type})
        };
        let ir = match &self.convolver.ir_recipe {
            Some(recipe) => Some(hse_core::convolver::build_ir_recipe(recipe)?),
            None => None,
        };
        canonical["reverb"] = json!({
            "enabled": reverb_enabled,
            "mode": reverb_mode,
            "algorithmic": algorithmic,
            "convolution": {
                "ir": ir,
                "irName": if ir.is_some() { Value::String("setParams-recipe".into()) } else { Value::Null },
                "mix": self.convolver.mix,
                "preDelayMs": self.convolver.pre_delay_ms,
                "dePeriodize": true,
            }
        });
        if self.reverb_route == ReverbRouteKind::Convolver && ir.is_none() {
            return Err("convolver 路由需要 convolver.irRecipe（delta / expNoise 配方）".into());
        }

        canonical["bassEnhancer"] = json!({
            "enabled":self.bass_enhancer.enabled,"cutoffHz":self.bass_enhancer.cutoff_hz,
            "q":self.bass_enhancer.q,"harmonicType":self.bass_enhancer.harmonic_type,
            "harmonicGain":self.bass_enhancer.harmonic_gain,"mix":self.bass_enhancer.mix,
            "levelDb":self.bass_enhancer.level_db,"lowBoostDb":self.bass_enhancer.low_boost_db,
        });
        canonical["loudnessCompensation"] = json!({
            "enabled": source.get("loudnessComp").is_some(),
            "mode":self.loudness_comp.mode,"preset":self.loudness_comp.preset,
            "bands":self.loudness_comp.bands.iter().map(|band| json!({"frequency":band.frequency,"gain":band.gain})).collect::<Vec<_>>(),
            "volumePercent":self.loudness_comp.volume_percent,"maxBoostDb":self.loudness_comp.max_boost_db,
            "smoothingSeconds":self.loudness_comp.smoothing_seconds,
        });
        canonical["dynamicEq"] = json!({
            "enabled":self.dynamic_eq.enabled,"strength":self.dynamic_eq.strength,
            "thresholdDb":self.dynamic_eq.threshold_db,"ratio":self.dynamic_eq.ratio,
            "attackMs":self.dynamic_eq.attack_ms,"releaseMs":self.dynamic_eq.release_ms,
            "bands":self.dynamic_eq.bands.iter().map(|band| json!({"enabled":band.enabled,"targetGainDb":band.target_gain_db})).collect::<Vec<_>>(),
        });
        canonical["modulation"] = json!({
            "enabled": !self.mod_matrix.routes.is_empty(),
            "routes": self.mod_matrix.routes.iter().map(|route| json!({"source":route.source,"target":route.target,"amount":route.amount,"offset":route.offset})).collect::<Vec<_>>(),
            "lfo":{"shape":self.mod_matrix.lfo.shape,"rateHz":self.mod_matrix.lfo.rate_hz,"depth":self.mod_matrix.lfo.depth},
            "envelope":{"attackMs":self.mod_matrix.envelope.attack_ms,"releaseMs":self.mod_matrix.envelope.release_ms,"amount":self.mod_matrix.envelope.amount},
        });
        canonical["limiter"] = json!({
            "enabled":self.limiter.enabled,"thresholdDb":self.limiter.threshold_db,
            "lookaheadMs":self.limiter.lookahead_ms,"attackMs":self.limiter.attack_ms,
            "releaseMs":self.limiter.release_ms,"truePeak":self.limiter.true_peak,
        });
        Ok(canonical)
    }

    /// 将已解析快照按公开协议键重新编码；只保留请求中出现过的可识别顶层键。
    pub fn to_wire_json(&self, source: &Value) -> Value {
        let Some(source) = source.as_object() else {
            return json!({});
        };
        let mut out = Map::new();
        for key in source.keys() {
            let value = match key.as_str() {
                "midSide" => json!({
                    "width": self.mid_side.width,
                    "voiceBalance": self.mid_side.voice_balance,
                }),
                "biquad" => match &self.biquad {
                    Some(value) => json!({
                        "type": value.filter_type,
                        "f0": value.f0,
                        "q": value.q,
                        "gainDb": value.gain_db,
                    }),
                    None => continue,
                },
                "eqChain" => match &self.eq_chain {
                    Some(value) => json!({
                        "bands": value.bands.iter().map(|band| json!({
                            "frequency": band.frequency,
                            "gain": band.gain,
                            "q": band.q,
                        })).collect::<Vec<_>>(),
                        "bandCount": value.band_count,
                        "qCompensation": value.q_compensation,
                    }),
                    None => continue,
                },
                "deesser" => json!({
                    "enabled": self.deesser.enabled,
                    "centerHz": self.deesser.center_hz,
                    "q": self.deesser.q,
                    "thresholdDb": self.deesser.threshold_db,
                    "ratio": self.deesser.ratio,
                    "attackMs": self.deesser.attack_ms,
                    "releaseMs": self.deesser.release_ms,
                    "splitBand": self.deesser.split_band,
                    "mix": self.deesser.mix,
                    "sidechainEnabled": self.deesser.sidechain_enabled,
                }),
                "compressor" => json!({
                    "enabled": self.compressor.enabled,
                    "thresholdDb": self.compressor.threshold_db,
                    "ratio": self.compressor.ratio,
                    "kneeDb": self.compressor.knee_db,
                    "attackMs": self.compressor.attack_ms,
                    "releaseMs": self.compressor.release_ms,
                    "makeupDb": self.compressor.makeup_db,
                    "outputGain": self.compressor.output_gain,
                    "sidechainEnabled": self.compressor.sidechain_enabled,
                }),
                "modEffects" => json!({
                    "delay": {
                        "enabled": self.mod_effects.delay.enabled,
                        "delayMs": self.mod_effects.delay.delay_ms,
                        "feedback": self.mod_effects.delay.feedback,
                        "mix": self.mod_effects.delay.mix,
                    },
                    "chorus": {
                        "enabled": self.mod_effects.chorus.enabled,
                        "rateHz": self.mod_effects.chorus.rate_hz,
                        "depthMs": self.mod_effects.chorus.depth_ms,
                        "mix": self.mod_effects.chorus.mix,
                    },
                    "flanger": {
                        "enabled": self.mod_effects.flanger.enabled,
                        "rateHz": self.mod_effects.flanger.rate_hz,
                        "depthMs": self.mod_effects.flanger.depth_ms,
                        "feedback": self.mod_effects.flanger.feedback,
                        "mix": self.mod_effects.flanger.mix,
                    },
                    "phaser": {
                        "enabled": self.mod_effects.phaser.enabled,
                        "rateHz": self.mod_effects.phaser.rate_hz,
                        "depth": self.mod_effects.phaser.depth,
                        "feedback": self.mod_effects.phaser.feedback,
                        "mix": self.mod_effects.phaser.mix,
                        "stages": self.mod_effects.phaser.stages,
                    },
                    "tremolo": {
                        "enabled": self.mod_effects.tremolo.enabled,
                        "rateHz": self.mod_effects.tremolo.rate_hz,
                        "depth": self.mod_effects.tremolo.depth,
                        "mix": self.mod_effects.tremolo.mix,
                    },
                }),
                "reverbSimple" => json!({
                    "roomSize": self.reverb_simple.room_size,
                    "damping": self.reverb_simple.damping,
                    "wet": self.reverb_simple.wet,
                    "dry": self.reverb_simple.dry,
                    "preDelayMs": self.reverb_simple.pre_delay_ms,
                    "width": self.reverb_simple.width,
                    "type": self.reverb_simple.reverb_type,
                }),
                "reverbRoute" => json!(match self.reverb_route {
                    ReverbRouteKind::Simple => "simple",
                    ReverbRouteKind::Fdn => "fdn",
                    ReverbRouteKind::Convolver => "convolver",
                    ReverbRouteKind::Off => "off",
                }),
                "fdnReverb" => json!({
                    "roomSize": self.fdn_reverb.room_size,
                    "damping": self.fdn_reverb.damping,
                    "wet": self.fdn_reverb.wet,
                    "dry": self.fdn_reverb.dry,
                    "preDelayMs": self.fdn_reverb.pre_delay_ms,
                    "width": self.fdn_reverb.width,
                    "type": self.fdn_reverb.reverb_type,
                    "lines": self.fdn_reverb.lines,
                }),
                "convolver" => {
                    let ir_recipe = self
                        .convolver
                        .ir_recipe
                        .as_ref()
                        .map(|recipe| match recipe {
                            IrRecipe::Delta { delay } => json!({"kind":"delta", "delay":delay}),
                            IrRecipe::ExpNoise {
                                length,
                                seed,
                                decay,
                                amp,
                            } => json!({
                                "kind":"expNoise", "length":length, "seed":seed,
                                "decay":decay, "amp":amp,
                            }),
                        });
                    json!({
                        "irRecipe": ir_recipe,
                        "mix": self.convolver.mix,
                        "preDelayMs": self.convolver.pre_delay_ms,
                    })
                }
                "bassEnhancer" => json!({
                    "enabled": self.bass_enhancer.enabled,
                    "cutoffHz": self.bass_enhancer.cutoff_hz,
                    "q": self.bass_enhancer.q,
                    "harmonicType": self.bass_enhancer.harmonic_type,
                    "harmonicGain": self.bass_enhancer.harmonic_gain,
                    "mix": self.bass_enhancer.mix,
                    "levelDb": self.bass_enhancer.level_db,
                    "lowBoostDb": self.bass_enhancer.low_boost_db,
                }),
                "loudnessComp" => json!({
                    "mode": self.loudness_comp.mode,
                    "preset": self.loudness_comp.preset,
                    "bands": self.loudness_comp.bands.iter().map(|band| json!({
                        "frequency": band.frequency, "gain": band.gain,
                    })).collect::<Vec<_>>(),
                    "volumePercent": self.loudness_comp.volume_percent,
                    "maxBoostDb": self.loudness_comp.max_boost_db,
                    "smoothingSeconds": self.loudness_comp.smoothing_seconds,
                }),
                "dynamicEq" => json!({
                    "enabled": self.dynamic_eq.enabled,
                    "strength": self.dynamic_eq.strength,
                    "thresholdDb": self.dynamic_eq.threshold_db,
                    "ratio": self.dynamic_eq.ratio,
                    "attackMs": self.dynamic_eq.attack_ms,
                    "releaseMs": self.dynamic_eq.release_ms,
                    "bands": self.dynamic_eq.bands.iter().map(|band| json!({
                        "enabled": band.enabled, "targetGainDb": band.target_gain_db,
                    })).collect::<Vec<_>>(),
                }),
                "modMatrix" => json!({
                    "routes": self.mod_matrix.routes.iter().map(|route| json!({
                        "source": route.source, "target": route.target,
                        "amount": route.amount, "offset": route.offset,
                    })).collect::<Vec<_>>(),
                    "lfo": {
                        "shape": self.mod_matrix.lfo.shape,
                        "rateHz": self.mod_matrix.lfo.rate_hz,
                        "depth": self.mod_matrix.lfo.depth,
                    },
                    "envelope": {
                        "attackMs": self.mod_matrix.envelope.attack_ms,
                        "releaseMs": self.mod_matrix.envelope.release_ms,
                        "amount": self.mod_matrix.envelope.amount,
                    },
                }),
                "limiter" => json!({
                    "enabled": self.limiter.enabled,
                    "thresholdDb": self.limiter.threshold_db,
                    "lookaheadMs": self.limiter.lookahead_ms,
                    "attackMs": self.limiter.attack_ms,
                    "releaseMs": self.limiter.release_ms,
                    "truePeak": self.limiter.true_peak,
                }),
                _ => continue,
            };
            out.insert(key.clone(), value);
        }
        Value::Object(out)
    }
}

/// 解析 params.params 整体对象。
///
/// Err(说明) 表示结构违规（非对象 / 可识别键子字段类型不符），由调用方映射 -32602。
pub fn parse_pilot_params(value: &Value) -> Result<(PilotParams, Vec<String>), String> {
    let obj = value.as_object().ok_or("params.params 必须是 JSON 对象")?;
    let mut p = PilotParams::default();
    let mut warnings: Vec<String> = Vec::new();
    for (k, v) in obj {
        match k.as_str() {
            "midSide" => {
                let o = v.as_object().ok_or("midSide 必须是 JSON 对象")?;
                let mut seen = HashSet::new();
                if let Some(x) = opt_num(o, "width", "midSide", &mut seen)? {
                    p.mid_side.width = x;
                }
                if let Some(x) = opt_num(o, "voiceBalance", "midSide", &mut seen)? {
                    p.mid_side.voice_balance = x;
                }
                collect_unknown(o, &seen, "midSide", &mut warnings);
            }
            "biquad" => {
                let o = v.as_object().ok_or("biquad 必须是 JSON 对象")?;
                let mut seen = HashSet::new();
                let filter_type =
                    opt_str(o, "type", "biquad", &mut seen)?.unwrap_or_else(|| "peaking".into());
                let f0 = opt_num(o, "f0", "biquad", &mut seen)?.unwrap_or(1000.0);
                let q = opt_num(o, "q", "biquad", &mut seen)?.unwrap_or(1.0);
                let gain_db = opt_num(o, "gainDb", "biquad", &mut seen)?.unwrap_or(0.0);
                collect_unknown(o, &seen, "biquad", &mut warnings);
                p.biquad = Some(BiquadSpec {
                    filter_type,
                    f0,
                    q,
                    gain_db,
                });
            }
            "eqChain" => {
                let o = v.as_object().ok_or("eqChain 必须是 JSON 对象")?;
                let mut seen = HashSet::new();
                // 显式配置即装配该级；省略的子键回落 TS createDefaultParams().eq
                // （10 段 PRO 0dB peaking、bandCount 10、Q 补偿开）。
                let mut spec = EqChainSpec {
                    bands: PRO_EQ_DEFAULT_BANDS
                        .iter()
                        .map(|&f| EqBandSpec {
                            frequency: f,
                            gain: 0.0,
                            q: 1.1,
                        })
                        .collect(),
                    band_count: 10.0,
                    q_compensation: true,
                };
                if let Some(arr) = opt_arr(o, "bands", "eqChain", &mut seen)? {
                    let mut bands = Vec::with_capacity(arr.len());
                    for (i, item) in arr.iter().enumerate() {
                        let bo = item
                            .as_object()
                            .ok_or(format!("eqChain.bands[{i}] 必须是 JSON 对象"))?;
                        let mut bseen = HashSet::new();
                        let frequency = opt_num(bo, "frequency", "eqChain.bands", &mut bseen)?
                            .unwrap_or(1000.0);
                        let gain = opt_num(bo, "gain", "eqChain.bands", &mut bseen)?.unwrap_or(0.0);
                        let q = opt_num(bo, "q", "eqChain.bands", &mut bseen)?.unwrap_or(1.0);
                        collect_unknown(bo, &bseen, "eqChain.bands", &mut warnings);
                        bands.push(EqBandSpec { frequency, gain, q });
                    }
                    spec.bands = bands;
                }
                if let Some(x) = opt_num(o, "bandCount", "eqChain", &mut seen)? {
                    spec.band_count = x;
                }
                if let Some(x) = opt_bool(o, "qCompensation", "eqChain", &mut seen)? {
                    spec.q_compensation = x;
                }
                collect_unknown(o, &seen, "eqChain", &mut warnings);
                p.eq_chain = Some(spec);
            }
            "deesser" => {
                let o = v.as_object().ok_or("deesser 必须是 JSON 对象")?;
                let mut seen = HashSet::new();
                if let Some(x) = opt_bool(o, "enabled", "deesser", &mut seen)? {
                    p.deesser.enabled = x;
                }
                if let Some(x) = opt_num(o, "centerHz", "deesser", &mut seen)? {
                    p.deesser.center_hz = x;
                }
                if let Some(x) = opt_num(o, "q", "deesser", &mut seen)? {
                    p.deesser.q = x;
                }
                if let Some(x) = opt_num(o, "thresholdDb", "deesser", &mut seen)? {
                    p.deesser.threshold_db = x;
                }
                if let Some(x) = opt_num(o, "ratio", "deesser", &mut seen)? {
                    p.deesser.ratio = x;
                }
                if let Some(x) = opt_num(o, "attackMs", "deesser", &mut seen)? {
                    p.deesser.attack_ms = x;
                }
                if let Some(x) = opt_num(o, "releaseMs", "deesser", &mut seen)? {
                    p.deesser.release_ms = x;
                }
                if let Some(x) = opt_bool(o, "splitBand", "deesser", &mut seen)? {
                    p.deesser.split_band = x;
                }
                if let Some(x) = opt_num(o, "mix", "deesser", &mut seen)? {
                    p.deesser.mix = x;
                }
                // TS 可选字段；核心模块不读取（仅快照形状对齐），缺省保留 false。
                if let Some(x) = opt_bool(o, "sidechainEnabled", "deesser", &mut seen)? {
                    p.deesser.sidechain_enabled = x;
                }
                collect_unknown(o, &seen, "deesser", &mut warnings);
            }
            "compressor" => {
                let o = v.as_object().ok_or("compressor 必须是 JSON 对象")?;
                let mut seen = HashSet::new();
                if let Some(x) = opt_bool(o, "enabled", "compressor", &mut seen)? {
                    p.compressor.enabled = x;
                }
                if let Some(x) = opt_num(o, "thresholdDb", "compressor", &mut seen)? {
                    p.compressor.threshold_db = x;
                }
                if let Some(x) = opt_num(o, "ratio", "compressor", &mut seen)? {
                    p.compressor.ratio = x;
                }
                if let Some(x) = opt_num(o, "kneeDb", "compressor", &mut seen)? {
                    p.compressor.knee_db = x;
                }
                if let Some(x) = opt_num(o, "attackMs", "compressor", &mut seen)? {
                    p.compressor.attack_ms = x;
                }
                if let Some(x) = opt_num(o, "releaseMs", "compressor", &mut seen)? {
                    p.compressor.release_ms = x;
                }
                if let Some(x) = opt_num(o, "makeupDb", "compressor", &mut seen)? {
                    p.compressor.makeup_db = x;
                }
                if let Some(x) = opt_num(o, "outputGain", "compressor", &mut seen)? {
                    p.compressor.output_gain = x;
                }
                if let Some(x) = opt_bool(o, "sidechainEnabled", "compressor", &mut seen)? {
                    p.compressor.sidechain_enabled = x;
                }
                collect_unknown(o, &seen, "compressor", &mut warnings);
            }
            "modEffects" => {
                let o = v.as_object().ok_or("modEffects 必须是 JSON 对象")?;
                let mut seen = HashSet::new();
                if let Some(so) = opt_obj(o, "delay", "modEffects", &mut seen)? {
                    let mut sseen = HashSet::new();
                    if let Some(x) = opt_bool(so, "enabled", "modEffects.delay", &mut sseen)? {
                        p.mod_effects.delay.enabled = x;
                    }
                    if let Some(x) = opt_num(so, "delayMs", "modEffects.delay", &mut sseen)? {
                        p.mod_effects.delay.delay_ms = x;
                    }
                    if let Some(x) = opt_num(so, "feedback", "modEffects.delay", &mut sseen)? {
                        p.mod_effects.delay.feedback = x;
                    }
                    if let Some(x) = opt_num(so, "mix", "modEffects.delay", &mut sseen)? {
                        p.mod_effects.delay.mix = x;
                    }
                    collect_unknown(so, &sseen, "modEffects.delay", &mut warnings);
                }
                if let Some(so) = opt_obj(o, "chorus", "modEffects", &mut seen)? {
                    let mut sseen = HashSet::new();
                    if let Some(x) = opt_bool(so, "enabled", "modEffects.chorus", &mut sseen)? {
                        p.mod_effects.chorus.enabled = x;
                    }
                    if let Some(x) = opt_num(so, "rateHz", "modEffects.chorus", &mut sseen)? {
                        p.mod_effects.chorus.rate_hz = x;
                    }
                    if let Some(x) = opt_num(so, "depthMs", "modEffects.chorus", &mut sseen)? {
                        p.mod_effects.chorus.depth_ms = x;
                    }
                    if let Some(x) = opt_num(so, "mix", "modEffects.chorus", &mut sseen)? {
                        p.mod_effects.chorus.mix = x;
                    }
                    collect_unknown(so, &sseen, "modEffects.chorus", &mut warnings);
                }
                if let Some(so) = opt_obj(o, "flanger", "modEffects", &mut seen)? {
                    let mut sseen = HashSet::new();
                    if let Some(x) = opt_bool(so, "enabled", "modEffects.flanger", &mut sseen)? {
                        p.mod_effects.flanger.enabled = x;
                    }
                    if let Some(x) = opt_num(so, "rateHz", "modEffects.flanger", &mut sseen)? {
                        p.mod_effects.flanger.rate_hz = x;
                    }
                    if let Some(x) = opt_num(so, "depthMs", "modEffects.flanger", &mut sseen)? {
                        p.mod_effects.flanger.depth_ms = x;
                    }
                    if let Some(x) = opt_num(so, "feedback", "modEffects.flanger", &mut sseen)? {
                        p.mod_effects.flanger.feedback = x;
                    }
                    if let Some(x) = opt_num(so, "mix", "modEffects.flanger", &mut sseen)? {
                        p.mod_effects.flanger.mix = x;
                    }
                    collect_unknown(so, &sseen, "modEffects.flanger", &mut warnings);
                }
                if let Some(so) = opt_obj(o, "phaser", "modEffects", &mut seen)? {
                    let mut sseen = HashSet::new();
                    if let Some(x) = opt_bool(so, "enabled", "modEffects.phaser", &mut sseen)? {
                        p.mod_effects.phaser.enabled = x;
                    }
                    if let Some(x) = opt_num(so, "rateHz", "modEffects.phaser", &mut sseen)? {
                        p.mod_effects.phaser.rate_hz = x;
                    }
                    if let Some(x) = opt_num(so, "depth", "modEffects.phaser", &mut sseen)? {
                        p.mod_effects.phaser.depth = x;
                    }
                    if let Some(x) = opt_num(so, "feedback", "modEffects.phaser", &mut sseen)? {
                        p.mod_effects.phaser.feedback = x;
                    }
                    if let Some(x) = opt_num(so, "mix", "modEffects.phaser", &mut sseen)? {
                        p.mod_effects.phaser.mix = x;
                    }
                    if let Some(x) = opt_num(so, "stages", "modEffects.phaser", &mut sseen)? {
                        p.mod_effects.phaser.stages = x;
                    }
                    collect_unknown(so, &sseen, "modEffects.phaser", &mut warnings);
                }
                if let Some(so) = opt_obj(o, "tremolo", "modEffects", &mut seen)? {
                    let mut sseen = HashSet::new();
                    if let Some(x) = opt_bool(so, "enabled", "modEffects.tremolo", &mut sseen)? {
                        p.mod_effects.tremolo.enabled = x;
                    }
                    if let Some(x) = opt_num(so, "rateHz", "modEffects.tremolo", &mut sseen)? {
                        p.mod_effects.tremolo.rate_hz = x;
                    }
                    if let Some(x) = opt_num(so, "depth", "modEffects.tremolo", &mut sseen)? {
                        p.mod_effects.tremolo.depth = x;
                    }
                    if let Some(x) = opt_num(so, "mix", "modEffects.tremolo", &mut sseen)? {
                        p.mod_effects.tremolo.mix = x;
                    }
                    collect_unknown(so, &sseen, "modEffects.tremolo", &mut warnings);
                }
                collect_unknown(o, &seen, "modEffects", &mut warnings);
            }
            "reverbSimple" => {
                let o = v.as_object().ok_or("reverbSimple 必须是 JSON 对象")?;
                let mut seen = HashSet::new();
                if let Some(x) = opt_num(o, "roomSize", "reverbSimple", &mut seen)? {
                    p.reverb_simple.room_size = x;
                }
                if let Some(x) = opt_num(o, "damping", "reverbSimple", &mut seen)? {
                    p.reverb_simple.damping = x;
                }
                if let Some(x) = opt_num(o, "wet", "reverbSimple", &mut seen)? {
                    p.reverb_simple.wet = x;
                }
                if let Some(x) = opt_num(o, "dry", "reverbSimple", &mut seen)? {
                    p.reverb_simple.dry = x;
                }
                if let Some(x) = opt_num(o, "preDelayMs", "reverbSimple", &mut seen)? {
                    p.reverb_simple.pre_delay_ms = x;
                }
                if let Some(x) = opt_num(o, "width", "reverbSimple", &mut seen)? {
                    p.reverb_simple.width = x;
                }
                if let Some(x) = opt_str(o, "type", "reverbSimple", &mut seen)? {
                    p.reverb_simple.reverb_type = x;
                }
                collect_unknown(o, &seen, "reverbSimple", &mut warnings);
            }
            "reverbRoute" => match v {
                Value::Null => {}
                Value::String(s) => {
                    p.reverb_route = match s.as_str() {
                        "fdn" => ReverbRouteKind::Fdn,
                        "convolver" => ReverbRouteKind::Convolver,
                        "off" => ReverbRouteKind::Off,
                        // "simple" 与一切枚举外值：回退缺省路（镜像 reverbSimple.type
                        // 的「枚举外交由模块回退、无 warnings」惯例）。
                        _ => ReverbRouteKind::Simple,
                    };
                }
                _ => {
                    return Err(
                        "reverbRoute 必须是字符串（simple | fdn | convolver | off）".to_string()
                    )
                }
            },
            "fdnReverb" => {
                let o = v.as_object().ok_or("fdnReverb 必须是 JSON 对象")?;
                let mut seen = HashSet::new();
                if let Some(x) = opt_num(o, "roomSize", "fdnReverb", &mut seen)? {
                    p.fdn_reverb.room_size = x;
                }
                if let Some(x) = opt_num(o, "damping", "fdnReverb", &mut seen)? {
                    p.fdn_reverb.damping = x;
                }
                if let Some(x) = opt_num(o, "wet", "fdnReverb", &mut seen)? {
                    p.fdn_reverb.wet = x;
                }
                if let Some(x) = opt_num(o, "dry", "fdnReverb", &mut seen)? {
                    p.fdn_reverb.dry = x;
                }
                if let Some(x) = opt_num(o, "preDelayMs", "fdnReverb", &mut seen)? {
                    p.fdn_reverb.pre_delay_ms = x;
                }
                if let Some(x) = opt_num(o, "width", "fdnReverb", &mut seen)? {
                    p.fdn_reverb.width = x;
                }
                if let Some(x) = opt_str(o, "type", "fdnReverb", &mut seen)? {
                    p.fdn_reverb.reverb_type = x;
                }
                // 缺省/null 保留 None（= 线数 8）；显式值严格限制为核心支持的线数。
                if let Some(x) = opt_num(o, "lines", "fdnReverb", &mut seen)? {
                    if !matches!(x as i64, 2 | 4 | 8 | 16) || x.fract() != 0.0 {
                        return Err("fdnReverb.lines 仅支持 2/4/8/16".into());
                    }
                    p.fdn_reverb.lines = Some(x);
                }
                collect_unknown(o, &seen, "fdnReverb", &mut warnings);
            }
            "convolver" => {
                let o = v.as_object().ok_or("convolver 必须是 JSON 对象")?;
                let mut seen = HashSet::new();
                if let Some(io) = opt_obj(o, "irRecipe", "convolver", &mut seen)? {
                    let mut iseen = HashSet::new();
                    let kind = opt_str(io, "kind", "convolver.irRecipe", &mut iseen)?
                        .ok_or("convolver.irRecipe.kind 必须为 \"delta\" 或 \"expNoise\"")?;
                    let recipe = match kind.as_str() {
                        "delta" => {
                            let delay = opt_num(io, "delay", "convolver.irRecipe", &mut iseen)?.unwrap_or(0.0);
                            IrRecipe::Delta { delay }
                        }
                        "expNoise" => {
                            let length = opt_num(io, "length", "convolver.irRecipe", &mut iseen)?.unwrap_or(4096.0);
                            let seed = opt_num(io, "seed", "convolver.irRecipe", &mut iseen)?.unwrap_or(1.0);
                            let decay = opt_num(io, "decay", "convolver.irRecipe", &mut iseen)?.unwrap_or(3.0);
                            let amp = opt_num(io, "amp", "convolver.irRecipe", &mut iseen)?.unwrap_or(0.5);
                            IrRecipe::ExpNoise { length, seed: seed_to_u32(seed)?, decay, amp }
                        }
                        // IR 配方无模块内回退形态（与 hse-parity 驱动器一致：未知判别值报错）。
                        other => {
                            return Err(format!(
                                "convolver.irRecipe.kind 必须为 \"delta\" 或 \"expNoise\"，收到 \"{other}\""
                            ))
                        }
                    };
                    collect_unknown(io, &iseen, "convolver.irRecipe", &mut warnings);
                    p.convolver.ir_recipe = Some(recipe);
                }
                if let Some(x) = opt_num(o, "mix", "convolver", &mut seen)? {
                    p.convolver.mix = x;
                }
                if let Some(x) = opt_num(o, "preDelayMs", "convolver", &mut seen)? {
                    p.convolver.pre_delay_ms = x;
                }
                collect_unknown(o, &seen, "convolver", &mut warnings);
            }
            "bassEnhancer" => {
                let o = v.as_object().ok_or("bassEnhancer 必须是 JSON 对象")?;
                let mut seen = HashSet::new();
                if let Some(x) = opt_bool(o, "enabled", "bassEnhancer", &mut seen)? {
                    p.bass_enhancer.enabled = x;
                }
                if let Some(x) = opt_num(o, "cutoffHz", "bassEnhancer", &mut seen)? {
                    p.bass_enhancer.cutoff_hz = x;
                }
                if let Some(x) = opt_num(o, "q", "bassEnhancer", &mut seen)? {
                    p.bass_enhancer.q = x;
                }
                if let Some(x) = opt_str(o, "harmonicType", "bassEnhancer", &mut seen)? {
                    p.bass_enhancer.harmonic_type = x;
                }
                if let Some(x) = opt_num(o, "harmonicGain", "bassEnhancer", &mut seen)? {
                    p.bass_enhancer.harmonic_gain = x;
                }
                if let Some(x) = opt_num(o, "mix", "bassEnhancer", &mut seen)? {
                    p.bass_enhancer.mix = x;
                }
                if let Some(x) = opt_num(o, "levelDb", "bassEnhancer", &mut seen)? {
                    p.bass_enhancer.level_db = x;
                }
                // 缺省/null 保留默认 Some(0.0)（对齐 TS Number.isFinite 防御语义）
                if let Some(x) = opt_num(o, "lowBoostDb", "bassEnhancer", &mut seen)? {
                    p.bass_enhancer.low_boost_db = Some(x);
                }
                collect_unknown(o, &seen, "bassEnhancer", &mut warnings);
            }
            "loudnessComp" => {
                let o = v.as_object().ok_or("loudnessComp 必须是 JSON 对象")?;
                let mut seen = HashSet::new();
                if let Some(x) = opt_str(o, "mode", "loudnessComp", &mut seen)? {
                    p.loudness_comp.mode = x;
                }
                if let Some(x) = opt_str(o, "preset", "loudnessComp", &mut seen)? {
                    p.loudness_comp.preset = x;
                }
                if let Some(arr) = opt_arr(o, "bands", "loudnessComp", &mut seen)? {
                    let mut bands = Vec::with_capacity(arr.len());
                    for (i, item) in arr.iter().enumerate() {
                        let bo = item
                            .as_object()
                            .ok_or(format!("loudnessComp.bands[{i}] 必须是 JSON 对象"))?;
                        let mut bseen = HashSet::new();
                        let frequency = opt_num(bo, "frequency", "loudnessComp.bands", &mut bseen)?
                            .unwrap_or(1000.0);
                        let gain =
                            opt_num(bo, "gain", "loudnessComp.bands", &mut bseen)?.unwrap_or(0.0);
                        collect_unknown(bo, &bseen, "loudnessComp.bands", &mut warnings);
                        bands.push(hse_core::loudness_comp::LoudnessBandParam { frequency, gain });
                    }
                    p.loudness_comp.bands = bands;
                }
                if let Some(x) = opt_num(o, "volumePercent", "loudnessComp", &mut seen)? {
                    p.loudness_comp.volume_percent = x;
                }
                if let Some(x) = opt_num(o, "maxBoostDb", "loudnessComp", &mut seen)? {
                    p.loudness_comp.max_boost_db = x;
                }
                if let Some(x) = opt_num(o, "smoothingSeconds", "loudnessComp", &mut seen)? {
                    p.loudness_comp.smoothing_seconds = x;
                }
                collect_unknown(o, &seen, "loudnessComp", &mut warnings);
            }
            "dynamicEq" => {
                let o = v.as_object().ok_or("dynamicEq 必须是 JSON 对象")?;
                let mut seen = HashSet::new();
                if let Some(x) = opt_bool(o, "enabled", "dynamicEq", &mut seen)? {
                    p.dynamic_eq.enabled = x;
                }
                if let Some(x) = opt_num(o, "strength", "dynamicEq", &mut seen)? {
                    p.dynamic_eq.strength = x;
                }
                if let Some(x) = opt_num(o, "thresholdDb", "dynamicEq", &mut seen)? {
                    p.dynamic_eq.threshold_db = x;
                }
                if let Some(x) = opt_num(o, "ratio", "dynamicEq", &mut seen)? {
                    p.dynamic_eq.ratio = x;
                }
                if let Some(x) = opt_num(o, "attackMs", "dynamicEq", &mut seen)? {
                    p.dynamic_eq.attack_ms = x;
                }
                if let Some(x) = opt_num(o, "releaseMs", "dynamicEq", &mut seen)? {
                    p.dynamic_eq.release_ms = x;
                }
                if let Some(arr) = opt_arr(o, "bands", "dynamicEq", &mut seen)? {
                    let mut bands = Vec::with_capacity(arr.len());
                    for (i, item) in arr.iter().enumerate() {
                        let bo = item
                            .as_object()
                            .ok_or(format!("dynamicEq.bands[{i}] 必须是 JSON 对象"))?;
                        let mut bseen = HashSet::new();
                        let enabled =
                            opt_bool(bo, "enabled", "dynamicEq.bands", &mut bseen)?.unwrap_or(true);
                        // 缺省/null = 保持该带当前/默认静态偏移（TS targetGainDb 可选）。
                        let target_gain_db =
                            opt_num(bo, "targetGainDb", "dynamicEq.bands", &mut bseen)?;
                        collect_unknown(bo, &bseen, "dynamicEq.bands", &mut warnings);
                        bands.push(DynamicEqBandSpec {
                            enabled,
                            target_gain_db,
                        });
                    }
                    p.dynamic_eq.bands = bands;
                }
                collect_unknown(o, &seen, "dynamicEq", &mut warnings);
            }
            "modMatrix" => {
                let o = v.as_object().ok_or("modMatrix 必须是 JSON 对象")?;
                let mut seen = HashSet::new();
                if let Some(arr) = opt_arr(o, "routes", "modMatrix", &mut seen)? {
                    let mut routes = Vec::with_capacity(arr.len());
                    for (i, item) in arr.iter().enumerate() {
                        let ro = item
                            .as_object()
                            .ok_or(format!("modMatrix.routes[{i}] 必须是 JSON 对象"))?;
                        let mut rseen = HashSet::new();
                        let source = opt_str(ro, "source", "modMatrix.routes", &mut rseen)?
                            .unwrap_or_else(|| "lfo".into());
                        let target = opt_str(ro, "target", "modMatrix.routes", &mut rseen)?
                            .unwrap_or_else(|| "masterGain".into());
                        let amount =
                            opt_num(ro, "amount", "modMatrix.routes", &mut rseen)?.unwrap_or(0.0);
                        let offset =
                            opt_num(ro, "offset", "modMatrix.routes", &mut rseen)?.unwrap_or(0.0);
                        collect_unknown(ro, &rseen, "modMatrix.routes", &mut warnings);
                        routes.push(ModMatrixRouteSpec {
                            source,
                            target,
                            amount,
                            offset,
                        });
                    }
                    p.mod_matrix.routes = routes;
                }
                if let Some(lo) = opt_obj(o, "lfo", "modMatrix", &mut seen)? {
                    let mut lseen = HashSet::new();
                    if let Some(x) = opt_str(lo, "shape", "modMatrix.lfo", &mut lseen)? {
                        p.mod_matrix.lfo.shape = x;
                    }
                    if let Some(x) = opt_num(lo, "rateHz", "modMatrix.lfo", &mut lseen)? {
                        p.mod_matrix.lfo.rate_hz = x;
                    }
                    if let Some(x) = opt_num(lo, "depth", "modMatrix.lfo", &mut lseen)? {
                        p.mod_matrix.lfo.depth = x;
                    }
                    collect_unknown(lo, &lseen, "modMatrix.lfo", &mut warnings);
                }
                if let Some(eo) = opt_obj(o, "envelope", "modMatrix", &mut seen)? {
                    let mut eseen = HashSet::new();
                    if let Some(x) = opt_num(eo, "attackMs", "modMatrix.envelope", &mut eseen)? {
                        p.mod_matrix.envelope.attack_ms = x;
                    }
                    if let Some(x) = opt_num(eo, "releaseMs", "modMatrix.envelope", &mut eseen)? {
                        p.mod_matrix.envelope.release_ms = x;
                    }
                    if let Some(x) = opt_num(eo, "amount", "modMatrix.envelope", &mut eseen)? {
                        p.mod_matrix.envelope.amount = x;
                    }
                    collect_unknown(eo, &eseen, "modMatrix.envelope", &mut warnings);
                }
                collect_unknown(o, &seen, "modMatrix", &mut warnings);
            }
            "limiter" => {
                let o = v.as_object().ok_or("limiter 必须是 JSON 对象")?;
                let mut seen = HashSet::new();
                if let Some(x) = opt_bool(o, "enabled", "limiter", &mut seen)? {
                    p.limiter.enabled = x;
                }
                if let Some(x) = opt_num(o, "thresholdDb", "limiter", &mut seen)? {
                    p.limiter.threshold_db = x;
                }
                if let Some(x) = opt_num(o, "lookaheadMs", "limiter", &mut seen)? {
                    p.limiter.lookahead_ms = x;
                }
                if let Some(x) = opt_num(o, "attackMs", "limiter", &mut seen)? {
                    p.limiter.attack_ms = x;
                }
                if let Some(x) = opt_num(o, "releaseMs", "limiter", &mut seen)? {
                    p.limiter.release_ms = x;
                }
                if let Some(x) = opt_bool(o, "truePeak", "limiter", &mut seen)? {
                    p.limiter.true_peak = x;
                }
                collect_unknown(o, &seen, "limiter", &mut warnings);
            }
            other => warnings.push(other.to_string()), // 未知顶层键：键名原文入 warnings
        }
    }
    warnings.sort(); // 字典序升序（确定性输出）
    Ok((p, warnings))
}

/// IR 配方 seed：0..=u32::MAX 的整数（对齐 parity 的 to_uint32 值域）。
fn seed_to_u32(v: f64) -> Result<u32, String> {
    if !v.is_finite() || v < 0.0 || v.fract() != 0.0 || v > u32::MAX as f64 {
        return Err("convolver.irRecipe.seed 必须为 0..=4294967295 的整数".to_string());
    }
    Ok(v as u32)
}

/// 把对象中未出现在 seen 集合里的子键记为 "顶层键.子键" warnings。
fn collect_unknown(
    o: &Map<String, Value>,
    seen: &HashSet<&str>,
    top: &str,
    warnings: &mut Vec<String>,
) {
    for k in o.keys() {
        if !seen.contains(k.as_str()) {
            warnings.push(format!("{}.{}", top, k));
        }
    }
}

fn opt_num<'a>(
    o: &Map<String, Value>,
    key: &'a str,
    top: &str,
    seen: &mut HashSet<&'a str>,
) -> Result<Option<f64>, String> {
    match o.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => match n.as_f64() {
            Some(x) if x.is_finite() => {
                seen.insert(key);
                Ok(Some(x))
            }
            _ => Err(format!("{}.{} 必须是有限数字", top, key)),
        },
        Some(_) => Err(format!("{}.{} 必须是数字", top, key)),
    }
}

fn opt_bool<'a>(
    o: &Map<String, Value>,
    key: &'a str,
    top: &str,
    seen: &mut HashSet<&'a str>,
) -> Result<Option<bool>, String> {
    match o.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => {
            seen.insert(key);
            Ok(Some(*b))
        }
        Some(_) => Err(format!("{}.{} 必须是布尔", top, key)),
    }
}

fn opt_str<'a>(
    o: &Map<String, Value>,
    key: &'a str,
    top: &str,
    seen: &mut HashSet<&'a str>,
) -> Result<Option<String>, String> {
    match o.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => {
            seen.insert(key);
            Ok(Some(s.clone()))
        }
        Some(_) => Err(format!("{}.{} 必须是字符串", top, key)),
    }
}

/// 可选子对象：存在且为对象 → 记键并返回借用；缺失/null → None；其他类型 → 结构违规。
fn opt_obj<'a>(
    o: &'a Map<String, Value>,
    key: &'a str,
    top: &str,
    seen: &mut HashSet<&'a str>,
) -> Result<Option<&'a Map<String, Value>>, String> {
    match o.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(m)) => {
            seen.insert(key);
            Ok(Some(m))
        }
        Some(_) => Err(format!("{}.{} 必须是 JSON 对象", top, key)),
    }
}

/// 可选数组：存在且为数组 → 记键并返回借用；缺失/null → None；其他类型 → 结构违规。
fn opt_arr<'a>(
    o: &'a Map<String, Value>,
    key: &'a str,
    top: &str,
    seen: &mut HashSet<&'a str>,
) -> Result<Option<&'a Vec<Value>>, String> {
    match o.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(a)) => {
            seen.insert(key);
            Ok(Some(a))
        }
        Some(_) => Err(format!("{}.{} 必须是数组", top, key)),
    }
}

#[cfg(test)]
#[allow(non_snake_case)] // 测试名引用协议键原文（eqChain/irRecipe 等 camelCase）
mod tests {
    use super::*;

    #[test]
    fn 空对象得到全缺省且无警告() {
        let (p, w) = parse_pilot_params(&serde_json::json!({})).unwrap();
        assert!(w.is_empty());
        assert!(p.biquad.is_none());
        assert_eq!(p.reverb_simple.wet, 0.3);
        assert_eq!(p.reverb_simple.reverb_type, "hall");
        assert_eq!(p.limiter.threshold_db, -1.0);
        assert!(p.limiter.true_peak);
        // 新增键缺省形态全部为直通/关闭。
        assert_eq!(p.reverb_route, ReverbRouteKind::Simple);
        assert!(!p.deesser.enabled);
        assert!(!p.dynamic_eq.enabled);
        assert!(p.mod_matrix.routes.is_empty());
        assert!(p.convolver.ir_recipe.is_none());
        // eqChain 缺省 = 级不装配（逐位直通锚，见字段文档）。
        assert!(p.eq_chain.is_none());
        assert_eq!(p.loudness_comp.mode, "custom");
        assert!(p.loudness_comp.bands.is_empty());
        assert!(!p.mod_effects.delay.enabled);
        assert!(!p.mod_effects.tremolo.enabled);
    }

    #[test]
    fn 识别键覆盖且未知键按契约格式警告并排序() {
        let v = serde_json::json!({
            "myPluginKey": {"x": 1},
            "biquad": {"type": "highshelf", "f0": 3000, "gainDb": -4, "order": 2},
            "reverbSimple": {"wet": 0.5}
        });
        let (p, w) = parse_pilot_params(&v).unwrap();
        let bq = p.biquad.unwrap();
        assert_eq!(bq.filter_type, "highshelf");
        assert_eq!(bq.f0, 3000.0);
        assert_eq!(bq.gain_db, -4.0);
        assert_eq!(p.reverb_simple.wet, 0.5);
        // 元素格式：未知顶层键=键名原文；未知子键="顶层键.子键"；字典序升序
        assert_eq!(
            w,
            vec!["biquad.order".to_string(), "myPluginKey".to_string()]
        );
    }

    #[test]
    fn 枚举外取值交由模块处理不产生警告() {
        // reverbSimple.type 未知值：控制面放行（无 warnings），模块侧回退 hall
        let v = serde_json::json!({"reverbSimple": {"type": "cathedral"}});
        let (p, w) = parse_pilot_params(&v).unwrap();
        assert!(w.is_empty());
        assert_eq!(p.reverb_simple.reverb_type, "cathedral"); // 原样下发，clamp/回退归模块
                                                              // reverbRoute 枚举外：同样回退缺省路且无 warnings。
        let v = serde_json::json!({"reverbRoute": "cavern"});
        let (p, w) = parse_pilot_params(&v).unwrap();
        assert!(w.is_empty());
        assert_eq!(p.reverb_route, ReverbRouteKind::Simple);
        let v = serde_json::json!({"reverbRoute": "fdn"});
        let (p, _) = parse_pilot_params(&v).unwrap();
        assert_eq!(p.reverb_route, ReverbRouteKind::Fdn);
        let v = serde_json::json!({"reverbRoute": "off"});
        let (p, _) = parse_pilot_params(&v).unwrap();
        assert_eq!(p.reverb_route, ReverbRouteKind::Off);
    }

    #[test]
    fn 子键类型不符属结构违规整体拒绝() {
        let v = serde_json::json!({"limiter": {"thresholdDb": "-6"}});
        assert!(parse_pilot_params(&v).is_err());
        let v = serde_json::json!({"biquad": {"f0": "x"}});
        assert!(parse_pilot_params(&v).is_err());
        // 新增键同款纪律
        assert!(parse_pilot_params(&serde_json::json!({"reverbRoute": 3})).is_err());
        assert!(parse_pilot_params(&serde_json::json!({"dynamicEq": {"strength": "x"}})).is_err());
        assert!(parse_pilot_params(&serde_json::json!({"deesser": {"enabled": 1}})).is_err());
        assert!(parse_pilot_params(&serde_json::json!({"eqChain": {"bands": "x"}})).is_err());
        assert!(parse_pilot_params(&serde_json::json!({"eqChain": {"bands": [42]}})).is_err());
        assert!(parse_pilot_params(&serde_json::json!({"modEffects": {"delay": "x"}})).is_err());
        assert!(parse_pilot_params(&serde_json::json!({"convolver": {"mix": true}})).is_err());
        assert!(parse_pilot_params(&serde_json::json!({"modMatrix": {"routes": {}}})).is_err());
    }

    #[test]
    fn 省略子键回落模块缺省_biquad空对象即中性直通() {
        let v = serde_json::json!({"biquad": {}});
        let (p, w) = parse_pilot_params(&v).unwrap();
        let bq = p.biquad.unwrap();
        assert_eq!(
            (bq.filter_type.as_str(), bq.f0, bq.q, bq.gain_db),
            ("peaking", 1000.0, 1.0, 0.0)
        );
        assert!(w.is_empty());
    }

    #[test]
    fn 非对象整体报错() {
        assert!(parse_pilot_params(&serde_json::json!(42)).is_err());
        assert!(parse_pilot_params(&serde_json::json!("x")).is_err());
    }

    #[test]
    fn eqChain_段参数与开关解析_未知子键入警告() {
        let v = serde_json::json!({
            "eqChain": {
                "bands": [
                    {"frequency": 100, "gain": 3.5, "q": 0.9, "mystery": 1},
                    {"frequency": 8000, "gain": -2, "q": 2.0}
                ],
                "bandCount": 6,
                "qCompensation": false,
                "extra": 1
            }
        });
        let (p, w) = parse_pilot_params(&v).unwrap();
        let eq = p.eq_chain.as_ref().unwrap();
        assert_eq!(eq.bands.len(), 2);
        assert_eq!(
            (eq.bands[0].frequency, eq.bands[0].gain, eq.bands[0].q),
            (100.0, 3.5, 0.9)
        );
        assert_eq!(eq.band_count, 6.0);
        assert!(!eq.q_compensation);
        assert_eq!(
            w,
            vec![
                "eqChain.bands.mystery".to_string(),
                "eqChain.extra".to_string()
            ]
        );
        // bands 缺省：整段回落 TS createDefaultParams().eq（10 段 0 增益）。
        let (p, _) = parse_pilot_params(&serde_json::json!({"eqChain": {}})).unwrap();
        let eq = p.eq_chain.as_ref().unwrap();
        assert_eq!(eq.bands.len(), 10);
        assert_eq!(eq.band_count, 10.0);
        assert!(eq.q_compensation);
        assert!(eq.bands.iter().all(|b| b.gain == 0.0));
    }

    #[test]
    fn modEffects_五子对象解析_嵌套未知子键警告() {
        let v = serde_json::json!({
            "modEffects": {
                "delay": {"enabled": true, "delayMs": 120},
                "chorus": {"enabled": true, "rateHz": 2, "depthMs": 5, "mix": 0.6},
                "flanger": {"enabled": true, "rateHz": 0.3, "depthMs": 1, "feedback": 0.2, "mix": 0.4},
                "phaser": {"enabled": true, "rateHz": 0.8, "depth": 0.7, "feedback": 0.5, "mix": 1, "stages": 6},
                "tremolo": {"enabled": true, "rateHz": 4, "depth": 0.8, "mix": 1, "wobble": 1}
            }
        });
        let (p, w) = parse_pilot_params(&v).unwrap();
        assert!(p.mod_effects.delay.enabled);
        assert_eq!(p.mod_effects.delay.delay_ms, 120.0);
        assert_eq!(p.mod_effects.delay.feedback, 0.3); // 缺省回落
        assert!(p.mod_effects.chorus.enabled && p.mod_effects.flanger.enabled);
        assert_eq!(p.mod_effects.phaser.stages, 6.0);
        assert!(p.mod_effects.tremolo.enabled);
        assert_eq!(w, vec!["modEffects.tremolo.wobble".to_string()]);
    }

    #[test]
    fn convolver_irRecipe_两配方解析_kind枚举外与seed域违规拒绝() {
        let v = serde_json::json!({"convolver": {"irRecipe": {"kind": "delta", "delay": 0}, "mix": 0.4}});
        let (p, w) = parse_pilot_params(&v).unwrap();
        assert_eq!(p.convolver.ir_recipe, Some(IrRecipe::Delta { delay: 0.0 }));
        assert_eq!(p.convolver.mix, 0.4);
        assert!(w.is_empty());

        let v = serde_json::json!({
            "convolver": {"irRecipe": {"kind": "expNoise", "length": 8192, "seed": 424242, "decay": 4.0, "amp": 0.4}, "preDelayMs": 12}
        });
        let (p, w) = parse_pilot_params(&v).unwrap();
        assert_eq!(
            p.convolver.ir_recipe,
            Some(IrRecipe::ExpNoise {
                length: 8192.0,
                seed: 424242,
                decay: 4.0,
                amp: 0.4
            })
        );
        assert_eq!(p.convolver.pre_delay_ms, 12.0);
        assert!(w.is_empty());

        // kind 枚举外 → 结构违规（IR 配方无模块内回退形态）
        assert!(parse_pilot_params(
            &serde_json::json!({"convolver": {"irRecipe": {"kind": "sine"}}})
        )
        .is_err());
        // seed 越域 → 结构违规
        assert!(parse_pilot_params(
            &serde_json::json!({"convolver": {"irRecipe": {"kind": "expNoise", "seed": -1}}})
        )
        .is_err());
        assert!(parse_pilot_params(
            &serde_json::json!({"convolver": {"irRecipe": {"kind": "expNoise", "seed": 4294967296.0}}})
        )
        .is_err());
        // irRecipe 缺失保留 None；缺省子键回落
        let (p, _) = parse_pilot_params(&serde_json::json!({"convolver": {}})).unwrap();
        assert!(p.convolver.ir_recipe.is_none());
        assert_eq!(p.convolver.mix, 0.3);
    }

    #[test]
    fn dynamicEq_带数组解析_交叉频率由服务侧注入不暴露协议键() {
        let v = serde_json::json!({
            "dynamicEq": {
                "enabled": true, "strength": 0.7, "thresholdDb": -30, "ratio": 3,
                "attackMs": 10, "releaseMs": 300,
                "bands": [
                    {"enabled": true, "targetGainDb": 2},
                    {"enabled": false},
                    {"enabled": true}
                ]
            }
        });
        let (p, w) = parse_pilot_params(&v).unwrap();
        assert!(p.dynamic_eq.enabled);
        assert_eq!(p.dynamic_eq.strength, 0.7);
        assert_eq!(p.dynamic_eq.bands.len(), 3);
        assert_eq!(p.dynamic_eq.bands[0].target_gain_db, Some(2.0));
        // 缺省（省略 targetGainDb）= 保持该带当前/默认静态偏移（TS 可选语义；
        // 显式 null 按既有全库约定视同未提供并产生未知子键警告，此处不展开）。
        assert_eq!(p.dynamic_eq.bands[1].target_gain_db, None);
        assert!(!p.dynamic_eq.bands[1].enabled, "显式 false 透传");
        assert_eq!(p.dynamic_eq.bands[2].target_gain_db, None);
        assert!(w.is_empty());
    }

    #[test]
    fn loudnessComp_解析_custom空带缺省为直通形态() {
        let (p, w) = parse_pilot_params(
            &serde_json::json!({"loudnessComp": {"mode": "auto", "volumePercent": 60}}),
        )
        .unwrap();
        assert_eq!(p.loudness_comp.mode, "auto");
        assert_eq!(p.loudness_comp.volume_percent, 60.0);
        assert_eq!(p.loudness_comp.max_boost_db, 12.0, "缺省回落 TS 值");
        assert!(w.is_empty());
        // 显式 custom 曲线
        let v = serde_json::json!({"loudnessComp": {"mode": "custom", "bands": [{"frequency": 100, "gain": 4}], "preset": "warm"}});
        let (p, w) = parse_pilot_params(&v).unwrap();
        assert_eq!(p.loudness_comp.bands.len(), 1);
        assert_eq!(p.loudness_comp.bands[0].gain, 4.0);
        assert_eq!(p.loudness_comp.preset, "warm");
        assert!(w.is_empty());
    }

    #[test]
    fn modMatrix_路由_lfo_envelope解析_offset缺省0() {
        let v = serde_json::json!({
            "modMatrix": {
                "routes": [
                    {"source": "lfo", "target": "masterGain", "amount": 0.4},
                    {"source": "envelope", "target": "stereoWidth", "amount": 0.2, "offset": 0.1}
                ],
                "lfo": {"shape": "triangle", "rateHz": 2, "depth": 1},
                "envelope": {"attackMs": 5, "releaseMs": 100, "amount": 0.8}
            }
        });
        let (p, w) = parse_pilot_params(&v).unwrap();
        assert_eq!(p.mod_matrix.routes.len(), 2);
        assert_eq!(p.mod_matrix.routes[0].offset, 0.0, "TS offset ?? 0");
        assert_eq!(p.mod_matrix.routes[1].offset, 0.1);
        assert_eq!(p.mod_matrix.lfo.shape, "triangle");
        assert_eq!(p.mod_matrix.envelope.amount, 0.8);
        assert!(w.is_empty());
    }

    #[test]
    fn mid_side_解析并投影到完整链() {
        let value = serde_json::json!({"midSide": {"width": 1.5, "voiceBalance": -0.25}});
        let (params, warnings) = parse_pilot_params(&value).unwrap();
        assert_eq!(params.mid_side.width, 1.5);
        assert_eq!(params.mid_side.voice_balance, -0.25);
        assert!(warnings.is_empty());
        let canonical = params.to_canonical_json(&value, 48_000.0).unwrap();
        assert_eq!(canonical["stereoWidth"], 1.5);
        assert_eq!(canonical["pitch"]["voiceBalance"], -0.25);
    }

    #[test]
    fn fdnReverb_解析_lines保留原值校验归模块() {
        let v = serde_json::json!({"fdnReverb": {"type": "plate", "lines": 16, "wet": 0.4, "dry": 0.6}});
        let (p, w) = parse_pilot_params(&v).unwrap();
        assert_eq!(p.fdn_reverb.reverb_type, "plate");
        assert_eq!(p.fdn_reverb.lines, Some(16.0));
        assert_eq!(p.fdn_reverb.wet, 0.4);
        assert!(w.is_empty());
        // lines 缺省 = None（模块 normalizeLines 回退 8）
        let (p, _) = parse_pilot_params(&serde_json::json!({"fdnReverb": {}})).unwrap();
        assert_eq!(p.fdn_reverb.lines, None);
    }
}
