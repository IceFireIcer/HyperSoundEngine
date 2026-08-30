//! hse-cli —— 最小调参客户端（连接 ws://host:port 的 JSON-RPC 控制面）。
//!
//! 用法：hse-cli [--url ws://127.0.0.1:4780] <子命令> [参数]
//! 子命令：list-devices / get-state / configure / start / stop / set-params <json 文件>

use serde_json::{json, Value};
use tungstenite::{Message, WebSocket};

const RESET: &str = "\x1b[0m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RED: &str = "\x1b[31m";
const CYAN: &str = "\x1b[36m";
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";

/// CLI 侧连接类型：connect 返回可能带 TLS 包装的流（ws 场景即 Plain）。
type CliWs = WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>;

const USAGE: &str = "hse-cli —— HyperSoundEngine 引擎服务调参客户端\n\n用法：\n  hse-cli [--url ws://127.0.0.1:4780] <子命令> [参数]\n\n子命令：\n  list-devices                枚举渲染/捕获设备（--full 显示完整设备 id）\n  get-state                   查询相位/配置/统计/参数快照\n  configure [--mode loopback|capture] [--input-device <id>] [--output-device <id>] [--rate N] [--block N]\n                              配置捕获源与独立输出（缺省=默认 loopback 源和默认输出）\n                              兼容别名 --device <id> 等同 loopback 的 --input-device\n  start                       启动引擎管线\n  stop                        停止引擎管线\n  set-params <file.json>      发送参数快照（JSON 对象）并显示警告";

/// CLI 主入口，返回进程退出码。
pub fn run(args: impl Iterator<Item = String>) -> i32 {
    let all: Vec<String> = args.collect();
    let mut url: Option<String> = None;
    let mut full_ids = false;
    let mut positional: Vec<String> = Vec::new();
    let mut iter = all.iter().peekable();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-h" | "--help" | "help" => {
                println!("{}", USAGE);
                return 0;
            }
            "--url" => match iter.next() {
                Some(v) => url = Some(v.clone()),
                None => return fail("--url 缺少取值"),
            },
            "--full" => full_ids = true,
            other => positional.push(other.to_string()),
        }
    }
    let url = url
        .or_else(|| std::env::var("HSE_SERVICE_URL").ok())
        .unwrap_or_else(|| "ws://127.0.0.1:4780".to_string());

    let Some(command) = positional.first() else {
        println!("{}", USAGE);
        return 2;
    };
    let rest = &positional[1..];

    // set-params 需要先读文件再连网；其余命令直接连。
    match command.as_str() {
        "list-devices" => with_connection(&url, |ws| {
            let resp = request(ws, "listDevices", json!({}))?;
            print_devices(&resp["result"], full_ids);
            Ok(0)
        }),
        "get-state" => with_connection(&url, |ws| {
            let resp = request(ws, "getState", json!({}))?;
            print_state(&resp["result"]);
            Ok(0)
        }),
        "configure" => match build_configure(rest) {
            Ok(params) => with_connection(&url, |ws| {
                let resp = request(ws, "configure", params)?;
                println!("{}已应用：{}{}", GREEN, resp["result"]["applied"], RESET);
                Ok(0)
            }),
            Err(m) => fail(&m),
        },
        "start" => with_connection(&url, |ws| {
            let resp = request(ws, "start", json!({}))?;
            println!("{}已启动：{}{}", GREEN, resp["result"], RESET);
            Ok(0)
        }),
        "stop" => with_connection(&url, |ws| {
            let resp = request(ws, "stop", json!({}))?;
            println!("{}已停止：{}{}", GREEN, resp["result"], RESET);
            Ok(0)
        }),
        "set-params" => {
            let Some(path) = rest.first() else {
                return fail("set-params 需要 JSON 文件路径");
            };
            match std::fs::read_to_string(path) {
                Ok(text) => match serde_json::from_str::<Value>(&text) {
                    Ok(value) if value.is_object() => with_connection(&url, move |ws| {
                        let resp = request(ws, "setParams", json!({"params": value}))?;
                        println!("{}已接受{}", GREEN, RESET);
                        if let Some(warnings) = resp["result"]["warnings"].as_array() {
                            for w in warnings {
                                println!("{}[警告] {}{}", YELLOW, w, RESET);
                            }
                        }
                        Ok(0)
                    }),
                    Ok(_) => fail("JSON 文件顶层必须是对象"),
                    Err(e) => fail(&format!("JSON 解析失败：{}", e)),
                },
                Err(e) => fail(&format!("读取文件失败：{}", e)),
            }
        }
        other => fail(&format!("未知子命令：{}（-h 查看用法）", other)),
    }
}

