//! parity_biquad —— BiquadStage 的 criterion 基准雏形。
//!
//! 场景对齐 TS benchmark 口径（仓库根 `scripts/benchmark.mjs`）：48kHz /
//! 立体声 / 主块长 128 帧；滤波器参数取 Phase 1 试点典型值
//! peaking@1kHz、Q=1.2、+4dB（与冻结向量 biquad 用例同量级，见
//! `specs/dsp/biquad.md`）。
//!
//! 另附 128/256/512 块长对比组（规划书 §五"基准矩阵：块长 128/256/512"
//! 的雏形）：每次迭代总帧数恒为 32768 帧，只改分块大小——组内数字差异
//! 即"每调用固定开销 × 分块粒度"的净效应。模块真实算法落地前，
//! 本文件所有数字均为占位直通基线。

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use hse_benches::{push_blocks, StereoBuffer, FRAMES_PER_ITER, MAIN_BLOCK_FRAMES, SAMPLE_RATE_HZ};
use hse_core::biquad::BiquadStage;
use hse_core::Stage;

/// 构造并预分配一个就绪阶段（构造/prepare 的全部堆分配都发生在计时区外，
/// 对齐 TS 脚本 engine.prepare(block) 先于计时的口径与实时铁律）。
fn ready_stage(max_block_frames: usize) -> BiquadStage {
    let mut stage = BiquadStage::new(SAMPLE_RATE_HZ, "peaking", 1000.0, 1.2, 4.0)
        .expect("基准用滤波器参数合法，构造不应失败");
    stage.prepare(max_block_frames);
    stage
}

fn bench_parity_biquad(c: &mut Criterion) {
    // 母带缓冲整个基准期间只填充一次（确定性合成信号）。
    let master = StereoBuffer::synthesized(FRAMES_PER_ITER);

    // 主场景组：对齐 TS benchmark 口径（48kHz / 128 帧 / 立体声）。
    let mut main = c.benchmark_group("parity_biquad/main");
    main.sample_size(40);
    main.throughput(Throughput::Elements(FRAMES_PER_ITER as u64));
    main.bench_function("peaking_1k_q1p2_g+4dB_block128", |b| {
        let mut stage = ready_stage(MAIN_BLOCK_FRAMES);
        let mut work = StereoBuffer::zeroed(FRAMES_PER_ITER);
        b.iter(|| {
            let acc = push_blocks(
                &mut stage,
                &master,
                &mut work,
                MAIN_BLOCK_FRAMES,
                FRAMES_PER_ITER / MAIN_BLOCK_FRAMES,
            );
            black_box(acc)
        });
    });
    main.finish();

    // 块长对比组：总帧数恒定（32768 帧/迭代），分块 128 → 256 → 512。
    let mut matrix = c.benchmark_group("parity_biquad/block_matrix");
    matrix.sample_size(40);
    matrix.throughput(Throughput::Elements(FRAMES_PER_ITER as u64));
    for block in [MAIN_BLOCK_FRAMES, 256, 512] {
        matrix.bench_with_input(BenchmarkId::from_parameter(block), &block, |b, &block| {
            let mut stage = ready_stage(block);
            let mut work = StereoBuffer::zeroed(FRAMES_PER_ITER);
            b.iter(|| {
                let acc = push_blocks(&mut stage, &master, &mut work, block, FRAMES_PER_ITER / block);
                black_box(acc)
            });
        });
    }
    matrix.finish();
}

criterion_group!(benches, bench_parity_biquad);
criterion_main!(benches);
