//! bench_modulation_matrix —— ModulationMatrixStage（控制率调制矩阵）的 criterion 基准。
//!
//! 路由对齐冻结向量 modulation-matrix.case1：lfo(sine 4Hz, depth 1)→masterGain
//! amount 0.5，包络 attack 10ms / release 200ms / amount 0.5。每块先推进矩阵
//! （LFO 相位 + 包络状态，包络读取增益前输入），再把 masterGain 逐样本乘到
//! L/R——成本 = 块率控制计算 + 一次逐样本乘法，属于全链最轻一级。

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use hse_benches::{push_blocks, StereoBuffer, BLOCK_MATRIX, FRAMES_PER_ITER, SAMPLE_RATE_HZ};
use hse_core::modulation_matrix::{
    EnvelopeParams, LfoParams, LfoShape, ModSource, ModTarget, ModulationMatrixStage,
    ModulationRoute,
};
use hse_core::Stage;

fn ready_stage(max_block_frames: usize) -> ModulationMatrixStage {
    let mut stage = ModulationMatrixStage::from_params(
        SAMPLE_RATE_HZ,
        vec![ModulationRoute {
            source: ModSource::Lfo,
            target: ModTarget::MasterGain,
            amount: 0.5,
            offset: 0.0,
        }],
        LfoParams { shape: LfoShape::Sine, rate_hz: 4.0, depth: 1.0 },
        EnvelopeParams { attack_ms: 10.0, release_ms: 200.0, amount: 0.5 },
    )
    .expect("基准用调制矩阵参数合法");
    stage.prepare(max_block_frames);
    stage
}

fn bench_modulation_matrix(c: &mut Criterion) {
    let master = StereoBuffer::synthesized(FRAMES_PER_ITER);

    let mut group = c.benchmark_group("bench_modulation_matrix/block_matrix");
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

criterion_group!(benches, bench_modulation_matrix);
criterion_main!(benches);