fn fail(message: &str) -> i32 {
    eprintln!("{}错误：{}{}", RED, message, RESET);
    2
}

fn build_configure(rest: &[String]) -> Result<Value, String> {
    let mut mode = "loopback".to_string();
    let mut input_device: Option<String> = None;
    let mut legacy_device = false;
    let mut output_device: Option<String> = None;
    let mut output_explicit = false;
    let mut rate: u32 = 48_000;
    let mut block: u32 = 256;
    let mut idx = 0usize;
    while idx < rest.len() {
        match rest[idx].as_str() {
            "--mode" => {
                idx += 1;
                mode = rest.get(idx).ok_or("--mode 缺少取值")?.clone();
                if mode != "loopback" && mode != "capture" {
                    return Err("--mode 仅支持 loopback 或 capture".into());
                }
            }
            "--device" => {
                idx += 1;
                legacy_device = true;
                input_device = Some(rest.get(idx).ok_or("--device 缺少取值")?.clone());
            }
            "--input-device" => {
                idx += 1;
                input_device = Some(rest.get(idx).ok_or("--input-device 缺少取值")?.clone());
            }
            "--output-device" => {
                idx += 1;
                output_device = Some(rest.get(idx).ok_or("--output-device 缺少取值")?.clone());
                output_explicit = true;
            }
            "--rate" => {
                idx += 1;
                rate = rest
                    .get(idx)
                    .ok_or("--rate 缺少取值")?
                    .parse()
                    .map_err(|_| "--rate 需要整数")?;
            }
            "--block" => {
                idx += 1;
                block = rest
                    .get(idx)
                    .ok_or("--block 缺少取值")?
                    .parse()
                    .map_err(|_| "--block 需要整数")?;
            }
            other => return Err(format!("configure 未知参数：{}", other)),
        }
        idx += 1;
    }
    if legacy_device && mode != "loopback" {
        return Err("--device 仅是 loopback 模式的兼容别名".into());
    }
    let mut params = json!({
        "mode": mode,
        "sampleRate": rate,
        "blockSizeFrames": block,
    });
    let source_key = if mode == "capture" {
        "captureDeviceId"
    } else {
        "renderDeviceId"
    };
    params[source_key] = json!(input_device);
    if output_explicit {
        params["outputDeviceId"] = json!(output_device);
    }
    Ok(params)
}

fn connect(url: &str) -> Result<CliWs, String> {
    let (ws, _resp) = tungstenite::connect(url).map_err(|e| format!("连接 {} 失败：{}", url, e))?;
    Ok(ws)
}

type CliResult = Result<i32, String>;

fn with_connection(url: &str, body: impl FnOnce(&mut CliWs) -> CliResult) -> i32 {
    let mut ws = match connect(url) {
        Ok(w) => w,
        Err(e) => return fail(&e),
    };
    let outcome = body(&mut ws);
    let _ = ws.close(None);
    match outcome {
        Ok(code) => code,
        Err(message) => fail(&message),
    }
}

