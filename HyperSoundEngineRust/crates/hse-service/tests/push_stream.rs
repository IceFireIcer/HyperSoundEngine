//! 推流入口（Phase 3，specs/service/push-stream.md）集成测试：假后端全程离线，
//! 不触碰任何真实音频设备、不发声。
//!
//! 覆盖：openSession/closeSession 生命周期与错误码、二进制帧路由与违规丢弃、
//! 混后处理（会话+回环求和进 DSP）、背压 drop-oldest 与 xrunsIn/event.xrun、
//! 非运行相位入队淘汰、断线自动清理、分流正交性，以及无会话时的纯回环回归。

use std::net::TcpListener;
use std::sync::mpsc::{sync_channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hse_service::backend::BackendFactory;
use hse_service::engine::EngineHandle;
use hse_service::fake_backend::FakeFactory;
use hse_service::server;
use hse_service::state::ServiceEvent;
use serde_json::{json, Value};
use tungstenite::Message;

type Ws = tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>;

/// 组装一条合法二进制帧：sessionId u32 LE + seq u64 LE + 交错 f32LE 载荷。
fn frame(session_id: u32, seq: u64, samples: &[f32]) -> Vec<u8> {
    let mut f = Vec::with_capacity(12 + samples.len() * 4);
    f.extend_from_slice(&session_id.to_le_bytes());
    f.extend_from_slice(&seq.to_le_bytes());
    for s in samples {
        f.extend_from_slice(&s.to_le_bytes());
    }
    f
}

struct Setup {
    engine: Arc<EngineHandle>,
    factory: Arc<FakeFactory>,
    events: Receiver<ServiceEvent>,
}

fn setup_silent(capture_period: Duration, render_period: Duration) -> Setup {
    let (tx, rx) = sync_channel::<ServiceEvent>(4096);
    let factory = FakeFactory::silent_loopback(capture_period, render_period);
    let engine = Arc::new(EngineHandle::new(
        Arc::clone(&factory) as Arc<dyn BackendFactory>,
        tx,
    ));
    Setup {
        engine,
        factory,
        events: rx,
    }
}

fn setup_working(capture_period: Duration, render_period: Duration) -> Setup {
    let (tx, rx) = sync_channel::<ServiceEvent>(4096);
    let factory = FakeFactory::working(capture_period, render_period);
    let engine = Arc::new(EngineHandle::new(
        Arc::clone(&factory) as Arc<dyn BackendFactory>,
        tx,
    ));
    Setup {
        engine,
        factory,
        events: rx,
    }
}

fn wait_until(pred: impl Fn() -> bool, timeout: Duration, what: &str) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if pred() {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("等待超时：{}", what);
}

fn configure_48k(engine: &EngineHandle, block: u64) {
    engine
        .configure(
            json!({"mode": "loopback", "renderDeviceId": null, "sampleRate": 48_000u64, "blockSizeFrames": block})
                .as_object()
                .unwrap(),
        )
        .unwrap();
}

/// 直通参数：biquad 缺省直通 + reverb 全干 + limiter 旁路 ⇒ 混合值逐位透传到渲染。
fn enable_bypass(engine: &EngineHandle) {
    let resp = engine
        .set_params(&json!({"reverbSimple": {"wet": 0.0, "dry": 1.0, "preDelayMs": 0.0}, "limiter": {"enabled": false}}))
        .unwrap();
    assert_eq!(resp["accepted"], true);
}

fn open_session(engine: &EngineHandle) -> u32 {
    let resp = engine
        .open_session(
            json!({"sampleRate": 48_000u64, "channels": 2u16, "format": "f32le"})
                .as_object()
                .unwrap(),
            0,
        )
        .unwrap();
    resp["sessionId"].as_u64().unwrap() as u32
}

/// 向会话灌 N 块、每块 64 帧的恒定立体声样本。
fn push_constant(
    engine: &EngineHandle,
    session_id: u32,
    value: f32,
    chunks: usize,
    seq_base: &mut u64,
) {
    let payload = [value; 128]; // 64 立体声帧
    for _ in 0..chunks {
        assert!(engine
            .sessions()
            .ingest_frame(&frame(session_id, *seq_base, &payload)));
        *seq_base += 1;
    }
}

#[test]
fn 混后处理_双会话求和进渲染_无串扰_无丢失() {
    // idle 期预装两条等长时间线，避免用微秒级 sleep 假设线程调度速率。
    // 共 18 块，小于 capture→DSP 输入环 24 块容量；即使 DSP 暂未获调度也不会溢出。
    let s = setup_silent(Duration::ZERO, Duration::from_micros(250));
    configure_48k(&s.engine, 64);
    enable_bypass(&s.engine);
    let a = open_session(&s.engine); // id 1
    assert_eq!(a, 1);
    let b = open_session(&s.engine);
    assert_eq!(b, 2);

    const BLOCKS_PER_STAGE: usize = 6;
    const FRAMES_PER_BLOCK: usize = 64;
    const STAGES: usize = 3;
    let expected_samples = STAGES * BLOCKS_PER_STAGE * FRAMES_PER_BLOCK * 2;

    let mut seq_a = 0u64;
    let mut seq_b = 0u64;
    // 阶段一 A 独占；阶段二 A+B；阶段三 B 独占。零块用于保持两会话帧对齐。
    for (value_a, value_b) in [(0.25, 0.0), (0.25, 0.5), (0.0, 0.5)] {
        push_constant(&s.engine, a, value_a, BLOCKS_PER_STAGE, &mut seq_a);
        push_constant(&s.engine, b, value_b, BLOCKS_PER_STAGE, &mut seq_b);
    }

    s.engine.start().unwrap();
    let stage_samples = BLOCKS_PER_STAGE * FRAMES_PER_BLOCK * 2;
    let nonzero_count = || {
        s.factory
            .render_received
            .lock()
            .unwrap()
            .iter()
            .filter(|&&sample| sample != 0.0)
            .count()
    };
    wait_until(
        || nonzero_count() >= expected_samples,
        Duration::from_secs(10),
        "三段会话时间线完整到达渲染",
    );

    // xrunsIn 同时统计会话背压与 capture→DSP 输入环溢出，本用例两者都不得发生。
    assert_eq!(
        s.engine.sessions().xruns_in_total(),
        0,
        "有界预装输入不应发生丢弃"
    );
    let received = s.factory.render_received.lock().unwrap();
    let mut timeline = received.iter().copied().filter(|&sample| sample != 0.0);
    assert!(
        timeline.by_ref().take(stage_samples).all(|x| x == 0.25),
        "A 独占区被污染"
    );
    assert!(
        timeline.by_ref().take(stage_samples).all(|x| x == 0.75),
        "A+B 求和区不正确"
    );
    assert!(
        timeline.by_ref().take(stage_samples).all(|x| x == 0.5),
        "B 独占区被污染"
    );
    assert_eq!(
        timeline.next(),
        None,
        "会话时间线结束后不得出现额外非零样本"
    );
    drop(received);

    // 停止后相位回 idle；会话与相位解耦，仍存活可关
    s.engine.stop().unwrap();
    assert_eq!(s.engine.get_state()["phase"], "idle");
    assert_eq!(
        s.engine.sessions().active_ids(),
        vec![a, b],
        "stop 不影响会话生命周期"
    );
    assert_eq!(
        s.engine
            .close_session(json!({"sessionId": a}).as_object().unwrap())
            .unwrap()["closed"],
        true
    );
}

#[test]
fn 背压_运行中超速灌帧_丢旧计入xruns_in并上报事件() {
    // 捕获 20ms 一轮：消费速率 64帧/20ms，远低于灌帧速率 ⇒ 会话环必然溢出
    let s = setup_silent(Duration::from_millis(20), Duration::ZERO);
    configure_48k(&s.engine, 64);
    s.engine.start().unwrap();
    let sid = open_session(&s.engine);

    // GWT-PS-11：远超实时速率灌帧（3000 块 × 64 帧 = 192000 帧 > 环预算 131072 帧）
    let payload = [0.125_f32; 128];
    for k in 0..3000u64 {
        assert!(s.engine.sessions().ingest_frame(&frame(sid, k, &payload)));
    }
    wait_until(
        || s.engine.sessions().xruns_in_total() >= 100,
        Duration::from_secs(10),
        "运行期溢出丢弃计数增长",
    );
    // getState.stats.xrunsIn 与事件总量同源同值
    let via_state = s.engine.get_state()["stats"]["xrunsIn"].as_u64().unwrap();
    assert_eq!(via_state, s.engine.sessions().xruns_in_total());
    assert!(via_state >= 100);
    // 首次丢弃即发 event.xrun {dir:"in"}（count ≥ 1）。
    // 注意：静默回环 + 慢供块拓扑下渲染欠供的 Xrun{dir:"out"} 是合法事件，逐条跳过。
    let mut saw_event = false;
    while let Ok(ev) = s.events.try_recv() {
        if let ServiceEvent::Xrun { dir, count } = ev {
            if dir == "out" {
                continue;
            }
            assert_eq!(dir, "in");
            assert!(count >= 1);
            saw_event = true;
        }
    }
    assert!(saw_event, "溢出应产生 event.xrun(dir=in) 通知");
    // DSP 消费侧持续推进（始终取得较新数据）
    wait_until(
        || s.engine.frames_processed() > 0,
        Duration::from_secs(10),
        "DSP 持续消费",
    );
    s.engine.stop().unwrap();
    assert_eq!(s.engine.get_state()["phase"], "idle");
}

#[test]
fn 非运行相位照常入队与淘汰_丢弃计入xruns_in_启动后继续消费() {
    // specs/service/push-stream.md §七.4：idle 期无消费者，入环淘汰同样计入 xrunsIn
    let s = setup_silent(Duration::ZERO, Duration::ZERO);
    configure_48k(&s.engine, 64);
    let sid = open_session(&s.engine);
    assert_eq!(s.engine.get_state()["phase"], "idle");

    let payload = [0.25_f32; 128];
    for k in 0..3000u64 {
        assert!(s.engine.sessions().ingest_frame(&frame(sid, k, &payload)));
    }
    wait_until(
        || s.engine.sessions().xruns_in_total() >= 100,
        Duration::from_secs(10),
        "idle 期淘汰计数增长",
    );
    assert_eq!(
        s.engine.get_state()["stats"]["xrunsIn"].as_u64().unwrap(),
        s.engine.sessions().xruns_in_total()
    );

    // 恢复运行后 DSP 从中断处继续取得较新数据（陈旧数据已被淘汰清除）
    s.engine.start().unwrap();
    wait_until(
        || s.engine.frames_processed() > 0,
        Duration::from_secs(10),
        "启动后继续消费",
    );
    s.engine.stop().unwrap();
}

#[test]
fn 会话帧按帧头路由_互不串扰_关闭即断流() {
    let s = setup_silent(Duration::ZERO, Duration::ZERO);
    configure_48k(&s.engine, 64);
    let a = open_session(&s.engine);
    let b = open_session(&s.engine);

    // GWT-PS-07：只推 A 时载荷只进 A 的环
    let mut seq = 0u64;
    push_constant(&s.engine, a, 0.25, 3, &mut seq);
    assert_eq!(s.engine.sessions().queued_frames(a), Some(192));
    assert_eq!(s.engine.sessions().queued_frames(b), Some(0));
    assert_eq!(
        s.engine.get_state()["sessions"],
        json!([
            {"sessionId": a, "queuedFrames": 192, "ingestedFrames": 192, "consumedFrames": 0},
            {"sessionId": b, "queuedFrames": 0, "ingestedFrames": 0, "consumedFrames": 0}
        ])
    );
    // 混合前级只产出 A 的内容
    let mut mix = vec![0.0_f32; 128];
    let total = s.engine.sessions().drain_and_mix(&mut mix, 0, 64);
    assert_eq!(total, 64);
    assert!(mix.iter().all(|&x| x == 0.25));
    assert_eq!(
        s.engine.get_state()["sessions"],
        json!([
            {"sessionId": a, "queuedFrames": 128, "ingestedFrames": 192, "consumedFrames": 64},
            {"sessionId": b, "queuedFrames": 0, "ingestedFrames": 0, "consumedFrames": 0}
        ])
    );

    // GWT-PS-03：closeSession 即时生效——未消费块丢弃，后续帧按未知会话静默丢弃
    assert_eq!(
        s.engine
            .close_session(json!({"sessionId": a}).as_object().unwrap())
            .unwrap()["closed"],
        true
    );
    assert_eq!(
        s.engine.sessions().queued_frames(a),
        None,
        "环内未消费块直接丢弃"
    );
    assert!(!s
        .engine
        .sessions()
        .ingest_frame(&frame(a, 99, &[0.25; 128])));
    assert_eq!(s.engine.sessions().queued_frames(b), Some(0), "B 不受影响");
}

#[test]
fn loopback_only_无会话时行为与推流实施前一致() {
    // 回归：无会话时捕获线程走快路径，回环斜坡逐位经直通链到渲染。
    // 节奏同混后处理测试（捕获 500µs < 渲染消费 250µs）⇒ 入环无溢出、无丢帧。
    let s = setup_working(Duration::from_micros(500), Duration::from_micros(250));
    configure_48k(&s.engine, 64);
    enable_bypass(&s.engine);
    s.engine.start().unwrap();
    wait_until(
        || s.engine.frames_processed() > 200,
        Duration::from_secs(10),
        "纯回环持续出帧",
    );
    assert_eq!(
        s.engine.get_state()["stats"]["xrunsIn"].as_u64().unwrap(),
        0,
        "受控速率下无入环丢弃"
    );
    let received = s.factory.render_received.lock().unwrap();
    // 渲染值域 ⊆ 回环斜坡 997 值集合 ∪ {0.0（欠供补零）}——斜坡逐位经链未被改动
    let mut ramp: Vec<f32> = (0..997u64)
        .map(|i| ((i % 997) as f32 / 997.0) * 2.0 - 1.0)
        .collect();
    ramp.sort_by(|a, b| a.partial_cmp(b).unwrap());
    assert!(
        received.iter().all(|&x| x == 0.0
            || ramp
                .binary_search_by(|p| p.partial_cmp(&x).unwrap())
                .is_ok()),
        "渲染出现斜坡集合之外的样本，回环路径被污染"
    );
    // 流首帧指纹：首块（cursor=0）首帧 = (test_sample(0), test_sample(500000))，
    // 首块入空环必不丢，故该相邻对必达渲染（先于其间的只有欠供补零）。
    let head_l = -1.0_f32; // test_sample(0)
    let head_r = ((500_000u64 % 997) as f32 / 997.0) * 2.0 - 1.0;
    assert!(
        received
            .windows(2)
            .any(|w| w[0] == head_l && w[1] == head_r),
        "回环斜坡流首帧指纹未达渲染"
    );
    drop(received);
    s.engine.stop().unwrap();
}

// ---------- WebSocket 端到端（真实 TCP + tungstenite，音频仍是假后端） ----------

/// 发送请求并等待其响应（跳过中途的事件通知）。
fn rpc_request(ws: &mut Ws, id: u64, method: &str, params: Value) -> Value {
    let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string();
    ws.send(Message::text(line)).unwrap();
    loop {
        match ws.read().unwrap() {
            Message::Text(t) => {
                let v: Value = serde_json::from_str(&t).unwrap();
                if v.get("id").is_some() {
                    return v;
                }
                // 事件通知：跳过继续等
            }
            Message::Ping(p) => {
                ws.send(Message::Pong(p)).unwrap();
            }
            _ => {}
        }
    }
}

#[test]
fn ws端到端_文本控制_二进制推流_断线自动清理_分流正交() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = sync_channel::<ServiceEvent>(4096);
    let factory = FakeFactory::silent_loopback(Duration::ZERO, Duration::ZERO);
    let engine = Arc::new(EngineHandle::new(
        Arc::clone(&factory) as Arc<dyn BackendFactory>,
        tx,
    ));
    let clients: server::ClientTable = Arc::new(Mutex::new(Vec::new()));

    // 事件中枢 + 接受循环（真实服务拓扑；本测试不启动数据面，音频只入会话环）
    {
        let engine = Arc::clone(&engine);
        let clients = Arc::clone(&clients);
        std::thread::Builder::new()
            .name("test-hub".into())
            .spawn(move || server::run_hub(engine, rx, clients))
            .unwrap();
    }
    {
        let engine = Arc::clone(&engine);
        let clients = Arc::clone(&clients);
        std::thread::Builder::new()
            .name("test-serve".into())
            .spawn(move || server::serve(listener, engine, clients))
            .unwrap();
    }

    let (mut ws, _resp) = tungstenite::connect(format!("ws://127.0.0.1:{port}/")).unwrap();

    // 控制面：configure → openSession（granted 回显 + id 从 1 起）
    let resp = rpc_request(
        &mut ws,
        1,
        "configure",
        json!({"mode": "loopback", "renderDeviceId": null, "sampleRate": 48_000u64, "blockSizeFrames": 64u64}),
    );
    assert_eq!(resp["result"]["applied"]["sampleRate"], 48_000);

    let resp = rpc_request(
        &mut ws,
        2,
        "openSession",
        json!({"sampleRate": 48_000u64, "channels": 2u16, "format": "f32le"}),
    );
    assert_eq!(
        resp["result"]["granted"],
        json!({"sampleRate": 48_000u64, "channels": 2u16, "format": "f32le"})
    );
    let sid_a = resp["result"]["sessionId"].as_u64().unwrap();
    assert_eq!(sid_a, 1);
    let resp = rpc_request(
        &mut ws,
        3,
        "openSession",
        json!({"sampleRate": 48_000u64, "channels": 2u16, "format": "f32le"}),
    );
    let sid_b = resp["result"]["sessionId"].as_u64().unwrap();
    assert_eq!(sid_b, 2);

    // 数据面：二进制帧入对应会话环（GWT-PS-07 帧头路由）。
    // B 帧在 A 帧之后发出且同连接串行处理：等待条件必须覆盖最后一帧（B），
    // 否则快机器上 A 先达标而 B 帧仍在连接线程排队 → 假阴性竞态。
    for seq in 0..3u64 {
        ws.send(Message::Binary(frame(sid_a as u32, seq, &[0.25, 0.75])))
            .unwrap();
    }
    ws.send(Message::Binary(frame(sid_b as u32, 0, &[1.0, -1.0])))
        .unwrap();
    wait_until(
        || engine.sessions().queued_frames(sid_b as u32) == Some(1),
        Duration::from_secs(5),
        "B 会话收到自己的帧（同连接串行 ⇒ A 的 3 帧必已入环）",
    );
    assert_eq!(
        engine.sessions().queued_frames(sid_a as u32),
        Some(3),
        "A 会话收到 3 帧载荷"
    );
    assert_eq!(
        engine.sessions().queued_frames(sid_b as u32),
        Some(1),
        "B 会话只收自己的帧"
    );

    // 违规帧静默：未知会话 / sessionId=0 / 载荷非 8 倍数 / 不足帧头
    ws.send(Message::Binary(frame(999_999, 0, &[0.0, 0.0])))
        .unwrap();
    ws.send(Message::Binary(frame(0, 0, &[0.0, 0.0]))).unwrap();
    ws.send(Message::Binary(vec![0u8; 14])).unwrap();
    ws.send(Message::Binary(
        frame(sid_a as u32, 9, &[0.5])[..14].to_vec(),
    ))
    .unwrap();
    assert_eq!(
        engine.sessions().queued_frames(sid_a as u32),
        Some(3),
        "违规帧不入环"
    );

    // GWT-PS-13 分流正交：非法 JSON 文本帧 → -32700（id null），绝不触达音频路径
    ws.send(Message::text("{not json".to_string())).unwrap();
    let mut parse_err = None;
    for _ in 0..20 {
        if let Message::Text(t) = ws.read().unwrap() {
            let v: Value = serde_json::from_str(&t).unwrap();
            if v["error"]["code"] == -32700 {
                parse_err = Some(v);
                break;
            }
        }
    }
    let v = parse_err.expect("应收到 -32700 解析错误响应");
    assert!(v["id"].is_null());
    // 反向：载荷恰似 JSON 的二进制帧（未知会话）→ 静默丢弃、不产生 JSON 解析错误
    let mut json_like = frame(424_242, 0, &[]);
    json_like.extend_from_slice(br#"{"jsonrpc":"2.0"}"#);
    json_like.extend_from_slice(b"    "); // 补齐到 8 的倍数
    ws.send(Message::Binary(json_like)).unwrap();
    let resp = rpc_request(&mut ws, 4, "getState", json!({}));
    assert_eq!(
        resp["result"]["phase"], "idle",
        "连接健康、未受二进制帧影响"
    );
    assert_eq!(
        engine.sessions().active_ids(),
        vec![sid_a as u32, sid_b as u32]
    );

    // closeSession：成功 → closed:true；重复 → -32602（不幂等）
    let resp = rpc_request(&mut ws, 5, "closeSession", json!({"sessionId": sid_b}));
    assert_eq!(resp["result"]["closed"], true);
    let resp = rpc_request(&mut ws, 6, "closeSession", json!({"sessionId": sid_b}));
    assert_eq!(resp["error"]["code"], -32602);

    // GWT-PS-06：断线自动清理——直接丢弃连接（无 Close 握手），两会话同时消失
    let sid_a_u32 = sid_a as u32;
    drop(ws);
    wait_until(
        || engine.sessions().active_ids().is_empty(),
        Duration::from_secs(5),
        "断线后本连接全部会话自动关闭",
    );
    assert!(engine.sessions().queued_frames(sid_a_u32).is_none());

    // 重连后服务仍健康（未知会话帧此前未断开连接的对照验证）
    let (mut ws2, _r) = tungstenite::connect(format!("ws://127.0.0.1:{port}/")).unwrap();
    let resp = rpc_request(&mut ws2, 1, "getState", json!({}));
    assert_eq!(resp["result"]["phase"], "idle");
    // 新连接 openSession 的 id 继续单调（永不复用）
    let resp = rpc_request(
        &mut ws2,
        2,
        "openSession",
        json!({"sampleRate": 48_000u64, "channels": 2u16, "format": "f32le"}),
    );
    assert!(resp["result"]["sessionId"].as_u64().unwrap() > sid_b);
}

#[test]
fn ws端到端_两条连接各自拥有并独立清理会话() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = sync_channel::<ServiceEvent>(4096);
    let factory = FakeFactory::silent_loopback(Duration::ZERO, Duration::ZERO);
    let engine = Arc::new(EngineHandle::new(
        Arc::clone(&factory) as Arc<dyn BackendFactory>,
        tx,
    ));
    let clients: server::ClientTable = Arc::new(Mutex::new(Vec::new()));

    {
        let engine = Arc::clone(&engine);
        let clients = Arc::clone(&clients);
        std::thread::spawn(move || server::run_hub(engine, rx, clients));
    }
    {
        let engine = Arc::clone(&engine);
        let clients = Arc::clone(&clients);
        std::thread::spawn(move || server::serve(listener, engine, clients));
    }

    let (mut first, _) = tungstenite::connect(format!("ws://127.0.0.1:{port}/")).unwrap();
    let (mut second, _) = tungstenite::connect(format!("ws://127.0.0.1:{port}/")).unwrap();
    rpc_request(
        &mut first,
        1,
        "configure",
        json!({"mode":"loopback","renderDeviceId":null,"sampleRate":48000,"blockSizeFrames":64}),
    );
    let sid_first = rpc_request(
        &mut first,
        2,
        "openSession",
        json!({"sampleRate":48000,"channels":2,"format":"f32le"}),
    )["result"]["sessionId"]
        .as_u64()
        .unwrap() as u32;
    let sid_second = rpc_request(
        &mut second,
        1,
        "openSession",
        json!({"sampleRate":48000,"channels":2,"format":"f32le"}),
    )["result"]["sessionId"]
        .as_u64()
        .unwrap() as u32;
    assert_ne!(sid_first, sid_second);
    assert_eq!(engine.sessions().active_ids(), vec![sid_first, sid_second]);

    drop(first);
    wait_until(
        || engine.sessions().active_ids() == vec![sid_second],
        Duration::from_secs(5),
        "第一条连接断开后只清理自己的会话",
    );
    let state = rpc_request(&mut second, 2, "getState", json!({}));
    assert_eq!(state["result"]["sessions"][0]["sessionId"], sid_second);
    assert_eq!(state["result"]["sessions"].as_array().unwrap().len(), 1);
}

