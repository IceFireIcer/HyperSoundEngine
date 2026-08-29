//! bench_eq_chain —— EqChainStage（Pre-EQ 10 段级联）的 criterion 基准。
//!
//! 参数对齐冻结向量 eq-chain.case1 的 10 段 (f, q) 集合，但增益改为逐段非零
//! （-6..+6dB 交错），确保级联双二阶处于真实滤波工况而非 0dB 逐位直通；
//! Q 补偿开（引擎默认形态）。块长矩阵 128/256/512/1024：每迭代总帧数恒为
//! 32768 帧，组内差异即"每调用固定开销 × 分块粒度"的净效应。

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use hse_benches::{push_blocks, StereoBuffer, BLOCK_MATRIX, FRAMES_PER_ITER, SAMPLE_RATE_HZ};
use hse_core::eq_chain::{EqBandParam, EqChainStage};
use hse_core::Stage;

/// 频率取 TS PRO_EQ_DEFAULT_BANDS 的 octave 中心，Q 取向量 case1 集合，
/// 增益 ±6dB 交错（避免 0dB 短路，让每级都做真实双二阶运算）。
fn bench_bands() -> Vec<EqBandParam> {
    let fq: [(f64, f64); 10] = [
        (40.0, 0.5),
        (80.0, 0.8),
        (160.0, 1.0),
        (320.0, 1.2),
        (640.0, 1.4),
        (1280.0, 2.0),
        (2560.0, 3.0),
        (5120.0, 4.0),
        (10240.0, 0.707),
        (16000.0, 6.0),
    ];
    fq.iter()
        .enumerate()
        .map(|(i, &(f, q))| EqBandParam {
            frequency: f,
            gain: if i % 2 == 0 { 6.0 } else { -6.0 },
            q,
        })
        .collect()
}

fn ready_stage(max_block_frames: usize) -> EqChainStage {
    let mut stage = EqChainStage::new(SAMPLE_RATE_HZ, 10.0).expect("基准用 bandCount 合法");
    stage.set_bands(&bench_bands());
    stage.set_q_compensation(true);
    stage.prepare(max_block_frames);
    stage
}

fn bench_eq_chain(c: &mut Criterion) {
    let master = StereoBuffer::synthesized(FRAMES_PER_ITER);

    let mut group = c.benchmark_group("bench_eq_chain/block_matrix");
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

criterion_group!(benches, bench_eq_chain);
criterion_main!(benches);
