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
#[test]
fn configure_未知渲染设备引用拒绝且不留痕() {
    let (engine, _f) = setup(Duration::ZERO, Duration::ZERO);
    // 先成功配置一次：验证失败路径不清掉既有 config（GWT-CP-08 第二分句）
    configure_default(&engine);
    let err = engine
        .configure(
            json!({"mode": "loopback",
                   "renderDeviceId": "{0.0.0.00000000}.{deadbeef-0000-0000-0000-000000000000}",
                   "sampleRate": 48_000u64, "blockSizeFrames": 64u64})
                .as_object()
                .unwrap(),
        )
        .unwrap_err();
    assert_eq!(err.code, -32000, "未知渲染端点引用应报后端失败");
    let st = engine.get_state();
    assert_eq!(st["config"]["renderDeviceId"], json!(null), "既有 config 应保持不变");
}

#[test]
fn configure_非idle拒绝优先于结构校验() {
    let (engine, _f) = setup(Duration::ZERO, Duration::ZERO);
    configure_default(&engine);
    engine.start().unwrap();
    // GWT-CP-06：非 idle 时无论内容是否合法一律 -32001（此例内容同时结构非法）
    let err = engine
        .configure(
            json!({"mode": "capture", "renderDeviceId": null,
                   "sampleRate": 0u64, "blockSizeFrames": 0u64})
                .as_object()
                .unwrap(),
        )
        .unwrap_err();
    assert_eq!(err.code, -32001);
    engine.stop().unwrap();
}

// ---------- Phase 3 全序链：新增 setParams 键的运行态集成（假后端） ----------

#[test]
fn 新键快照热更换_运行态继续处理且无警告_三路混响路由切换() {
    let (engine, _factory) = setup(Duration::ZERO, Duration::ZERO);
    configure_default(&engine);
    engine.start().unwrap();
    wait_until(|| engine.frames_processed() > 0, Duration::from_secs(10), "开始处理帧");
    let before = engine.frames_processed();

    // 一次性下发全部新键（各自非直通形态）：必须 accepted 且零 warnings。
    let five_bands: Vec<serde_json::Value> =
        (0..5).map(|_| json!({"enabled": true, "targetGainDb": 0})).collect();
    let resp = engine
        .set_params(&json!({
            "eqChain": {"bands": [{"frequency": 1000, "gain": 3.0, "q": 1.0}], "bandCount": 10, "qCompensation": true},
            "deesser": {"enabled": true, "centerHz": 6000, "q": 0.7, "thresholdDb": -30,
                        "ratio": 8, "attackMs": 1, "releaseMs": 80, "splitBand": true, "mix": 1},
            "modEffects": {"delay": {"enabled": true, "delayMs": 120, "feedback": 0.2, "mix": 0.3}},
            "reverbRoute": "fdn",
            "fdnReverb": {"roomSize": 0.6, "damping": 0.4, "wet": 0.25, "dry": 0.75,
                          "preDelayMs": 10, "width": 1, "type": "hall", "lines": 8},
            "loudnessComp": {"mode": "auto", "volumePercent": 60, "maxBoostDb": 12, "smoothingSeconds": 0.2},
            "dynamicEq": {"enabled": true, "strength": 0.5, "thresholdDb": -20, "ratio": 2,
                          "attackMs": 20, "releaseMs": 200, "bands": five_bands},
            "modMatrix": {"routes": [{"source": "lfo", "target": "masterGain", "amount": 0.2, "offset": 0}],
                          "lfo": {"shape": "sine", "rateHz": 2, "depth": 0.5},
                          "envelope": {"attackMs": 10, "releaseMs": 200, "amount": 0.5}}
        }))
        .unwrap();
    assert_eq!(resp["accepted"], true);
    assert_eq!(resp["warnings"], json!([]), "全部新键可识别，不得产生 warnings");

    wait_until(
        || engine.frames_processed() > before + 300,
        Duration::from_secs(15),
        "全序链热更换后继续处理",
    );

    // 运行态切换 convolver 路（delta IR 配方）→ 继续处理。
    let mid = engine.frames_processed();
    let resp = engine
        .set_params(&json!({
            "reverbRoute": "convolver",
            "convolver": {"irRecipe": {"kind": "delta", "delay": 0}, "mix": 0.4, "preDelayMs": 0}
        }))
        .unwrap();
    assert_eq!(resp["accepted"], true);
    wait_until(
        || engine.frames_processed() > mid + 300,
        Duration::from_secs(15),
        "convolver 路由热更换后继续处理",
    );

    // 切 off（整级直通）→ 继续处理。
    let mid = engine.frames_processed();
    let resp = engine.set_params(&json!({"reverbRoute": "off"})).unwrap();
    assert_eq!(resp["accepted"], true);
    wait_until(
        || engine.frames_processed() > mid + 300,
        Duration::from_secs(15),
        "off 路由热更换后继续处理",
    );

    let stopped = engine.stop().unwrap();
    assert_eq!(stopped["stopped"], true);
    // lastParams 为最后一条快照（整体替换语义）：只剩 reverbRoute 键。
    assert_eq!(engine.get_state()["lastParams"]["reverbRoute"], "off");
}

#[test]
fn 新键结构违规与构建失败路径() {
    let (engine, _factory) = setup(Duration::ZERO, Duration::ZERO);
    configure_default(&engine);
    engine.start().unwrap();
    wait_until(|| engine.frames_processed() > 0, Duration::from_secs(10), "开始处理帧");

    // 子键类型不符 → -32602（既有纪律在新增键上同样生效）。
    for bad in [
        json!({"dynamicEq": {"strength": "x"}}),
        json!({"deesser": {"enabled": 1}}),
        json!({"eqChain": {"bands": [42]}}),
        json!({"modEffects": {"delay": "x"}}),
        json!({"convolver": {"mix": true}}),
        json!({"modMatrix": {"routes": {}}}),
        json!({"loudnessComp": {"bands": [3]}}),
        json!({"reverbRoute": 3}),
        // IR 配方判别值未知：结构违规（无模块内回退形态）。
        json!({"convolver": {"irRecipe": {"kind": "sine"}}}),
        // fdnReverb.lines 非 2/4/8/16：构建失败 → -32602（参数无法应用）。
        json!({"reverbRoute": "fdn", "fdnReverb": {"lines": 3}}),
        // route=convolver 但缺 IR 配方：构建失败 → -32602。
        json!({"reverbRoute": "convolver"}),
    ] {
        let err = engine.set_params(&bad).unwrap_err();
        assert_eq!(err.code, -32602, "应拒绝：{bad}");
    }

    // 未知顶层键/子键照旧走 warnings + accepted。
    let resp = engine
        .set_params(&json!({"dynamicEq": {"enabled": false, "mystery": 1}, "reverbRoute": "off"}))
        .unwrap();
    assert_eq!(resp["accepted"], true);
    assert_eq!(resp["warnings"], json!(["dynamicEq.mystery"]));

    engine.stop().unwrap();
}
