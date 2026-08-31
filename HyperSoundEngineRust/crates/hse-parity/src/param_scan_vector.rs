use std::fs;
use std::path::Path;

use serde_json::{Map, Value};

#[derive(Debug)]
pub struct ParamScanFixture {
    pub tolerance_value: f64,
    pub tolerance_floor: f64,
    pub cases: Vec<ParamScanCase>,
}

#[derive(Debug)]
pub struct ParamScanCase {
    pub id: String,
    pub kind: String,
    pub sample_rate: f64,
    pub block_size: usize,
    pub frames: usize,
    pub input_seed: u32,
    pub overrides: Value,
    pub expected_left: ParamScanSummary,
    pub expected_right: ParamScanSummary,
}

#[derive(Debug, Clone, Copy)]
pub struct ParamScanSummary {
    pub finite_ratio: f64,
    pub non_zero_ratio: f64,
    pub peak_order: f64,
    pub rms_order: f64,
}

pub fn load_fixture(path: &Path) -> Result<ParamScanFixture, String> {
    let text =
        fs::read_to_string(path).map_err(|err| format!("读取 {} 失败：{err}", path.display()))?;
    let root: Value = serde_json::from_str(&text).map_err(|err| format!("JSON 解析失败：{err}"))?;
    let object = root.as_object().ok_or("顶层必须是对象")?;
    if object.get("schemaVersion").and_then(Value::as_u64) != Some(1) {
        return Err("schemaVersion 必须为 1".to_string());
    }
    let generator = object
        .get("generator")
        .and_then(Value::as_object)
        .ok_or("缺少 generator 对象")?;
    if generator.get("algorithm").and_then(Value::as_str) != Some("phase4-lcg-1664525-1013904223")
        || generator.get("caseCount").and_then(Value::as_u64) != Some(40)
    {
        return Err("generator 算法或 caseCount 不符合固定契约".to_string());
    }
    let tolerance = object
        .get("tolerance")
        .and_then(Value::as_object)
        .ok_or("缺少 tolerance 对象")?;
    if tolerance.get("kind").and_then(Value::as_str) != Some("relative") {
        return Err("tolerance.kind 必须为 relative".to_string());
    }
    let tolerance_value = number(tolerance.get("value"), "tolerance.value")?;
    let tolerance_floor = number(tolerance.get("floor"), "tolerance.floor")?;
    if tolerance_value != 1.0e-6 || tolerance_floor != 1.0e-9 {
        return Err("容差必须固定为 value=1e-6、floor=1e-9".to_string());
    }

    let values = object
        .get("cases")
        .and_then(Value::as_array)
        .ok_or("缺少 cases 数组")?;
    if values.len() != 40 {
        return Err(format!("cases 必须恰为 40 个，实际 {}", values.len()));
    }
    let mut cases = Vec::with_capacity(values.len());
    for (index, value) in values.iter().enumerate() {
        let item = value
            .as_object()
            .ok_or_else(|| format!("cases[{index}] 必须是对象"))?;
        let id = string(item.get("id"), &format!("cases[{index}].id"))?;
        let kind = string(item.get("kind"), &format!("{id}.kind"))?;
        if !["seed", "minimum", "maximum"].contains(&kind.as_str()) {
            return Err(format!("{id}.kind 非法：{kind}"));
        }
        let sample_rate = number(item.get("sampleRate"), &format!("{id}.sampleRate"))?;
        let block_size = usize_value(item.get("blockSize"), &format!("{id}.blockSize"))?;
        let frames = usize_value(item.get("frames"), &format!("{id}.frames"))?;
        if frames != block_size.saturating_mul(5).saturating_add(17) {
            return Err(format!("{id}.frames 必须等于 blockSize*5+17"));
        }
        let input_seed_u64 = item
            .get("inputSeed")
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("{id}.inputSeed 必须是 u32"))?;
        let input_seed =
            u32::try_from(input_seed_u64).map_err(|_| format!("{id}.inputSeed 超出 u32"))?;
        let params = item
            .get("params")
            .and_then(Value::as_object)
            .ok_or_else(|| format!("{id}.params 必须是对象"))?;
        let overrides = params
            .get("overrides")
            .filter(|value| value.is_object())
            .cloned()
            .ok_or_else(|| format!("{id}.params.overrides 必须是对象"))?;
        if overrides.pointer("/spatial/mode").and_then(Value::as_str) != Some("off") {
            return Err(format!("{id} 必须固定 spatial.mode=off"));
        }
        let expected = item
            .get("expected")
            .and_then(Value::as_object)
            .ok_or_else(|| format!("{id}.expected 必须是对象"))?;
        let expected_left = parse_summary(expected, "left", &id)?;
        let expected_right = parse_summary(expected, "right", &id)?;
        cases.push(ParamScanCase {
            id,
            kind,
            sample_rate,
            block_size,
            frames,
            input_seed,
            overrides,
            expected_left,
            expected_right,
        });
    }
    validate_matrix(&cases)?;
    Ok(ParamScanFixture {
        tolerance_value,
        tolerance_floor,
        cases,
    })
}

fn parse_summary(
    expected: &Map<String, Value>,
    channel: &str,
    id: &str,
) -> Result<ParamScanSummary, String> {
    let summary = expected
        .get(channel)
        .and_then(Value::as_object)
        .ok_or_else(|| format!("{id}.expected.{channel} 必须是对象"))?;
    let field = |key: &str| number(summary.get(key), &format!("{id}.expected.{channel}.{key}"));
    Ok(ParamScanSummary {
        finite_ratio: field("finiteRatio")?,
        non_zero_ratio: field("nonZeroRatio")?,
        peak_order: field("peakOrder")?,
        rms_order: field("rmsOrder")?,
    })
}

fn validate_matrix(cases: &[ParamScanCase]) -> Result<(), String> {
    let matrix = [
        (44_100.0, 63_usize),
        (48_000.0, 128),
        (48_000.0, 257),
        (96_000.0, 512),
    ];
    for &(sample_rate, block_size) in &matrix {
        let group: Vec<_> = cases
            .iter()
            .filter(|case| case.sample_rate == sample_rate && case.block_size == block_size)
            .collect();
        if group.len() != 10
            || group.iter().filter(|case| case.kind == "seed").count() != 8
            || group.iter().filter(|case| case.kind == "minimum").count() != 1
            || group.iter().filter(|case| case.kind == "maximum").count() != 1
        {
            return Err(format!(
                "矩阵 {sample_rate}/{block_size} 必须包含 8 seed + minimum + maximum"
            ));
        }
    }
    if cases
        .iter()
        .any(|case| !matrix.contains(&(case.sample_rate, case.block_size)))
    {
        return Err("cases 包含固定矩阵之外的采样率/块长".to_string());
    }
    Ok(())
}

fn string(value: Option<&Value>, path: &str) -> Result<String, String> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("{path} 必须是非空字符串"))
}

fn number(value: Option<&Value>, path: &str) -> Result<f64, String> {
    let value = value
        .and_then(Value::as_f64)
        .ok_or_else(|| format!("{path} 必须是数字"))?;
    if value.is_finite() {
        Ok(value)
    } else {
        Err(format!("{path} 必须是有限数字"))
    }
}

fn usize_value(value: Option<&Value>, path: &str) -> Result<usize, String> {
    let value = value
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("{path} 必须是非负整数"))?;
    usize::try_from(value).map_err(|_| format!("{path} 超出 usize"))
}