/// 发送一个请求并等待其响应（跳过中途到达的事件通知）。
fn request(ws: &mut CliWs, method: &str, params: Value) -> Result<Value, String> {
    static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string();
    ws.send(Message::text(line))
        .map_err(|e| format!("发送失败：{}", e))?;
    loop {
        let msg = ws.read().map_err(|e| format!("等待响应失败：{}", e))?;
        match msg {
            Message::Text(text) => {
                let v: Value =
                    serde_json::from_str(&text).map_err(|e| format!("响应非 JSON：{}", e))?;
                if v.get("method").is_some() && v.get("id").is_none() {
                    // 控制面事件通知：灰色展示后继续等本请求的响应。
                    println!("{}[事件] {} {}{}", DIM, v["method"], v["params"], RESET);
                    continue;
                }
                if v.get("id").and_then(|i| i.as_u64()) == Some(id) {
                    if let Some(err) = v.get("error") {
                        return Err(format!("服务端返回 {}：{}", err["code"], err["message"]));
                    }
                    return Ok(v);
                }
            }
            Message::Close(frame) => return Err(format!("连接被关闭：{:?}", frame)),
            _ => {}
        }
    }
}

fn trunc(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count <= max_chars {
        s.to_string()
    } else {
        let head: String = s.chars().take(max_chars).collect();
        format!("{}…", head)
    }
}

fn print_devices(result: &Value, full: bool) {
    for (label, key) in [
        ("渲染设备 (render)", "render"),
        ("捕获设备 (capture)", "capture"),
    ] {
        println!("{}{}{}", BOLD, label, RESET);
        let empty: Vec<Value> = Vec::new();
        for d in result[key].as_array().unwrap_or(&empty) {
            let mark = if d["isDefault"].as_bool().unwrap_or(false) {
                format!("{}[默认]{}", CYAN, RESET)
            } else {
                "      ".to_string()
            };
            let id_text = if full {
                d["id"].as_str().unwrap_or("").to_string()
            } else {
                trunc(d["id"].as_str().unwrap_or(""), 18)
            };
            println!(
                "  {} {}  {}{}{} {}",
                mark, YELLOW, d["name"], RESET, DIM, id_text
            );
            if !full {
                println!("        {}完整 id：{}{}", DIM, d["id"], RESET);
            }
        }
        if result[key].as_array().map(|a| a.is_empty()).unwrap_or(true) {
            println!("  {}（空）{}", DIM, RESET);
        }
    }
}

fn print_state(state: &Value) {
    let phase = state["phase"].as_str().unwrap_or("?");
    let color = match phase {
        "running" => GREEN,
        "idle" => CYAN,
        _ => YELLOW,
    };
    println!("相位：{}{}{}（{}）", color, BOLD, phase, RESET);
    println!("配置：{}", state["config"]);
    println!("统计：{}", state["stats"]);
    println!("最近参数：{}", state["lastParams"]);
}

#[cfg(test)]
mod tests {
    use super::build_configure;
    use serde_json::json;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn configure_旧参数保持四字段loopback请求() {
        assert_eq!(
            build_configure(&args(&[
                "--device",
                "render-headphone",
                "--rate",
                "44100",
                "--block",
                "128"
            ]))
            .unwrap(),
            json!({"mode":"loopback","renderDeviceId":"render-headphone","sampleRate":44100,"blockSizeFrames":128})
        );
    }

    #[test]
    fn configure_旧device别名不受参数顺序影响() {
        assert!(
            build_configure(&args(&["--device", "cable-output", "--mode", "capture"])).is_err()
        );
    }

    #[test]
    fn configure_capture与输出设备独立() {
        assert_eq!(
            build_configure(&args(&[
                "--mode",
                "capture",
                "--input-device",
                "cable-output",
                "--output-device",
                "render-headphone"
            ]))
            .unwrap(),
            json!({"mode":"capture","captureDeviceId":"cable-output","outputDeviceId":"render-headphone","sampleRate":48000,"blockSizeFrames":256})
        );
    }
}
