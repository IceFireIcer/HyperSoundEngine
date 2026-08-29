//! bench_midi —— MidiBindings::consume（MIDI 学习/自动化消费路径）的 criterion 基准。
//!
//! 场景（块长 128，48kHz，与引擎实时块同口径）：
//! - burst_16_bindings_128_events：16 条绑定（8 条白名单参数路径 + 6 条
//!   builtin masterGain/stereoWidth + 2 条 note 绑定）全部 learn 后，单次
//!   consume 消费环内 128 个 CC 事件（事件突发），含 smoothMs=0 直通与
//!   smoothMs=20 平滑两组；params 为 share_codec 默认参数骨架 JSON。
//! - idle_block128 / idle_block1024：环为空时每块 consume 的守卫路径
//!   （稳态无 MIDI 流量的真实常态成本），每次迭代推 256 块。
//!
//! 每次迭代确定性重建事件环（clear + 固定事件序列）；params 在迭代间幂等
//! （同值覆写），不引入随机与时钟。

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use hse_benches::{FRAMES_PER_ITER, MAIN_BLOCK_FRAMES, SAMPLE_RATE_HZ};
use hse_core::midi::{
    AutomationTarget, BuiltinParam, ControlKind, LearnOpts, MidiBindings, MidiEventIn,
    MidiEventRing,
};
use hse_core::share_codec::default_params_skeleton;

/// 8 条参数路径绑定（白名单内，混合 smooth 0/20ms）。
const BOUND_PATHS: [&str; 8] = [
    "compressor.thresholdDb",
    "compressor.ratio",
    "deesser.thresholdDb",
    "reverb.algorithmic.wet",
    "modEffects.delay.delayMs",
    "dynamicEq.strength",
    "limiter.thresholdDb",
    "bassEnhancer.mix",
];

fn learned_bindings() -> MidiBindings {
    let mut b = MidiBindings::new();
    for (i, path) in BOUND_PATHS.iter().enumerate() {
        b.learn(
            i as i32,
            AutomationTarget::Path((*path).to_string()),
            LearnOpts { smooth_ms: Some(if i % 2 == 0 { 0.0 } else { 20.0 }), ..LearnOpts::default() },
        )
        .expect("白名单路径 learn 不应失败");
    }
    // builtin 两条（cc 8/9）+ note 两条（note 60/62）。
    b.learn(8, AutomationTarget::Builtin(BuiltinParam::MasterGain), LearnOpts::default())
        .expect("builtin learn 不应失败");
    b.learn(9, AutomationTarget::Builtin(BuiltinParam::StereoWidth), LearnOpts::default())
        .expect("builtin learn 不应失败");
    b.learn(
        60,
        AutomationTarget::Builtin(BuiltinParam::MasterGain),
        LearnOpts { event_type: ControlKind::Note, ..LearnOpts::default() },
    )
    .expect("note learn 不应失败");
    b.learn(
        62,
        AutomationTarget::Path("reverb.algorithmic.dry".to_string()),
        LearnOpts { event_type: ControlKind::Note, ..LearnOpts::default() },
    )
    .expect("note learn 不应失败");
    b
}

/// 确定性事件序列：128 个 CC（cc 0..15 循环、value 由固定序列驱动）。
fn burst_events() -> Vec<MidiEventIn> {
    (0..128)
        .map(|i| MidiEventIn::cc((i % 16) as f64, ((i * 37) % 128) as f64))
        .collect()
}

fn bench_midi(c: &mut Criterion) {
    let mut group = c.benchmark_group("bench_midi");
    group.sample_size(40);
    group.warm_up_time(std::time::Duration::from_secs(1));
    group.measurement_time(std::time::Duration::from_secs(3));

    let events = burst_events();

    // 突发消费：单次 consume 吃掉 128 事件（吞吐按事件计）。
    group.throughput(Throughput::Elements(events.len() as u64));
    group.bench_function("burst_16_bindings_128_events", |b| {
        let mut bindings = learned_bindings();
        let mut ring = MidiEventRing::new();
        let mut params = default_params_skeleton(SAMPLE_RATE_HZ);
        b.iter(|| {
            ring.clear();
            ring.push_slice(&events);
            let sections = bindings.consume(&mut ring, &mut params, SAMPLE_RATE_HZ, MAIN_BLOCK_FRAMES);
            black_box(sections.len())
        });
    });

    // 空闲守卫：环为空，每迭代推 256 块（吞吐按帧计）。
    group.throughput(Throughput::Elements(FRAMES_PER_ITER as u64));
    for block in [MAIN_BLOCK_FRAMES, 1024] {
        group.bench_with_input(BenchmarkId::new("idle", block), &block, |b, &block| {
            let mut bindings = learned_bindings();
            let mut ring = MidiEventRing::new();
            let mut params = default_params_skeleton(SAMPLE_RATE_HZ);
            let blocks_per_iter = FRAMES_PER_ITER / block;
            b.iter(|| {
                let mut sections_total = 0usize;
                for _ in 0..blocks_per_iter {
                    let sections =
                        bindings.consume(&mut ring, &mut params, SAMPLE_RATE_HZ, block);
                    sections_total += sections.len();
                }
                black_box(sections_total)
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_midi);
criterion_main!(benches);