#[test]
fn ws端到端_start_stop相位事件严格先于响应() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = sync_channel::<ServiceEvent>(4096);
    let factory = FakeFactory::silent_loopback(Duration::from_millis(1), Duration::from_millis(1));
    let engine = Arc::new(EngineHandle::new(
        Arc::clone(&factory) as Arc<dyn BackendFactory>,
        tx,
    ));
    let clients: server::ClientTable = Arc::new(Mutex::new(Vec::new()));

    {
        let engine = Arc::clone(&engine);
        let clients = Arc::clone(&clients);
        std::thread::spawn(move || server::run_hub(engine, rx, clients));
    }
    {
        let engine = Arc::clone(&engine);
        let clients = Arc::clone(&clients);
        std::thread::spawn(move || server::serve(listener, engine, clients));
    }

    let (mut ws, _) = tungstenite::connect(format!("ws://127.0.0.1:{port}/")).unwrap();
    rpc_request(
        &mut ws,
        1,
        "configure",
        json!({"mode":"loopback","renderDeviceId":null,"sampleRate":48000,"blockSizeFrames":64}),
    );

    for (id, method, expected_edges) in [
        (2, "start", [("idle", "starting"), ("starting", "running")]),
        (3, "stop", [("running", "stopping"), ("stopping", "idle")]),
    ] {
        ws.send(Message::text(
            json!({"jsonrpc":"2.0","id":id,"method":method,"params":{}}).to_string(),
        ))
        .unwrap();
        let mut received = Vec::new();
        while received.len() < 3 {
            if let Message::Text(text) = ws.read().unwrap() {
                received.push(serde_json::from_str::<Value>(&text).unwrap());
            }
        }
        for (index, (from, to)) in expected_edges.into_iter().enumerate() {
            assert_eq!(received[index]["method"], "event.phase");
            assert_eq!(received[index]["params"], json!({"from":from,"to":to}));
        }
        assert_eq!(
            received[2]["id"], id,
            "{method} 响应必须排在两条 phase 事件之后"
        );
    }
}

