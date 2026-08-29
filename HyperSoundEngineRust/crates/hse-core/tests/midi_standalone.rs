//! midi 独立验证（Phase 3 · MIDI 事件接口 / MIDI Learn）。
//!
//! **lib.rs 不注册本模块**——由协调方在集成时统一登记；本文件以
//! `#[path = "../src/midi.rs"]` 独立编译 `midi.rs` 做行为验证（不依赖引擎链）。
//!
//! 行为事实标准：仓库根 `test/midi.test.ts`（14 用例 = 行为规格）+
//! `src/engine/HyperSoundEngine.ts` MIDI 机制段。用例逐一移植：
//! TS 用例经引擎 `process()` 后以 `getParams()` 观测；Rust 侧无引擎上下文，
//! 参数快照为 `serde_json::Value`（服务层 PilotParams 形态），以
//! `MidiBindings::consume` 块头消费后直接断言 JSON 叶子。TS 第 10 用例
//! （builtin masterGain）断言音频输出采样（需音频链），此处适配为断言
//! 顶层 `masterGain` 叶子值（该叶子即 TS `_modMasterGain` 的 JSON 形态）。

#![allow(dead_code)]
// 用例名保留 TS 命名（masterGain / stereoWidth 等驼峰字段名属白名单事实标准词面）
#![allow(non_snake_case)]

#[path = "../src/midi.rs"]
mod midi;

use midi::*;
use serde_json::{json, Value};

// ==================== 夹具 ====================

/// PilotParams 形态参数快照（对齐 TS createDefaultParams 的处理相关子集；
/// 无顶层 masterGain 叶子——TS `_params.masterGain` 不存在，builtin 写回才创建）。
fn default_params() -> Value {
    json!({
        "stereoWidth": 1.0,
        "compressor": {
            "enabled": false, "thresholdDb": -20.0, "ratio": 4.0,
            "attackMs": 10.0, "releaseMs": 150.0, "makeupDb": 0.0
        },
        "deesser": { "thresholdDb": -30.0, "mix": 1.0 },
        "bassEnhancer": { "cutoffHz": 90.0, "harmonicGain": 0.6, "mix": 0.5 },
        "reverb": {
            "enabled": false,
            "algorithmic": { "wet": 0.3, "dry": 0.7, "roomSize": 0.5, "damping": 0.5, "preDelayMs": 0.0 }
        },
        "modEffects": {
            "delay": { "delayMs": 250.0, "feedback": 0.3, "mix": 0.3 },
            "chorus": { "rateHz": 1.0, "mix": 0.4 },
            "flanger": { "rateHz": 0.5, "mix": 0.5 },
            "phaser": { "rateHz": 0.5, "mix": 0.5 },
            "tremolo": { "rateHz": 5.0, "depth": 0.5 }
        },
        "ieq": { "strength": 0.5 },
        "dynamicEq": { "strength": 0.5, "thresholdDb": -20.0, "ratio": 2.0 },
        "limiter": { "thresholdDb": -1.0 },
        "pitch": { "semitones": 0.0, "rate": 1.0, "voiceBalance": 0.0 }
    })
}

/// learn 到白名单 path 目标的简写（min/max/smooth 显式给出）。
fn learn_path(b: &mut MidiBindings, cc: i32, path: &str, min: f64, max: f64, smooth_ms: f64) {
    b.learn(
        cc,
        AutomationTarget::Path(path.to_string()),
        LearnOpts { min: Some(min), max: Some(max), smooth_ms: Some(smooth_ms), ..LearnOpts::default() },
    )
    .unwrap();
}

/// 按点分路径读数值叶子。
fn num(p: &Value, path: &str) -> f64 {
    let mut cur = p;
    let mut it = path.split('.');
    let leaf = it.next_back().unwrap();
    for k in it {
        cur = &cur[k];
    }
    cur[leaf].as_f64().unwrap_or(f64::NAN)
}

/// 按点分路径读布尔叶子。
fn boolean(p: &Value, path: &str) -> bool {
    let mut cur = p;
    let mut it = path.split('.');
    let leaf = it.next_back().unwrap();
    for k in it {
        cur = &cur[k];
    }
    cur[leaf].as_bool().unwrap_or(false)
}

/// 一个"块"：TS sendMidi(本块事件) + process(128) 的镜像（fs=48000、块 128）。
fn block(b: &mut MidiBindings, ring: &mut MidiEventRing, p: &mut Value, events: &[MidiEventIn]) -> Vec<&'static str> {
    ring.push_slice(events);
    b.consume(ring, p, 48000.0, 128)
}

