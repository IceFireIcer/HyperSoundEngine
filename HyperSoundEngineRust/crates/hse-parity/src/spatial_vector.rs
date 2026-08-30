use std::collections::HashSet;
use std::fs;
use std::path::Path;

use serde_json::Value;

#[derive(Debug, Clone, Copy)]
pub struct Vec3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct Listener {
    pub position: Vec3,
    pub yaw: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct Expected {
    pub azimuth_deg: f64,
    pub elevation_deg: f64,
    pub distance: f64,
}

#[derive(Debug)]
pub struct SpatialCase {
    pub id: String,
    pub listener: Listener,
    pub source: Vec3,
    pub expected: Expected,
}

#[derive(Debug)]
pub struct SpatialFixture {
    pub angle_abs: f64,
    pub distance_abs: f64,
    pub cases: Vec<SpatialCase>,
}

pub fn load_fixture(path: &Path) -> Result<SpatialFixture, String> {
    let text =
        fs::read_to_string(path).map_err(|err| format!("读取 {} 失败：{err}", path.display()))?;
    let root: Value = serde_json::from_str(&text)
        .map_err(|err| format!("解析 {} 失败：{err}", path.display()))?;
    let root_object = object(&root, "root")?;
    exact_keys(
        root_object,
        &[
            "schemaVersion",
            "fixture",
            "coordinateSystem",
            "tolerance",
            "cases",
        ],
        "root",
    )?;
    if root["schemaVersion"].as_u64() != Some(1)
        || root["fixture"].as_str() != Some("world-listener")
    {
        return Err("world-listener 夹具版本或名称无效".to_string());
    }
    validate_coordinate_system(&root["coordinateSystem"])?;
    let tolerance = object(&root["tolerance"], "tolerance")?;
    exact_keys(tolerance, &["angleAbs", "distanceAbs"], "tolerance")?;
    let angle_abs = finite_positive(&root["tolerance"]["angleAbs"], "tolerance.angleAbs")?;
    let distance_abs = finite_positive(&root["tolerance"]["distanceAbs"], "tolerance.distanceAbs")?;
    let case_values = root["cases"].as_array().ok_or("cases 必须是数组")?;
    if case_values.len() != 12 {
        return Err(format!(
            "world-listener 夹具必须包含 12 个 case，实际为 {}",
            case_values.len()
        ));
    }
    let mut ids = HashSet::new();
    let mut cases = Vec::with_capacity(case_values.len());
    for value in case_values {
        let case = object(value, "case")?;
        exact_keys(case, &["id", "listener", "source", "expected"], "case")?;
        let id = value["id"]
            .as_str()
            .ok_or("case.id 必须是字符串")?
            .to_string();
        if !valid_case_id(&id) {
            return Err(format!("case id 必须为 kebab-case：{id}"));
        }
        if !ids.insert(id.clone()) {
            return Err(format!("case id 重复：{id}"));
        }
        let listener_value = object(&value["listener"], "listener")?;
        exact_keys(listener_value, &["position", "yaw"], "listener")?;
        let expected_value = object(&value["expected"], "expected")?;
        exact_keys(
            expected_value,
            &["azimuthDeg", "elevationDeg", "distance"],
            "expected",
        )?;
        let expected = Expected {
            azimuth_deg: finite(&value["expected"]["azimuthDeg"], "expected.azimuthDeg")?,
            elevation_deg: finite(&value["expected"]["elevationDeg"], "expected.elevationDeg")?,
            distance: finite(&value["expected"]["distance"], "expected.distance")?,
        };
        if !(-180.0..180.0).contains(&expected.azimuth_deg)
            || !(-90.0..=90.0).contains(&expected.elevation_deg)
            || expected.distance < 0.0
        {
            return Err(format!("case {id} 的 expected 超出合法范围"));
        }
        cases.push(SpatialCase {
            id,
            listener: Listener {
                position: vec3(&value["listener"]["position"], "listener.position")?,
                yaw: finite(&value["listener"]["yaw"], "listener.yaw")?,
            },
            source: vec3(&value["source"], "source")?,
            expected,
        });
    }
    Ok(SpatialFixture {
        angle_abs,
        distance_abs,
        cases,
    })
}

fn validate_coordinate_system(value: &Value) -> Result<(), String> {
    let object = object(value, "coordinateSystem")?;
    exact_keys(
        object,
        &[
            "handedness",
            "rightAxis",
            "upAxis",
            "forwardAxis",
            "angleUnit",
            "distanceUnit",
            "azimuthRange",
        ],
        "coordinateSystem",
    )?;
    let expected = [
        ("handedness", "right"),
        ("rightAxis", "+x"),
        ("upAxis", "+y"),
        ("forwardAxis", "+z"),
        ("angleUnit", "degree"),
        ("distanceUnit", "meter"),
        ("azimuthRange", "[-180,180)"),
    ];
    for (key, want) in expected {
        if value[key].as_str() != Some(want) {
            return Err(format!("coordinateSystem.{key} 必须为 {want}"));
        }
    }
    Ok(())
}

fn vec3(value: &Value, label: &str) -> Result<Vec3, String> {
    let object = object(value, label)?;
    exact_keys(object, &["x", "y", "z"], label)?;
    Ok(Vec3 {
        x: finite(&value["x"], &format!("{label}.x"))?,
        y: finite(&value["y"], &format!("{label}.y"))?,
        z: finite(&value["z"], &format!("{label}.z"))?,
    })
}

fn object<'a>(value: &'a Value, label: &str) -> Result<&'a serde_json::Map<String, Value>, String> {
    value
        .as_object()
        .ok_or_else(|| format!("{label} 必须是对象"))
}

