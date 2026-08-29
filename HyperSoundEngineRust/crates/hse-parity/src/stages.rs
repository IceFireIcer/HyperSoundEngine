//! 模块分发器：按向量用例的 `module` id 构造被测阶段。
//!
//! 已落地的模块（biquad / limiter / reverb-simple / compressor / bass-enhancer /
//! mid-side / eq-chain / fdn-reverb / deesser / loudness-comp）从 hse-core 构造
//! 真实实现；尚未落地的模块一律回退 [`PassthroughStage`]（数值 FAIL 属预期）。
//! 参数字段名以 TS 源码为准（camelCase），缺失/类型不符即报夹具错误。

use serde_json::{Map, Value};

use crate::runner::PassthroughStage;
use crate::vector::VectorCase;
use hse_core::bass_enhancer::{BassEnhancerSettings, BassEnhancerStage};
use hse_core::biquad::BiquadStage;
use hse_core::compressor::{CompressorSettings, CompressorStage};
use hse_core::deesser::{DeesserSettings, DeesserStage};
use hse_core::eq_chain::{EqBandParam, EqChainStage};
use hse_core::fdn_reverb::{FdnReverbParams, FdnReverbStage};
use hse_core::limiter::{LimiterSettings, LimiterStage};
use hse_core::loudness_comp::{LoudnessBandParam, LoudnessCompSettings, LoudnessCompStage};
use hse_core::mid_side::MidSideStage;
use hse_core::reverb_simple::{ReverbSimpleParams, ReverbSimpleStage};
use hse_core::Stage;

/// 按用例构造被测阶段。
pub fn make_stage(case: &VectorCase) -> Result<Box<dyn Stage>, String> {
    let obj = case
        .params
        .as_object()
        .ok_or_else(|| "params 必须是 JSON 对象".to_string())?;
    match case.module.as_str() {
        "biquad" => Ok(Box::new(BiquadStage::new(
            case.sample_rate,
            string_field(obj, "type")?.as_str(),
            number_field(obj, "f0")?,
            number_field(obj, "q")?,
            number_field(obj, "gainDb")?,
        )?)),
        "limiter" => Ok(Box::new(LimiterStage::from_settings(
            case.sample_rate,
            LimiterSettings {
                enabled: bool_field(obj, "enabled")?,
                threshold_db: number_field(obj, "thresholdDb")?,
                lookahead_ms: number_field(obj, "lookaheadMs")?,
                attack_ms: number_field(obj, "attackMs")?,
                release_ms: number_field(obj, "releaseMs")?,
                true_peak: bool_field(obj, "truePeak")?,
            },
        )?)),
        "reverb-simple" => Ok(Box::new(ReverbSimpleStage::from_params(
            case.sample_rate,
            ReverbSimpleParams {
                room_size: number_field(obj, "roomSize")?,
                damping: number_field(obj, "damping")?,
                wet: number_field(obj, "wet")?,
                dry: number_field(obj, "dry")?,
                pre_delay_ms: number_field(obj, "preDelayMs")?,
                width: number_field(obj, "width")?,
                reverb_type: string_field(obj, "type")?,
            },
        )?)),
        "compressor" => Ok(Box::new(CompressorStage::from_settings(
            case.sample_rate,
            CompressorSettings {
                enabled: bool_field(obj, "enabled")?,
                threshold_db: number_field(obj, "thresholdDb")?,
                ratio: number_field(obj, "ratio")?,
                knee_db: number_field(obj, "kneeDb")?,
                attack_ms: number_field(obj, "attackMs")?,
                release_ms: number_field(obj, "releaseMs")?,
                makeup_db: number_field(obj, "makeupDb")?,
                output_gain: number_field(obj, "outputGain")?,
                // TS 可选字段；向量固定显式给出，缺省按 false（undefined 语义）。
                // 置 true 时按 specs/dsp/compressor.md §4.5 由阶段内派生单声道和
                // sidechain（sideL=sideR=inL+inR，f64 加法、f32 快照、处理前快照）。
                sidechain_enabled: optional_bool_field(obj, "sidechainEnabled")?,
            },
        )?)),
        "bass-enhancer" => Ok(Box::new(BassEnhancerStage::from_settings(
            case.sample_rate,
            BassEnhancerSettings {
                enabled: bool_field(obj, "enabled")?,
                cutoff_hz: number_field(obj, "cutoffHz")?,
                q: number_field(obj, "q")?,
                harmonic_type: string_field(obj, "harmonicType")?,
                harmonic_gain: number_field(obj, "harmonicGain")?,
                mix: number_field(obj, "mix")?,
                level_db: number_field(obj, "levelDb")?,
                // TS 可选字段：缺省/null 交由模块内 Number.isFinite 防御按 0 处理。
                low_boost_db: optional_number_field(obj, "lowBoostDb")?,
            },
        )?)),
        "mid-side" => {
            // MidSide 无采样率概念（构造无参，规格 mid-side §4.4）；
            // setParams 为位置参数接口，sampleRate 不得传入模块。
            let mut stage = MidSideStage::new();
            stage.set_params(number_field(obj, "width")?, number_field(obj, "voiceBalance")?);
            Ok(Box::new(stage))
        }
        "eq-chain" => {
            // 驱动顺序采用引擎接线顺序（先 setBands 后 setQCompensation；
            // specs/dsp/eq-chain.md §4.3 实证两种顺序终态逐位一致）。
            // bandCount 生效值 = max(1, floor(bandCount))，由阶段内复刻。
            let band_count = number_field(obj, "bandCount")?;
            let q_compensation = bool_field(obj, "qCompensation")?;
            let bands_val = obj.get("bands").ok_or_else(|| "缺少 params.bands".to_string())?;
            let arr = bands_val
                .as_array()
                .ok_or_else(|| "params.bands 必须是数组".to_string())?;
            let mut bands = Vec::with_capacity(arr.len());
            for (i, item) in arr.iter().enumerate() {
                let o = item
                    .as_object()
                    .ok_or_else(|| format!("params.bands[{i}] 必须是对象"))?;
                bands.push(EqBandParam {
                    frequency: number_field(o, "frequency")?,
                    gain: number_field(o, "gain")?,
                    q: number_field(o, "q")?,
                });
            }
            let mut stage = EqChainStage::new(case.sample_rate, band_count)?;
            stage.set_bands(&bands);
            stage.set_q_compensation(q_compensation);
            Ok(Box::new(stage))
        }
        "fdn-reverb" => Ok(Box::new(FdnReverbStage::from_params(
            case.sample_rate,
            FdnReverbParams {
                room_size: number_field(obj, "roomSize")?,
                damping: number_field(obj, "damping")?,
                wet: number_field(obj, "wet")?,
                dry: number_field(obj, "dry")?,
                pre_delay_ms: number_field(obj, "preDelayMs")?,
                width: number_field(obj, "width")?,
                reverb_type: string_field(obj, "type")?,
                // TS 可选字段；向量固定显式给出（缺省按 undefined → 8 线语义）。
                lines: optional_number_field(obj, "lines")?,
            },
        )?)),
        "deesser" => Ok(Box::new(DeesserStage::from_settings(
            case.sample_rate,
            DeesserSettings {
                enabled: bool_field(obj, "enabled")?,
                center_hz: number_field(obj, "centerHz")?,
                q: number_field(obj, "q")?,
                threshold_db: number_field(obj, "thresholdDb")?,
                ratio: number_field(obj, "ratio")?,
                attack_ms: number_field(obj, "attackMs")?,
                release_ms: number_field(obj, "releaseMs")?,
                split_band: bool_field(obj, "splitBand")?,
                mix: number_field(obj, "mix")?,
                // TS 可选字段；本模块自身不读取（引擎接线层标志，specs/dsp/deesser.md
                // §4.5）。置 true 时按 §4.6 由驱动器派生单声道和 sidechain
                // （sideL=sideR=inL+inR，f64 加法、f32 快照、处理前快照）——本批
                // 冻结向量全部为两参形态（sidechainEnabled=false）。
                sidechain_enabled: optional_bool_field(obj, "sidechainEnabled")?,
            },
        )?)),
        "loudness-comp" => {
            // 规格（specs/dsp/loudness-comp.md §一）：本模块没有 enabled 字段——
            // 直接按六个快照字段构造，向量 params 不含 enabled。
            let bands_val = obj.get("bands").ok_or_else(|| "缺少 params.bands".to_string())?;
            let arr = bands_val
                .as_array()
                .ok_or_else(|| "params.bands 必须是数组".to_string())?;
            let mut bands = Vec::with_capacity(arr.len());
            for (i, item) in arr.iter().enumerate() {
                let o = item
                    .as_object()
                    .ok_or_else(|| format!("params.bands[{i}] 必须是对象"))?;
                bands.push(LoudnessBandParam {
                    frequency: number_field(o, "frequency")?,
                    gain: number_field(o, "gain")?,
                });
            }
            Ok(Box::new(LoudnessCompStage::from_settings(
                case.sample_rate,
                LoudnessCompSettings {
                    volume_percent: number_field(obj, "volumePercent")?,
                    max_boost_db: number_field(obj, "maxBoostDb")?,
                    preset: string_field(obj, "preset")?,
                    bands,
                    mode: string_field(obj, "mode")?,
                    smoothing_seconds: number_field(obj, "smoothingSeconds")?,
                },
            )?))
        }
        // 未落地模块：直通占位（FAIL 属预期，证明比对链路仍在工作）。
        _ => Ok(Box::new(PassthroughStage)),
    }
}

