//! hse-service —— 引擎服务进程入口。
//!
//! 端口解析优先级：--port 参数 > HSE_SERVICE_PORT 环境变量 > 默认 4780。
//! 监听固定 127.0.0.1（控制面仅限本机，规划书 §2.2）。

use std::net::TcpListener;
use std::sync::mpsc::sync_channel;
use std::sync::{Arc, Mutex};

use hse_service::backend::WasapiFactory;
use hse_service::engine::EngineHandle;
use hse_service::server;
use hse_service::state::ServiceEvent;

/// 契约默认端口。
const DEFAULT_PORT: u16 = 4780;

fn resolve_port(args: impl Iterator<Item = String>) -> Option<u16> {
    let args: Vec<String> = args.collect();
    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        if arg == "--port" {
            if let Some(v) = iter.next() {
                return v.parse::<u16>().ok();
            }
        } else if let Some(rest) = arg.strip_prefix("--port=") {
            return rest.parse::<u16>().ok();
        }
    }
    None
}

fn main() {
    let port = resolve_port(std::env::args().skip(1))
        .or_else(|| std::env::var("HSE_SERVICE_PORT").ok().and_then(|v| v.parse::<u16>().ok()))
        .unwrap_or(DEFAULT_PORT);

    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("hse-service：绑定 127.0.0.1:{} 失败：{}", port, e);
            std::process::exit(1);
        }
    };

    // 数据面→控制面的有界事件通道（容量 512；满则数据面丢事件不阻塞）。
    let (event_tx, event_rx) = sync_channel::<ServiceEvent>(512);
    let engine = Arc::new(EngineHandle::new(Arc::new(WasapiFactory), event_tx));
    let clients: server::ClientTable = Arc::new(Mutex::new(Vec::new()));

    let hub_engine = Arc::clone(&engine);
    let hub_clients = Arc::clone(&clients);
    std::thread::Builder::new()
        .name("hse-ctrl-hub".into())
        .spawn(move || server::run_hub(hub_engine, event_rx, hub_clients))
        .expect("事件中枢线程创建失败");

    println!(
        "hse-service {} —— 控制面 ws://127.0.0.1:{}/（--port 或 HSE_SERVICE_PORT 可覆盖端口）",
        env!("CARGO_PKG_VERSION"),
        port
    );

    if let Err(e) = server::serve(listener, engine, clients) {
        eprintln!("hse-service：接受循环异常退出：{}", e);
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 端口解析_参数与环境缺省() {
        assert_eq!(resolve_port(["--port".to_string(), "5000".to_string()].into_iter()), Some(5000));
        assert_eq!(resolve_port(["--port=5001".to_string()].into_iter()), Some(5001));
        assert_eq!(resolve_port(Vec::<String>::new().into_iter()), None);
        assert_eq!(resolve_port(["--port".to_string(), "abc".to_string()].into_iter()), None);
    }
}