fn bits(x: f64) -> u64 {
    x.to_bits()
}

// ==================== TS test/midi.test.ts 14 用例移植 ====================

/// TS#1：CC 事件按范围线性映射到目标参数（smoothMs=0 直接到位）
#[test]
fn ts01_cc线性映射_smooth0直接到位() {
    let p = &mut default_params();
    let b = &mut MidiBindings::new();
    let ring = &mut MidiEventRing::new();
    learn_path(b, 7, "compressor.thresholdDb", -60.0, 0.0, 0.0);

    let sections = block(b, ring, p, &[MidiEventIn::cc(7.0, 64.0)]);
    let want = -60.0 + (64.0 / 127.0) * 60.0;
    let got = num(p, "compressor.thresholdDb");
    assert!((got - want).abs() < 1e-9, "got {} want {}", got, want);
    assert_eq!(sections, vec!["compressor"]);
}

/// TS#2：clamp：CC 0 → min，CC 127 → max
#[test]
fn ts02_clamp_cc两端到min_max() {
    let p = &mut default_params();
    let b = &mut MidiBindings::new();
    let ring = &mut MidiEventRing::new();
    learn_path(b, 1, "compressor.thresholdDb", -60.0, 0.0, 0.0);

    block(b, ring, p, &[MidiEventIn::cc(1.0, 0.0)]);
    let got = num(p, "compressor.thresholdDb");
    assert!((got - (-60.0)).abs() < 1e-9, "got {}", got);

    block(b, ring, p, &[MidiEventIn::cc(1.0, 127.0)]);
    let got = num(p, "compressor.thresholdDb");
    assert!((got - 0.0).abs() < 1e-9, "got {}", got);
}

/// TS#3：invert：CC 0 → max，CC 127 → min
#[test]
fn ts03_invert映射() {
    let p = &mut default_params();
    let b = &mut MidiBindings::new();
    let ring = &mut MidiEventRing::new();
    b.learn(
        1,
        AutomationTarget::Path("compressor.thresholdDb".into()),
        LearnOpts {
            min: Some(-60.0),
            max: Some(0.0),
            smooth_ms: Some(0.0),
            invert: Some(true),
            ..LearnOpts::default()
        },
    )
    .unwrap();

    block(b, ring, p, &[MidiEventIn::cc(1.0, 0.0)]);
    let got = num(p, "compressor.thresholdDb");
    assert!((got - 0.0).abs() < 1e-9, "got {}", got);

    block(b, ring, p, &[MidiEventIn::cc(1.0, 127.0)]);
    let got = num(p, "compressor.thresholdDb");
    assert!((got - (-60.0)).abs() < 1e-9, "got {}", got);
}

/// TS#4：smoothMs>0：参数向目标单调收敛（单事件 + 80 块；镜像 TS 用例形态）
#[test]
fn ts04_smooth单调收敛() {
    let p = &mut default_params();
    let b = &mut MidiBindings::new();
    let ring = &mut MidiEventRing::new();
    learn_path(b, 7, "compressor.thresholdDb", -60.0, 0.0, 200.0);

    // 起始 ≈ -20（默认）；单事件 target=0
    block(b, ring, p, &[MidiEventIn::cc(7.0, 127.0)]);
    let mut prev = -20.0;
    let mut converged = false;
    for _ in 0..80 {
        block(b, ring, p, &[]);
        let cur = num(p, "compressor.thresholdDb");
        assert!(cur >= prev, "单调非降被破坏：{} < {}", cur, prev);
        prev = cur;
        if (cur - 0.0).abs() < 0.01 {
            converged = true;
        }
    }
    assert!(converged, "80 块内未收敛到 0");
}

/// TS#5：note on/off 驱动布尔参数开关
#[test]
fn ts05_note开关布尔参数() {
    let p = &mut default_params();
    let b = &mut MidiBindings::new();
    let ring = &mut MidiEventRing::new();
    b.learn(
        60,
        AutomationTarget::Path("reverb.enabled".into()),
        LearnOpts {
            event_type: ControlKind::Note,
            min: Some(0.0),
            max: Some(1.0),
            smooth_ms: Some(0.0),
            ..LearnOpts::default()
        },
    )
    .unwrap();
    assert!(!boolean(p, "reverb.enabled"));

    block(b, ring, p, &[MidiEventIn::note_on(60.0, 100.0)]);
    assert!(boolean(p, "reverb.enabled"));

    block(b, ring, p, &[MidiEventIn::note_off(60.0)]);
    assert!(!boolean(p, "reverb.enabled"));
}

