//! JSON-RPC 2.0 解析与方法分发表（over WebSocket 文本帧）。
//!
//! 错误码：-32700 解析错误｜-32600 无效请求｜-32601 方法不存在｜-32602 参数无效｜-32000 后端失败｜-32001 状态不允许。
//! 方法表：Phase 2 六方法（listDevices/getState/configure/start/stop/setParams）
//! + Phase 3 推流会话两方法（openSession/closeSession，specs/service/push-stream.md）。
//! 无 id 的请求按通知处理：照常执行副作用，但不回包。

use serde_json::{json, Value};

use crate::engine::{EngineHandle, RpcFault};

/// 处理一行请求文本；返回 Some(响应文本) 或 None（通知/不回包）。
///
/// `owner` 为发起连接的标识，仅供 openSession 记录会话归属（断线自动清理）。
pub fn handle_line(engine: &EngineHandle, owner: u64, line: &str) -> Option<String> {
    let parsed: Result<Value, _> = serde_json::from_str(line);
    let value = match parsed {
        Ok(v) => v,
        Err(err) => return Some(error_response(Value::Null, -32700, format!("Parse error：{}", err)).to_string()),
    };
    let obj = match value.as_object() {
        Some(o) => o,
        None => return Some(error_response(Value::Null, -32600, "Invalid Request：顶层必须是 JSON 对象".into()).to_string()),
    };
    // 批量请求不在契约内：按无效请求处理。
    if obj.is_empty() || value.is_array() {
        return Some(error_response(Value::Null, -32600, "Invalid Request：不支持批量或空对象".into()).to_string());
    }
    let id = obj.get("id").cloned().unwrap_or(Value::Null);
    let is_notification = !obj.contains_key("id");
    if obj.get("jsonrpc").and_then(|j| j.as_str()) != Some("2.0") {
        return reply(is_notification, error_response(id, -32600, "Invalid Request：jsonrpc 必须为 \"2.0\"".into()));
    }
    let method = match obj.get("method").and_then(|m| m.as_str()) {
        Some(m) => m.to_string(),
        None => return reply(is_notification, error_response(id, -32600, "Invalid Request：缺少 method 字符串".into())),
    };
    let params = obj.get("params").cloned().unwrap_or(Value::Null);
    let response = match dispatch(engine, owner, &method, &params) {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err(fault) => error_response(id, fault.code, fault.message),
    };
    reply(is_notification, response)
}

fn reply(is_notification: bool, response: Value) -> Option<String> {
    if is_notification {
        None
    } else {
        Some(response.to_string())
    }
}

