//! bench_chain_full —— 引擎全链（hse-service ServiceEngineChain 1–21 级）离线吞吐基准。
//!
//! 这是《规划书》§三指标 "离线吞吐 ≥3× TS 基线" 的正式测量：
//! - 60 秒 48kHz 立体声音频（2,880,000 帧），主块长 128（22,500 块/迭代）；
//! - 驱动 hse-service 的 [`ServiceEngineChain`]（引擎服务实际运行的装配形态），
//!   装配顺序 = Rust `EngineChainStage` 1–21 级；spatial 固定 off；
//! - TS 对照基线：`npm run benchmark` 本机实测 5000ms 音频 279.61ms = 5.59%
//!   realtime（48kHz/128，恒定 0.1 激励，createDefaultParams 默认链）。
//!   3× 目标 ⇒ Rust 需 ≤1.863% realtime（≤186.4ms 处理 10s 音频…按 5s 口径
//!   ≤93.2ms）。
//!
//! 场景：
//! - `ts_default_*`：TS createDefaultParams 等价快照（eqChain 10 段 0dB + Q
//!   补偿显式装配、reverb simple wet .3/dry .7、limiter on/truePeak、其余
//!   模块缺省关闭）——与 TS 基线同链同参。
//!   `constant_0p1` 输入与 TS 脚本逐字同口径（headline 对比数字）；
//!   `synthesized` 输入为满幅激励（限幅器真实增益衰减的保守上界）。
//! - `all_on_*`：全部模块开启的满载链（见 all_on_params），混响路分别
//!   simple / fdn / convolver(expNoise 6000)，输入恒为满幅合成信号。
//! - `heavy_convolver_ir4s`：§三"最重场景"口径——全链开启 + IR≈4s
//!   （192000 样本 @48kHz）卷积混响，对齐 ≤25% 单核目标的验收场景。
//!
//! 每次迭代：母带复制进工作缓冲 → 链 reset → 22,500 块连续 process_planar。
//! 全部构造/预分配在计时区外。 Throughput 按帧计：criterion 报告的
//! elements/s ÷ 48000 即 ×realtime。

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use hse_benches::{StereoBuffer, SAMPLE_RATE_HZ};
use hse_service::dsp_chain::ServiceEngineChain;
use hse_service::params::{
    BiquadSpec, EqBandSpec, EqChainSpec, ModMatrixRouteSpec, PilotParams, ReverbRouteKind,
};

/// 全链基准音频时长（§三指标口径：60s）。
const CHAIN_SECONDS: u64 = 60;
/// 全链总帧数 = 48000 × 60。
const CHAIN_FRAMES: usize = (SAMPLE_RATE_HZ as usize) * (CHAIN_SECONDS as usize);
/// 全链块长（TS benchmark 与实时目标同口径）。
const CHAIN_BLOCK: usize = 128;

/// PRO 10 段 octave 中心频率（TS PRO_EQ_DEFAULT_BANDS）。
const PRO_EQ_FREQS: [f64; 10] = [
    31.5, 63.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0,
];

/// TS createDefaultParams 等价快照：显式装配 eqChain（10 段 0dB + Q 补偿），
/// 其余键取 PilotParams::default()（与 TS 默认逐键对齐，见 params.rs 注释）。
fn ts_default_params() -> PilotParams {
    let mut p = PilotParams::default();
    // TS 默认 eq：PRO 10 段 octave 中心，0dB，qCompensation on。0dB 增益下
    // 滤波器仍逐样本运算（~1e-15 级恒等），成本与 TS 默认链同级。
    p.eq_chain = Some(EqChainSpec {
        bands: PRO_EQ_FREQS
            .iter()
            .map(|&f| EqBandSpec {
                frequency: f,
                gain: 0.0,
                q: 1.0,
            })
            .collect(),
        band_count: 10.0,
        q_compensation: true,
    });
    p
}

