//! 假后端下的管线线程集成测试：启停、帧计数、参数热更新与异常路径。
//! 全程不依赖真实音频设备。

use std::sync::mpsc::sync_channel;
use std::sync::Arc;
use std::time::{Duration, Instant};

use hse_service::backend::BackendFactory;
use hse_service::engine::EngineHandle;
use hse_service::fake_backend::FakeFactory;
use hse_service::state::ServiceEvent;
use serde_json::json;

fn setup(capture_period: Duration, render_period: Duration) -> (Arc<EngineHandle>, Arc<FakeFactory>) {
    let (tx, _rx) = sync_channel::<ServiceEvent>(4096);
    let factory = FakeFactory::working(capture_period, render_period);
    let handle = Arc::new(EngineHandle::new(
        Arc::clone(&factory) as Arc<dyn BackendFactory>,
        tx,
    ));
    (handle, factory)
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

fn configure_default(engine: &EngineHandle) {
    engine
        .configure(
            json!({"mode": "loopback", "renderDeviceId": null, "sampleRate": 48_000u64, "blockSizeFrames": 64u64})
                .as_object()
                .unwrap(),
        )
        .unwrap();
}

#[test]
fn 管线启停_帧计数增长_热更新不断流() {
    let (engine, factory) = setup(Duration::ZERO, Duration::ZERO);
    configure_default(&engine);
    engine.start().unwrap();
    assert_eq!(engine.get_state()["phase"], "running");

    wait_until(|| engine.frames_processed() > 0, Duration::from_secs(10), "开始处理帧");
    let before = engine.frames_processed();

    // 运行中热更换参数：加入 biquad 与湿声混响；处理必须继续推进。
    let resp = engine
        .set_params(&json!({
            "biquad": {"type": "peaking", "f0": 1000.0, "q": 1.0, "gainDb": 6.0},
            "reverbSimple": {"wet": 0.25},
        }))
        .unwrap();
    assert_eq!(resp["accepted"], true);

    wait_until(
        || engine.frames_processed() > before + 300,
        Duration::from_secs(15),
        "热更新后继续处理",
    );

    let stopped = engine.stop().unwrap();
    assert_eq!(stopped["stopped"], true);
    assert_eq!(engine.get_state()["phase"], "idle");

    // 渲染端收到的帧不少于 DSP 处理的帧（含欠供补零）。
    let received_frames = factory.render_received.lock().unwrap().len() as u64 / 2;
    assert!(
        received_frames >= engine.frames_processed(),
        "渲染收帧 {} 应 ≥ 处理帧 {}",
        received_frames,
        engine.frames_processed()
    );

    // 停止后 lastParams 与 config 快照仍在。
    let state = engine.get_state();
    assert_eq!(state["lastParams"]["biquad"]["type"], "peaking");
    assert_eq!(state["config"]["blockSizeFrames"], 64);
}

#[test]
fn 慢捕获时渲染欠供计入_xruns_out_且服务保持可用() {
    let (engine, _factory) = setup(Duration::from_millis(3), Duration::ZERO);
    configure_default(&engine);
    engine.start().unwrap();

    wait_until(
        || engine.get_state()["stats"]["xrunsOut"].as_u64().unwrap_or(0) > 0,
        Duration::from_secs(10),
        "出现渲染欠供计数",
    );
    let mid = engine.frames_processed();
    wait_until(
        || engine.frames_processed() > mid + 100,
        Duration::from_secs(10),
        "欠供期间处理继续推进",
    );

    engine.stop().unwrap();
    assert_eq!(engine.get_state()["phase"], "idle");
}

#[test]
fn 开流失败回滚_idle_并报后端错误() {
    let (tx, _rx) = sync_channel::<ServiceEvent>(16);
    let engine = EngineHandle::new(FakeFactory::broken("无声卡（假工厂）"), tx);
    configure_default(&engine);
    let err = engine.start().unwrap_err();
    assert_eq!(err.code, -32000);
    assert!(err.message.contains("无声卡"), "错误消息应透传原因：{}", err.message);
    assert_eq!(engine.get_state()["phase"], "idle");
}

#[test]
fn 相位机守门_运行中禁止重复start与configure() {
    let (engine, _factory) = setup(Duration::from_millis(1), Duration::from_millis(1));
    configure_default(&engine);
    engine.start().unwrap();

    let second = engine.start().unwrap_err();
    assert_eq!(second.code, -32001);
    let reconf = engine.configure(
        json!({"mode":"loopback","renderDeviceId":null,"sampleRate":44100u64,"blockSizeFrames":128u64})
            .as_object().unwrap());
    assert_eq!(reconf.unwrap_err().code, -32001);

    engine.stop().unwrap();
    assert_eq!(engine.stop().unwrap_err().code, -32001, "idle 下二次 stop 应被拒");

    // 停止后可以重新 configure 并再次完整启动一轮。
    configure_default(&engine);
    engine.start().unwrap();
    wait_until(|| engine.frames_processed() > 0, Duration::from_secs(10), "重启后继续出帧");
    engine.stop().unwrap();
}