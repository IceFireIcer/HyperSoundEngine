//! bench_dynamic_eq —— DynamicEqStage（5 带动态均衡）的 criterion 基准。
//!
//! 参数对齐冻结向量 dynamic-eq.case2 的开启形态（strength 0.5 /
//! threshold -10dB / ratio 2 / 5 带全部 enabled 且目标增益非零）；模块内部分析
//! blockSize 固定 128（向量 case2 同值），与顶层驱动分块相互独立——这正是
//! 块长矩阵要展示的两个独立分块参数的关系。

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use hse_benches::{push_blocks, StereoBuffer, BLOCK_MATRIX, FRAMES_PER_ITER, SAMPLE_RATE_HZ};
use hse_core::dynamic_eq::{DynamicEqBandParam, DynamicEqParams, DynamicEqStage};
use hse_core::Stage;

fn ready_stage(max_block_frames: usize) -> DynamicEqStage {
    let bands = vec![
        DynamicEqBandParam { enabled: true, frequency: 200.0, target_gain_db: Some(6.0) },
        DynamicEqBandParam { enabled: true, frequency: 800.0, target_gain_db: Some(4.0) },
        DynamicEqBandParam { enabled: true, frequency: 2500.0, target_gain_db: Some(3.0) },
        DynamicEqBandParam { enabled: true, frequency: 8000.0, target_gain_db: Some(2.0) },
        DynamicEqBandParam { enabled: true, frequency: 0.0, target_gain_db: Some(1.0) },
    ];
    let mut stage = DynamicEqStage::from_params(
        SAMPLE_RATE_HZ,
        DynamicEqParams {
            enabled: Some(true),
            strength: Some(0.5),
            threshold_db: Some(-10.0),
            ratio: Some(2.0),
            knee_db: Some(6.0),
            attack_ms: Some(20.0),
            release_ms: Some(200.0),
            block_size: Some(128.0),
            bands: Some(bands),
        },
    )
    .expect("基准用动态均衡参数合法");
    stage.prepare(max_block_frames);
    stage
}

fn bench_dynamic_eq(c: &mut Criterion) {
    let master = StereoBuffer::synthesized(FRAMES_PER_ITER);

    let mut group = c.benchmark_group("bench_dynamic_eq/block_matrix");
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

criterion_group!(benches, bench_dynamic_eq);
criterion_main!(benches);
