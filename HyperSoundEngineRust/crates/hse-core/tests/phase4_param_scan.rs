use hse_core::{
    engine_chain::{EngineChainParams, EngineChainStage},
    Stage,
};
use serde_json::{json, Value};

const CASES: [(f64, usize); 4] = [
    (44_100.0, 63),
    (48_000.0, 128),
    (48_000.0, 257),
    (96_000.0, 512),
];

#[derive(Clone, Copy)]
struct Lcg(u32);

impl Lcg {
    fn next_u32(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        self.0
    }

    fn unit(&mut self) -> f64 {
        f64::from(self.next_u32()) / 4_294_967_296.0
    }

    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.unit()
    }

    fn bool(&mut self) -> bool {
        self.next_u32() & 1 == 0
    }

    fn pick<'a>(&mut self, values: &'a [&'a str]) -> &'a str {
        values[self.next_u32() as usize % values.len()]
    }
}

fn random_overrides(seed: u32, fs: f64) -> Value {
    let mut rng = Lcg(seed);
    let reverb_mode = rng.pick(&["off", "algorithmic", "fdn", "convolution"]);
    let convolution_ir = if reverb_mode == "convolution" {
        json!([1.0, 0.25, -0.125, 0.0625])
    } else {
        Value::Null
    };
    let frequencies: [f64; 10] = [
        31.5, 63.0, 125.0, 250.0, 500.0, 1_000.0, 2_000.0, 4_000.0, 8_000.0, 16_000.0,
    ];
    let pro_bands: Vec<Value> = frequencies
        .into_iter()
        .map(|frequency| {
            json!({
                "frequency": frequency.min(fs * 0.45),
                "gain": rng.range(-12.0, 12.0),
                "q": rng.range(0.2, 8.0)
            })
        })
        .collect();
    let dynamic_bands: Vec<Value> = (0..5)
        .map(|_| {
            json!({
                "enabled": rng.bool(),
                "targetGainDb": rng.range(-12.0, 12.0)
            })
        })
        .collect();

    json!({
        "eq": {
            "enabled": true,
            "mode": if rng.bool() { "pro" } else { "simple" },
            "simpleBands": (0..5).map(|_| rng.range(-12.0, 12.0)).collect::<Vec<_>>(),
            "proBands": pro_bands,
            "bandCount": 1 + (rng.next_u32() % 10),
            "qCompensation": rng.bool()
        },
        "deesser": {"enabled": true, "centerHz": rng.range(100.0, fs * 0.45), "q": rng.range(0.1, 20.0), "thresholdDb": rng.range(-80.0, 0.0), "ratio": rng.range(1.0, 100.0), "attackMs": rng.range(0.05, 100.0), "releaseMs": rng.range(1.0, 1000.0), "splitBand": rng.bool(), "mix": rng.unit(), "sidechainEnabled": rng.bool()},
        "compressor": {"enabled": true, "thresholdDb": rng.range(-80.0, 0.0), "ratio": rng.range(1.0, 100.0), "kneeDb": rng.range(0.0, 40.0), "attackMs": rng.range(0.05, 100.0), "releaseMs": rng.range(1.0, 1000.0), "makeupDb": rng.range(-24.0, 24.0), "outputGain": rng.range(0.0, 2.0), "sidechainEnabled": rng.bool()},
        "nightMode": {"enabled": true, "amount": rng.range(0.0, 10.0)},
        "bassEnhancer": {"enabled": true, "cutoffHz": rng.range(20.0, fs * 0.4), "q": rng.range(0.1, 20.0), "harmonicType": rng.pick(&["odd", "even", "atan", "soft"]), "harmonicGain": rng.unit(), "mix": rng.unit(), "levelDb": rng.range(-6.0, 6.0), "lowBoostDb": rng.range(-6.0, 12.0)},
        "reverb": {"enabled": reverb_mode != "off", "mode": reverb_mode, "algorithmic": {"type": rng.pick(&["hall", "room", "plate", "spring", "stage"]), "roomSize": rng.unit(), "damping": rng.unit(), "wet": rng.unit(), "dry": rng.unit(), "preDelayMs": rng.range(0.0, 250.0), "width": rng.range(0.0, 2.0)}, "convolution": {"ir": convolution_ir, "mix": rng.unit(), "preDelayMs": rng.range(0.0, 100.0), "dePeriodize": rng.bool()}},
        "surround3d": {"enabled": true, "distance": rng.unit(), "speed": rng.range(0.0, 4.0), "angle": rng.range(-180.0, 180.0), "direction": if rng.bool() { 1 } else { -1 }},
        "loudnessCompensation": {"enabled": true, "mode": rng.pick(&["auto", "preset", "custom"]), "preset": rng.pick(&["flat", "bass", "vocal", "warm", "bright", "night"]), "bands": [{"frequency": 80.0, "gain": rng.range(-24.0, 24.0)}, {"frequency": 4000.0, "gain": rng.range(-24.0, 24.0)}], "volumePercent": rng.range(0.0, 100.0), "maxBoostDb": rng.range(0.0, 24.0), "smoothingSeconds": rng.range(0.01, 2.0)},
        "loudnessNormalization": {"enabled": true, "targetLufs": rng.range(-40.0, 0.0), "maxGainDb": rng.range(0.0, 24.0), "minGainDb": rng.range(-24.0, 0.0), "useRealtimeMeter": rng.bool(), "externalGainDb": rng.range(-24.0, 24.0)},
        "limiter": {"enabled": true, "thresholdDb": rng.range(-60.0, 0.0), "lookaheadMs": rng.range(0.0, 50.0), "attackMs": rng.range(0.05, 100.0), "releaseMs": rng.range(1.0, 1000.0), "truePeak": rng.bool()},
        "ieq": {"enabled": rng.bool(), "strength": rng.unit(), "targetCurve": rng.pick(&["flat", "warm", "bright", "vocal"]), "timeConstantSec": rng.range(0.1, 10.0)},
        "dynamicEq": {"enabled": true, "strength": rng.unit(), "thresholdDb": rng.range(-80.0, 0.0), "ratio": rng.range(1.0, 100.0), "attackMs": rng.range(0.05, 100.0), "releaseMs": rng.range(1.0, 1000.0), "bands": dynamic_bands},
        "pitch": {"enabled": true, "voiceBalance": rng.range(-1.0, 1.0)},
        "modulation": {"enabled": true, "lfo": {"shape": rng.pick(&["sine", "triangle", "square", "saw"]), "rateHz": rng.range(0.01, 30.0), "depth": rng.unit()}, "envelope": {"attackMs": rng.range(0.05, 100.0), "releaseMs": rng.range(1.0, 1000.0), "amount": rng.unit()}, "routes": [{"source": "lfo", "target": "masterGain", "amount": rng.range(-2.0, 2.0), "offset": rng.range(-1.0, 1.0)}, {"source": "envelope", "target": "stereoWidth", "amount": rng.range(-2.0, 2.0), "offset": rng.range(-1.0, 1.0)}]},
        "modEffects": {
            "delay": {"enabled": true, "delayMs": rng.range(0.0, 2000.0), "feedback": rng.range(0.0, 0.98), "mix": rng.unit()},
            "chorus": {"enabled": true, "rateHz": rng.range(0.01, 20.0), "depthMs": rng.range(0.0, 50.0), "mix": rng.unit()},
            "flanger": {"enabled": true, "rateHz": rng.range(0.01, 20.0), "depthMs": rng.range(0.0, 50.0), "feedback": rng.range(0.0, 0.98), "mix": rng.unit()},
            "phaser": {"enabled": true, "rateHz": rng.range(0.01, 20.0), "depth": rng.unit(), "feedback": rng.range(0.0, 0.98), "mix": rng.unit(), "stages": 1 + (rng.next_u32() % 8)},
            "tremolo": {"enabled": true, "rateHz": rng.range(0.01, 30.0), "depth": rng.unit(), "mix": rng.unit()}
        },
        "stereoWidth": rng.range(0.0, 2.0),
        "spatial": {"mode": "off"}
    })
}

