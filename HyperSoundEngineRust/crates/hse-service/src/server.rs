//! WebSocket 控制面：接受连接、逐请求分派、事件广播与数据面兜底监督。
//!
//! 每个客户端连接一个线程；事件中枢线程消费数据面经有界通道转发来的事件，
//! 序列化成通知后 try_send 给每个在线客户端（满/断开即剔除）。中枢同时
//! 周期调用 poll_supervision 兜底数据面异常停机。
//!
//! **分流规则**（specs/service/push-stream.md §二，Phase 3 起二进制帧有语义）：
//! 同端口复用同一条 WS 连接，按 opcode 分流——文本帧 → JSON-RPC 分发器；
//! 二进制帧 → 音频入口解析器（sessions::ingest_frame，违规/未知会话一律
//! 静默丢弃，不回错误不发事件）；Ping/Pong/Close 为传输层语义。
//! 二进制帧入队发生在本连接线程（控制面线程允许阻塞/分配，§九），
//! 不触碰 DSP/渲染线程的实时纪律。

use std::collections::VecDeque;
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use tungstenite::Message;

use crate::engine::EngineHandle;
use crate::rpc;
use crate::state::ServiceEvent;

/// 在线客户端广播表（连接 id + 每客户端一条有界回传通道）。
pub type ClientTable = Arc<Mutex<Vec<(u64, SyncSender<String>)>>>;

const HUB_TICK_MS: u64 = 50;
/// 连接标识分配器：openSession 记录归属，断线时据此自动清理会话。
static NEXT_CONN_ID: AtomicU64 = AtomicU64::new(1);

