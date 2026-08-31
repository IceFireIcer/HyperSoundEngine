//! JSON-RPC 2.0 解析与方法分发表（over WebSocket 文本帧）。
//!
//! 错误码：-32700 解析错误｜-32600 无效请求｜-32601 方法不存在｜-32602 参数无效｜-32000 后端失败｜-32001 状态不允许。
//! 方法表：Phase 2 六方法（listDevices/getState/configure/start/stop/setParams）
//! + stage 22 加性方法 loadHrtf
//! + Phase 3 推流会话两方法（openSession/closeSession，specs/service/push-stream.md）。
//! 无 id 的请求按通知处理：照常执行副作用，但不回包。

use serde_json::{json, Value};

use crate::engine::{EngineHandle, RpcFault};

/// 处理一行请求文本；返回 Some(响应文本) 或 None（通知/不回包）。
///
/// `owner` 为发起连接的标识，仅供 openSession 记录会话归属（断线自动清理）。
pub fn handle_line(engine: &EngineHandle, owner: u64, line: &str) -> Option<String> {
    handle_messages(engine, owner, line)
        .into_iter()
        .find(|message| {
            serde_json::from_str::<Value>(message)
                .ok()
                .is_some_and(|value| value.get("id").is_some())
        })
}

/// 处理一条控制请求，返回必须按数组顺序发送的通知与响应。
/// start/stop 的同步 phase 通知由当前连接直接发送，避免异步广播抢序。
pub fn handle_messages(engine: &EngineHandle, owner: u64, line: &str) -> Vec<String> {
    let parsed: Result<Value, _> = serde_json::from_str(line);
    let value = match parsed {
        Ok(v) => v,
        Err(err) => {
            return vec![
                error_response(Value::Null, -32700, format!("Parse error：{}", err)).to_string(),
            ]
        }
    };
    let obj = match value.as_object() {
        Some(o) if !o.is_empty() => o,
        _ => {
            return vec![error_response(
                Value::Null,
                -32600,
                "Invalid Request：顶层必须是非空 JSON 对象".into(),
            )
            .to_string()]
        }
    };
    let is_notification = !obj.contains_key("id");
    let id = match obj.get("id") {
        None => Value::Null,
        Some(Value::String(s)) => Value::String(s.clone()),
        Some(Value::Number(n)) if n.is_i64() || n.is_u64() => Value::Number(n.clone()),
        Some(_) => {
            return vec![error_response(
                Value::Null,
                -32600,
                "Invalid Request：id 必须为整数或字符串".into(),
            )
            .to_string()]
        }
    };
    if obj.get("jsonrpc").and_then(|j| j.as_str()) != Some("2.0") {
        return reply(
            is_notification,
            error_response(id, -32600, "Invalid Request：jsonrpc 必须为 \"2.0\"".into()),
        )
        .into_iter()
        .collect();
    }
    let method = match obj.get("method").and_then(|m| m.as_str()) {
        Some(m) => m.to_string(),
        None => {
            return reply(
                is_notification,
                error_response(id, -32600, "Invalid Request：缺少 method 字符串".into()),
            )
            .into_iter()
            .collect()
        }
    };
    let params = match obj.get("params") {
        None => json!({}),
        Some(value) if value.is_object() => value.clone(),
        Some(_) => {
            return reply(
                is_notification,
                error_response(id, -32602, "Invalid params：params 必须是 JSON 对象".into()),
            )
            .into_iter()
            .collect()
        }
    };
    if matches!(
        method.as_str(),
        "listDevices" | "getState" | "start" | "stop"
    ) && !params.as_object().is_some_and(serde_json::Map::is_empty)
    {
        return reply(
            is_notification,
            error_response(id, -32602, format!("Invalid params：{method} 只接受空对象")),
        )
        .into_iter()
        .collect();
    }
    let phase_before = engine.get_state()["phase"]
        .as_str()
        .unwrap_or("idle")
        .to_owned();
    let response = match dispatch(engine, owner, &method, &params) {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err(fault) => error_response(id, fault.code, fault.message),
    };

    let mut messages = Vec::new();
    let succeeded = response.get("result").is_some();
    if method == "start" && succeeded {
        messages.push(phase_event(&phase_before, "starting"));
        messages.push(phase_event("starting", "running"));
    } else if method == "start" && response["error"]["code"] == -32000 {
        messages.push(phase_event(&phase_before, "starting"));
        messages.push(phase_event("starting", "idle"));
    } else if method == "stop" && succeeded {
        messages.push(phase_event(&phase_before, "stopping"));
        messages.push(phase_event("stopping", "idle"));
    }
    if !is_notification {
        messages.push(response.to_string());
    }
    messages
}