fn error_response(id: Value, code: i64, message: String) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn dispatch(engine: &EngineHandle, owner: u64, method: &str, params: &Value) -> Result<Value, RpcFault> {
    match method {
        "listDevices" => engine.list_devices(),
        "getState" => Ok(engine.get_state()),
        "configure" => {
            let obj = params
                .as_object()
                .ok_or_else(|| RpcFault::invalid_params("configure 需要 params 对象"))?;
            engine.configure(obj)
        }
        "start" => engine.start(),
        "stop" => engine.stop(),
        "setParams" => {
            let obj = params
                .as_object()
                .ok_or_else(|| RpcFault::invalid_params("setParams 需要 params 对象"))?;
            let inner = obj
                .get("params")
                .ok_or_else(|| RpcFault::invalid_params("setParams 缺少 params.params"))?;
            engine.set_params(inner)
        }
        // Phase 3（specs/service/push-stream.md）：推流会话生命周期方法。
        "openSession" => {
            let obj = params
                .as_object()
                .ok_or_else(|| RpcFault::invalid_params("openSession 需要 params 对象"))?;
            engine.open_session(obj, owner)
        }
        "closeSession" => {
            let obj = params
                .as_object()
                .ok_or_else(|| RpcFault::invalid_params("closeSession 需要 params 对象"))?;
            engine.close_session(obj)
        }
        other => Err(RpcFault::new(-32601, format!("Method not found：未知方法 {}", other))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake_backend::FakeFactory;
    use std::sync::mpsc::sync_channel;
    use std::time::Duration;

    fn engine() -> EngineHandle {
        let (tx, _rx) = sync_channel::<crate::state::ServiceEvent>(256);
        EngineHandle::new(FakeFactory::working(Duration::ZERO, Duration::ZERO), tx)
    }

    fn call(engine: &EngineHandle, line: &str) -> Option<Value> {
        handle_line(engine, 0, line).map(|t| serde_json::from_str(&t).unwrap())
    }

    #[test]
    fn 坏json报解析错误且id为null() {
        let eng = engine();
        let resp = call(&eng, "{not json").unwrap();
        assert_eq!(resp["error"]["code"], -32700);
        assert!(resp["id"].is_null());
    }

    #[test]
    fn 未知方法_坏版本_顶层非对象均无效请求族() {
        let eng = engine();
        let r = call(&eng, r#"{"jsonrpc":"2.0","id":1,"method":"nope"}"#).unwrap();
        assert_eq!(r["error"]["code"], -32601);
        assert_eq!(r["id"], 1);

        let r = call(&eng, r#"{"jsonrpc":"1.9","id":2,"method":"getState"}"#).unwrap();
        assert_eq!(r["error"]["code"], -32600);

        let r = call(&eng, r#""just a string""#).unwrap();
        assert_eq!(r["error"]["code"], -32600);
    }

    #[test]
    fn configure缺参报参数无效_状态错误码正确() {
        let eng = engine();
        let r = call(&eng, r#"{"jsonrpc":"2.0","id":"a","method":"configure","params":{"sampleRate":48000}}"#).unwrap();
        assert_eq!(r["error"]["code"], -32602);
        assert_eq!(r["id"], "a");

        let r = call(&eng, r#"{"jsonrpc":"2.0","id":3,"method":"start"}"#).unwrap();
        assert_eq!(r["error"]["code"], -32001, "未配置先 start 应为状态不允许");
    }

    #[test]
    fn set_params_缺params键报参数无效() {
        let eng = engine();
        let r = call(&eng, r#"{"jsonrpc":"2.0","id":4,"method":"setParams","params":{}}"#).unwrap();
        assert_eq!(r["error"]["code"], -32602);
    }

    #[test]
    fn 通知执行副作用但不回包() {
        let eng = engine();
        let out = handle_line(&eng, 0, r#"{"jsonrpc":"2.0","method":"configure","params":{"mode":"loopback","renderDeviceId":null,"sampleRate":48000,"blockSizeFrames":128}}"#);
        assert!(out.is_none(), "通知不得回包");
        assert_eq!(eng.get_state()["config"]["blockSizeFrames"], 128, "副作用已生效");
    }

    #[test]
    fn get_state_返回契约形态() {
        let eng = engine();
        let r = call(&eng, r#"{"jsonrpc":"2.0","id":7,"method":"getState"}"#).unwrap();
        let res = &r["result"];
        assert_eq!(res["phase"], "idle");
        assert!(res["config"].is_null());
        assert_eq!(res["stats"]["xrunsIn"], 0);
        assert_eq!(res["stats"]["framesProcessed"], 0);
        assert_eq!(res["stats"]["uptimeMs"], 0);
        assert!(res["lastParams"].is_null());
    }

    #[test]
    fn list_devices_分列并带默认标记() {
        let eng = engine();
        let r = call(&eng, r#"{"jsonrpc":"2.0","id":8,"method":"listDevices"}"#).unwrap();
        let render = r["result"]["render"].as_array().unwrap();
        let capture = r["result"]["capture"].as_array().unwrap();
        assert_eq!(render.len(), 2);
        assert_eq!(capture.len(), 2);
        assert_eq!(render[0]["isDefault"], true);
        assert!(render[0].get("id").is_some() && render[0].get("name").is_some());
    }

    #[test]
    fn 完整生命周期往返_configure_start_set_params_stop() {
        let eng = engine();
        let cfg_line = r#"{"jsonrpc":"2.0","id":1,"method":"configure","params":{"mode":"loopback","renderDeviceId":null,"sampleRate":48000,"blockSizeFrames":64}}"#;
        assert_eq!(call(&eng, cfg_line).unwrap()["result"]["applied"]["sampleRate"], 48000);
        assert_eq!(call(&eng, r#"{"jsonrpc":"2.0","id":2,"method":"start"}"#).unwrap()["result"]["started"], true);
        assert_eq!(eng.get_state()["phase"], "running");
        let sp = call(&eng, r#"{"jsonrpc":"2.0","id":3,"method":"setParams","params":{"params":{"biquad":{"type":"peaking","f0":1000,"q":1,"gainDb":6},"unknownKey":1}}}"#).unwrap();
        assert_eq!(sp["result"]["accepted"], true);
        assert_eq!(sp["result"]["warnings"].as_array().unwrap().len(), 1);
        assert_eq!(call(&eng, r#"{"jsonrpc":"2.0","id":4,"method":"stop"}"#).unwrap()["result"]["stopped"], true);
        assert_eq!(eng.get_state()["phase"], "idle");
        // 运行中重复 stop / configure 都应被拒
        assert_eq!(call(&eng, r#"{"jsonrpc":"2.0","id":5,"method":"stop"}"#).unwrap()["error"]["code"], -32001);
        assert_eq!(call(&eng, cfg_line).is_some(), true); // idle 下又可配置了
    }

    fn configure_48k(engine: &EngineHandle) {
        call(engine, r#"{"jsonrpc":"2.0","id":1,"method":"configure","params":{"mode":"loopback","renderDeviceId":null,"sampleRate":48000,"blockSizeFrames":64}}"#).unwrap();
    }

    #[test]
    #[allow(non_snake_case)]
    fn openSession_未配置报32001_配置后成功回显granted() {
        let eng = engine();
        // 图未配置采样率 → -32001（specs/service/push-stream.md §3.1）
        let r = call(&eng, r#"{"jsonrpc":"2.0","id":10,"method":"openSession","params":{"sampleRate":48000,"channels":2,"format":"f32le"}}"#).unwrap();
        assert_eq!(r["error"]["code"], -32001);

        configure_48k(&eng);
        let r = call(&eng, r#"{"jsonrpc":"2.0","id":10,"method":"openSession","params":{"sampleRate":48000,"channels":2,"format":"f32le"}}"#).unwrap();
        let res = &r["result"];
        assert!(res["sessionId"].is_u64() && res["sessionId"].as_u64().unwrap() >= 1);
        assert_eq!(res["granted"], serde_json::json!({"sampleRate": 48000, "channels": 2, "format": "f32le"}));
    }

    #[test]
    #[allow(non_snake_case)]
    fn openSession_协商违规报32602且不消耗id() {
        let eng = engine();
        configure_48k(&eng);
        // GWT-PS-02：三种违规逐一拒绝
        for params in [
            r#"{"sampleRate":48000,"channels":1,"format":"f32le"}"#,
            r#"{"sampleRate":48000,"channels":2,"format":"s16le"}"#,
            r#"{"sampleRate":44100,"channels":2,"format":"f32le"}"#,
        ] {
            let line = format!(r#"{{"jsonrpc":"2.0","id":11,"method":"openSession","params":{}}}"#, params);
            assert_eq!(call(&eng, &line).unwrap()["error"]["code"], -32602);
        }
        // 参数缺失 / 类型错误同属 -32602
        assert_eq!(call(&eng, r#"{"jsonrpc":"2.0","id":11,"method":"openSession","params":{"channels":2,"format":"f32le"}}"#).unwrap()["error"]["code"], -32602);
        assert_eq!(call(&eng, r#"{"jsonrpc":"2.0","id":11,"method":"openSession","params":{"sampleRate":48000,"channels":"2","format":"f32le"}}"#).unwrap()["error"]["code"], -32602);
        assert_eq!(call(&eng, r#"{"jsonrpc":"2.0","id":11,"method":"openSession","params":{"sampleRate":48.5,"channels":2,"format":"f32le"}}"#).unwrap()["error"]["code"], -32602);
        // 被拒请求未消耗 id：合法 open 得到的 id 严格递增且无空洞
        let r = call(&eng, r#"{"jsonrpc":"2.0","id":12,"method":"openSession","params":{"sampleRate":48000,"channels":2,"format":"f32le"}}"#).unwrap();
        assert_eq!(r["result"]["sessionId"], 1);
        let r = call(&eng, r#"{"jsonrpc":"2.0","id":13,"method":"openSession","params":{"sampleRate":48000,"channels":2,"format":"f32le"}}"#).unwrap();
        assert_eq!(r["result"]["sessionId"], 2);
    }

    #[test]
    #[allow(non_snake_case)]
    fn closeSession_成功回closed_未知与重复报32602() {
        let eng = engine();
        configure_48k(&eng);
        // GWT-PS-04：未知 id 拒绝
        assert_eq!(call(&eng, r#"{"jsonrpc":"2.0","id":14,"method":"closeSession","params":{"sessionId":7}}"#).unwrap()["error"]["code"], -32602);
        let r = call(&eng, r#"{"jsonrpc":"2.0","id":15,"method":"openSession","params":{"sampleRate":48000,"channels":2,"format":"f32le"}}"#).unwrap();
        let sid = r["result"]["sessionId"].as_u64().unwrap();
        // 成功关闭
        assert_eq!(call(&eng, &format!(r#"{{"jsonrpc":"2.0","id":16,"method":"closeSession","params":{{"sessionId":{sid}}}}}"#)).unwrap()["result"]["closed"], true);
        // 重复 close 不幂等
        assert_eq!(call(&eng, &format!(r#"{{"jsonrpc":"2.0","id":17,"method":"closeSession","params":{{"sessionId":{sid}}}}}"#)).unwrap()["error"]["code"], -32602);
        // sessionId 类型错 / 缺失 → -32602
        assert_eq!(call(&eng, r#"{"jsonrpc":"2.0","id":18,"method":"closeSession","params":{"sessionId":"7"}}"#).unwrap()["error"]["code"], -32602);
        assert_eq!(call(&eng, r#"{"jsonrpc":"2.0","id":19,"method":"closeSession","params":{}}"#).unwrap()["error"]["code"], -32602);
    }

    #[test]
    #[allow(non_snake_case)]
    fn openSession_运行相位可开_会话跨启停存活() {
        let eng = engine();
        configure_48k(&eng);
        let r = call(&eng, r#"{"jsonrpc":"2.0","id":20,"method":"openSession","params":{"sampleRate":48000,"channels":2,"format":"f32le"}}"#).unwrap();
        let sid = r["result"]["sessionId"].as_u64().unwrap();
        call(&eng, r#"{"jsonrpc":"2.0","id":21,"method":"start"}"#).unwrap();
        assert_eq!(eng.get_state()["phase"], "running");
        // 运行中再次打开与关闭均合法（会话与相位解耦）
        let r2 = call(&eng, r#"{"jsonrpc":"2.0","id":22,"method":"openSession","params":{"sampleRate":48000,"channels":2,"format":"f32le"}}"#).unwrap();
        assert!(r2["result"]["sessionId"].as_u64().unwrap() > sid);
        assert_eq!(eng.sessions().active_ids().len(), 2);
        call(&eng, r#"{"jsonrpc":"2.0","id":23,"method":"stop"}"#).unwrap();
        assert_eq!(eng.get_state()["phase"], "idle");
        assert_eq!(eng.sessions().active_ids().len(), 2, "stop 不影响会话生命周期");
    }

    #[test]
    #[allow(non_snake_case)]
    fn openSession_id空间耗尽报32000() {
        let eng = engine();
        configure_48k(&eng);
        // 测试钩子把 id 计数推到 u32 上限：最后一个 id 仍可分配，其后耗尽 → -32000
        eng.sessions().force_next_session_id(u32::MAX);
        let open_line = r#"{"jsonrpc":"2.0","id":24,"method":"openSession","params":{"sampleRate":48000,"channels":2,"format":"f32le"}}"#;
        let r = call(&eng, open_line).unwrap();
        assert_eq!(r["result"]["sessionId"].as_u64().unwrap(), u32::MAX as u64);
        assert_eq!(call(&eng, open_line).unwrap()["error"]["code"], -32000);
    }
}