/// TS#6：note 驱动数值参数：noteOn→max，noteOff→min
#[test]
fn ts06_note数值参数_max_min() {
    let p = &mut default_params();
    let b = &mut MidiBindings::new();
    let ring = &mut MidiEventRing::new();
    b.learn(
        48,
        AutomationTarget::Path("compressor.thresholdDb".into()),
        LearnOpts {
            event_type: ControlKind::Note,
            min: Some(-60.0),
            max: Some(0.0),
            smooth_ms: Some(0.0),
            ..LearnOpts::default()
        },
    )
    .unwrap();

    block(b, ring, p, &[MidiEventIn::note_on(48.0, 100.0)]);
    let got = num(p, "compressor.thresholdDb");
    assert!((got - 0.0).abs() < 1e-9, "got {}", got);

    block(b, ring, p, &[MidiEventIn::note_off(48.0)]);
    let got = num(p, "compressor.thresholdDb");
    assert!((got - (-60.0)).abs() < 1e-9, "got {}", got);
}

/// TS#7：learn 非法路径立即抛错
#[test]
fn ts07_learn非法路径报错() {
    let mut b = MidiBindings::new();
    let r1 = b.learn(7, AutomationTarget::Path("compressor.nonexistent".into()), LearnOpts::default());
    let r2 = b.learn(7, AutomationTarget::Path("fake.module.field".into()), LearnOpts::default());
    for r in [r1, r2] {
        let err = r.unwrap_err();
        assert!(err.contains("unknown automatable path"), "err = {}", err);
    }
    assert!(b.is_empty(), "报错路径不得入库");
}

/// TS#8：unlearn 后 CC 不再生效；未绑定 CC 无副作用
#[test]
fn ts08_unlearn后失效_未绑定无副作用() {
    let p = &mut default_params();
    let b = &mut MidiBindings::new();
    let ring = &mut MidiEventRing::new();
    learn_path(b, 7, "compressor.thresholdDb", -60.0, 0.0, 0.0);

    let before = num(p, "compressor.thresholdDb");
    assert!(b.unlearn(7, ControlKind::Cc));
    block(b, ring, p, &[MidiEventIn::cc(7.0, 127.0)]);
    assert_eq!(bits(num(p, "compressor.thresholdDb")), bits(before), "unlearn 后 CC 失效");

    // 未绑定 CC（如 cc 99）无副作用
    block(b, ring, p, &[MidiEventIn::cc(99.0, 127.0)]);
    assert_eq!(bits(num(p, "compressor.thresholdDb")), bits(before));
    assert!(!b.unlearn(7, ControlKind::Cc), "重复 unlearn 返回 false");
}

/// TS#9：getMidiBindings 返回当前绑定副本
#[test]
fn ts09_get_bindings副本() {
    let mut b = MidiBindings::new();
    learn_path(&mut b, 7, "compressor.thresholdDb", -60.0, 0.0, 10.0);
    b.learn(
        60,
        AutomationTarget::Path("reverb.enabled".into()),
        LearnOpts {
            event_type: ControlKind::Note,
            min: Some(0.0),
            max: Some(1.0),
            smooth_ms: Some(0.0),
            ..LearnOpts::default()
        },
    )
    .unwrap();

    let v = b.get_bindings();
    assert_eq!(v.len(), 2);
    assert!(v.iter().any(|x| x.cc == 7));
    assert!(v.iter().any(|x| x.cc == 60));
    let b7 = v.iter().find(|x| x.cc == 7).unwrap();
    assert_eq!((b7.min, b7.max, b7.smooth_ms), (-60.0, 0.0, 10.0));
    assert!(!b7.invert);
    assert_eq!(b7.target, AutomationTarget::Path("compressor.thresholdDb".into()));
    let b60 = v.iter().find(|x| x.cc == 60).unwrap();
    assert_eq!(b60.target, AutomationTarget::Path("reverb.enabled".into()));
}

