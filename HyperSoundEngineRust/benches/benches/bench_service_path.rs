//! 服务数据面纯内存路径基准：不启动线程，不打开 WASAPI 设备。

use std::sync::mpsc::sync_channel;
use std::sync::Arc;

use criterion::{
    black_box, criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput,
};
use hse_service::dsp_chain::{deinterleave, interleave, ServiceEngineChain};
use hse_service::sessions::SessionTable;
use hse_service::state::{ServiceEvent, StatsAtomic};
use rtrb::RingBuffer;

const SAMPLE_RATE: f64 = 48_000.0;
const BLOCKS: [usize; 3] = [128, 256, 512];

fn interleaved(block: usize, seed: u32) -> Vec<f32> {
    (0..block * 2)
        .map(|index| {
            let value = (index as u32).wrapping_mul(1_664_525).wrapping_add(seed);
            (value as f32 / u32::MAX as f32) * 0.5 - 0.25
        })
        .collect()
}

fn frame(session_id: u32, seq: u64, samples: &[f32]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(12 + samples.len() * 4);
    frame.extend_from_slice(&session_id.to_le_bytes());
    frame.extend_from_slice(&seq.to_le_bytes());
    for sample in samples {
        frame.extend_from_slice(&sample.to_le_bytes());
    }
    frame
}

fn session_mix_fixture(block: usize) -> (SessionTable, Vec<f32>) {
    let (events, _rx) = sync_channel::<ServiceEvent>(16);
    let table = SessionTable::new(Arc::new(StatsAtomic::default()), events);
    for (owner, seed) in [(1_u64, 11_u32), (2, 29), (3, 47)] {
        let id = table.open(owner).unwrap();
        assert!(table.ingest_frame(&frame(id, 0, &interleaved(block, seed))));
    }
    (table, interleaved(block, 3))
}

fn default_chain(block: usize) -> ServiceEngineChain {
    ServiceEngineChain::build(&serde_json::json!({}), SAMPLE_RATE, block).unwrap()
}

fn bench_service_path(c: &mut Criterion) {
    let mut group = c.benchmark_group("service_path");
    group.sample_size(40);

    for block in BLOCKS {
        group.throughput(Throughput::Elements(block as u64));
        let input = interleaved(block, 7);

        let mut left = vec![0.0_f32; block];
        let mut right = vec![0.0_f32; block];
        group.bench_with_input(BenchmarkId::new("deinterleave", block), &block, |b, _| {
            b.iter(|| {
                deinterleave(black_box(&input), &mut left, &mut right);
                black_box(left[0] + right[0])
            })
        });

        let mut output = vec![0.0_f32; block * 2];
        group.bench_with_input(BenchmarkId::new("interleave", block), &block, |b, _| {
            b.iter(|| {
                interleave(black_box(&left), black_box(&right), &mut output);
                black_box(output[0])
            })
        });

        group.bench_with_input(BenchmarkId::new("session_mix_3", block), &block, |b, _| {
            b.iter_batched(
                || session_mix_fixture(block),
                |(table, mut mix)| {
                    black_box(table.drain_and_mix(&mut mix, block, block));
                    black_box(mix[0])
                },
                BatchSize::SmallInput,
            )
        });

        let mut chain = default_chain(block);
        group.bench_with_input(
            BenchmarkId::new("dsp_default_chain", block),
            &block,
            |b, _| {
                b.iter(|| {
                    left.copy_from_slice(&input[..block]);
                    right.copy_from_slice(&input[block..]);
                    chain.process_planar(&mut left, &mut right);
                    black_box(left[0] + right[0])
                })
            },
        );

        group.bench_with_input(BenchmarkId::new("ring_roundtrip", block), &block, |b, _| {
            let (mut in_prod, mut in_cons) = RingBuffer::<f32>::new(block * 2);
            let (mut out_prod, mut out_cons) = RingBuffer::<f32>::new(block * 2);
            b.iter(|| {
                for &sample in &input {
                    in_prod.push(sample).unwrap();
                }
                for _ in 0..input.len() {
                    out_prod.push(in_cons.pop().unwrap()).unwrap();
                }
                let mut checksum = 0.0_f32;
                for _ in 0..input.len() {
                    checksum += out_cons.pop().unwrap();
                }
                black_box(checksum)
            })
        });

        group.bench_with_input(BenchmarkId::new("combined_block", block), &block, |b, _| {
            let mut chain = default_chain(block);
            let mut planar_l = vec![0.0_f32; block];
            let mut planar_r = vec![0.0_f32; block];
            let mut rendered = vec![0.0_f32; block * 2];
            let (mut in_prod, mut in_cons) = RingBuffer::<f32>::new(block * 2);
            let (mut out_prod, mut out_cons) = RingBuffer::<f32>::new(block * 2);
            b.iter(|| {
                for &sample in &input {
                    in_prod.push(sample).unwrap();
                }
                for sample in &mut rendered {
                    *sample = in_cons.pop().unwrap();
                }
                deinterleave(&rendered, &mut planar_l, &mut planar_r);
                chain.process_planar(&mut planar_l, &mut planar_r);
                interleave(&planar_l, &planar_r, &mut rendered);
                for &sample in &rendered {
                    out_prod.push(sample).unwrap();
                }
                let mut checksum = 0.0_f32;
                for _ in 0..rendered.len() {
                    checksum += out_cons.pop().unwrap();
                }
                black_box(checksum)
            })
        });
    }
    group.finish();
}

criterion_group!(benches, bench_service_path);
criterion_main!(benches);
