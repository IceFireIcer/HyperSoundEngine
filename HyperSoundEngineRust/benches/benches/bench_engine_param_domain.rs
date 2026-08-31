//! EngineChainStage 参数域基准，不依赖服务层或音频设备。

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use hse_benches::{push_blocks, StereoBuffer, FRAMES_PER_ITER, SAMPLE_RATE_HZ};
use hse_core::{
    engine_chain::{EngineChainParams, EngineChainStage},
    Stage,
};
use serde_json::{json, Value};

const BLOCK: usize = 128;

fn all_enabled() -> Value {
    json!({
        "loudnessNormalization": {"enabled": true, "useRealtimeMeter": false, "externalGainDb": 9.0},
        "surround3d": {"enabled": true, "distance": 1.0, "speed": 4.0, "angle": 180.0, "direction": -1.0},
        "eq": {"enabled": true, "mode": "pro", "bandCount": 10, "qCompensation": true,
            "proBands": [
                {"frequency":31.5,"gain":12.0,"q":8.0},{"frequency":63.0,"gain":-12.0,"q":8.0},
                {"frequency":125.0,"gain":12.0,"q":8.0},{"frequency":250.0,"gain":-12.0,"q":8.0},
                {"frequency":500.0,"gain":12.0,"q":8.0},{"frequency":1000.0,"gain":-12.0,"q":8.0},
                {"frequency":2000.0,"gain":12.0,"q":8.0},{"frequency":4000.0,"gain":-12.0,"q":8.0},
                {"frequency":8000.0,"gain":12.0,"q":8.0},{"frequency":16000.0,"gain":-12.0,"q":8.0}
            ]},
        "deesser": {"enabled": true, "centerHz": 16000.0, "q": 20.0, "thresholdDb": -80.0, "ratio": 100.0, "attackMs": 0.05, "releaseMs": 1000.0, "splitBand": true, "mix": 1.0},
        "compressor": {"enabled": true, "thresholdDb": -80.0, "ratio": 100.0, "kneeDb": 40.0, "attackMs": 0.05, "releaseMs": 1000.0, "makeupDb": 24.0, "outputGain": 2.0},
        "nightMode": {"enabled": true, "amount": 10.0},
        "modEffects": {
            "delay": {"enabled": true, "delayMs": 2000.0, "feedback": 0.98, "mix": 1.0},
            "chorus": {"enabled": true, "rateHz": 20.0, "depthMs": 50.0, "mix": 1.0},
            "flanger": {"enabled": true, "rateHz": 20.0, "depthMs": 50.0, "feedback": 0.98, "mix": 1.0},
            "phaser": {"enabled": true, "rateHz": 20.0, "depth": 1.0, "feedback": 0.98, "mix": 1.0, "stages": 8.0},
            "tremolo": {"enabled": true, "rateHz": 30.0, "depth": 1.0, "mix": 1.0}},
        "reverb": {"enabled": true, "mode": "fdn", "algorithmic": {"type": "stage", "roomSize": 1.0, "damping": 1.0, "wet": 1.0, "dry": 1.0, "preDelayMs": 250.0, "width": 2.0}},
        "bassEnhancer": {"enabled": true, "cutoffHz": 20000.0, "q": 20.0, "harmonicType": "soft", "harmonicGain": 1.0, "mix": 1.0, "levelDb": 6.0, "lowBoostDb": 12.0},
        "loudnessCompensation": {"enabled": true, "mode": "custom", "bands": [{"frequency":80.0,"gain":24.0},{"frequency":4000.0,"gain":-24.0}], "volumePercent": 0.0, "maxBoostDb": 24.0, "smoothingSeconds": 0.01},
        "ieq": {"enabled": true, "strength": 1.0, "targetCurve": "bright", "timeConstantSec": 0.1},
        "dynamicEq": {"enabled": true, "strength": 1.0, "thresholdDb": -80.0, "ratio": 100.0, "attackMs": 0.05, "releaseMs": 1000.0, "bands": [{"enabled":true,"targetGainDb":12.0},{"enabled":true,"targetGainDb":-12.0},{"enabled":true,"targetGainDb":12.0},{"enabled":true,"targetGainDb":-12.0},{"enabled":true,"targetGainDb":12.0}]},
        "pitch": {"enabled": true, "voiceBalance": 1.0},
        "modulation": {"enabled": true, "lfo": {"shape": "square", "rateHz": 30.0, "depth": 1.0}, "envelope": {"attackMs": 0.05, "releaseMs": 1000.0, "amount": 1.0}, "routes": [{"source":"lfo","target":"masterGain","amount":2.0,"offset":1.0},{"source":"envelope","target":"stereoWidth","amount":2.0,"offset":1.0}]},
        "limiter": {"enabled": true, "thresholdDb": -60.0, "lookaheadMs": 50.0, "attackMs": 0.05, "releaseMs": 1000.0, "truePeak": true},
        "stereoWidth": 2.0,
        "spatial": {"mode": "off"}
    })
}

fn ready_stage(overrides: &Value) -> EngineChainStage {
    let params =
        EngineChainParams::from_overrides(SAMPLE_RATE_HZ, overrides).expect("基准参数必须合法");
    let mut stage =
        EngineChainStage::from_params(SAMPLE_RATE_HZ, params).expect("基准参数必须可装配");
    stage.prepare(BLOCK);
    stage
}

fn bench_engine_param_domain(c: &mut Criterion) {
    let master = StereoBuffer::synthesized(FRAMES_PER_ITER);
    let cases = [
        ("defaults", json!({})),
        (
            "all_bypass_boundaries",
            json!({"eq":{"enabled":false},"limiter":{"enabled":false},"stereoWidth":0.0}),
        ),
        ("all_enabled_upper_boundaries", all_enabled()),
    ];
    let mut group = c.benchmark_group("bench_engine_param_domain");
    group.sample_size(20);
    group.warm_up_time(std::time::Duration::from_secs(1));
    group.measurement_time(std::time::Duration::from_secs(3));
    group.throughput(Throughput::Elements(FRAMES_PER_ITER as u64));

    for (label, overrides) in cases {
        group.bench_function(BenchmarkId::from_parameter(label), |b| {
            let mut stage = ready_stage(&overrides);
            let mut work = StereoBuffer::zeroed(FRAMES_PER_ITER);
            b.iter(|| {
                black_box(push_blocks(
                    &mut stage,
                    &master,
                    &mut work,
                    BLOCK,
                    FRAMES_PER_ITER / BLOCK,
                ))
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_engine_param_domain);
criterion_main!(benches);
