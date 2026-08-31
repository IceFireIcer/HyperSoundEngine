//! LufsMeter 计量路径的采样率与块长基准。

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use hse_benches::StereoBuffer;
use hse_core::lufs_meter::LufsMeter;

const FRAMES: usize = 48_000;

fn bench_lufs_meter(c: &mut Criterion) {
    let master = StereoBuffer::lcg_noise(FRAMES, 0x1234_5678, 0x9abc_def0, 0.5);
    let cases = [
        (44_100.0, 128),
        (48_000.0, 128),
        (48_000.0, 512),
        (96_000.0, 512),
    ];
    let mut group = c.benchmark_group("bench_lufs_meter/sample_rate_block_domain");
    group.sample_size(30);
    group.warm_up_time(std::time::Duration::from_secs(1));
    group.measurement_time(std::time::Duration::from_secs(3));
    group.throughput(Throughput::Elements(FRAMES as u64));

    for (sample_rate, block) in cases {
        group.bench_function(
            BenchmarkId::new(format!("{}hz", sample_rate as u32), block),
            |b| {
                let mut meter = LufsMeter::new(sample_rate).expect("基准采样率必须合法");
                b.iter(|| {
                    meter.reset();
                    for (left, right) in master.left.chunks(block).zip(master.right.chunks(block)) {
                        meter.process_stereo(left, right);
                    }
                    black_box((
                        meter.get_integrated_lufs(),
                        meter.get_momentary_lufs(),
                        meter.get_peak_db(),
                        meter.get_true_peak_db(),
                    ))
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_lufs_meter);
criterion_main!(benches);
