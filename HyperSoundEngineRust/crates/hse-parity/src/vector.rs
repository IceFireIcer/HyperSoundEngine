//! 向量用例 JSON 的解析：只提取已知标量键。
//!
//! 契约（specs/ 向量格式）：`schemaVersion/module/case/sampleRate/blockSize/
//! channels/frames/tolerance`。`params` 是模块参数快照，字段名以 TS 源码为准，
//! 由生成方写入；本 harness 不解释其内容，整体跳过。未知字段一律容忍。
//!
//! 计量型扩展（ADDITIVE，specs/dsp/lufs-meter.md §三，既有流式向量不受影响）：
//! 可选 `moduleKind`（缺省视为 "stream"）与 `readings` 标量读数对象，两者双向
//! 绑定——有 readings 则必为 meter、为 meter 则必有 readings；读数名限定为
//! [`READING_NAMES`] 六项（未知名由加载器拒绝）。

use std::fs;
use std::path::Path;

use serde_json::{Map, Value};

use crate::tolerance::ReadingWant;

/// 当前支持的向量 schema 版本。
pub const SUPPORTED_SCHEMA_VERSION: u64 = 1;
/// `sampleRate` 缺省时的采样率。
pub const DEFAULT_SAMPLE_RATE: f64 = 48_000.0;
/// 契约固定的声道数。
pub const REQUIRED_CHANNELS: usize = 2;
/// readings 的全部合法读数名（specs/dsp/lufs-meter.md §二/§四；两侧加载器共用
/// 同一张名字表，未知名视为向量非法）。
pub const READING_NAMES: [&str; 6] = [
    "integratedLufs",
    "momentaryLufs",
    "shortTermLufs",
    "lra",
    "peakDb",
    "truePeakDb",
];

/// 容差声明（kind 目前仅支持 relative）。
#[derive(Debug, Clone)]
pub struct ToleranceSpec {
    /// 判定口径；当前仅支持 relative，保留快照便于未来扩展其他类型。
    #[allow(dead_code)]
    pub kind: String,
    pub value: f64,
    pub floor: f64,
}

/// 一条 readings 标量读数声明：读数名 → { want, tol }。
#[derive(Debug, Clone)]
pub struct ReadingSpec {
    /// 期望读数：有限数（绝对容差判定）或非有限哨兵（等值判定，tol 不参与）。
    pub want: ReadingWant,
    /// 绝对容差（want 为哨兵时不参与）。
    pub tol: f64,
}

/// 一个可执行的向量用例描述。
#[derive(Debug, Clone)]
pub struct VectorCase {
    /// 契约版本；解析时已校验取值，保留为快照供诊断与后续阶段使用。
    #[allow(dead_code)]
    pub schema_version: u64,
    pub module: String,
    pub case: String,
    pub sample_rate: f64,
    pub block_size: usize,
    pub channels: usize,
    pub frames: usize,
    pub tolerance: ToleranceSpec,
    /// 模块参数快照原始对象；字段名以 TS 源码为准，由各模块实现解释。
    pub params: Value,
    /// 可选模块驱动形态：None 视为 "stream"（四段布局流式语义，既有向量）；
    /// Some("meter") 为计量型（两段输入布局 + readings 读数契约）。
    pub module_kind: Option<String>,
    /// 计量读数契约（仅 moduleKind='meter'）；按 JSON 对象键序保存（serde_json
    /// 默认 BTreeMap → 字典序，判定结果与顺序无关）。
    pub readings: Option<Vec<(String, ReadingSpec)>>,
}

impl VectorCase {
    /// 展示名：`<module>.<case>`，与向量文件名一一对应。
    pub fn display_name(&self) -> String {
        format!("{}.{}", self.module, self.case)
    }

    /// 是否为计量型用例（moduleKind='meter'；与 readings 双向绑定，解析时已校验）。
    pub fn is_meter(&self) -> bool {
        self.module_kind.as_deref() == Some("meter")
    }
}

