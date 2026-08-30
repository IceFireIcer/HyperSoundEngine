//! 12 个内置组合场景，镜像 TS `ScenePresets`。

use serde_json::{json, Value};

use crate::params::default_params;

const SNAPSHOT_FS: f64 = 48_000.0;
const IDS: [&str; 12] = [
    "pop",
    "enhance",
    "jazz",
    "dance",
    "classical",
    "livehouse",
    "studio",
    "warm",
    "dts",
    "vocal-stage",
    "night-bass",
    "heavy-bass",
];

/// 内置场景 ID，顺序与 TS `SCENE_IDS` 固定一致。
pub fn scene_ids() -> &'static [&'static str] {
    &IDS
}

fn set_eq(p: &mut Value, gains: &[f64; 10]) {
    let frequencies = [
        31.5, 63.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0,
    ];
    p["eq"]["enabled"] = json!(true);
    p["eq"]["mode"] = json!("pro");
    p["eq"]["bandCount"] = json!(10);
    p["eq"]["proBands"] = Value::Array(
        frequencies
            .iter()
            .zip(gains)
            .map(|(&frequency, &gain)| json!({"frequency": frequency, "gain": gain, "q": 1.1}))
            .collect(),
    );
}

fn set_compressor(
    p: &mut Value,
    threshold: f64,
    ratio: f64,
    knee: f64,
    attack: f64,
    release: f64,
    makeup: f64,
) {
    let c = &mut p["compressor"];
    c["enabled"] = json!(true);
    c["thresholdDb"] = json!(threshold);
    c["ratio"] = json!(ratio);
    c["kneeDb"] = json!(knee);
    c["attackMs"] = json!(attack);
    c["releaseMs"] = json!(release);
    c["makeupDb"] = json!(makeup);
}

fn set_reverb(
    p: &mut Value,
    kind: &str,
    room_size: f64,
    damping: f64,
    wet: f64,
    dry: f64,
    pre_delay: f64,
    width: f64,
) {
    p["reverb"]["enabled"] = json!(true);
    p["reverb"]["mode"] = json!("algorithmic");
    let a = &mut p["reverb"]["algorithmic"];
    a["type"] = json!(kind);
    a["roomSize"] = json!(room_size);
    a["damping"] = json!(damping);
    a["wet"] = json!(wet);
    a["dry"] = json!(dry);
    a["preDelayMs"] = json!(pre_delay);
    a["width"] = json!(width);
}

fn disable_reverb(p: &mut Value) {
    p["reverb"]["enabled"] = json!(false);
    p["reverb"]["mode"] = json!("off");
}

fn set_bass(
    p: &mut Value,
    cutoff: f64,
    harmonic_type: &str,
    harmonic_gain: f64,
    mix: f64,
    level: f64,
) {
    let b = &mut p["bassEnhancer"];
    b["enabled"] = json!(true);
    b["cutoffHz"] = json!(cutoff);
    b["harmonicType"] = json!(harmonic_type);
    b["harmonicGain"] = json!(harmonic_gain);
    b["mix"] = json!(mix);
    b["levelDb"] = json!(level);
}

fn set_deesser(p: &mut Value, center: f64, threshold: f64, ratio: f64, mix: f64) {
    let d = &mut p["deesser"];
    d["enabled"] = json!(true);
    d["centerHz"] = json!(center);
    d["thresholdDb"] = json!(threshold);
    d["ratio"] = json!(ratio);
    d["mix"] = json!(mix);
}

fn finish(mut p: Value, id: &str, name: &str, description: &str) -> Value {
    p["sceneId"] = json!(id);
    p["customized"] = json!(false);
    json!({"id": id, "name": name, "description": description, "builtin": true, "params": p})
}

