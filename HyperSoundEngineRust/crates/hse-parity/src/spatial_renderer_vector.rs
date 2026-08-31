use std::{collections::HashSet, fs, path::Path};

use serde_json::Value;

const TOTAL_CASES: usize = 14;
const DISTANCE_CASE_IDS: [&str; 4] = [
    "inverse-reference",
    "inverse-far",
    "linear-far",
    "exponential-clamped",
];
const NEAREST_CASE_IDS: [&str; 5] = [
    "exact-center",
    "wrap-positive",
    "wrap-negative",
    "nearest-right-upper",
    "tie-keeps-first",
];
const RENDERER_CASE_IDS: [&str; 5] = [
    "delta-right-asymmetric",
    "distance-air-step",
    "short-final-block",
    "reset-replays-initial-state",
    "configured-room-zero-bypass",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DistanceModelSpec {
    Inverse,
    Linear,
    Exponential,
}

#[derive(Debug)]
pub struct Direction {
    pub azimuth_deg: f32,
    pub elevation_deg: f32,
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

#[derive(Debug)]
pub struct GridSpec {
    pub sample_rate: u32,
    pub azimuths: Vec<f32>,
    pub elevations: Vec<f32>,
    pub hrir_length: usize,
    pub directions: Vec<Direction>,
}

#[derive(Debug, Clone, Copy)]
pub struct DistanceParamsSpec {
    pub reference_distance: f32,
    pub maximum_distance: f32,
    pub rolloff_factor: f32,
}

#[derive(Debug)]
pub struct DistanceCase {
    pub id: String,
    pub model: DistanceModelSpec,
    pub distance: f32,
    pub expected_gain: f32,
    pub expected_air_coefficient: f32,
}

#[derive(Debug)]
pub struct NearestCase {
    pub id: String,
    pub azimuth_deg: f32,
    pub elevation_deg: f32,
    pub azimuth_index: usize,
    pub elevation_index: usize,
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomModeSpec {
    Off,
    ConfiguredZero,
}

#[derive(Debug)]
pub struct RendererCase {
    pub id: String,
    pub input: Vec<f32>,
    pub input_stride: usize,
    pub object_slots: Vec<u32>,
    pub block_sizes: Vec<usize>,
    pub azimuth_deg: f32,
    pub elevation_deg: f32,
    pub distance: f32,
    pub gain: f32,
    pub distance_model: DistanceModelSpec,
    pub room_mode: RoomModeSpec,
    pub reset_replay: bool,
    pub expected_left: Vec<f32>,
    pub expected_right: Vec<f32>,
}

#[derive(Debug)]
pub struct RendererFixture {
    pub tolerance_value: f64,
    pub tolerance_floor: f64,
    pub grid: GridSpec,
    pub distance_params: DistanceParamsSpec,
    pub distance_cases: Vec<DistanceCase>,
    pub nearest_cases: Vec<NearestCase>,
    pub renderer_cases: Vec<RendererCase>,
}

pub fn load_fixture(path: &Path) -> Result<RendererFixture, String> {
    let text =
        fs::read_to_string(path).map_err(|err| format!("读取 {} 失败：{err}", path.display()))?;
    let root: Value = serde_json::from_str(&text)
        .map_err(|err| format!("解析 {} 失败：{err}", path.display()))?;
    exact_keys(
        object(&root, "root")?,
        &[
            "schemaVersion",
            "fixture",
            "scope",
            "abi",
            "tolerance",
            "grid",
            "distance",
            "nearestCases",
            "rendererCases",
        ],
        "root",
    )?;
    if root["schemaVersion"].as_u64() != Some(1) || root["fixture"].as_str() != Some("renderer-abi")
    {
        return Err("renderer-abi 夹具版本或名称无效".into());
    }
    validate_scope(&root["scope"])?;
    validate_abi(&root["abi"])?;
    let tolerance = object(&root["tolerance"], "tolerance")?;
    exact_keys(tolerance, &["kind", "value", "floor"], "tolerance")?;
    if root["tolerance"]["kind"].as_str() != Some("relative") {
        return Err("tolerance.kind 必须为 relative".into());
    }
    let tolerance_value = positive(&root["tolerance"]["value"], "tolerance.value")?;
    let tolerance_floor = positive(&root["tolerance"]["floor"], "tolerance.floor")?;

    let grid_value = object(&root["grid"], "grid")?;
    exact_keys(
        grid_value,
        &[
            "sampleRate",
            "azimuths",
            "elevations",
            "hrirLength",
            "directions",
        ],
        "grid",
    )?;
    let sample_rate = u32_integer(&root["grid"]["sampleRate"], "grid.sampleRate")?;
    let azimuths = numbers(&root["grid"]["azimuths"], "grid.azimuths")?;
    let elevations = numbers(&root["grid"]["elevations"], "grid.elevations")?;
    let hrir_length = integer(&root["grid"]["hrirLength"], "grid.hrirLength")?;
    let mut directions = Vec::new();
    for value in array(&root["grid"]["directions"], "grid.directions")? {
        exact_keys(
            object(value, "direction")?,
            &["azimuthDeg", "elevationDeg", "left", "right"],
            "direction",
        )?;
        let left = numbers(&value["left"], "direction.left")?;
        let right = numbers(&value["right"], "direction.right")?;
        if left.len() != hrir_length || right.len() != hrir_length {
            return Err("direction HRIR 长度与 grid.hrirLength 不一致".into());
        }
        directions.push(Direction {
            azimuth_deg: finite(&value["azimuthDeg"], "direction.azimuthDeg")? as f32,
            elevation_deg: finite(&value["elevationDeg"], "direction.elevationDeg")? as f32,
            left,
            right,
        });
    }
    if directions.len() != azimuths.len() * elevations.len() {
        return Err("grid.directions 数量与轴乘积不一致".into());
    }
    let grid = GridSpec {
        sample_rate,
        azimuths,
        elevations,
        hrir_length,
        directions,
    };

    let distance = object(&root["distance"], "distance")?;
    exact_keys(distance, &["params", "cases"], "distance")?;
    let params = object(&root["distance"]["params"], "distance.params")?;
    exact_keys(
        params,
        &["referenceDistance", "maximumDistance", "rolloffFactor"],
        "distance.params",
    )?;
    let distance_params = DistanceParamsSpec {
        reference_distance: finite(
            &root["distance"]["params"]["referenceDistance"],
            "referenceDistance",
        )? as f32,
        maximum_distance: finite(
            &root["distance"]["params"]["maximumDistance"],
            "maximumDistance",
        )? as f32,
        rolloff_factor: finite(
            &root["distance"]["params"]["rolloffFactor"],
            "rolloffFactor",
        )? as f32,
    };

    let mut ids = HashSet::new();
    let mut distance_cases = Vec::new();
    for value in array(&root["distance"]["cases"], "distance.cases")? {
        exact_keys(
            object(value, "distance case")?,
            &[
                "id",
                "model",
                "distance",
                "expectedGain",
                "expectedAirCoefficient",
            ],
            "distance case",
        )?;
        distance_cases.push(DistanceCase {
            id: unique_id(value, &mut ids)?,
            model: model(&value["model"])?,
            distance: finite(&value["distance"], "distance")? as f32,
            expected_gain: finite(&value["expectedGain"], "expectedGain")? as f32,
            expected_air_coefficient: finite(
                &value["expectedAirCoefficient"],
                "expectedAirCoefficient",
            )? as f32,
        });
    }

    let mut nearest_cases = Vec::new();
    for value in array(&root["nearestCases"], "nearestCases")? {
        exact_keys(
            object(value, "nearest case")?,
            &["id", "azimuthDeg", "elevationDeg", "expected"],
            "nearest case",
        )?;
        exact_keys(
            object(&value["expected"], "nearest expected")?,
            &["azimuthIndex", "elevationIndex", "left", "right"],
            "nearest expected",
        )?;
        let nearest_left = numbers(&value["expected"]["left"], "expected.left")?;
        let nearest_right = numbers(&value["expected"]["right"], "expected.right")?;
        if nearest_left.len() != hrir_length || nearest_right.len() != hrir_length {
            return Err("nearest expected HRIR 长度与 grid.hrirLength 不一致".into());
        }
        nearest_cases.push(NearestCase {
            id: unique_id(value, &mut ids)?,
            azimuth_deg: finite(&value["azimuthDeg"], "azimuthDeg")? as f32,
            elevation_deg: finite(&value["elevationDeg"], "elevationDeg")? as f32,
            azimuth_index: integer(&value["expected"]["azimuthIndex"], "azimuthIndex")?,
            elevation_index: integer(&value["expected"]["elevationIndex"], "elevationIndex")?,
            left: nearest_left,
            right: nearest_right,
        });
    }

    let mut renderer_cases = Vec::new();
    for value in array(&root["rendererCases"], "rendererCases")? {
        exact_keys(
            object(value, "renderer case")?,
            &[
                "id",
                "input",
                "inputStride",
                "objectSlots",
                "blockSizes",
                "azimuthDeg",
                "elevationDeg",
                "distance",
                "gain",
                "distanceModel",
                "roomMode",
                "resetReplay",
                "expected",
            ],
            "renderer case",
        )?;
        exact_keys(
            object(&value["expected"], "renderer expected")?,
            &["left", "right"],
            "renderer expected",
        )?;
        let input = numbers(&value["input"], "input")?;
        let object_slots: Vec<u32> = array(&value["objectSlots"], "objectSlots")?
            .iter()
            .map(|item| {
                item.as_u64()
                    .and_then(|slot| u32::try_from(slot).ok())
                    .ok_or_else(|| "objectSlots 必须是 u32 数组".to_string())
            })
            .collect::<Result<_, _>>()?;
        if object_slots != [0] {
            return Err("单源夹具 objectSlots 必须为 [0]".into());
        }
        let block_sizes: Vec<usize> = array(&value["blockSizes"], "blockSizes")?
            .iter()
            .map(|item| integer(item, "blockSize"))
            .collect::<Result<_, _>>()?;
        let expected_left = numbers(&value["expected"]["left"], "expected.left")?;
        let expected_right = numbers(&value["expected"]["right"], "expected.right")?;
        if block_sizes.iter().sum::<usize>() != input.len()
            || expected_left.len() != input.len()
            || expected_right.len() != input.len()
        {
            return Err("renderer case 的 blockSizes/input/expected 长度不一致".into());
        }
        renderer_cases.push(RendererCase {
            id: unique_id(value, &mut ids)?,
            input,
            input_stride: integer(&value["inputStride"], "inputStride")?,
            object_slots,
            block_sizes,
            azimuth_deg: finite(&value["azimuthDeg"], "azimuthDeg")? as f32,
            elevation_deg: finite(&value["elevationDeg"], "elevationDeg")? as f32,
            distance: finite(&value["distance"], "distance")? as f32,
            gain: finite(&value["gain"], "gain")? as f32,
            distance_model: model(&value["distanceModel"])?,
            room_mode: match value["roomMode"].as_str() {
                Some("off") => RoomModeSpec::Off,
                Some("configured-zero") => RoomModeSpec::ConfiguredZero,
                _ => return Err("roomMode 无效".into()),
            },
            reset_replay: value["resetReplay"]
                .as_bool()
                .ok_or("resetReplay 必须是 boolean")?,
            expected_left,
            expected_right,
        });
    }
    exact_case_ids(
        &distance_cases,
        &DISTANCE_CASE_IDS,
        |case| case.id.as_str(),
        "distance",
    )?;
    exact_case_ids(
        &nearest_cases,
        &NEAREST_CASE_IDS,
        |case| case.id.as_str(),
        "nearest",
    )?;
    exact_case_ids(
        &renderer_cases,
        &RENDERER_CASE_IDS,
        |case| case.id.as_str(),
        "renderer",
    )?;
    let total_cases = distance_cases.len() + nearest_cases.len() + renderer_cases.len();
    if total_cases != TOTAL_CASES {
        return Err(format!(
            "renderer-abi case 总数必须为 {TOTAL_CASES}，实际 {total_cases}"
        ));
    }
    Ok(RendererFixture {
        tolerance_value,
        tolerance_floor,
        grid,
        distance_params,
        distance_cases,
        nearest_cases,
        renderer_cases,
    })
}

fn exact_case_ids<T>(
    cases: &[T],
    expected: &[&str],
    id: impl Fn(&T) -> &str,
    label: &str,
) -> Result<(), String> {
    let actual: Vec<_> = cases.iter().map(id).collect();
    if actual != expected {
        return Err(format!(
            "renderer-abi {label} case 集合必须为 {expected:?}，实际 {actual:?}"
        ));
    }
    Ok(())
}

fn validate_scope(value: &Value) -> Result<(), String> {
    exact_keys(
        object(value, "scope")?,
        &[
            "interpolation",
            "convolution",
            "sourceCount",
            "room",
            "excludedNumericParity",
        ],
        "scope",
    )?;
    if value["interpolation"] != "nearest"
        || value["convolution"] != "time-domain"
        || value["sourceCount"] != 1
        || value["room"] != "off-only"
        || value["excludedNumericParity"] != serde_json::json!(["spherical", "room-nonzero"])
    {
        return Err("scope 与冻结范围不一致".into());
    }
    Ok(())
}

fn validate_abi(value: &Value) -> Result<(), String> {
    exact_keys(
        object(value, "abi")?,
        &[
            "inputLayout",
            "inputStrideUnit",
            "objectSlotsUnit",
            "objectParams",
            "outputLayout",
            "lengthUnit",
            "successCode",
        ],
        "abi",
    )?;
    if value["inputLayout"] != "object-major-planar-mono"
        || value["inputStrideUnit"] != "f32-elements"
        || value["objectSlotsUnit"] != "u32-elements"
        || value["objectParams"]
            != serde_json::json!(["azimuthDeg", "elevationDeg", "distance", "gain"])
        || value["outputLayout"] != "planar-stereo"
        || value["lengthUnit"] != "f32-elements"
        || value["successCode"] != 0
    {
        return Err("ABI 布局字段无效".into());
    }
    Ok(())
}

fn model(value: &Value) -> Result<DistanceModelSpec, String> {
    match value.as_str() {
        Some("inverse") => Ok(DistanceModelSpec::Inverse),
        Some("linear") => Ok(DistanceModelSpec::Linear),
        Some("exponential") => Ok(DistanceModelSpec::Exponential),
        _ => Err("distance model 无效".into()),
    }
}
fn object<'a>(value: &'a Value, label: &str) -> Result<&'a serde_json::Map<String, Value>, String> {
    value
        .as_object()
        .ok_or_else(|| format!("{label} 必须是对象"))
}
fn array<'a>(value: &'a Value, label: &str) -> Result<&'a Vec<Value>, String> {
    value
        .as_array()
        .ok_or_else(|| format!("{label} 必须是数组"))
}
fn exact_keys(
    object: &serde_json::Map<String, Value>,
    expected: &[&str],
    label: &str,
) -> Result<(), String> {
    let keys: HashSet<_> = expected.iter().copied().collect();
    for key in object.keys() {
        if !keys.contains(key.as_str()) {
            return Err(format!("{label} 包含未知字段：{key}"));
        }
    }
    for key in expected {
        if !object.contains_key(*key) {
            return Err(format!("{label} 缺少字段：{key}"));
        }
    }
    Ok(())
}
fn finite(value: &Value, label: &str) -> Result<f64, String> {
    let number = value
        .as_f64()
        .ok_or_else(|| format!("{label} 必须是 number"))?;
    if !number.is_finite() {
        return Err(format!("{label} 必须是有限数"));
    }
    Ok(number)
}
fn positive(value: &Value, label: &str) -> Result<f64, String> {
    let number = finite(value, label)?;
    if number <= 0.0 {
        return Err(format!("{label} 必须大于零"));
    }
    Ok(number)
}
fn integer(value: &Value, label: &str) -> Result<usize, String> {
    value
        .as_u64()
        .and_then(|number| usize::try_from(number).ok())
        .filter(|number| *number > 0 || label.contains("Index"))
        .ok_or_else(|| format!("{label} 必须是合法整数"))
}
fn u32_integer(value: &Value, label: &str) -> Result<u32, String> {
    value
        .as_u64()
        .and_then(|number| u32::try_from(number).ok())
        .filter(|number| *number > 0)
        .ok_or_else(|| format!("{label} 必须是合法 u32 正整数"))
}
fn numbers(value: &Value, label: &str) -> Result<Vec<f32>, String> {
    let values = array(value, label)?;
    if values.is_empty() {
        return Err(format!("{label} 不得为空"));
    }
    values
        .iter()
        .map(|value| finite(value, label).map(|number| number as f32))
        .collect()
}
fn unique_id(value: &Value, ids: &mut HashSet<String>) -> Result<String, String> {
    let id = value["id"].as_str().ok_or("id 必须是字符串")?.to_owned();
    if id.is_empty()
        || !id.split('-').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
    {
        return Err(format!("case id 必须为 kebab-case：{id}"));
    }
    if !ids.insert(id.clone()) {
        return Err(format!("case id 重复：{id}"));
    }
    Ok(id)
}