/// TS#10：builtin masterGain（ADAPTED：TS 断言音频输出采样
/// `0.5 × (64/127) × 2`，需音频链；Rust 断言顶层 `masterGain` 叶子
/// ——该叶子即 TS `_modMasterGain` 的 JSON 形态，值同为 (64/127)×2）。
#[test]
fn ts10_builtin_masterGain_顶层叶子_已适配() {
    let p = &mut default_params();
    assert!(p.get("masterGain").is_none(), "夹具无顶层 masterGain（TS _params 同样没有；builtin 写回才出现）");
    let b = &mut MidiBindings::new();
    let ring = &mut MidiEventRing::new();
    b.learn(
        7,
        AutomationTarget::Builtin(BuiltinParam::MasterGain),
        LearnOpts { min: Some(0.0), max: Some(2.0), smooth_ms: Some(0.0), ..LearnOpts::default() },
    )
    .unwrap();
    assert!(b.master_bound(), "masterGain 绑定须置位（镜像 _midiMasterBound）");

    let sections = block(b, ring, p, &[MidiEventIn::cc(7.0, 64.0)]);
    let want = (64.0 / 127.0) * 2.0; // ≈ 1.007874，与 TS 断言的输出采样系数一致
    let got = p["masterGain"].as_f64().unwrap();
    assert!((got - want).abs() < 1e-9, "got {} want {}", got, want);
    assert!(sections.is_empty(), "builtin 写回不走 refreshModuleForPath（镜像 TS 提前返回）");
}

/// TS#11：builtin stereoWidth 绑定改变立体声宽度
#[test]
fn ts11_builtin_stereoWidth() {
    let p = &mut default_params();
    let b = &mut MidiBindings::new();
    let ring = &mut MidiEventRing::new();
    b.learn(
        1,
        AutomationTarget::Builtin(BuiltinParam::StereoWidth),
        LearnOpts { min: Some(0.0), max: Some(2.0), smooth_ms: Some(0.0), ..LearnOpts::default() },
    )
    .unwrap();

    block(b, ring, p, &[MidiEventIn::cc(1.0, 127.0)]);
    let got = p["stereoWidth"].as_f64().unwrap();
    assert!((got - 2.0).abs() < 1e-9, "got {}", got);
}

/// TS#12：确定性：同事件序列两次处理结果一致（位型级）
#[test]
fn ts12_确定性_两次一致() {
    fn run() -> u64 {
        let p = &mut default_params();
        let b = &mut MidiBindings::new();
        let ring = &mut MidiEventRing::new();
        learn_path(b, 7, "compressor.thresholdDb", -60.0, 0.0, 50.0);
        let events = [MidiEventIn::cc(7.0, 30.0), MidiEventIn::cc(7.0, 100.0), MidiEventIn::cc(7.0, 60.0)];
        for blk in 0..5 {
            let ev: &[MidiEventIn] = if blk == 0 { &events } else { &[] };
            block(b, ring, p, ev);
        }
        bits(num(p, "compressor.thresholdDb"))
    }
    let r1 = run();
    let r2 = run();
    assert_eq!(r1, r2, "同输入同参数必须逐位一致");
}

/// TS#13：reset 保留绑定但清空运行时队列与平滑状态
#[test]
fn ts13_reset保留绑定_清空运行时() {
    let p = &mut default_params();
    let b = &mut MidiBindings::new();
    let ring = &mut MidiEventRing::new();
    learn_path(b, 7, "compressor.thresholdDb", -60.0, 0.0, 0.0);
    block(b, ring, p, &[MidiEventIn::cc(7.0, 127.0)]);
    assert_eq!(b.get_bindings().len(), 1);
    let got = num(p, "compressor.thresholdDb");
    assert!((got - 0.0).abs() < 1e-9, "got {}", got);

    // TS reset()：队列清空 + 平滑状态归 0，绑定保留（dropped 保留）
    ring.push(MidiEventIn::cc(7.0, 100.0)); // 入队但未消费
    ring.clear();
    b.reset_runtime();
    assert_eq!(b.get_bindings().len(), 1, "绑定属配置，reset 保留");
    let before = bits(num(p, "compressor.thresholdDb"));
    block(b, ring, p, &[]); // 空块
    assert_eq!(bits(num(p, "compressor.thresholdDb")), before, "队列已清，参数不变");

    // 复位后绑定仍可用
    block(b, ring, p, &[MidiEventIn::cc(7.0, 127.0)]);
    let got = num(p, "compressor.thresholdDb");
    assert!((got - 0.0).abs() < 1e-9, "got {}", got);
}