fn build_scene(id: &str) -> Option<Value> {
    let mut p = default_params(SNAPSHOT_FS);
    let scene = match id {
        "pop" => {
            set_eq(&mut p, &[3.5, 2.5, 1.5, 0.5, -0.5, 0.0, 1.0, 2.0, 2.5, 1.5]);
            set_compressor(&mut p, -18.0, 2.5, 8.0, 12.0, 180.0, 5.0);
            disable_reverb(&mut p);
            set_bass(&mut p, 100.0, "odd", 0.35, 0.3, 0.0);
            set_deesser(&mut p, 6500.0, -30.0, 8.0, 1.0);
            finish(
                p,
                id,
                "流行",
                "流行乐通用：微笑 EQ 曲线 + 人声突出 + 干净直达人声",
            )
        }
        "enhance" => {
            set_eq(
                &mut p,
                &[3.5, 3.0, 0.5, -1.5, -1.5, 0.0, 1.5, 2.5, 3.0, 2.0],
            );
            set_compressor(&mut p, -22.0, 5.0, 4.0, 5.0, 120.0, 13.0);
            disable_reverb(&mut p);
            set_bass(&mut p, 70.0, "odd", 0.6, 0.5, 0.0);
            finish(
                p,
                id,
                "增强",
                "增强：中频凹陷 + 强压缩 + 低频下潜冲击（干声，无齿音限制）",
            )
        }
        "jazz" => {
            set_eq(
                &mut p,
                &[2.0, 1.5, 1.0, 0.5, 0.0, 0.0, 0.5, 0.5, -0.5, -1.0],
            );
            set_compressor(&mut p, -16.0, 1.8, 10.0, 20.0, 250.0, 4.0);
            set_reverb(&mut p, "hall", 0.55, 0.45, 0.35, 0.8, 10.0, 1.0);
            finish(
                p,
                id,
                "爵士",
                "爵士俱乐部：温暖音色 + 轻大厅空间 + 柔和动态",
            )
        }
        "dance" => {
            set_eq(&mut p, &[4.0, 3.0, 1.5, 0.5, -0.5, 0.0, 1.0, 2.0, 3.0, 3.0]);
            set_compressor(&mut p, -14.0, 4.0, 4.0, 8.0, 90.0, 4.0);
            disable_reverb(&mut p);
            set_bass(&mut p, 100.0, "even", 0.7, 0.6, 1.0);
            set_deesser(&mut p, 7500.0, -26.0, 8.0, 1.0);
            p["stereoWidth"] = json!(1.2);
            finish(
                p,
                id,
                "舞曲",
                "舞池能量：重低音 + 泵感压缩 + 高频光泽 + 宽声场（干声）",
            )
        }
        "classical" => {
            set_eq(&mut p, &[0.5, 0.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.5, 0.5]);
            set_compressor(&mut p, -24.0, 1.5, 12.0, 30.0, 400.0, 1.0);
            set_reverb(&mut p, "hall", 0.75, 0.3, 0.55, 0.7, 15.0, 1.0);
            p["stereoWidth"] = json!(1.15);
            finish(
                p,
                id,
                "古典",
                "音乐厅演绎：平直频响 + 长混响尾音 + 宽广声场",
            )
        }
        "livehouse" => {
            set_eq(&mut p, &[1.0, 1.0, 0.5, 0.0, 0.0, 0.5, 1.5, 2.0, 2.0, 1.0]);
            set_compressor(&mut p, -20.0, 3.0, 6.0, 10.0, 200.0, 3.0);
            set_reverb(&mut p, "stage", 0.7, 0.35, 0.6, 0.65, 20.0, 1.0);
            finish(
                p,
                id,
                "现场",
                "LiveHouse 现场：大房间混响 + 临场中高频 + 稳健压缩",
            )
        }
        "studio" => {
            set_eq(&mut p, &[0.0; 10]);
            set_compressor(&mut p, -16.0, 2.0, 10.0, 15.0, 200.0, 4.0);
            disable_reverb(&mut p);
            set_deesser(&mut p, 7000.0, -30.0, 8.0, 0.5);
            finish(
                p,
                id,
                "录音棚",
                "录音棚监听：平直频响 + 极轻处理，完全干声忠于原声",
            )
        }
        "warm" => {
            set_eq(
                &mut p,
                &[3.0, 2.5, 2.0, 1.0, 0.5, 0.0, -0.5, -1.5, -2.5, -3.0],
            );
            set_compressor(&mut p, -18.0, 2.0, 10.0, 20.0, 300.0, 5.0);
            disable_reverb(&mut p);
            set_bass(&mut p, 110.0, "odd", 0.4, 0.35, 0.0);
            finish(p, id, "温暖", "温暖模拟味：饱满低音 + 柔和高频（干声）")
        }
        "dts" => {
            set_eq(&mut p, &[1.0, 1.0, 0.5, 0.0, 0.0, 0.0, 1.0, 2.0, 3.0, 3.0]);
            set_compressor(&mut p, -20.0, 2.5, 8.0, 15.0, 250.0, 2.0);
            set_reverb(&mut p, "hall", 0.85, 0.25, 0.7, 0.55, 25.0, 1.4);
            p["stereoWidth"] = json!(1.3);
            finish(
                p,
                id,
                "浩渺",
                "DTS 浩渺：极开阔混响 + 空气感高频 + 超宽声场",
            )
        }
        "vocal-stage" => {
            set_eq(&mut p, &[-0.5, 0.0, 0.0, 1.0, 1.5, 2.5, 2.0, 1.5, 0.5, 0.0]);
            set_compressor(&mut p, -18.0, 3.0, 6.0, 8.0, 150.0, 0.0);
            set_reverb(&mut p, "stage", 0.5, 0.45, 0.45, 0.75, 8.0, 1.0);
            set_deesser(&mut p, 6500.0, -32.0, 10.0, 1.0);
            finish(
                p,
                id,
                "悠扬舞台",
                "悠扬舞台：人声临场提升 + 齿音收敛 + 舞台空间",
            )
        }
        "night-bass" => {
            set_eq(
                &mut p,
                &[4.0, 3.5, 2.0, 0.5, 0.0, 0.0, -0.5, -1.0, -0.5, 0.0],
            );
            set_compressor(&mut p, -24.0, 6.0, 4.0, 5.0, 200.0, 15.0);
            disable_reverb(&mut p);
            set_bass(&mut p, 120.0, "even", 0.8, 0.7, 1.0);
            set_deesser(&mut p, 6000.0, -36.0, 6.0, 1.0);
            p["nightMode"] = json!({"enabled": true, "amount": 1});
            p["loudnessCompensation"]["enabled"] = json!(true);
            p["loudnessCompensation"]["mode"] = json!("preset");
            p["loudnessCompensation"]["preset"] = json!("warm");
            p["loudnessCompensation"]["volumePercent"] = json!(30);
            p["loudnessCompensation"]["maxBoostDb"] = json!(12);
            finish(
                p,
                id,
                "深夜低音",
                "深夜低音：夜间模式 + 虚拟低频增强 + 高频收敛，低音量均衡耐听",
            )
        }
        "heavy-bass" => {
            set_eq(&mut p, &[6.0, 5.0, 3.0, 1.0, 0.0, 0.0, 0.5, 0.5, 0.0, -1.0]);
            set_compressor(&mut p, -20.0, 3.5, 6.0, 10.0, 150.0, 6.0);
            disable_reverb(&mut p);
            set_bass(&mut p, 60.0, "even", 0.9, 0.75, 2.0);
            p["stereoWidth"] = json!(1.1);
            finish(
                p,
                id,
                "重低音",
                "重低音：超低频提升 + 虚拟低频谐波增强 + 略宽声场，纯低频冲击力",
            )
        }
        _ => return None,
    };
    Some(scene)
}

