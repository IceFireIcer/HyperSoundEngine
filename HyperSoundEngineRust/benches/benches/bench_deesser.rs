//! bench_deesser —— DeesserStage 的 criterion 基准。
//!
//! 参数对齐冻结向量 deesser.case2 的开启形态（center 8kHz / Q 0.7 /
//! threshold -30dB / ratio 8 / splitBand on / mix 1，enabled）。splitBand
//! 开启时为"高通侧链压缩 + 全频带支路重组"双路结构，是本模块的成本主体。
//! 合成激励含 3.1kHz 以上的高频成分，齿音检测包络持续非平凡。

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use hse_benches::{push_blocks, StereoBuffer, BLOCK_MATRIX, FRAMES_PER_ITER, SAMPLE_RATE_HZ};
use hse_core::deesser::{DeesserSettings, DeesserStage};
use hse_core::Stage;

fn ready_stage(max_block_frames: usize) -> DeesserStage {
    let mut stage = DeesserStage::from_settings(
        SAMPLE_RATE_HZ,
        DeesserSettings {
            enabled: true,
            center_hz: 8000.0,
            q: 0.7,
            threshold_db: -30.0,
            ratio: 8.0,
            attack_ms: 1.0,
            release_ms: 80.0,
            split_band: true,
            mix: 1.0,
            sidechain_enabled: false,
        },
    )
    .expect("基准用齿音消除器参数合法");
    stage.prepare(max_block_frames);
    stage
}

fn bench_deesser(c: &mut Criterion) {
    let master = StereoBuffer::synthesized(FRAMES_PER_ITER);

    let mut group = c.benchmark_group("bench_deesser/block_matrix");
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

criterion_group!(benches, bench_deesser);
criterion_main!(benches);
