//! bench_convolver —— ConvolverStage（分区卷积混响）的 criterion 基准。
//!
//! 四个 IR 场景（引擎接线顺序：构造 → loadIR → setMix → setPreDelayMs）：
//! - delta（delay 0，向量 case1 同配方）：IR 仅 1 点，量化"分区机制固定开销"；
//! - expNoise L=6000（向量 case3 同配方：seed 12345 / decay 12 / amp 0.5）：
//!   长尾真实混响，短分区区 + 长分区区的双区成本主体；
//! - expNoise L=1024（向量 case4 同配方：seed 777 / decay 5 / amp 0.5，
//!   partition 256 / long 2048 / dePeriodize off）：短 IR 对照。
//! delta 与 expNoise L=6000 跑块长矩阵；短 IR 对照组固定块长 512（向量 case1
//! 的驱动块长；384 等非 2 幂块长保留给对拍 harness，基准统一用 2 幂便于矩阵
//! 对比）。IR 与 FFT 计划的全部构造成本都在计时区外（对齐实时铁律：
//! loadIR 不在音频回调内发生）。

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use hse_benches::{push_blocks, StereoBuffer, BLOCK_MATRIX, FRAMES_PER_ITER, SAMPLE_RATE_HZ};
use hse_core::convolver::{build_ir_recipe, ConvolverOptions, ConvolverStage, IrRecipe};
use hse_core::Stage;

fn ready_stage(recipe: &IrRecipe, opts: ConvolverOptions, max_block_frames: usize) -> ConvolverStage {
    let ir = build_ir_recipe(recipe).expect("基准用 IR 配方合法");
    let mut stage = ConvolverStage::new(SAMPLE_RATE_HZ, opts).expect("基准用卷积选项合法");
    stage.load_ir(&ir, Some("bench-ir")).expect("基准 IR 加载不应失败");
    stage.set_mix(0.3);
    stage.set_pre_delay_ms(0.0);
    stage.prepare(max_block_frames);
    stage
}

fn default_opts() -> ConvolverOptions {
    ConvolverOptions::default() // 512 / 4096 / 100ms / dePeriodize on
}

fn bench_convolver(c: &mut Criterion) {
    let master = StereoBuffer::synthesized(FRAMES_PER_ITER);

    // 主组：expNoise L=6000（真实混响尾）× 块长矩阵。
    let mut group = c.benchmark_group("bench_convolver/exp6000/block_matrix");
    group.sample_size(40);
    group.warm_up_time(std::time::Duration::from_secs(1));
    group.measurement_time(std::time::Duration::from_secs(3));
    group.throughput(Throughput::Elements(FRAMES_PER_ITER as u64));
    for block in BLOCK_MATRIX {
        group.bench_with_input(BenchmarkId::from_parameter(block), &block, |b, &block| {
            let mut stage = ready_stage(
                &IrRecipe::ExpNoise { length: 6000.0, seed: 12345, decay: 12.0, amp: 0.5 },
                default_opts(),
                block,
            );
            let mut work = StereoBuffer::zeroed(FRAMES_PER_ITER);
            b.iter(|| {
                let acc = push_blocks(&mut stage, &master, &mut work, block, FRAMES_PER_ITER / block);
                black_box(acc)
            });
        });
    }
    group.finish();

    // 对照组（固定块长 512）：delta / exp6000 / exp1024。
    let mut cases = c.benchmark_group("bench_convolver/ir_cases");
    cases.sample_size(40);
    cases.warm_up_time(std::time::Duration::from_secs(1));
    cases.measurement_time(std::time::Duration::from_secs(3));
    cases.throughput(Throughput::Elements(FRAMES_PER_ITER as u64));

    cases.bench_function("delta_delay0_block512", |b| {
        let mut stage = ready_stage(&IrRecipe::Delta { delay: 0.0 }, default_opts(), 512);
        let mut work = StereoBuffer::zeroed(FRAMES_PER_ITER);
        b.iter(|| {
            let acc = push_blocks(&mut stage, &master, &mut work, 512, FRAMES_PER_ITER / 512);
            black_box(acc)
        });
    });
    cases.bench_function("exp6000_block512", |b| {
        let mut stage = ready_stage(
            &IrRecipe::ExpNoise { length: 6000.0, seed: 12345, decay: 12.0, amp: 0.5 },
            default_opts(),
            512,
        );
        let mut work = StereoBuffer::zeroed(FRAMES_PER_ITER);
        b.iter(|| {
            let acc = push_blocks(&mut stage, &master, &mut work, 512, FRAMES_PER_ITER / 512);
            black_box(acc)
        });
    });
    cases.bench_function("exp1024_p256_deperiodize_off_block512", |b| {
        let mut stage = ready_stage(
            &IrRecipe::ExpNoise { length: 1024.0, seed: 777, decay: 5.0, amp: 0.5 },
            ConvolverOptions {
                partition_size: 256.0,
                long_partition_size: 2048.0,
                short_region_ms: 100.0,
                de_periodize: false,
            },
            512,
        );
        let mut work = StereoBuffer::zeroed(FRAMES_PER_ITER);
        b.iter(|| {
            let acc = push_blocks(&mut stage, &master, &mut work, 512, FRAMES_PER_ITER / 512);
            black_box(acc)
        });
    });
    cases.finish();
}

criterion_group!(benches, bench_convolver);
criterion_main!(benches);