/// 按 ID 创建内置场景快照；未知 ID 返回 `None`。
pub fn scene_by_id(id: &str) -> Option<Value> {
    build_scene(id)
}

/// 创建全部 12 个内置场景快照，顺序与 [`scene_ids`] 一致。
pub fn builtin_scenes() -> Vec<Value> {
    IDS.iter()
        .map(|id| build_scene(id).expect("内置场景 ID 必须有效"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json_contract_equal(left: &Value, right: &Value) -> bool {
        match (left, right) {
            (Value::Number(a), Value::Number(b)) => a.as_f64() == b.as_f64(),
            (Value::Array(a), Value::Array(b)) => {
                a.len() == b.len() && a.iter().zip(b).all(|(x, y)| json_contract_equal(x, y))
            }
            (Value::Object(a), Value::Object(b)) => {
                a.len() == b.len()
                    && a.iter().all(|(key, value)| {
                        b.get(key)
                            .is_some_and(|want| json_contract_equal(value, want))
                    })
            }
            _ => left == right,
        }
    }

    fn first_diff(path: &str, left: &Value, right: &Value) -> Option<String> {
        match (left, right) {
            (Value::Object(a), Value::Object(b)) => {
                for key in a.keys().chain(b.keys()) {
                    if a.get(key) != b.get(key) {
                        let child = format!("{path}.{key}");
                        return match (a.get(key), b.get(key)) {
                            (Some(x), Some(y)) => first_diff(&child, x, y).or(Some(child)),
                            _ => Some(child),
                        };
                    }
                }
                None
            }
            (Value::Array(a), Value::Array(b)) => {
                if a.len() != b.len() {
                    return Some(format!("{path}.length"));
                }
                a.iter().zip(b).enumerate().find_map(|(index, (x, y))| {
                    (x != y)
                        .then(|| first_diff(&format!("{path}[{index}]"), x, y))
                        .flatten()
                })
            }
            _ => (left != right).then(|| path.to_string()),
        }
    }

    #[test]
    fn 十二场景逐字段命中共享夹具() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../../specs/engine/vectors/scenes.48000.json"
        ))
        .unwrap();
        let ids: Vec<Value> = scene_ids().iter().map(|id| json!(id)).collect();
        assert_eq!(ids.as_slice(), fixture["sceneIds"].as_array().unwrap());
        let scenes = builtin_scenes();
        let expected = fixture["scenes"].as_array().unwrap();
        assert_eq!(scenes.len(), expected.len());
        for (index, (actual, want)) in scenes.iter().zip(expected).enumerate() {
            assert!(
                json_contract_equal(actual, want),
                "场景 {} 首个差异：{:?}",
                scene_ids()[index],
                first_diff("scene", actual, want)
            );
        }
    }

    #[test]
    fn 十二场景顺序身份与快照契约固定() {
        let scenes = builtin_scenes();
        assert_eq!(scenes.len(), 12);
        for (id, scene) in scene_ids().iter().zip(&scenes) {
            assert_eq!(scene["id"], *id);
            assert_eq!(scene["builtin"], true);
            assert_eq!(scene["params"]["sampleRate"], 48_000);
            assert_eq!(scene["params"]["sceneId"], *id);
            assert_eq!(scene["params"]["customized"], false);
            assert_eq!(scene["params"]["spatial"]["mode"], "off");
        }
        assert!(scene_by_id("missing").is_none());
    }

    #[test]
    fn 场景关键覆盖与ts一致() {
        let pop = scene_by_id("pop").unwrap();
        assert_eq!(pop["params"]["eq"]["proBands"][0]["gain"], 3.5);
        assert_eq!(pop["params"]["compressor"]["makeupDb"].as_f64(), Some(5.0));
        assert_eq!(pop["params"]["reverb"]["mode"], "off");

        let night = scene_by_id("night-bass").unwrap();
        assert_eq!(night["params"]["nightMode"]["amount"], 1);
        assert_eq!(night["params"]["loudnessCompensation"]["preset"], "warm");

        let dts = scene_by_id("dts").unwrap();
        assert_eq!(dts["params"]["reverb"]["algorithmic"]["width"], 1.4);
        assert_eq!(dts["params"]["stereoWidth"], 1.3);
    }
}