fn exact_keys(
    object: &serde_json::Map<String, Value>,
    expected: &[&str],
    label: &str,
) -> Result<(), String> {
    let expected: HashSet<&str> = expected.iter().copied().collect();
    for key in object.keys() {
        if !expected.contains(key.as_str()) {
            return Err(format!("{label} 包含未知字段：{key}"));
        }
    }
    for key in expected {
        if !object.contains_key(key) {
            return Err(format!("{label} 缺少字段：{key}"));
        }
    }
    Ok(())
}

fn valid_case_id(id: &str) -> bool {
    !id.is_empty()
        && id.split('-').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
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

fn finite_positive(value: &Value, label: &str) -> Result<f64, String> {
    let number = finite(value, label)?;
    if number <= 0.0 {
        return Err(format!("{label} 必须大于 0"));
    }
    Ok(number)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn valid_fixture() -> Value {
        let template = serde_json::json!({
            "id": "case-0",
            "listener": { "position": { "x": 0, "y": 0, "z": 0 }, "yaw": 0 },
            "source": { "x": 0, "y": 0, "z": 1 },
            "expected": { "azimuthDeg": 0, "elevationDeg": 0, "distance": 1 }
        });
        let cases: Vec<Value> = (0..12)
            .map(|index| {
                let mut case = template.clone();
                case["id"] = Value::from(format!("case-{index}"));
                case
            })
            .collect();
        serde_json::json!({
            "schemaVersion": 1,
            "fixture": "world-listener",
            "coordinateSystem": {
                "handedness": "right", "rightAxis": "+x", "upAxis": "+y",
                "forwardAxis": "+z", "angleUnit": "degree", "distanceUnit": "meter",
                "azimuthRange": "[-180,180)"
            },
            "tolerance": { "angleAbs": 1e-9, "distanceAbs": 1e-9 },
            "cases": cases
        })
    }

    fn load_value(value: &Value) -> Result<SpatialFixture, String> {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("hse-spatial-{suffix}.json"));
        fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap();
        let result = load_fixture(&path);
        let _ = fs::remove_file(path);
        result
    }

    #[test]
    fn 合法夹具完整解析() {
        let fixture = load_value(&valid_fixture()).unwrap();
        assert_eq!(fixture.cases.len(), 12);
        assert_eq!(fixture.cases[0].id, "case-0");
    }

    #[test]
    fn 未知字段被拒绝() {
        let mut fixture = valid_fixture();
        fixture["cases"][0]["pitch"] = Value::from(1);
        assert!(load_value(&fixture).unwrap_err().contains("未知字段"));
    }

    #[test]
    fn 重复id与case数量错误被拒绝() {
        let mut duplicate = valid_fixture();
        duplicate["cases"][1]["id"] = Value::from("case-0");
        assert!(load_value(&duplicate).unwrap_err().contains("重复"));

        let mut empty = valid_fixture();
        empty["cases"] = serde_json::json!([]);
        assert!(load_value(&empty)
            .unwrap_err()
            .contains("必须包含 12 个 case"));
    }

    #[test]
    fn 非kebab_case_id被拒绝() {
        let mut fixture = valid_fixture();
        fixture["cases"][0]["id"] = Value::from("Bad_ID");
        assert!(load_value(&fixture).unwrap_err().contains("kebab-case"));
    }

    #[test]
    fn 期望值越界与非数值被拒绝() {
        let mut out_of_range = valid_fixture();
        out_of_range["cases"][0]["expected"]["azimuthDeg"] = Value::from(180);
        assert!(load_value(&out_of_range)
            .unwrap_err()
            .contains("超出合法范围"));

        let mut non_number = valid_fixture();
        non_number["cases"][0]["listener"]["yaw"] = Value::from("NaN");
        assert!(load_value(&non_number)
            .unwrap_err()
            .contains("必须是 number"));
    }
}
