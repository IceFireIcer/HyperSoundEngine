//! setParams 参数快照解析（specs/service/control-plane.md §5.6）。
//!
//! 校验分层：协议层只做「键存在性 + JSON 类型匹配」的结构检查——
//! - 可识别顶层键内**未知的子键**：忽略并记 warnings，元素形如 "顶层键.子键"；
//! - **未知的顶层键**：整体忽略并记 warnings，元素为该键名原文；
//! - 子键**类型不符**属结构违规 → 整体拒绝（调用方映射 -32602），不做静默回退；
//! - 数值越界/枚举外取值不在此层判定，交由模块自身 clamp/回退（如 reverbSimple.type
//!   未知值按模块规格回退 hall），不产生 warnings、不算错误。
//! warnings 最终按字典序升序排列（确定性输出）。
//! 缺省值对齐 TS 支线 createDefaultParams 与各模块构造默认（快照整体替换语义：
//! 省略的顶层键回落内置缺省，不是增量合并）。

use std::collections::HashSet;

use serde_json::{Map, Value};
use hse_core::bass_enhancer::BassEnhancerSettings;
use hse_core::compressor::CompressorSettings;
use hse_core::limiter::LimiterSettings;
use hse_core::reverb_simple::ReverbSimpleParams;

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

/// 引擎子链的参数快照（控制面可识别键：midSide / biquad / compressor /
/// reverbSimple / bassEnhancer / limiter——按全链相对顺序入链）。
#[derive(Debug, Clone)]
pub struct PilotParams {
    pub mid_side: MidSideParams,
    /// None = 未配置滤波器，按 TS 构造默认直通。
    pub biquad: Option<BiquadSpec>,
    pub compressor: CompressorSettings,
    pub reverb_simple: ReverbSimpleParams,
    pub bass_enhancer: BassEnhancerSettings,
    pub limiter: LimiterSettings,
}