#[test]
fn ws端到端_phase事件向其他连接广播且请求方不重复() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = sync_channel::<ServiceEvent>(4096);
    let factory = FakeFactory::silent_loopback(Duration::from_millis(1), Duration::from_millis(1));
    let engine = Arc::new(EngineHandle::new(
        Arc::clone(&factory) as Arc<dyn BackendFactory>,
        tx,
    ));
    let clients: server::ClientTable = Arc::new(Mutex::new(Vec::new()));

    {
        let engine = Arc::clone(&engine);
        let clients = Arc::clone(&clients);
        std::thread::spawn(move || server::run_hub(engine, rx, clients));
    }
    {
        let engine = Arc::clone(&engine);
        let clients = Arc::clone(&clients);
        std::thread::spawn(move || server::serve(listener, engine, clients));
    }

    let (mut requester, _) = tungstenite::connect(format!("ws://127.0.0.1:{port}/")).unwrap();
    let (mut observer, _) = tungstenite::connect(format!("ws://127.0.0.1:{port}/")).unwrap();
    rpc_request(
        &mut requester,
        1,
        "configure",
        json!({"mode":"loopback","renderDeviceId":null,"sampleRate":48000,"blockSizeFrames":64}),
    );

    requester
        .send(Message::text(
            json!({"jsonrpc":"2.0","id":2,"method":"start","params":{}}).to_string(),
        ))
        .unwrap();
    let mut requester_messages = Vec::new();
    while requester_messages.len() < 3 {
        if let Message::Text(text) = requester.read().unwrap() {
            requester_messages.push(serde_json::from_str::<Value>(&text).unwrap());
        }
    }
    assert_eq!(requester_messages[0]["method"], "event.phase");
    assert_eq!(requester_messages[1]["method"], "event.phase");
    assert_eq!(requester_messages[2]["id"], 2);

    for (from, to) in [("idle", "starting"), ("starting", "running")] {
        let Message::Text(text) = observer.read().unwrap() else {
            panic!("观察连接应收到 phase 文本通知");
        };
        let event: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(event["method"], "event.phase");
        assert_eq!(event["params"], json!({"from":from,"to":to}));
    }

    rpc_request(&mut requester, 3, "stop", json!({}));
}
