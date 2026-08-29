//! bench_bass_enhancer —— BassEnhancerStage 的 criterion 基准。
//!
//! 参数对齐冻结向量 bass-enhancer.case1（cutoff 90Hz / Q 0.7 / even 谐波 /
//! harmonicGain 0.8 / mix 0.6，enabled）。主组为块长矩阵；另附 lowBoostDb
//! 对照组（case3 的 +6dB：谐波路径之外多一条低频架补偿支路）。

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use hse_benches::{push_blocks, StereoBuffer, BLOCK_MATRIX, FRAMES_PER_ITER, SAMPLE_RATE_HZ};
use hse_core::bass_enhancer::{BassEnhancerSettings, BassEnhancerStage};
use hse_core::Stage;

fn settings(low_boost_db: Option<f64>) -> BassEnhancerSettings {
    BassEnhancerSettings {
        enabled: true,
        cutoff_hz: 90.0,
        q: 0.7,
        harmonic_type: "even".to_string(),
        harmonic_gain: 0.8,
        mix: 0.6,
        level_db: 0.0,
        low_boost_db,
    }
}

fn ready_stage(low_boost_db: Option<f64>, max_block_frames: usize) -> BassEnhancerStage {
    let mut stage = BassEnhancerStage::from_settings(SAMPLE_RATE_HZ, settings(low_boost_db))
        .expect("基准用低音增强参数合法");
    stage.prepare(max_block_frames);
    stage
}

fn bench_bass_enhancer(c: &mut Criterion) {
    let master = StereoBuffer::synthesized(FRAMES_PER_ITER);

    // 主组：块长矩阵（lowBoostDb = 0，向量 case1 同值）。
    let mut group = c.benchmark_group("bench_bass_enhancer/block_matrix");
    group.sample_size(40);
    group.warm_up_time(std::time::Duration::from_secs(1));
    group.measurement_time(std::time::Duration::from_secs(3));
    group.throughput(Throughput::Elements(FRAMES_PER_ITER as u64));
    for block in BLOCK_MATRIX {
        group.bench_with_input(BenchmarkId::from_parameter(block), &block, |b, &block| {
            let mut stage = ready_stage(Some(0.0), block);
            let mut work = StereoBuffer::zeroed(FRAMES_PER_ITER);
            b.iter(|| {
                let acc = push_blocks(&mut stage, &master, &mut work, block, FRAMES_PER_ITER / block);
                black_box(acc)
            });
        });
    }
    group.finish();

    // 对照组：lowBoostDb = +6（多一条低频架支路），块长 128。
    let mut lb = c.benchmark_group("bench_bass_enhancer/low_boost");
    lb.sample_size(40);
    lb.warm_up_time(std::time::Duration::from_secs(1));
    lb.measurement_time(std::time::Duration::from_secs(3));
    lb.throughput(Throughput::Elements(FRAMES_PER_ITER as u64));
    lb.bench_function("low_boost_6dB_block128", |b| {
        let mut stage = ready_stage(Some(6.0), 128);
        let mut work = StereoBuffer::zeroed(FRAMES_PER_ITER);
        b.iter(|| {
            let acc = push_blocks(&mut stage, &master, &mut work, 128, FRAMES_PER_ITER / 128);
            black_box(acc)
        });
    });
    lb.finish();
}

criterion_group!(benches, bench_bass_enhancer);
criterion_main!(benches);