/// TS#14：队列溢出：dropped 累计（并验证「丢弃最旧」顺序）
#[test]
fn ts14_队列溢出_dropped累计() {
    let p = &mut default_params();
    let b = &mut MidiBindings::new();
    let ring = &mut MidiEventRing::new();
    learn_path(b, 7, "compressor.thresholdDb", -60.0, 0.0, 0.0);
    assert_eq!(ring.dropped(), 0);

    let flood: Vec<MidiEventIn> = (0..5000).map(|i| MidiEventIn::cc(7.0, i as f64)).collect();
    ring.push_slice(&flood);
    assert_eq!(ring.dropped(), 904, "5000 入 4096 容量：丢弃最旧 904 个");
    assert_eq!(ring.len(), 4096);

    // 最先弹出的是被保留的最旧事件 = 第 904 个（value = 904）
    let first = ring.pop().unwrap();
    assert_eq!(first.b, 904.0, "溢出语义 = 丢弃最旧");

    // 消费剩余全队列：末事件 value=4999 → clamp 到 127 → max
    b.consume(ring, p, 48000.0, 128);
    let got = num(p, "compressor.thresholdDb");
    assert!((got - 0.0).abs() < 1e-9, "got {}", got);
    assert!(ring.is_empty());
}

// ==================== 补充契约用例（TS 行为事实的边角固化） ====================

/// TS 引擎守卫（HE L675 `if (_midiCount > 0)`）：队列为空的块不做平滑收敛——
/// 单事件后参数冻结，直到下一事件到达。
#[test]
fn extra01_空环块平滑冻结_镜像引擎守卫() {
    let p = &mut default_params();
    let b = &mut MidiBindings::new();
    let ring = &mut MidiEventRing::new();
    learn_path(b, 7, "compressor.thresholdDb", -60.0, 0.0, 200.0);

    block(b, ring, p, &[MidiEventIn::cc(7.0, 0.0)]); // target = min = -60
    let after_event = bits(num(p, "compressor.thresholdDb"));
    assert!(after_event != bits(-60.0), "单块只走有限步 alpha（current 初值 0，未到位）");

    for _ in 0..3 {
        block(b, ring, p, &[]);
    }
    assert_eq!(bits(num(p, "compressor.thresholdDb")), after_event, "空环块不推进收敛（镜像 TS 守卫）");

    // 下一事件到达才继续走
    block(b, ring, p, &[MidiEventIn::cc(7.0, 0.0)]);
    assert!(bits(num(p, "compressor.thresholdDb")) != after_event, "新事件块继续收敛");
}

/// 平滑收敛（逐块事件驱动）：current 从 0 出发向 target=-60 单调逼近并收敛。
#[test]
fn extra02_平滑逐块事件_单调收敛到target() {
    let p = &mut default_params();
    let b = &mut MidiBindings::new();
    let ring = &mut MidiEventRing::new();
    learn_path(b, 7, "compressor.thresholdDb", -60.0, 0.0, 100.0);

    let mut prev = 0.0;
    let mut converged = false;
    for i in 0..500 {
        block(b, ring, p, &[MidiEventIn::cc(7.0, 0.0)]);
        let cur = num(p, "compressor.thresholdDb");
        assert!(cur <= prev + 1e-12, "单调非增被破坏：块 {} {} > {}", i, cur, prev);
        prev = cur;
        if (cur - (-60.0)).abs() < 0.01 {
            converged = true;
        }
    }
    assert!(converged, "500 块内未收敛到 -60（终值 {}）", prev);
}

/// CC 绑定布尔参数：白名单 boolean 元数据按 0.5 阈值离散化（CC≥64 → true）。
#[test]
fn extra03_cc驱动布尔参数_0_5阈值() {
    let p = &mut default_params();
    let b = &mut MidiBindings::new();
    let ring = &mut MidiEventRing::new();
    learn_path(b, 1, "reverb.enabled", 0.0, 1.0, 0.0);

    block(b, ring, p, &[MidiEventIn::cc(1.0, 63.0)]);
    assert!(!boolean(p, "reverb.enabled"), "63/127 ≈ 0.4961 < 0.5 → false");

    block(b, ring, p, &[MidiEventIn::cc(1.0, 64.0)]);
    assert!(boolean(p, "reverb.enabled"), "64/127 ≈ 0.5039 ≥ 0.5 → true");

    block(b, ring, p, &[MidiEventIn::cc(1.0, 0.0)]);
    assert!(!boolean(p, "reverb.enabled"));
}

