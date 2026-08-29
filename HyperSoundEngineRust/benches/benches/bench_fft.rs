//! bench_fft —— FftStage（原位复 FFT，L=Re 平面 / R=Im 平面）的 criterion 基准。
//!
//! 尺寸矩阵 1024/2048/4096/8192（冻结向量 fft 域同集合）：块长 = N（向量
//! 契约固定 blockSize = frames = N，单块驱动一次变换）。每次迭代总帧数恒为
//! 32768，组内吞吐可直接横向对比"每样本 FFT 成本随尺寸的增长"。twiddle 表
//! 在 prepare（计时区外）预建，process 内复用；右声道平面用 LCG 噪声与
//! 左平面的合成正弦区分，保证蝶形运算输入非退化。

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use hse_benches::{fill_lcg_noise, push_blocks, StereoBuffer, FRAMES_PER_ITER};
use hse_core::fft::FftStage;
use hse_core::Stage;

const FFT_SIZES: [usize; 4] = [1024, 2048, 4096, 8192];

fn bench_fft(c: &mut Criterion) {
    // 母带：Re 平面用合成正弦叠加，Im 平面用固定种子 LCG 噪声（均确定性）。
    let mut master = StereoBuffer::synthesized(FRAMES_PER_ITER);
    let mut im_noise_l = vec![0.0_f32; FRAMES_PER_ITER];
    let mut im_noise_r = vec![0.0_f32; FRAMES_PER_ITER];
    fill_lcg_noise(&mut im_noise_l, &mut im_noise_r, 2024, 2025, 0.9);
    master.right.copy_from_slice(&im_noise_r);
    let _ = im_noise_l;

    let mut group = c.benchmark_group("bench_fft/sizes");
    group.sample_size(40);
    group.warm_up_time(std::time::Duration::from_secs(1));
    group.measurement_time(std::time::Duration::from_secs(3));
    group.throughput(Throughput::Elements(FRAMES_PER_ITER as u64));
    for n in FFT_SIZES {
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, &n| {
            let mut stage = FftStage::new(false);
            stage.prepare(n);
            let mut work = StereoBuffer::zeroed(FRAMES_PER_ITER);
            b.iter(|| {
                let acc = push_blocks(&mut stage, &master, &mut work, n, FRAMES_PER_ITER / n);
                black_box(acc)
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_fft);
criterion_main!(benches);
