//! parity_reverb_simple —— ReverbSimpleStage 的 criterion 基准雏形。
//!
//! 参数取引擎默认快照的 hall 典型组合（`specs/dsp/reverb-simple.md`
//! §3.1 默认列 × §3.2 hall 行）：roomSize/damping 用户值 0.5 即类型基准本身
//! （等效 effRoom=0.7 / effDamp=0.4）、wet 0.3 / dry 0.7 / preDelayMs 0 /
//! width 1 / type=hall。8 梳状 + 4 全通的逐样本反馈递推是该模块的成本主体，
//! 真实算法落地前数字为占位直通基线。

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use hse_benches::{push_blocks, StereoBuffer, FRAMES_PER_ITER, MAIN_BLOCK_FRAMES, SAMPLE_RATE_HZ};
use hse_core::reverb_simple::{ReverbSimpleParams, ReverbSimpleStage};
use hse_core::Stage;

fn ready_stage(max_block_frames: usize) -> ReverbSimpleStage {
    let mut stage = ReverbSimpleStage::from_params(
        SAMPLE_RATE_HZ,
        ReverbSimpleParams {
            room_size: 0.5,
            damping: 0.5,
            wet: 0.3,
            dry: 0.7,
            pre_delay_ms: 0.0,
            width: 1.0,
            reverb_type: "hall".to_string(),
        },
    )
    .expect("基准用混响参数合法，构造不应失败");
    stage.prepare(max_block_frames);
    stage
}

fn bench_parity_reverb_simple(c: &mut Criterion) {
    let master = StereoBuffer::synthesized(FRAMES_PER_ITER);

    let mut group = c.benchmark_group("parity_reverb_simple/main");
    group.sample_size(40);
    group.throughput(Throughput::Elements(FRAMES_PER_ITER as u64));
    group.bench_function("hall_typical_block128", |b| {
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
    group.finish();
}

criterion_group!(benches, bench_parity_reverb_simple);
criterion_main!(benches);
