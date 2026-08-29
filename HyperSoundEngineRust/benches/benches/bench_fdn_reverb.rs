//! bench_fdn_reverb —— FdnReverbStage（8 线反馈延迟网络混响）的 criterion 基准。
//!
//! 参数对齐引擎默认混响工况（roomSize/damping 0.5、wet 0.3 / dry 0.7、hall、
//! 8 线、preDelay 0——与 parity_reverb_simple 同一快照口径，便于两个混响
//! 实现的成本对照）。8 条延迟线的逐样本反馈递推 + 阻尼滤波是成本主体。

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use hse_benches::{push_blocks, StereoBuffer, BLOCK_MATRIX, FRAMES_PER_ITER, SAMPLE_RATE_HZ};
use hse_core::fdn_reverb::{FdnReverbParams, FdnReverbStage};
use hse_core::Stage;

fn ready_stage(max_block_frames: usize) -> FdnReverbStage {
    let mut stage = FdnReverbStage::from_params(
        SAMPLE_RATE_HZ,
        FdnReverbParams {
            room_size: 0.5,
            damping: 0.5,
            wet: 0.3,
            dry: 0.7,
            pre_delay_ms: 0.0,
            width: 1.0,
            reverb_type: "hall".to_string(),
            lines: Some(8.0),
        },
    )
    .expect("基准用 FDN 混响参数合法");
    stage.prepare(max_block_frames);
    stage
}

fn bench_fdn_reverb(c: &mut Criterion) {
    let master = StereoBuffer::synthesized(FRAMES_PER_ITER);

    let mut group = c.benchmark_group("bench_fdn_reverb/block_matrix");
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

criterion_group!(benches, bench_fdn_reverb);
criterion_main!(benches);
