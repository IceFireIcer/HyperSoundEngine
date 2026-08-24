//! parity_limiter —— LimiterStage 的 criterion 基准雏形。
//!
//! 参数取 TS `Limiter` 构造默认快照 + 真峰值开：threshold −1dB /
//! lookahead 5ms / attack 0.5ms / release 150ms / truePeak on
//! （见 `specs/dsp/limiter.md` 与 hse-core 桩注释）。另附真峰值关对照组：
//! 真峰值 4× 过采样是预期热点之一，该对照为规划书 §五性能冲刺的热点分析
//! 预留口径。合成激励峰值逼近满幅（越过 -1dBFS 阈值），保证限幅增益路径
//! 有真实工作量。真实算法落地前数字均为占位直通基线。

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use hse_benches::{push_blocks, StereoBuffer, FRAMES_PER_ITER, MAIN_BLOCK_FRAMES, SAMPLE_RATE_HZ};
use hse_core::limiter::{LimiterSettings, LimiterStage};
use hse_core::Stage;

/// 对齐 TS 构造默认的参数快照，仅真峰值开关可变。
fn settings(true_peak: bool) -> LimiterSettings {
    LimiterSettings {
        enabled: true,
        threshold_db: -1.0,
        lookahead_ms: 5.0,
        attack_ms: 0.5,
        release_ms: 150.0,
        true_peak,
    }
}

fn ready_stage(true_peak: bool, max_block_frames: usize) -> LimiterStage {
    let mut stage = LimiterStage::from_settings(SAMPLE_RATE_HZ, settings(true_peak))
        .expect("基准用限幅器参数合法，构造不应失败");
    stage.prepare(max_block_frames);
    stage
}

fn bench_parity_limiter(c: &mut Criterion) {
    let master = StereoBuffer::synthesized(FRAMES_PER_ITER);

    let mut group = c.benchmark_group("parity_limiter/main");
    group.sample_size(40);
    group.throughput(Throughput::Elements(FRAMES_PER_ITER as u64));

    for (label, true_peak) in [("default_true_peak_on_block128", true), ("default_true_peak_off_block128", false)] {
        group.bench_with_input(BenchmarkId::from_parameter(label), &true_peak, |b, &tp| {
            let mut stage = ready_stage(tp, MAIN_BLOCK_FRAMES);
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
    }
    group.finish();
}

criterion_group!(benches, bench_parity_limiter);
criterion_main!(benches);