/// 全模块开启满载快照（reverb 路由由调用方指定）。
fn all_on_params(route: ReverbRouteKind) -> PilotParams {
    let mut p = PilotParams::default();
    p.biquad = Some(BiquadSpec {
        filter_type: "peaking".to_string(),
        f0: 1000.0,
        q: 1.2,
        gain_db: 4.0,
    });
    // Pre-EQ：10 段非零增益交错（真实滤波工况）。
    p.eq_chain = Some(EqChainSpec {
        bands: PRO_EQ_FREQS
            .iter()
            .enumerate()
            .map(|(i, &f)| EqBandSpec {
                frequency: f,
                gain: if i % 2 == 0 { 4.0 } else { -4.0 },
                q: 1.2,
            })
            .collect(),
        band_count: 10.0,
        q_compensation: true,
    });
    p.deesser.enabled = true;
    p.compressor.enabled = true;
    p.compressor.threshold_db = -6.0;
    p.mod_effects.delay.enabled = true;
    p.mod_effects.chorus.enabled = true;
    p.mod_effects.flanger.enabled = true;
    p.mod_effects.phaser.enabled = true;
    p.mod_effects.tremolo.enabled = true;
    p.reverb_route = route;
    p.fdn_reverb.wet = 0.3; // all_on 下 FDN 也吃湿声
    p.bass_enhancer.enabled = true;
    p.bass_enhancer.low_boost_db = Some(6.0);
    p.loudness_comp.mode = "auto".to_string();
    p.loudness_comp.volume_percent = 100.0;
    p.dynamic_eq.enabled = true;
    p.dynamic_eq.strength = 0.5;
    p.dynamic_eq.threshold_db = -10.0;
    p.mod_matrix.routes = vec![ModMatrixRouteSpec {
        source: "lfo".to_string(),
        target: "masterGain".to_string(),
        amount: 0.5,
        offset: 0.0,
    }];
    p.mod_matrix.lfo.shape = "square".to_string();
    p.mod_matrix.lfo.rate_hz = 30.0;
    p.mod_matrix.lfo.depth = 1.0;
    p.limiter.enabled = true; // truePeak on（缺省已对齐）
    p
}

fn bench_chain_full(c: &mut Criterion) {
    let mut group = c.benchmark_group("bench_chain_full/offline_60s");
    group.sample_size(10);
    group.warm_up_time(std::time::Duration::from_secs(2));
    group.measurement_time(std::time::Duration::from_secs(10));
    group.throughput(Throughput::Elements(CHAIN_FRAMES as u64));

    let master_const = StereoBuffer::constant(CHAIN_FRAMES, 0.1);
    let master_synth = StereoBuffer::synthesized(CHAIN_FRAMES);

    let cases: [(&str, PilotParams, &StereoBuffer); 6] = [
        (
            "ts_default_constant_0p1",
            ts_default_params(),
            &master_const,
        ),
        ("ts_default_synthesized", ts_default_params(), &master_synth),
        (
            "all_on_reverb_simple",
            all_on_params(ReverbRouteKind::Simple),
            &master_synth,
        ),
        (
            "all_on_reverb_fdn",
            all_on_params(ReverbRouteKind::Fdn),
            &master_synth,
        ),
        (
            "all_on_reverb_convolver_exp6000",
            all_on_params(ReverbRouteKind::Convolver),
            &master_synth,
        ),
        // §三最重场景：全链开启 + IR≈4s（192000 样本）卷积混响。
        (
            "heavy_convolver_ir4s_full_on",
            all_on_params(ReverbRouteKind::Convolver),
            &master_synth,
        ),
    ];

    for (name, mut params, master) in cases {
        // convolver 路由需要 IR 配方（确定性 expNoise；heavy 场景 IR≈4s）。
        if params.reverb_route == ReverbRouteKind::Convolver {
            let ir_len = if name.starts_with("heavy_convolver_ir4s") {
                192_000.0
            } else {
                6000.0
            };
            params.convolver.ir_recipe = Some(hse_core::convolver::IrRecipe::ExpNoise {
                length: ir_len,
                seed: 12345,
                decay: 12.0,
                amp: 0.5,
            });
        }
        group.bench_function(BenchmarkId::from_parameter(name), |b| {
            // 装配/预分配在计时区外（对齐实时铁律：构造只在控制面线程发生）。
            let source = if params.loudness_comp.mode == "auto" {
                serde_json::json!({"loudnessComp": {}})
            } else {
                serde_json::json!({})
            };
            let canonical = params
                .to_canonical_json(&source, SAMPLE_RATE_HZ)
                .expect("基准参数必须可投影为完整快照");
            let mut chain = ServiceEngineChain::build(&canonical, SAMPLE_RATE_HZ, CHAIN_BLOCK)
                .expect("基准用全链参数快照必须可装配");
            let mut work = StereoBuffer::zeroed(CHAIN_FRAMES);
            b.iter(|| {
                work.left.copy_from_slice(&master.left);
                work.right.copy_from_slice(&master.right);
                chain.reset();
                let mut checksum = 0.0_f32;
                for off in (0..CHAIN_FRAMES).step_by(CHAIN_BLOCK) {
                    chain.process_planar(
                        &mut work.left[off..off + CHAIN_BLOCK],
                        &mut work.right[off..off + CHAIN_BLOCK],
                    );
                    checksum += work.left[off];
                }
                black_box(checksum)
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_chain_full);
criterion_main!(benches);