fn boundary_overrides(maximum: bool, fs: f64) -> Value {
    let x = |lo: f64, hi: f64| if maximum { hi } else { lo };
    let mut value = random_overrides(if maximum { u32::MAX } else { 0 }, fs);
    value["deesser"]["centerHz"] = json!(x(100.0, fs * 0.45));
    value["deesser"]["q"] = json!(x(0.1, 20.0));
    value["compressor"]["thresholdDb"] = json!(x(-80.0, 0.0));
    value["compressor"]["ratio"] = json!(x(1.0, 100.0));
    value["compressor"]["kneeDb"] = json!(x(0.0, 40.0));
    value["bassEnhancer"]["lowBoostDb"] = json!(x(-6.0, 12.0));
    value["reverb"]["enabled"] = json!(true);
    value["reverb"]["mode"] = json!(if maximum { "fdn" } else { "algorithmic" });
    value["reverb"]["algorithmic"]["roomSize"] = json!(x(0.0, 1.0));
    value["reverb"]["algorithmic"]["damping"] = json!(x(0.0, 1.0));
    value["limiter"]["thresholdDb"] = json!(x(-60.0, 0.0));
    value["limiter"]["lookaheadMs"] = json!(x(0.0, 50.0));
    value["dynamicEq"]["strength"] = json!(x(0.0, 1.0));
    value["ieq"]["enabled"] = json!(!maximum);
    value["stereoWidth"] = json!(x(0.0, 2.0));
    value
}