/// cc 键与 note 键命名空间独立（键差 0x4000，镜像 TS）。
#[test]
fn extra04_cc与note命名空间独立() {
    let p = &mut default_params();
    let b = &mut MidiBindings::new();
    let ring = &mut MidiEventRing::new();
    learn_path(b, 60, "compressor.thresholdDb", -60.0, 0.0, 0.0);
    b.learn(
        60,
        AutomationTarget::Path("reverb.enabled".into()),
        LearnOpts {
            event_type: ControlKind::Note,
            min: Some(0.0),
            max: Some(1.0),
            smooth_ms: Some(0.0),
            ..LearnOpts::default()
        },
    )
    .unwrap();
    assert_eq!(b.get_bindings().len(), 2);

    block(b, ring, p, &[MidiEventIn::cc(60.0, 127.0)]);
    let got = num(p, "compressor.thresholdDb");
    assert!((got - 0.0).abs() < 1e-9, "got {}", got);
    assert!(!boolean(p, "reverb.enabled"), "cc 60 不得触碰 note 60 绑定");

    block(b, ring, p, &[MidiEventIn::note_on(60.0, 1.0)]);
    assert!(boolean(p, "reverb.enabled"));
    let got = num(p, "compressor.thresholdDb");
    assert!((got - 0.0).abs() < 1e-9, "note 60 不得触碰 cc 60 绑定");
}

/// refresh_sections_for_path 映射表（镜像 TS refreshModuleForPath 的 if 链）。
#[test]
fn extra05_refresh_sections映射表() {
    let cases: &[(&str, &[&str])] = &[
        ("compressor.enabled", &[]),
        ("compressor.thresholdDb", &["compressor"]),
        ("compressor.makeupDb", &["compressor"]),
        ("deesser.mix", &["deesser"]),
        ("bassEnhancer.mix", &["bassEnhancer"]),
        ("reverb.algorithmic.wet", &["reverb.algorithmic"]),
        ("reverb.enabled", &[]),
        ("modEffects.delay.mix", &["modEffects.delay"]),
        ("modEffects.chorus.mix", &["modEffects.chorus"]),
        ("modEffects.flanger.mix", &["modEffects.flanger"]),
        ("modEffects.phaser.mix", &["modEffects.phaser"]),
        ("modEffects.tremolo.depth", &["modEffects.tremolo"]),
        ("ieq.strength", &["ieq"]),
        ("dynamicEq.ratio", &["dynamicEq"]),
        ("limiter.thresholdDb", &["limiter"]),
        ("pitch.rate", &[]),
        ("masterGain", &[]),
        ("unknown.path.x", &[]),
    ];
    for (path, want) in cases {
        assert_eq!(refresh_sections_for_path(path), *want, "path = {}", path);
    }
}

/// 白名单缺省范围 / 缺省平滑 / 负平滑钳 0 / builtin 缺省 [0,2] / 同键覆盖。
#[test]
fn extra06_白名单缺省与钳制与覆盖() {
    let mut b = MidiBindings::new();
    // 缺省取白名单范围，缺省 smoothMs = 20
    b.learn(5, AutomationTarget::Path("compressor.thresholdDb".into()), LearnOpts::default()).unwrap();
    b.learn(6, AutomationTarget::Path("modEffects.delay.delayMs".into()), LearnOpts::default()).unwrap();
    b.learn(8, AutomationTarget::Path("limiter.thresholdDb".into()), LearnOpts::default()).unwrap();
    b.learn(9, AutomationTarget::Builtin(BuiltinParam::MasterGain), LearnOpts::default()).unwrap();
    // 负 smoothMs 钳 0（TS L984）
    b.learn(10, AutomationTarget::Path("deesser.mix".into()), LearnOpts { smooth_ms: Some(-5.0), ..LearnOpts::default() }).unwrap();

    let v = b.get_bindings();
    fn find<'a>(list: &'a [MidiBinding], cc: i32) -> &'a MidiBinding {
        list.iter().find(|x| x.cc == cc).unwrap()
    }
    assert_eq!((find(&v, 5).min, find(&v, 5).max, find(&v, 5).smooth_ms), (-60.0, 0.0, 20.0));
    assert_eq!((find(&v, 6).min, find(&v, 6).max), (0.0, 2000.0));
    assert_eq!((find(&v, 8).min, find(&v, 8).max), (-12.0, 0.0));
    assert_eq!((find(&v, 9).min, find(&v, 9).max), (0.0, 2.0), "builtin 缺省 [0,2]（TS L973–L974 两分支同值）");
    assert_eq!(find(&v, 10).smooth_ms, 0.0, "负 smoothMs 入库钳 0");
    assert_eq!(v.len(), 5);

    // 同键重复 learn：覆盖绑定 + 平滑状态重置（TS Map.set 整体替换）
    let p = &mut default_params();
    let ring = &mut MidiEventRing::new();
    b.learn(
        5,
        AutomationTarget::Path("deesser.mix".into()),
        LearnOpts { smooth_ms: Some(0.0), ..LearnOpts::default() },
    )
    .unwrap();
    let v2 = b.get_bindings();
    assert_eq!(v2.iter().filter(|x| x.cc == 5).count(), 1, "同键覆盖而非并存");
    assert_eq!(find(&v2, 5).target, AutomationTarget::Path("deesser.mix".into()));
    block(&mut b, ring, p, &[MidiEventIn::cc(5.0, 127.0)]);
    let got = num(p, "deesser.mix");
    assert!((got - 1.0).abs() < 1e-9, "覆盖后事件作用于新目标：got {}", got);
    let got = num(p, "compressor.thresholdDb");
    assert!((got - (-20.0)).abs() < 1e-12, "旧目标不再被驱动");
}

