//! HseStretch 块窗映射的参数域基准。

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use hse_benches::{StereoBuffer, SAMPLE_RATE_HZ};
use hse_core::{
    hse_stretch::{HseStretchParams, HseStretchStage},
    Stage,
};

const BLOCK: usize = 2_048;

fn bench_hse_stretch(c: &mut Criterion) {
    let master = StereoBuffer::synthesized(BLOCK);
    let cases = [
        ("rate_min_pitch_down", 0.1, -36.0),
        ("rate_unity_pitch_unity", 1.0, 0.0),
        ("rate_double_pitch_up", 2.0, 12.0),
        ("rate_max_pitch_up", 8.0, 36.0),
    ];
    let mut group = c.benchmark_group("bench_hse_stretch/parameter_domain");
    group.sample_size(20);
    group.warm_up_time(std::time::Duration::from_secs(1));
    group.measurement_time(std::time::Duration::from_secs(3));
    group.throughput(Throughput::Elements(BLOCK as u64));

    for (label, rate, semitones) in cases {
        group.bench_function(BenchmarkId::from_parameter(label), |b| {
            let mut stage =
                HseStretchStage::new(SAMPLE_RATE_HZ, 2.0, HseStretchParams { rate, semitones })
                    .expect("基准参数必须合法");
            stage.prepare(BLOCK);
            let mut work = StereoBuffer::zeroed(BLOCK);
            b.iter(|| {
                work.left.copy_from_slice(&master.left);
                work.right.copy_from_slice(&master.right);
                stage.reset();
                stage.process(&mut work.left, &mut work.right);
                black_box((work.left[BLOCK / 2], work.right[BLOCK / 2]))
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_hse_stretch);
criterion_main!(benches);
