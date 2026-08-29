//! bench_mid_side —— MidSideStage（M/S 编解码 + 宽度/人声平衡）的 criterion 基准。
//!
//! width=2.5 取冻结向量 mid-side.case1（越界钳制由模块内负责——此处为域内
//! 值，真实展开 M/S 往返）。该模块每样本只有常数次乘加，是全链最廉价的一级；
//! 块长矩阵主要展示"每调用固定开销"在极轻负载下的占比。

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use hse_benches::{push_blocks, StereoBuffer, BLOCK_MATRIX, FRAMES_PER_ITER};
use hse_core::mid_side::MidSideStage;
use hse_core::Stage;

fn ready_stage(max_block_frames: usize) -> MidSideStage {
    let mut stage = MidSideStage::new();
    stage.set_params(2.5, 0.0);
    stage.prepare(max_block_frames);
    stage
}

fn bench_mid_side(c: &mut Criterion) {
    let master = StereoBuffer::synthesized(FRAMES_PER_ITER);

    let mut group = c.benchmark_group("bench_mid_side/block_matrix");
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

criterion_group!(benches, bench_mid_side);
criterion_main!(benches);