impl Default for PilotParams {
    fn default() -> Self {
        Self {
            // 对齐 TS createDefaultParams().stereoWidth（M/S 恒活跃，width=1 恒等）。
            mid_side: MidSideParams { width: 1.0, voice_balance: 0.0 },
            biquad: None,
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

/// 解析 params.params 整体对象。
///
/// Err(说明) 表示结构违规（非对象 / 可识别键子字段类型不符），由调用方映射 -32602。
pub fn parse_pilot_params(value: &Value) -> Result<(PilotParams, Vec<String>), String> {
    let obj = value.as_object().ok_or("params.params 必须是 JSON 对象")?;
    let mut p = PilotParams::default();
    let mut warnings: Vec<String> = Vec::new();
    for (k, v) in obj {
        match k.as_str() {
            "biquad" => {
                let o = v.as_object().ok_or("biquad 必须是 JSON 对象")?;
                let mut seen = HashSet::new();
                let filter_type = opt_str(o, "type", "biquad", &mut seen)?.unwrap_or_else(|| "peaking".into());
                let f0 = opt_num(o, "f0", "biquad", &mut seen)?.unwrap_or(1000.0);
                let q = opt_num(o, "q", "biquad", &mut seen)?.unwrap_or(1.0);
                let gain_db = opt_num(o, "gainDb", "biquad", &mut seen)?.unwrap_or(0.0);
                collect_unknown(o, &seen, "biquad", &mut warnings);
                p.biquad = Some(BiquadSpec { filter_type, f0, q, gain_db });
            }
            "reverbSimple" => {
                let o = v.as_object().ok_or("reverbSimple 必须是 JSON 对象")?;
                let mut seen = HashSet::new();
                if let Some(x) = opt_num(o, "roomSize", "reverbSimple", &mut seen)? { p.reverb_simple.room_size = x; }
                if let Some(x) = opt_num(o, "damping", "reverbSimple", &mut seen)? { p.reverb_simple.damping = x; }
                if let Some(x) = opt_num(o, "wet", "reverbSimple", &mut seen)? { p.reverb_simple.wet = x; }
                if let Some(x) = opt_num(o, "dry", "reverbSimple", &mut seen)? { p.reverb_simple.dry = x; }
                if let Some(x) = opt_num(o, "preDelayMs", "reverbSimple", &mut seen)? { p.reverb_simple.pre_delay_ms = x; }
                if let Some(x) = opt_num(o, "width", "reverbSimple", &mut seen)? { p.reverb_simple.width = x; }
                if let Some(x) = opt_str(o, "type", "reverbSimple", &mut seen)? { p.reverb_simple.reverb_type = x; }
                collect_unknown(o, &seen, "reverbSimple", &mut warnings);
            }
            "compressor" => {
                let o = v.as_object().ok_or("compressor 必须是 JSON 对象")?;
                let mut seen = HashSet::new();
                if let Some(x) = opt_bool(o, "enabled", "compressor", &mut seen)? { p.compressor.enabled = x; }
                if let Some(x) = opt_num(o, "thresholdDb", "compressor", &mut seen)? { p.compressor.threshold_db = x; }
                if let Some(x) = opt_num(o, "ratio", "compressor", &mut seen)? { p.compressor.ratio = x; }
                if let Some(x) = opt_num(o, "kneeDb", "compressor", &mut seen)? { p.compressor.knee_db = x; }
                if let Some(x) = opt_num(o, "attackMs", "compressor", &mut seen)? { p.compressor.attack_ms = x; }
                if let Some(x) = opt_num(o, "releaseMs", "compressor", &mut seen)? { p.compressor.release_ms = x; }
                if let Some(x) = opt_num(o, "makeupDb", "compressor", &mut seen)? { p.compressor.makeup_db = x; }
                if let Some(x) = opt_num(o, "outputGain", "compressor", &mut seen)? { p.compressor.output_gain = x; }
                if let Some(x) = opt_bool(o, "sidechainEnabled", "compressor", &mut seen)? { p.compressor.sidechain_enabled = x; }
                collect_unknown(o, &seen, "compressor", &mut warnings);
            }
            "bassEnhancer" => {
                let o = v.as_object().ok_or("bassEnhancer 必须是 JSON 对象")?;
                let mut seen = HashSet::new();
                if let Some(x) = opt_bool(o, "enabled", "bassEnhancer", &mut seen)? { p.bass_enhancer.enabled = x; }
                if let Some(x) = opt_num(o, "cutoffHz", "bassEnhancer", &mut seen)? { p.bass_enhancer.cutoff_hz = x; }
                if let Some(x) = opt_num(o, "q", "bassEnhancer", &mut seen)? { p.bass_enhancer.q = x; }
                if let Some(x) = opt_str(o, "harmonicType", "bassEnhancer", &mut seen)? { p.bass_enhancer.harmonic_type = x; }
                if let Some(x) = opt_num(o, "harmonicGain", "bassEnhancer", &mut seen)? { p.bass_enhancer.harmonic_gain = x; }
                if let Some(x) = opt_num(o, "mix", "bassEnhancer", &mut seen)? { p.bass_enhancer.mix = x; }
                if let Some(x) = opt_num(o, "levelDb", "bassEnhancer", &mut seen)? { p.bass_enhancer.level_db = x; }
                // 缺省/null 保留默认 Some(0.0)（对齐 TS Number.isFinite 防御语义）
                if let Some(x) = opt_num(o, "lowBoostDb", "bassEnhancer", &mut seen)? { p.bass_enhancer.low_boost_db = Some(x); }
                collect_unknown(o, &seen, "bassEnhancer", &mut warnings);
            }
            "midSide" => {
                let o = v.as_object().ok_or("midSide 必须是 JSON 对象")?;
                let mut seen = HashSet::new();
                if let Some(x) = opt_num(o, "width", "midSide", &mut seen)? { p.mid_side.width = x; }
                if let Some(x) = opt_num(o, "voiceBalance", "midSide", &mut seen)? { p.mid_side.voice_balance = x; }
                collect_unknown(o, &seen, "midSide", &mut warnings);
            }
            "limiter" => {
                let o = v.as_object().ok_or("limiter 必须是 JSON 对象")?;
                let mut seen = HashSet::new();
                if let Some(x) = opt_bool(o, "enabled", "limiter", &mut seen)? { p.limiter.enabled = x; }
                if let Some(x) = opt_num(o, "thresholdDb", "limiter", &mut seen)? { p.limiter.threshold_db = x; }
                if let Some(x) = opt_num(o, "lookaheadMs", "limiter", &mut seen)? { p.limiter.lookahead_ms = x; }
                if let Some(x) = opt_num(o, "attackMs", "limiter", &mut seen)? { p.limiter.attack_ms = x; }
                if let Some(x) = opt_num(o, "releaseMs", "limiter", &mut seen)? { p.limiter.release_ms = x; }
                if let Some(x) = opt_bool(o, "truePeak", "limiter", &mut seen)? { p.limiter.true_peak = x; }
                collect_unknown(o, &seen, "limiter", &mut warnings);
            }
            other => warnings.push(other.to_string()), // 未知顶层键：键名原文入 warnings
        }
    }
    warnings.sort(); // 字典序升序（确定性输出）
    Ok((p, warnings))
}

/// 把对象中未出现在 seen 集合里的子键记为 "顶层键.子键" warnings。
fn collect_unknown(o: &Map<String, Value>, seen: &HashSet<&str>, top: &str, warnings: &mut Vec<String>) {
    for k in o.keys() {
        if !seen.contains(k.as_str()) {
            warnings.push(format!("{}.{}", top, k));
        }
    }
}

fn opt_num<'a>(o: &Map<String, Value>, key: &'a str, top: &str, seen: &mut HashSet<&'a str>) -> Result<Option<f64>, String> {
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

fn opt_bool<'a>(o: &Map<String, Value>, key: &'a str, top: &str, seen: &mut HashSet<&'a str>) -> Result<Option<bool>, String> {
    match o.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => {
            seen.insert(key);
            Ok(Some(*b))
        }
        Some(_) => Err(format!("{}.{} 必须是布尔", top, key)),
    }
}

fn opt_str<'a>(o: &Map<String, Value>, key: &'a str, top: &str, seen: &mut HashSet<&'a str>) -> Result<Option<String>, String> {
    match o.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => {
            seen.insert(key);
            Ok(Some(s.clone()))
        }
        Some(_) => Err(format!("{}.{} 必须是字符串", top, key)),
    }
}

