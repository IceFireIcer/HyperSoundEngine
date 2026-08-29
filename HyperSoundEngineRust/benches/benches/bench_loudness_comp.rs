//! bench_loudness_comp —— LoudnessCompStage（响度补偿）的 criterion 基准。
//!
//! 主形态对齐引擎活跃工况：mode=auto + volumePercent=100（auto 目标曲线
//! 全程重算 + 六带 shelf 平滑，向量 loudness-comp.case1 同参）。块长矩阵
//! 128/256/512/1024。平滑时间常数 0.2s（TS 缺省）。

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use hse_benches::{push_blocks, StereoBuffer, BLOCK_MATRIX, FRAMES_PER_ITER, SAMPLE_RATE_HZ};
use hse_core::loudness_comp::{LoudnessCompSettings, LoudnessCompStage};
use hse_core::Stage;

fn ready_stage(max_block_frames: usize) -> LoudnessCompStage {
    let mut stage = LoudnessCompStage::from_settings(
        SAMPLE_RATE_HZ,
        LoudnessCompSettings {
            volume_percent: 100.0,
            max_boost_db: 12.0,
            preset: "flat".to_string(),
            bands: Vec::new(),
            mode: "auto".to_string(),
            smoothing_seconds: 0.2,
        },
    )
    .expect("基准用响度补偿参数合法");
    stage.prepare(max_block_frames);
    stage
}

fn bench_loudness_comp(c: &mut Criterion) {
    let master = StereoBuffer::synthesized(FRAMES_PER_ITER);

    let mut group = c.benchmark_group("bench_loudness_comp/block_matrix");
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

criterion_group!(benches, bench_loudness_comp);
criterion_main!(benches);
