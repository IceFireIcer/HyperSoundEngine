//! bench_mod_effects —— ModEffectsStage（delay→chorus→flanger→phaser→tremolo
//! 五级级联，引擎接线顺序）的 criterion 基准。
//!
//! 与冻结向量"逐级开关"的用例不同，本基准把五级全部开启（delay 300ms/
//! fb0.5、chorus 4Hz/5ms、flanger 2.5Hz/4ms/fb0.6、phaser 4 级/fb0.4、
//! tremolo 5Hz/0.5——各级速率/深度取向量 case3 与引擎典型值），量化五效果
//! 链满载的成本。chorus/flanger 的 LFO 按整块步进 → 输出依赖顶层 blockSize，
//! 块长矩阵因此对该模块有数值语义（不只是开销摊薄）。

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use hse_benches::{push_blocks, StereoBuffer, BLOCK_MATRIX, FRAMES_PER_ITER, SAMPLE_RATE_HZ};
use hse_core::mod_effects::{
    ChorusSettings, DelaySettings, FlangerSettings, ModEffectsSettings, ModEffectsStage,
    PhaserSettings, TremoloSettings,
};
use hse_core::Stage;

fn ready_stage(max_block_frames: usize) -> ModEffectsStage {
    let mut stage = ModEffectsStage::from_settings(
        SAMPLE_RATE_HZ,
        ModEffectsSettings {
            delay: DelaySettings { enabled: true, delay_ms: 300.0, feedback: 0.5, mix: 0.3 },
            chorus: ChorusSettings { enabled: true, rate_hz: 4.0, depth_ms: 5.0, mix: 0.5 },
            flanger: FlangerSettings {
                enabled: true,
                rate_hz: 2.5,
                depth_ms: 4.0,
                feedback: 0.6,
                mix: 0.5,
            },
            phaser: PhaserSettings {
                enabled: true,
                rate_hz: 0.5,
                depth: 0.5,
                feedback: 0.4,
                mix: 0.5,
                stages: 4.0,
            },
            tremolo: TremoloSettings { enabled: true, rate_hz: 5.0, depth: 0.5, mix: 1.0 },
        },
    )
    .expect("基准用调制效果参数合法");
    stage.prepare(max_block_frames);
    stage
}

fn bench_mod_effects(c: &mut Criterion) {
    let master = StereoBuffer::synthesized(FRAMES_PER_ITER);

    let mut group = c.benchmark_group("bench_mod_effects/all_five_on_block_matrix");
    group.sample_size(40);
    group.warm_up_time(std::time::Duration::from_secs(1));
    group.measurement_time(std::time::Duration::from_secs(3));
    group.throughput(Throughput::Elements(FRAMES_PER_ITER as u64));
    for block in BLOCK_MATRIX {
        group.bench_with_input(BenchmarkId::from_parameter(block), &block, |b, &block| {
            let mut stage = ready_stage(block);
            let mut work = StereoBuffer::zeroed(FRAMES_PER_ITER);
            b.iter(|| {
                let acc = push_blocks(&mut stage, &master, &mut work, block, FRAMES_PER_ITER / block);
                black_box(acc)
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_mod_effects);
criterion_main!(benches);