/// write_param_path 守卫语义：缺 section 不写不 panic；叶子类型不符不写；
/// 中间层非对象整体放弃；section 仍按 TS 语义返回（refresh 与写回成败无关）。
#[test]
fn extra07_路径写回守卫() {
    let b0 = &mut MidiBindings::new();
    let ring = &mut MidiEventRing::new();
    learn_path(b0, 7, "reverb.algorithmic.wet", 0.0, 1.0, 0.0);

    // (a) 参数快照缺 reverb 子树：不写、不 panic；section 照常返回
    let p = &mut json!({ "compressor": { "thresholdDb": -20.0 } });
    let sections = block(b0, ring, p, &[MidiEventIn::cc(7.0, 127.0)]);
    assert!(p.get("reverb").is_none(), "缺子树不得创建");
    assert_eq!(sections, vec!["reverb.algorithmic"], "镜像 TS：writeParamPath 失败仍走 refreshModuleForPath");

    // (b) 叶子类型非 boolean/number（字符串）：不写
    let p2 = &mut json!({ "compressor": { "thresholdDb": "n/a" } });
    let b1 = &mut MidiBindings::new();
    learn_path(b1, 7, "compressor.thresholdDb", -60.0, 0.0, 0.0);
    block(b1, ring, p2, &[MidiEventIn::cc(7.0, 64.0)]);
    assert_eq!(p2["compressor"]["thresholdDb"].as_str(), Some("n/a"), "非数值叶子不得被改写");

    // (c) 中间层非对象：整体放弃
    let p3 = &mut json!({ "compressor": 7 });
    block(b1, ring, p3, &[MidiEventIn::cc(7.0, 64.0)]);
    assert_eq!(p3["compressor"].as_i64(), Some(7));
}

/// builtin masterGain 无条件创建顶层叶子；unlearn 复位 master_bound。
#[test]
fn extra08_builtin无条件写与master_bound() {
    let p = &mut default_params();
    let b = &mut MidiBindings::new();
    let ring = &mut MidiEventRing::new();
    b.learn(
        7,
        AutomationTarget::Builtin(BuiltinParam::MasterGain),
        LearnOpts { smooth_ms: Some(0.0), ..LearnOpts::default() },
    )
    .unwrap();

    // CC 127 → target 2（缺省 [0,2]）
    block(b, ring, p, &[MidiEventIn::cc(7.0, 127.0)]);
    let got = p["masterGain"].as_f64().unwrap();
    assert!((got - 2.0).abs() < 1e-12, "got {}", got);

    // unlearn 后 master_bound 复位（镜像 refreshMidiMasterBound）
    assert!(b.unlearn(7, ControlKind::Cc));
    assert!(!b.master_bound());
    // path 绑定不影响 master_bound
    learn_path(b, 1, "compressor.thresholdDb", -60.0, 0.0, 0.0);
    assert!(!b.master_bound());
    // 重学 builtin masterGain 再置位
    b.learn(3, AutomationTarget::Builtin(BuiltinParam::MasterGain), LearnOpts::default()).unwrap();
    assert!(b.master_bound());
}

/// dropped 计数跨 ring.clear() 保留（TS reset 不复位 _midiDropped）。
#[test]
fn extra09_dropped跨clear保留() {
    let mut ring = MidiEventRing::new();
    let flood: Vec<MidiEventIn> = (0..5000).map(|i| MidiEventIn::cc(7.0, i as f64)).collect();
    ring.push_slice(&flood);
    assert_eq!(ring.dropped(), 904);
    ring.clear();
    assert_eq!(ring.dropped(), 904, "clear 只清队列，不清丢弃计数");
    assert_eq!(ring.len(), 0);
    ring.push(MidiEventIn::cc(1.0, 1.0));
    assert_eq!(ring.len(), 1);
}

