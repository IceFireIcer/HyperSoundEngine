//! 模块分发器：按向量用例的 `module` id 构造被测阶段。
//!
//! 已落地的试点模块从 hse-core 构造真实实现；尚未落地的模块一律回退
//! [`PassthroughStage`]（数值 FAIL 属预期，与 Phase 0 行为一致）。
//! 参数字段名以 TS 源码为准（camelCase），缺失/类型不符即报夹具错误。

use serde_json::{Map, Value};

use crate::runner::PassthroughStage;
use crate::vector::VectorCase;
use hse_core::biquad::BiquadStage;
use hse_core::limiter::{LimiterSettings, LimiterStage};
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