/// 从磁盘读入并解析一份用例 JSON。
pub fn load_case(json_path: &Path) -> Result<VectorCase, String> {
    let text = fs::read_to_string(json_path)
        .map_err(|err| format!("读取 {} 失败：{err}", json_path.display()))?;
    parse_case(&text)
}

/// 解析用例 JSON 文本；只提取已知标量键，未知字段与 params 整体忽略。
pub fn parse_case(text: &str) -> Result<VectorCase, String> {
    let root: Value = serde_json::from_str(text).map_err(|err| format!("JSON 解析失败：{err}"))?;
    let obj = root
        .as_object()
        .ok_or_else(|| "顶层必须是 JSON 对象".to_string())?;

    let schema_version = unsigned_field(obj, "schemaVersion")?;
    if schema_version != SUPPORTED_SCHEMA_VERSION {
        return Err(format!(
            "不支持的 schemaVersion={schema_version}（当前仅支持 {SUPPORTED_SCHEMA_VERSION}）"
        ));
    }

    let module = string_field(obj, "module")?;
    let case = string_field(obj, "case")?;
    let sample_rate = match optional_number_field(obj, "sampleRate")? {
        Some(rate) => rate,
        None => DEFAULT_SAMPLE_RATE,
    };
    let block_size = positive_usize_field(obj, "blockSize")?;
    let channels = usize_field(obj, "channels")?;
    let frames = usize_field(obj, "frames")?;

    // params 保留原始对象：字段名以 TS 源码为准，解释权归各模块实现（见 stages.rs）。
    let params = obj.get("params").cloned().unwrap_or(Value::Object(Map::new()));

    let tolerance_node = obj
        .get("tolerance")
        .ok_or_else(|| "缺少 tolerance 字段".to_string())?;
    let tolerance_obj = tolerance_node
        .as_object()
        .ok_or_else(|| "tolerance 必须是对象".to_string())?;
    let kind = string_field(tolerance_obj, "kind")?;
    if kind != "relative" {
        return Err(format!(
            "暂不支持的容差类型 kind={kind}（当前仅支持 relative）"
        ));
    }
    let value = non_negative_number_field(tolerance_obj, "value")?;
    let floor = non_negative_number_field(tolerance_obj, "floor")?;

    // 计量型扩展（specs/dsp/lufs-meter.md §三）：moduleKind 与 readings 双向绑定。
    let module_kind = match obj.get("moduleKind") {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => Some(text.clone()),
        Some(_) => return Err("moduleKind 必须是字符串".to_string()),
    };
    if let Some(kind_text) = &module_kind {
        if kind_text != "meter" && kind_text != "stream" {
            return Err(format!(
                "未知的 moduleKind={kind_text}（当前仅支持 stream / meter）"
            ));
        }
    }
    let readings = parse_readings(obj)?;
    if readings.is_some() && !matches!(module_kind.as_deref(), Some("meter")) {
        return Err("readings 只允许出现在 moduleKind='meter' 的计量型用例".to_string());
    }
    if module_kind.as_deref() == Some("meter") && readings.is_none() {
        return Err("moduleKind='meter' 的计量型用例必须携带 readings".to_string());
    }

    if !sample_rate.is_finite() || sample_rate <= 0.0 {
        return Err(format!("sampleRate 必须为正有限数，实际为 {sample_rate}"));
    }
    if channels != REQUIRED_CHANNELS {
        return Err(format!(
            "channels 必须为 {REQUIRED_CHANNELS}（契约固定立体声），实际为 {channels}"
        ));
    }
    if !value.is_finite() || !floor.is_finite() {
        return Err("tolerance.value / tolerance.floor 必须为有限数".to_string());
    }

    Ok(VectorCase {
        schema_version,
        module,
        case,
        sample_rate,
        block_size,
        channels,
        frames,
        tolerance: ToleranceSpec { kind, value, floor },
        params,
        module_kind,
        readings,
    })
}