/// 空环 consume 是纯 no-op（sections 空、params 逐位不变）。
#[test]
fn extra10_空环consume无操作() {
    let p = &mut default_params();
    let snapshot = p.clone();
    let b = &mut MidiBindings::new();
    let ring = &mut MidiEventRing::new();
    learn_path(b, 7, "compressor.thresholdDb", -60.0, 0.0, 0.0);
    let sections = block(b, ring, p, &[]);
    assert!(sections.is_empty());
    assert_eq!(p, &snapshot, "空环不得触碰参数快照");
}

/// 事件 b 值经 f32 环存储（镜像 TS Float32Array 落点）：映射用 f32 量化后的 b；
/// noteOff 的 b 恒 0（TS sendMidi 对 noteOff 写 0）。
#[test]
fn extra11_事件值f32落点() {
    let p = &mut default_params();
    let b = &mut MidiBindings::new();
    let ring = &mut MidiEventRing::new();
    learn_path(b, 7, "compressor.thresholdDb", -60.0, 0.0, 0.0);

    block(b, ring, p, &[MidiEventIn::cc(7.0, 100.1)]);
    let b_f32 = 100.1f32 as f64; // 环内实际存储值
    let want = -60.0 + 60.0 * (b_f32 / 127.0);
    assert_eq!(num(p, "compressor.thresholdDb"), want, "映射必须用 f32 量化后的 b");

    ring.push(MidiEventIn { kind: MidiEventKind::NoteOff, a: 60.0, b: 55.0 });
    assert_eq!(ring.pop().unwrap().b, 0.0, "noteOff 入环时 b 置 0（镜像 TS L943）");
}

/// 同块多事件按到达顺序作用：后到者生效；sections 去重保序。
#[test]
fn extra12_同块多事件_后到生效_sections去重() {
    let p = &mut default_params();
    let b = &mut MidiBindings::new();
    let ring = &mut MidiEventRing::new();
    learn_path(b, 7, "compressor.thresholdDb", -60.0, 0.0, 0.0);
    learn_path(b, 8, "compressor.ratio", 1.0, 20.0, 0.0);

    let sections = block(
        b,
        ring,
        p,
        &[MidiEventIn::cc(7.0, 0.0), MidiEventIn::cc(7.0, 127.0), MidiEventIn::cc(8.0, 127.0)],
    );
    let got = num(p, "compressor.thresholdDb");
    assert!((got - 0.0).abs() < 1e-9, "同块后到事件生效：got {}", got);
    let got = num(p, "compressor.ratio");
    assert!((got - 20.0).abs() < 1e-9, "got {}", got);
    assert_eq!(sections, vec!["compressor"], "同 section 多路径写回去重且保序");
}

/// 白名单顶层路径 'masterGain' 以 path 形态 learn：叶子缺失 → 不写
/// （镜像 TS `_params.masterGain` 为 undefined → writeParamPath no-op 的事实）。
#[test]
fn extra13_顶层path形态masterGain叶子缺失不写() {
    let p = &mut default_params();
    assert!(p.get("masterGain").is_none());
    let b = &mut MidiBindings::new();
    let ring = &mut MidiEventRing::new();
    // TS 白名单第 0 条即 'masterGain'，path 形态合法
    learn_path(b, 7, "masterGain", 0.0, 2.0, 0.0);
    let sections = block(b, ring, p, &[MidiEventIn::cc(7.0, 127.0)]);
    assert!(p.get("masterGain").is_none(), "叶子缺失 → write_param_path 不创建（镜像 TS）");
    assert!(sections.is_empty(), "顶层路径无 section 映射（TS fall-through）");
}

/// 白名单完整性与 TS 对齐：38 条；含两条顶层 builtin 路径；boolean 元数据仅两条。
#[test]
fn extra14_白名单完整性() {
    assert_eq!(AUTOMATABLE_PARAMS.len(), 38);
    assert_eq!(AUTOMATABLE_PARAMS[0].path, "masterGain");
    assert_eq!(AUTOMATABLE_PARAMS[1].path, "stereoWidth");
    let boolean_paths: Vec<&str> = AUTOMATABLE_PARAMS.iter().filter(|m| m.boolean).map(|m| m.path).collect();
    assert_eq!(boolean_paths, vec!["compressor.enabled", "reverb.enabled"]);
    // find_automatable_param 命中 / 未命中
    assert!(find_automatable_param("compressor.thresholdDb").is_some());
    assert!(find_automatable_param("compressor.nonexistent").is_none());
    // 无重复路径
    let mut paths: Vec<&str> = AUTOMATABLE_PARAMS.iter().map(|m| m.path).collect();
    paths.sort_unstable();
    let n = paths.len();
    paths.dedup();
    assert_eq!(paths.len(), n, "白名单路径不得重复");
}