fn string_field(obj: &Map<String, Value>, key: &str) -> Result<String, String> {
    match obj.get(key) {
        Some(Value::String(s)) => Ok(s.clone()),
        Some(other) => Err(format!("params.{key} 必须是字符串，实际 {other}")),
        None => Err(format!("缺少 params.{key}")),
    }
}

fn number_field(obj: &Map<String, Value>, key: &str) -> Result<f64, String> {
    match obj.get(key) {
        Some(Value::Number(n)) => n
            .as_f64()
            .ok_or_else(|| format!("params.{key} 不是可表示为 f64 的数字")),
        Some(other) => Err(format!("params.{key} 必须是数字，实际 {other}")),
        None => Err(format!("缺少 params.{key}")),
    }
}

fn bool_field(obj: &Map<String, Value>, key: &str) -> Result<bool, String> {
    match obj.get(key) {
        Some(Value::Bool(b)) => Ok(*b),
        Some(other) => Err(format!("params.{key} 必须是布尔，实际 {other}")),
        None => Err(format!("缺少 params.{key}")),
    }
}

/// TS 可选布尔字段：缺省/null 视为 false（undefined 语义）。
fn optional_bool_field(obj: &Map<String, Value>, key: &str) -> Result<bool, String> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(other) => Err(format!("params.{key} 必须是布尔，实际 {other}")),
    }
}

/// TS 可选数字字段：缺省/null 映射为 None（由模块内缺省/防御逻辑决定取值）。
fn optional_number_field(obj: &Map<String, Value>, key: &str) -> Result<Option<f64>, String> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => n
            .as_f64()
            .map(Some)
            .ok_or_else(|| format!("params.{key} 不是可表示为 f64 的数字")),
        Some(other) => Err(format!("params.{key} 必须是数字，实际 {other}")),
    }
}