fn input(frames: usize, seed: u32) -> (Vec<f32>, Vec<f32>) {
    let mut rng = Lcg(seed);
    let mut left = Vec::with_capacity(frames);
    let mut right = Vec::with_capacity(frames);
    for _ in 0..frames {
        left.push(rng.range(-0.95, 0.95) as f32);
        right.push(rng.range(-0.95, 0.95) as f32);
    }
    (left, right)
}

fn process_blocks(stage: &mut EngineChainStage, left: &mut [f32], right: &mut [f32], block: usize) {
    for (l, r) in left.chunks_mut(block).zip(right.chunks_mut(block)) {
        stage.process(l, r);
    }
}

fn assert_same_bits(left: &[f32], right: &[f32], context: &str) {
    assert_eq!(left.len(), right.len());
    for (index, (&a, &b)) in left.iter().zip(right).enumerate() {
        assert_eq!(
            a.to_bits(),
            b.to_bits(),
            "{context}: sample {index}: {a:?} != {b:?}"
        );
    }
}

fn verify_case(fs: f64, block: usize, overrides: &Value, seed: u32) {
    let frames = block * 5 + 17;
    let source = input(frames, seed ^ 0xa5a5_5a5a);
    let make = || {
        let params = EngineChainParams::from_overrides(fs, overrides).expect("扫描参数必须合法");
        let mut stage = EngineChainStage::from_params(fs, params).expect("扫描参数必须可装配");
        stage.prepare(block);
        stage.reset();
        stage
    };

    let mut stage = make();
    let (mut first_l, mut first_r) = source.clone();
    process_blocks(&mut stage, &mut first_l, &mut first_r, block);
    assert!(
        first_l
            .iter()
            .chain(&first_r)
            .all(|sample| sample.is_finite()),
        "fs={fs} block={block} seed={seed:#010x} 出现非有限输出"
    );

    let (mut replay_l, mut replay_r) = source.clone();
    process_blocks(&mut make(), &mut replay_l, &mut replay_r, block);
    assert_same_bits(&first_l, &replay_l, "独立实例左声道确定性重放");
    assert_same_bits(&first_r, &replay_r, "独立实例右声道确定性重放");

    stage.reset();
    let (mut reset_l, mut reset_r) = source;
    process_blocks(&mut stage, &mut reset_l, &mut reset_r, block);
    assert!(
        reset_l
            .iter()
            .chain(&reset_r)
            .all(|sample| sample.is_finite()),
        "fs={fs} block={block} seed={seed:#010x} reset 后出现非有限输出"
    );
    if !overrides
        .pointer("/ieq/enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        assert_same_bits(&first_l, &reset_l, "reset 后左声道复现");
        assert_same_bits(&first_r, &reset_r, "reset 后右声道复现");
    }
}

#[test]
fn fixed_seed_legal_full_chain_parameter_scan_is_finite_and_replayable() {
    const SEEDS: [u32; 8] = [
        0x0000_0001,
        0x1234_5678,
        0x243f_6a88,
        0x5eed_f00d,
        0x7fff_ffff,
        0x8000_0000,
        0xdead_beef,
        0xffff_fffe,
    ];
    for (case_index, &(fs, block)) in CASES.iter().enumerate() {
        for &seed in &SEEDS {
            verify_case(
                fs,
                block,
                &random_overrides(seed, fs),
                seed ^ case_index as u32,
            );
        }
        verify_case(fs, block, &boundary_overrides(false, fs), 0x1111_1111);
        verify_case(fs, block, &boundary_overrides(true, fs), 0xeeee_eeee);
    }
}