/// 解析可选的 readings 对象：读数名 → { want, tol }（specs/dsp/lufs-meter.md §三.2）。
///
/// 未知名一律拒绝；want 为有限数字或三个字符串哨兵之一；tol 为非负有限数。
fn parse_readings(obj: &Map<String, Value>) -> Result<Option<Vec<(String, ReadingSpec)>>, String> {
    let node = match obj.get("readings") {
        None | Some(Value::Null) => return Ok(None),
        Some(node) => node,
    };
    let readings_obj = node
        .as_object()
        .ok_or_else(|| "readings 必须是对象（读数名 → {want, tol}）".to_string())?;
    let mut readings = Vec::with_capacity(readings_obj.len());
    for (name, item) in readings_obj {
        if !READING_NAMES.contains(&name.as_str()) {
            return Err(format!(
                "未知的 readings 读数名 {name}（合法：{}）",
                READING_NAMES.join(" / ")
            ));
        }
        let item_obj = item
            .as_object()
            .ok_or_else(|| format!("readings.{name} 必须是对象"))?;
        let want_node = item_obj
            .get("want")
            .ok_or_else(|| format!("readings.{name} 缺少 want"))?;
        let want = match want_node {
            Value::Number(number) => {
                let v = number
                    .as_f64()
                    .ok_or_else(|| format!("readings.{name}.want 不是可表示为 f64 的数字"))?;
                if !v.is_finite() {
                    return Err(format!(
                        "readings.{name}.want 必须为有限数（非有限值请使用哨兵字符串）"
                    ));
                }
                ReadingWant::Finite(v)
            }
            Value::String(sentinel) => match sentinel.as_str() {
                "NaN" => ReadingWant::Nan,
                "+Infinity" => ReadingWant::PositiveInfinity,
                "-Infinity" => ReadingWant::NegativeInfinity,
                other => {
                    return Err(format!(
                        "readings.{name}.want 非法哨兵 {other:?}（合法：\"NaN\" / \"+Infinity\" / \"-Infinity\"）"
                    ))
                }
            },
            _ => {
                return Err(format!(
                    "readings.{name}.want 必须是有限数字或哨兵字符串"
                ))
            }
        };
        let tol = match item_obj.get("tol") {
            Some(Value::Number(number)) => {
                let v = number
                    .as_f64()
                    .ok_or_else(|| format!("readings.{name}.tol 不是可表示为 f64 的数字"))?;
                if !v.is_finite() || v < 0.0 {
                    return Err(format!("readings.{name}.tol 必须为非负有限数"));
                }
                v
            }
            _ => return Err(format!("readings.{name} 缺少非负数字 tol")),
        };
        readings.push((name.clone(), ReadingSpec { want, tol }));
    }
    Ok(Some(readings))
}

// ---- 窄化的字段提取助手：类型不符时报出字段名，方便定位坏向量 ----

fn field<'a>(obj: &'a Map<String, Value>, key: &str) -> Result<&'a Value, String> {
    obj.get(key).ok_or_else(|| format!("缺少字段 {key}"))
}

fn string_field(obj: &Map<String, Value>, key: &str) -> Result<String, String> {
    match field(obj, key)? {
        Value::String(text) => {
            if text.is_empty() {
                Err(format!("字段 {key} 不能为空字符串"))
            } else {
                Ok(text.clone())
            }
        }
        _ => Err(format!("字段 {key} 必须是字符串")),
    }
}

fn unsigned_field(obj: &Map<String, Value>, key: &str) -> Result<u64, String> {
    field(obj, key)?
        .as_u64()
        .ok_or_else(|| format!("字段 {key} 必须是非负整数"))
}

fn usize_field(obj: &Map<String, Value>, key: &str) -> Result<usize, String> {
    let raw = unsigned_field(obj, key)?;
    usize::try_from(raw).map_err(|_| format!("字段 {key} 超出当前平台的 usize 范围"))
}

fn positive_usize_field(obj: &Map<String, Value>, key: &str) -> Result<usize, String> {
    let raw = usize_field(obj, key)?;
    if raw == 0 {
        Err(format!("字段 {key} 必须大于 0"))
    } else {
        Ok(raw)
    }
}