/// 事件中枢主循环。
pub fn run_hub(engine: Arc<EngineHandle>, rx: Receiver<ServiceEvent>, clients: ClientTable) {
    let mut pending: VecDeque<ServiceEvent> = VecDeque::new();
    loop {
        match rx.recv_timeout(Duration::from_millis(HUB_TICK_MS)) {
            Ok(ev) => {
                broadcast(&clients, render_event(&engine, ev));
                // 清空积压（若有），保证事件顺序转发。
                while let Ok(ev) = rx.try_recv() {
                    pending.push_back(ev);
                }
                while let Some(ev) = pending.pop_front() {
                    broadcast(&clients, render_event(&engine, ev));
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                // 发送端全灭（引擎已销毁）：继续空转直到进程退出也无妨，直接返回更干净。
                return;
            }
        }
        engine.poll_supervision();
    }
}

fn broadcast(clients: &ClientTable, text: String) {
    let mut list = clients.lock().unwrap();
    list.retain(|(_, client)| client.try_send(text.clone()).is_ok());
}

fn broadcast_except(clients: &ClientTable, excluded_owner: u64, text: String) {
    let mut list = clients.lock().unwrap();
    list.retain(|(owner, client)| {
        *owner == excluded_owner || client.try_send(text.clone()).is_ok()
    });
}

fn render_event(engine: &EngineHandle, ev: ServiceEvent) -> String {
    match ev {
        ServiceEvent::Phase { from, to } => json!({
            "jsonrpc": "2.0",
            "method": "event.phase",
            "params": {"from": from, "to": to},
        })
        .to_string(),
        ServiceEvent::Xrun { dir, count } => {
            let (total_in, total_out) = engine.xrun_totals();
            json!({
                "jsonrpc": "2.0",
                "method": "event.xrun",
                "params": {"dir": dir, "count": count, "totalIn": total_in, "totalOut": total_out},
            })
            .to_string()
        }
    }
}

/// 单连接处理：注册广播表 → 非阻塞轮询（读请求分派 + 转发事件通知）。
///
/// tungstenite 同步栈无法跨线程拆分读写，故在单线程内以短轮询同时服务
/// 入站请求/推流帧与事件出站（控制面线程不受实时纪律约束）。
/// 连接断开时自动清理本连接打开的全部推流会话（GWT-PS-06 防泄漏）。
fn handle_connection(
    stream: TcpStream,
    engine: &Arc<EngineHandle>,
    clients: &ClientTable,
    owner: u64,
) {
    let _ = stream.set_nodelay(true);
    let mut ws = match tungstenite::accept(stream) {
        Ok(ws) => ws,
        Err(e) => {
            eprintln!("[hse-service] WS 握手失败：{}", e);
            return;
        }
    };
    // 握手完成后切非阻塞：读不到数据时以 WouldBlock 返回，交还循环处理出站。
    if let Err(e) = ws.get_ref().set_nonblocking(true) {
        eprintln!("[hse-service] 切换非阻塞失败：{}", e);
        return;
    }
    let (tx, rx) = std::sync::mpsc::sync_channel::<String>(256);
    clients.lock().unwrap().push((owner, tx));
    loop {
        // —— 入站：逐帧读取（WouldBlock 视为暂无数据）——
        match ws.read() {
            Ok(Message::Text(text)) => {
                for response in rpc::handle_messages(engine, owner, &text) {
                    let is_phase_event = serde_json::from_str::<serde_json::Value>(&response)
                        .ok()
                        .and_then(|value| {
                            value
                                .get("method")
                                .and_then(|method| method.as_str())
                                .map(str::to_owned)
                        })
                        .as_deref()
                        == Some("event.phase");
                    if ws.send(Message::text(response.clone())).is_err() {
                        break;
                    }
                    if is_phase_event {
                        // 请求连接已按同步顺序收到事件；其余连接仍收到一次广播。
                        broadcast_except(clients, owner, response);
                    }
                }
            }
            // 音频入口：二进制帧按帧头路由进会话环；违规/未知会话静默丢弃
            // （GWT-PS-08/09/13：不回错误、不发事件、不影响文本通道）。
            Ok(Message::Binary(bytes)) => {
                engine.sessions().ingest_frame(&bytes);
            }
            Ok(Message::Ping(payload)) => {
                let _ = ws.send(Message::Pong(payload));
            }
            Ok(Message::Close(_)) => break,
            Ok(_) => {}
            Err(tungstenite::Error::ConnectionClosed) | Err(tungstenite::Error::AlreadyClosed) => {
                break
            }
            Err(tungstenite::Error::Io(ref e)) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(tungstenite::Error::Protocol(
                tungstenite::error::ProtocolError::ResetWithoutClosingHandshake,
            )) => break,
            Err(e) => {
                eprintln!("[hse-service] 连接读错误：{}", e);
                break;
            }
        }
        // —— 出站：把广播表里发给本连接的事件写回 WS ——
        let mut forwarded_any = false;
        while let Ok(text) = rx.try_recv() {
            forwarded_any = true;
            if ws.send(Message::text(text)).is_err() {
                break;
            }
        }
        if !forwarded_any {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    // 断线自动清理：本连接上打开的全部会话立即关闭，环内未消费块一并丢弃。
    engine.sessions().close_owner(owner);
}

/// 接受循环：阻塞监听并逐连接开线程。
pub fn serve(
    listener: TcpListener,
    engine: Arc<EngineHandle>,
    clients: ClientTable,
) -> std::io::Result<()> {
    for incoming in listener.incoming() {
        match incoming {
            Ok(stream) => {
                let engine = Arc::clone(&engine);
                let clients = Arc::clone(&clients);
                let owner = NEXT_CONN_ID.fetch_add(1, Ordering::Relaxed);
                let spawned = std::thread::Builder::new()
                    .name("hse-ctrl-conn".into())
                    .spawn(move || handle_connection(stream, &engine, &clients, owner));
                if let Err(e) = spawned {
                    eprintln!("[hse-service] 控制连接线程创建失败：{}", e);
                }
            }
            Err(e) => eprintln!("[hse-service] accept 失败：{}", e),
        }
    }
    Ok(())
}
