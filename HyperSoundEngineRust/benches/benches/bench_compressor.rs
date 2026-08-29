//! bench_compressor —— CompressorStage 的 criterion 基准。
//!
//! 参数对齐冻结向量 compressor.case1（threshold -6dB / ratio 4 / knee 6 /
//! attack 10ms / release 150ms，enabled），合成激励峰值逼近满幅、远越阈值，
//! 包络跟随与增益平滑全程有真实工作量。主组为块长矩阵 128/256/512/1024；
//! 另附 sidechain（单声道和差分派生）开启对照组，量化派生单声道的增量成本。

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use hse_benches::{push_blocks, StereoBuffer, BLOCK_MATRIX, FRAMES_PER_ITER, SAMPLE_RATE_HZ};
use hse_core::compressor::{CompressorSettings, CompressorStage};
use hse_core::Stage;

fn settings(sidechain: bool) -> CompressorSettings {
    CompressorSettings {
        enabled: true,
        threshold_db: -6.0,
        ratio: 4.0,
        knee_db: 6.0,
        attack_ms: 10.0,
        release_ms: 150.0,
        makeup_db: 0.0,
        output_gain: 1.0,
        sidechain_enabled: sidechain,
    }
}

fn ready_stage(sidechain: bool, max_block_frames: usize) -> CompressorStage {
    let mut stage = CompressorStage::from_settings(SAMPLE_RATE_HZ, settings(sidechain))
        .expect("基准用压缩器参数合法");
    stage.prepare(max_block_frames);
    stage
}

fn bench_compressor(c: &mut Criterion) {
    let master = StereoBuffer::synthesized(FRAMES_PER_ITER);

    // 主组：块长矩阵（sidechain 关）。
    let mut group = c.benchmark_group("bench_compressor/block_matrix");
    group.sample_size(40);
    group.warm_up_time(std::time::Duration::from_secs(1));
    group.measurement_time(std::time::Duration::from_secs(3));
    group.throughput(Throughput::Elements(FRAMES_PER_ITER as u64));
    for block in BLOCK_MATRIX {
        group.bench_with_input(BenchmarkId::from_parameter(block), &block, |b, &block| {
            let mut stage = ready_stage(false, block);
            let mut work = StereoBuffer::zeroed(FRAMES_PER_ITER);
            b.iter(|| {
                let acc = push_blocks(&mut stage, &master, &mut work, block, FRAMES_PER_ITER / block);
                black_box(acc)
            });
        });
    }
    group.finish();

    // 对照组：sidechain 开（阈值前派生单声道和），块长 128。
    let mut sc = c.benchmark_group("bench_compressor/sidechain");
    sc.sample_size(40);
    sc.warm_up_time(std::time::Duration::from_secs(1));
    sc.measurement_time(std::time::Duration::from_secs(3));
    sc.throughput(Throughput::Elements(FRAMES_PER_ITER as u64));
    sc.bench_function("sidechain_on_block128", |b| {
        let mut stage = ready_stage(true, 128);
        let mut work = StereoBuffer::zeroed(FRAMES_PER_ITER);
        b.iter(|| {
            let acc = push_blocks(&mut stage, &master, &mut work, 128, FRAMES_PER_ITER / 128);
            black_box(acc)
        });
    });
    sc.finish();
}

criterion_group!(benches, bench_compressor);
criterion_main!(benches);
