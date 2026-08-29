//! bench_wav —— wav::encode_wav / decode_wav（WAV 编解码工具）的 criterion 基准。
//!
//! 场景：48kHz 立体声 × 32768 帧（≈0.68 s，与其余模块基准同口径）确定性合成
//! 信号，位深 16-bit PCM（TS 缺省）与 32-bit float 两档。编码每次产出新字节流、
//! 解码每次产出新声道向量（分配语义）——与 midi/share 同属离线 I/O 工具路径，
//! 量化其量级供导出链路参考，不适用音频回调实时铁律。吞吐按输出/输入字节数计
//! （PCM16: 131156 B / Float32: 262196 B，含 44 字节头）。

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use hse_benches::{StereoBuffer, FRAMES_PER_ITER};
use hse_core::wav::{decode_wav, encode_wav, WavBitDepth, WavEncodeOptions};

const WAV_SAMPLE_RATE: u32 = 48_000;

fn encoded_bytes(bit_depth: WavBitDepth) -> Vec<u8> {
    let master = StereoBuffer::synthesized(FRAMES_PER_ITER);
    encode_wav(
        &[&master.left, &master.right],
        WAV_SAMPLE_RATE,
        &WavEncodeOptions { bit_depth },
    )
    .expect("基准用 WAV 编码不应失败")
}

fn bench_wav(c: &mut Criterion) {
    let master = StereoBuffer::synthesized(FRAMES_PER_ITER);
    let channels: [&[f32]; 2] = [&master.left, &master.right];

    let mut group = c.benchmark_group("bench_wav/1s_stereo");
    group.sample_size(40);
    group.warm_up_time(std::time::Duration::from_secs(1));
    group.measurement_time(std::time::Duration::from_secs(3));

    for (label, depth, bytes) in [
        ("pcm16", WavBitDepth::Pcm16, 2 * FRAMES_PER_ITER * 2 + 44),
        ("float32", WavBitDepth::Float32, 2 * FRAMES_PER_ITER * 4 + 44),
    ] {
        group.throughput(Throughput::Bytes(bytes as u64));
        group.bench_with_input(BenchmarkId::new("encode", label), &depth, |b, &depth| {
            b.iter(|| {
                let out = encode_wav(
                    black_box(channels.as_slice()),
                    WAV_SAMPLE_RATE,
                    &WavEncodeOptions { bit_depth: depth },
                )
                .expect("编码不应失败");
                black_box(out)
            });
        });
        let bytes_prebuilt = encoded_bytes(depth);
        group.bench_with_input(BenchmarkId::new("decode", label), &bytes_prebuilt, |b, bytes| {
            b.iter(|| {
                let data = decode_wav(black_box(bytes.as_slice())).expect("解码不应失败");
                black_box(data)
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_wav);
criterion_main!(benches);