fn phase_event(from: &str, to: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "method": "event.phase",
        "params": {"from": from, "to": to},
    })
    .to_string()
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

fn dispatch(
    engine: &EngineHandle,
    owner: u64,
    method: &str,
    params: &Value,
) -> Result<Value, RpcFault> {
    match method {
        "listDevices" => engine.list_devices(),
        "getState" => Ok(engine.get_state()),
        "configure" => {
            let obj = params
                .as_object()
                .ok_or_else(|| RpcFault::invalid_params("configure 需要 params 对象"))?;
            engine.configure(obj)
        }
        "loadHrtf" => {
            let obj = params
                .as_object()
                .ok_or_else(|| RpcFault::invalid_params("loadHrtf 需要 params 对象"))?;
            engine.load_hrtf(obj)
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
        other => Err(RpcFault::new(
            -32601,
            format!("Method not found：未知方法 {}", other),
        )),
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
        let r = call(
            &eng,
            r#"{"jsonrpc":"2.0","id":"a","method":"configure","params":{"sampleRate":48000}}"#,
        )
        .unwrap();
        assert_eq!(r["error"]["code"], -32602);
        assert_eq!(r["id"], "a");

        let r = call(&eng, r#"{"jsonrpc":"2.0","id":3,"method":"start"}"#).unwrap();
        assert_eq!(r["error"]["code"], -32001, "未配置先 start 应为状态不允许");
    }

    #[test]
    fn set_params_缺params键报参数无效() {
        let eng = engine();
        let r = call(
            &eng,
            r#"{"jsonrpc":"2.0","id":4,"method":"setParams","params":{}}"#,
        )
        .unwrap();
        assert_eq!(r["error"]["code"], -32602);
    }

    #[test]
    fn 显式params必须为对象且无参方法只接受空对象() {
        let eng = engine();
        let initial = eng.get_state();

        for (method, params) in [
            ("listDevices", json!(null)),
            ("getState", json!([])),
            ("start", json!("bad")),
            ("stop", json!(1)),
        ] {
            let response = call(
                &eng,
                &json!({"jsonrpc":"2.0","id":30,"method":method,"params":params}).to_string(),
            )
            .unwrap();
            assert_eq!(response["error"]["code"], -32602, "method={method}");
            assert_eq!(eng.get_state(), initial, "非法 params 不得产生副作用");
        }

        for method in ["listDevices", "getState", "start", "stop"] {
            let response = call(
                &eng,
                &json!({"jsonrpc":"2.0","id":31,"method":method,"params":{"extra":true}})
                    .to_string(),
            )
            .unwrap();
            assert_eq!(response["error"]["code"], -32602, "method={method}");
            assert_eq!(eng.get_state(), initial, "非空 params 不得产生副作用");
        }
    }

    #[test]
    fn 省略params按空对象处理() {
        let eng = engine();
        let response = call(&eng, r#"{"jsonrpc":"2.0","id":32,"method":"getState"}"#).unwrap();
        assert_eq!(response["result"]["phase"], "idle");
    }

    #[test]
    fn 通知执行副作用但不回包() {
        let eng = engine();
        let out = handle_line(
            &eng,
            0,
            r#"{"jsonrpc":"2.0","method":"configure","params":{"mode":"loopback","renderDeviceId":null,"sampleRate":48000,"blockSizeFrames":128}}"#,
        );
        assert!(out.is_none(), "通知不得回包");
        assert_eq!(
            eng.get_state()["config"]["blockSizeFrames"],
            128,
            "副作用已生效"
        );
    }

    #[test]
    fn 显式非法id报无效请求且不执行副作用() {
        let eng = engine();
        let initial = eng.get_state()["config"].clone();
        for id in [
            serde_json::json!(null),
            serde_json::json!(true),
            serde_json::json!({"nested": 1}),
            serde_json::json!([1]),
        ] {
            let line = serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "configure",
                "params": {
                    "mode": "loopback",
                    "renderDeviceId": null,
                    "sampleRate": 48000,
                    "blockSizeFrames": 128
                }
            })
            .to_string();
            let response = call(&eng, &line).unwrap();
            assert_eq!(response["error"]["code"], -32600);
            assert!(response["id"].is_null());
            assert_eq!(
                eng.get_state()["config"],
                initial,
                "非法 id 不得执行 configure"
            );
        }
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
    fn load_hrtf_通过rpc分派且校验本地路径() {
        let eng = engine();
        let before = call(
            &eng,
            r#"{"jsonrpc":"2.0","id":40,"method":"loadHrtf","params":{"path":"relative.sofa"}}"#,
        )
        .unwrap();
        assert_eq!(
            before["error"]["code"], -32001,
            "未 configure 优先报状态错误"
        );

        configure_48k(&eng);
        let response = call(
            &eng,
            r#"{"jsonrpc":"2.0","id":41,"method":"loadHrtf","params":{"path":"relative.sofa"}}"#,
        )
        .unwrap();
        assert_eq!(response["error"]["code"], -32602);
        assert_eq!(eng.get_state()["hrtf"], json!({"loaded":false}));
    }

    #[test]
    fn set_params_lastParams只暴露规范化已知wire键() {
        let eng = engine();
        let response = call(
            &eng,
            r#"{"jsonrpc":"2.0","id":33,"method":"setParams","params":{"params":{"unknownTop":1,"biquad":{"f0":1200,"unknownChild":2},"reverbRoute":"unknown-route","limiter":{"enabled":false}}}}"#,
        )
        .unwrap();
        assert_eq!(
            response["result"]["warnings"],
            json!(["biquad.unknownChild", "unknownTop"])
        );

        let last = eng.get_state()["lastParams"].clone();
        assert!(last.get("unknownTop").is_none());
        assert!(last["biquad"].get("unknownChild").is_none());
        assert_eq!(
            last["biquad"],
            json!({"type":"peaking","f0":1200.0,"q":1.0,"gainDb":0.0})
        );
        assert_eq!(last["reverbRoute"], "simple");
        assert_eq!(
            last["limiter"],
            json!({
                "enabled":false,
                "thresholdDb":-1.0,
                "lookaheadMs":5.0,
                "attackMs":0.5,
                "releaseMs":150.0,
                "truePeak":true
            })
        );
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
        assert_eq!(
            call(&eng, cfg_line).unwrap()["result"]["applied"]["sampleRate"],
            48000
        );
        assert_eq!(
            call(&eng, r#"{"jsonrpc":"2.0","id":2,"method":"start"}"#).unwrap()["result"]
                ["started"],
            true
        );
        assert_eq!(eng.get_state()["phase"], "running");
        let sp = call(&eng, r#"{"jsonrpc":"2.0","id":3,"method":"setParams","params":{"params":{"biquad":{"type":"peaking","f0":1000,"q":1,"gainDb":6},"unknownKey":1}}}"#).unwrap();
        assert_eq!(sp["result"]["accepted"], true);
        assert_eq!(sp["result"]["warnings"].as_array().unwrap().len(), 1);
        assert_eq!(
            call(&eng, r#"{"jsonrpc":"2.0","id":4,"method":"stop"}"#).unwrap()["result"]["stopped"],
            true
        );
        assert_eq!(eng.get_state()["phase"], "idle");
        // 运行中重复 stop / configure 都应被拒
        assert_eq!(
            call(&eng, r#"{"jsonrpc":"2.0","id":5,"method":"stop"}"#).unwrap()["error"]["code"],
            -32001
        );
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
        assert_eq!(
            res["granted"],
            serde_json::json!({"sampleRate": 48000, "channels": 2, "format": "f32le"})
        );
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
            let line = format!(
                r#"{{"jsonrpc":"2.0","id":11,"method":"openSession","params":{}}}"#,
                params
            );
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
        assert_eq!(
            call(
                &eng,
                r#"{"jsonrpc":"2.0","id":14,"method":"closeSession","params":{"sessionId":7}}"#
            )
            .unwrap()["error"]["code"],
            -32602
        );
        let r = call(&eng, r#"{"jsonrpc":"2.0","id":15,"method":"openSession","params":{"sampleRate":48000,"channels":2,"format":"f32le"}}"#).unwrap();
        let sid = r["result"]["sessionId"].as_u64().unwrap();
        // 成功关闭
        assert_eq!(call(&eng, &format!(r#"{{"jsonrpc":"2.0","id":16,"method":"closeSession","params":{{"sessionId":{sid}}}}}"#)).unwrap()["result"]["closed"], true);
        // 重复 close 不幂等
        assert_eq!(call(&eng, &format!(r#"{{"jsonrpc":"2.0","id":17,"method":"closeSession","params":{{"sessionId":{sid}}}}}"#)).unwrap()["error"]["code"], -32602);
        // sessionId 类型错 / 缺失 → -32602
        assert_eq!(
            call(
                &eng,
                r#"{"jsonrpc":"2.0","id":18,"method":"closeSession","params":{"sessionId":"7"}}"#
            )
            .unwrap()["error"]["code"],
            -32602
        );
        assert_eq!(
            call(
                &eng,
                r#"{"jsonrpc":"2.0","id":19,"method":"closeSession","params":{}}"#
            )
            .unwrap()["error"]["code"],
            -32602
        );
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
        assert_eq!(
            eng.sessions().active_ids().len(),
            2,
            "stop 不影响会话生命周期"
        );
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