fn optional_number_field(obj: &Map<String, Value>, key: &str) -> Result<Option<f64>, String> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(node) => node
            .as_f64()
            .map(Some)
            .ok_or_else(|| format!("字段 {key} 必须是数字")),
    }
}

fn non_negative_number_field(obj: &Map<String, Value>, key: &str) -> Result<f64, String> {
    let raw = field(obj, key)?
        .as_f64()
        .ok_or_else(|| format!("字段 {key} 必须是数字"))?;
    if raw < 0.0 {
        Err(format!("字段 {key} 不能为负，实际为 {raw}"))
    } else {
        Ok(raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_CASE: &str = r#"{
        "schemaVersion": 1,
        "module": "biquad",
        "case": "lowpass-basic",
        "sampleRate": 44100,
        "blockSize": 128,
        "channels": 2,
        "frames": 256,
        "params": {"type": "lowpass", "frequency": 1000, "q": 0.707, "nested": {"unknown": true}},
        "tolerance": {"kind": "relative", "value": 1e-06, "floor": 1e-09},
        "extraUnknown": [1, 2, 3]
    }"#;

    #[test]
    fn 合法用例完整解析() {
        let parsed = parse_case(VALID_CASE).expect("合法用例必须可解析");
        assert_eq!(parsed.schema_version, 1);
        assert_eq!(parsed.module, "biquad");
        assert_eq!(parsed.case, "lowpass-basic");
        assert_eq!(parsed.sample_rate, 44_100.0);
        assert_eq!(parsed.block_size, 128);
        assert_eq!(parsed.channels, 2);
        assert_eq!(parsed.frames, 256);
        assert_eq!(parsed.tolerance.kind, "relative");
        assert_eq!(parsed.tolerance.value, 1.0e-6);
        assert_eq!(parsed.tolerance.floor, 1.0e-9);
        assert_eq!(parsed.display_name(), "biquad.lowpass-basic");
    }

    #[test]
    fn 缺省采样率取契约默认值() {
        let text = r#"{"schemaVersion":1,"module":"limiter","case":"ceiling","blockSize":256,"channels":2,"frames":512,"tolerance":{"kind":"relative","value":1e-06,"floor":1e-09}}"#;
        let parsed = parse_case(text).expect("缺省采样率的用例必须可解析");
        assert_eq!(parsed.sample_rate, DEFAULT_SAMPLE_RATE);
        assert_eq!(parsed.sample_rate, 48_000.0);
    }

    #[test]
    fn 错误的架构版本被拒绝() {
        let text = VALID_CASE.replace(r#""schemaVersion": 1"#, r#""schemaVersion": 2"#);
        let err = parse_case(&text).unwrap_err();
        assert!(err.contains("schemaVersion"), "错误信息应指明字段：{err}");
    }

    #[test]
    fn 声道数偏离契约被拒绝() {
        let text = VALID_CASE.replace(r#""channels": 2"#, r#""channels": 1"#);
        assert!(parse_case(&text).is_err());
    }

    #[test]
    fn 非相对容差被拒绝() {
        let text = VALID_CASE.replace(r#""kind": "relative""#, r#""kind": "absolute""#);
        assert!(parse_case(&text).is_err());
    }

    #[test]
    fn 缺少必需标量字段被拒绝() {
        let text = VALID_CASE.replace(r#""blockSize": 128,"#, "");
        assert!(parse_case(&text).is_err());
    }

    #[test]
    fn 零块长被拒绝() {
        let text = VALID_CASE.replace(r#""blockSize": 128"#, r#""blockSize": 0"#);
        assert!(parse_case(&text).is_err());
    }

    #[test]
    fn 非对象顶层被拒绝() {
        assert!(parse_case("[]").is_err());
        assert!(parse_case("不是 JSON").is_err());
    }

    // ---------------- 计量型扩展（moduleKind + readings） ----------------

    fn meter_case_text() -> String {
        r#"{
        "schemaVersion": 1,
        "module": "lufs-meter",
        "case": "case1",
        "sampleRate": 48000,
        "blockSize": 512,
        "channels": 2,
        "frames": 160000,
        "params": {},
        "tolerance": {"kind": "relative", "value": 1e-06, "floor": 1e-09},
        "moduleKind": "meter",
        "readings": {
            "integratedLufs": {"want": -23.053606272675896, "tol": 0.1},
            "shortTermLufs": {"want": "NaN", "tol": 0.1},
            "peakDb": {"want": "-Infinity", "tol": 0.05},
            "truePeakDb": {"want": "+Infinity", "tol": 0.1}
        }
    }"#
        .to_string()
    }

    #[test]
    fn 计量型用例完整解析() {
        let parsed = parse_case(&meter_case_text()).expect("计量型用例必须可解析");
        assert!(parsed.is_meter());
        assert_eq!(parsed.module_kind.as_deref(), Some("meter"));
        let readings = parsed.readings.as_ref().expect("meter 必有 readings");
        assert_eq!(readings.len(), 4);
        let by_name = |name: &str| {
            readings
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, spec)| spec)
                .expect("读数名必须存在")
        };
        let integrated = by_name("integratedLufs");
        assert_eq!(integrated.want, ReadingWant::Finite(-23.053606272675896));
        assert_eq!(integrated.tol, 0.1);
        assert_eq!(by_name("shortTermLufs").want, ReadingWant::Nan);
        assert_eq!(by_name("peakDb").want, ReadingWant::NegativeInfinity);
        assert_eq!(by_name("truePeakDb").want, ReadingWant::PositiveInfinity);
    }

    /// 从计量型用例文本中整段移除 readings 对象（供绑定规则的反例构造）。
    fn without_readings(text: &str) -> String {
        text.replace(
            ",\n        \"readings\": {\n            \"integratedLufs\": {\"want\": -23.053606272675896, \"tol\": 0.1},\n            \"shortTermLufs\": {\"want\": \"NaN\", \"tol\": 0.1},\n            \"peakDb\": {\"want\": \"-Infinity\", \"tol\": 0.05},\n            \"truePeakDb\": {\"want\": \"+Infinity\", \"tol\": 0.1}\n        }",
            "",
        )
    }

    #[test]
    fn 流式用例缺省_moduleKind_无_readings() {
        let parsed = parse_case(VALID_CASE).expect("流式用例必须可解析");
        assert!(!parsed.is_meter());
        assert!(parsed.module_kind.is_none());
        assert!(parsed.readings.is_none());
    }

    #[test]
    fn 显式_stream_形态合法且不得携带_readings() {
        let text = without_readings(&meter_case_text()).replace("\"meter\"", "\"stream\"");
        let parsed = parse_case(&text).expect("显式 stream（无 readings）必须可解析");
        assert!(!parsed.is_meter());
        assert_eq!(parsed.module_kind.as_deref(), Some("stream"));
    }

    #[test]
    fn readings_出现在非_meter_用例被拒绝() {
        let text = meter_case_text().replace("\"meter\"", "\"stream\"");
        assert!(parse_case(&text).is_err());
        let text = meter_case_text().replace(",\n        \"moduleKind\": \"meter\"", "");
        assert!(parse_case(&text).is_err());
    }

    #[test]
    fn meter_用例缺少_readings_被拒绝() {
        assert!(parse_case(&without_readings(&meter_case_text())).is_err());
    }

    #[test]
    fn 未知名读数被拒绝() {
        let text = meter_case_text().replace("shortTermLufs", "loudnessRange");
        let err = parse_case(&text).unwrap_err();
        assert!(err.contains("未知的 readings 读数名"), "错误信息应指明读数名：{err}");
    }

    #[test]
    fn 非法哨兵与缺失_tol_被拒绝() {
        let text = meter_case_text().replace("\"NaN\"", "\"maybe\"");
        assert!(parse_case(&text).is_err());
        let text = meter_case_text().replace("\"tol\": 0.1},", "\"},");
        assert!(parse_case(&text).is_err());
        let text = meter_case_text().replace("\"tol\": 0.1", "\"tol\": -0.5");
        assert!(parse_case(&text).is_err());
    }
}