#[cfg(test)]
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
        assert_eq!(w, vec!["biquad.order".to_string(), "myPluginKey".to_string()]);
    }

    #[test]
    fn 枚举外取值交由模块处理不产生警告() {
        // reverbSimple.type 未知值：控制面放行（无 warnings），模块侧回退 hall
        let v = serde_json::json!({"reverbSimple": {"type": "cathedral"}});
        let (p, w) = parse_pilot_params(&v).unwrap();
        assert!(w.is_empty());
        assert_eq!(p.reverb_simple.reverb_type, "cathedral"); // 原样下发，clamp/回退归模块
    }

    #[test]
    fn 子键类型不符属结构违规整体拒绝() {
        let v = serde_json::json!({"limiter": {"thresholdDb": "-6"}});
        assert!(parse_pilot_params(&v).is_err());
        let v = serde_json::json!({"biquad": {"f0": "x"}});
        assert!(parse_pilot_params(&v).is_err());
    }

    #[test]
    fn 省略子键回落模块缺省_biquad空对象即中性直通() {
        let v = serde_json::json!({"biquad": {}});
        let (p, w) = parse_pilot_params(&v).unwrap();
        let bq = p.biquad.unwrap();
        assert_eq!((bq.filter_type.as_str(), bq.f0, bq.q, bq.gain_db), ("peaking", 1000.0, 1.0, 0.0));
        assert!(w.is_empty());
    }

    #[test]
    fn 非对象整体报错() {
        assert!(parse_pilot_params(&serde_json::json!(42)).is_err());
        assert!(parse_pilot_params(&serde_json::json!("x")).is_err());
    }
}