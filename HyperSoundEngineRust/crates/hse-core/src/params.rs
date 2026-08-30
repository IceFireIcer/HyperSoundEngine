//! 引擎参数默认快照，镜像 TS `createDefaultParams`。

use serde_json::Value;

/// 创建完整默认参数快照。
///
/// 返回值包含运行时参数模型中的 `reverb.convolution.ir = null`；分享串编码会按
/// TS `toShareObject` 语义移除该字段，仅保留 `irName` 引用。
pub fn default_params(sample_rate: f64) -> Value {
    let mut params = crate::share_codec::default_params_skeleton(sample_rate);
    params["reverb"]["convolution"]["ir"] = Value::Null;
    params
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 默认参数逐字段命中共享夹具() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../../specs/engine/vectors/default-params.48000.json"
        ))
        .unwrap();
        assert_eq!(default_params(48_000.0), fixture["params"]);
    }

    #[test]
    fn 默认参数镜像ts关键契约() {
        let p = default_params(44_100.0);
        assert_eq!(p["sampleRate"], 44_100);
        assert_eq!(p["eq"]["proBands"].as_array().unwrap().len(), 10);
        assert_eq!(p["limiter"]["enabled"], true);
        assert_eq!(p["spatial"]["mode"], "off");
        assert!(p["reverb"]["convolution"].get("ir").is_some());
        assert_eq!(p["sceneId"], Value::Null);
        assert_eq!(p["customized"], false);
    }
}
