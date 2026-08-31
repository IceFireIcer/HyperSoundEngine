use hrtf_core::{
    BinauralRenderer, ConvolutionMode, DistanceModel, DistanceParams, HrtfGrid, InterpolationMode,
    ObjectInput, RenderProfile, RoomPreset,
};
use std::{hint::black_box, time::Instant};

const FRAMES: usize = 128;

#[derive(Clone, Copy)]
struct Scenario {
    sample_rate: u32,
    hrir_length: usize,
    objects: usize,
    mode: ConvolutionMode,
    room: bool,
    iterations: usize,
}

fn grid(sample_rate: u32, hrir_length: usize) -> HrtfGrid {
    let azimuths = vec![-90.0, 0.0, 90.0];
    let elevations = vec![0.0];
    let mut left = vec![0.0; azimuths.len() * hrir_length];
    let mut right = vec![0.0; azimuths.len() * hrir_length];
    for direction in 0..azimuths.len() {
        for tap in 0..hrir_length {
            let base = direction * hrir_length + tap;
            left[base] = 0.98_f32.powi(tap as i32) * (0.08 + direction as f32 * 0.01);
            right[base] = 0.97_f32.powi(tap as i32) * (0.1 - direction as f32 * 0.01);
        }
    }
    HrtfGrid::new(sample_rate, azimuths, elevations, hrir_length, left, right).unwrap()
}

fn run(scenario: Scenario) {
    let mut renderer = BinauralRenderer::new(
        grid(scenario.sample_rate, scenario.hrir_length),
        RenderProfile::LowLatency,
        DistanceModel::Inverse,
        DistanceParams::default(),
    )
    .unwrap();
    renderer.prepare(scenario.objects, FRAMES).unwrap();
    renderer.set_convolution_mode(scenario.mode).unwrap();
    renderer
        .set_interpolation_mode(InterpolationMode::Nearest)
        .unwrap();
    if scenario.room {
        renderer.set_room_preset(Some(RoomPreset::Hall)).unwrap();
        renderer.set_room_amount(0.35).unwrap();
    }

    let inputs = vec![vec![0.25; FRAMES]; scenario.objects];
    let objects: Vec<_> = inputs
        .iter()
        .enumerate()
        .map(|(index, mono)| ObjectInput {
            slot: index,
            mono,
            azimuth_deg: [-90.0, 0.0, 90.0][index % 3],
            elevation_deg: 0.0,
            distance: 1.0 + index as f32 * 0.05,
            gain: 1.0,
        })
        .collect();
    let mut output_left = vec![0.0; FRAMES];
    let mut output_right = vec![0.0; FRAMES];

    for _ in 0..32 {
        renderer
            .process(&objects, &mut output_left, &mut output_right, FRAMES)
            .unwrap();
    }
    let start = Instant::now();
    for _ in 0..scenario.iterations {
        renderer
            .process(
                black_box(&objects),
                black_box(&mut output_left),
                black_box(&mut output_right),
                FRAMES,
            )
            .unwrap();
    }
    let elapsed = start.elapsed();
    let processed_frames = scenario.iterations * FRAMES;
    let realtime_seconds = processed_frames as f64 / scenario.sample_rate as f64;
    let cpu_percent = elapsed.as_secs_f64() / realtime_seconds * 100.0;
    println!(
        "fs={} hrir={} mode={:?} room={} objects={}: {:.2} ns/frame, {:.3}% realtime",
        scenario.sample_rate,
        scenario.hrir_length,
        scenario.mode,
        scenario.room,
        scenario.objects,
        elapsed.as_nanos() as f64 / processed_frames as f64,
        cpu_percent,
    );
    assert!(output_left
        .iter()
        .chain(&output_right)
        .all(|sample| sample.is_finite()));
    assert!(
        cpu_percent < 100.0,
        "renderer smoke exceeded realtime on this machine: {cpu_percent:.2}%"
    );
}

fn main() {
    for mode in [ConvolutionMode::Time, ConvolutionMode::Partitioned] {
        for objects in [1usize, 8, 16, 32, 64] {
            run(Scenario {
                sample_rate: 48_000,
                hrir_length: 256,
                objects,
                mode,
                room: false,
                iterations: if mode == ConvolutionMode::Time {
                    80
                } else {
                    300
                },
            });
        }
    }

    for sample_rate in [44_100, 48_000, 96_000] {
        run(Scenario {
            sample_rate,
            hrir_length: 512,
            objects: 64,
            mode: ConvolutionMode::Partitioned,
            room: true,
            iterations: 120,
        });
    }
}
