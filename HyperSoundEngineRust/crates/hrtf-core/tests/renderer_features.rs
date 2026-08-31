use hrtf_core::{
    BinauralRenderer, ConvolutionMode, DistanceModel, DistanceParams, HrtfGrid, InterpolationMode,
    ObjectEffects, ObjectInput, RenderProfile, RoomPreset, Vec3,
};

fn grid() -> HrtfGrid {
    let azimuths: Vec<f32> = vec![-157.5, -112.5, -67.5, -22.5, 22.5, 67.5, 112.5, 157.5];
    let elevations: Vec<f32> = vec![-70.0, -35.0, -10.0, 15.0, 45.0, 75.0];
    let hrir_length = 4;
    let mut left = Vec::new();
    let mut right = Vec::new();
    for &elevation in &elevations {
        for &azimuth in &azimuths {
            let azimuth_radians = azimuth.to_radians();
            let elevation_radians = elevation.to_radians();
            left.extend([
                0.4 + 0.1 * azimuth_radians.cos(),
                0.1 * elevation_radians.sin(),
                0.03,
                -0.01,
            ]);
            right.extend([
                0.4 - 0.1 * azimuth_radians.cos(),
                -0.1 * elevation_radians.sin(),
                0.02,
                0.01,
            ]);
        }
    }
    HrtfGrid::new(48_000, azimuths, elevations, hrir_length, left, right).unwrap()
}

fn long_grid() -> HrtfGrid {
    let hrir_length = 193;
    let mut left = vec![0.0; hrir_length];
    let mut right = vec![0.0; hrir_length];
    for tap in 0..hrir_length {
        left[tap] = 0.98_f32.powi(tap as i32) * 0.08;
        right[tap] = if tap == 3 {
            0.2
        } else {
            0.97_f32.powi(tap as i32) * 0.04
        };
    }
    HrtfGrid::new(48_000, vec![0.0], vec![0.0], hrir_length, left, right).unwrap()
}

fn renderer_with_objects(max_objects: usize, max_frames: usize) -> BinauralRenderer {
    let mut renderer = BinauralRenderer::new(
        grid(),
        RenderProfile::LowLatency,
        DistanceModel::Inverse,
        DistanceParams::default(),
    )
    .unwrap();
    renderer.prepare(max_objects, max_frames).unwrap();
    renderer
}

fn renderer(max_frames: usize) -> BinauralRenderer {
    renderer_with_objects(1, max_frames)
}

fn render(renderer: &mut BinauralRenderer, input: &[f32], left: &mut [f32], right: &mut [f32]) {
    renderer
        .process(
            &[ObjectInput {
                slot: 0,
                mono: input,
                azimuth_deg: 17.0,
                elevation_deg: 8.0,
                distance: 1.0,
                gain: 1.0,
            }],
            left,
            right,
            input.len(),
        )
        .unwrap();
}

#[test]
fn spherical_mode_runs_through_the_renderer_and_differs_from_nearest() {
    let input = [1.0, 0.0, 0.0, 0.0, 0.25, -0.5, 0.0, 0.0];
    let mut nearest = renderer(input.len());
    let mut spherical = renderer(input.len());
    spherical
        .set_interpolation_mode(InterpolationMode::Spherical)
        .unwrap();
    let mut nearest_left = [0.0; 8];
    let mut nearest_right = [0.0; 8];
    let mut spherical_left = [0.0; 8];
    let mut spherical_right = [0.0; 8];
    render(&mut nearest, &input, &mut nearest_left, &mut nearest_right);
    render(
        &mut spherical,
        &input,
        &mut spherical_left,
        &mut spherical_right,
    );
    assert_ne!(spherical_left, nearest_left);
    assert_ne!(spherical_right, nearest_right);
}

#[test]
fn partitioned_matches_time_with_reported_delay_across_profiles_and_blocks() {
    let mut input = vec![0.0; 701];
    for (index, sample) in input.iter_mut().enumerate() {
        *sample = ((index as f32 * 0.173).sin() + (index as f32 * 0.037).cos()) * 0.2;
    }
    for profile in [RenderProfile::LowLatency, RenderProfile::Compatibility] {
        let partition = profile.partition_size();
        let mut time = BinauralRenderer::new(
            long_grid(),
            profile,
            DistanceModel::Inverse,
            DistanceParams::default(),
        )
        .unwrap();
        time.prepare(1, input.len()).unwrap();
        let mut partitioned = BinauralRenderer::new(
            long_grid(),
            profile,
            DistanceModel::Inverse,
            DistanceParams::default(),
        )
        .unwrap();
        partitioned.prepare(1, 128).unwrap();
        partitioned
            .set_convolution_mode(ConvolutionMode::Partitioned)
            .unwrap();
        assert_eq!(partitioned.latency_samples(), partition);

        let mut expected_left = vec![0.0; input.len()];
        let mut expected_right = vec![0.0; input.len()];
        render(&mut time, &input, &mut expected_left, &mut expected_right);
        let mut actual_left = vec![0.0; input.len() + partition];
        let mut actual_right = vec![0.0; input.len() + partition];
        let mut start = 0;
        for frames in [17, 113, 31, 64, 7, 109, 53, 113, 113, 81] {
            if start >= input.len() {
                break;
            }
            let end = (start + frames).min(input.len());
            render(
                &mut partitioned,
                &input[start..end],
                &mut actual_left[start..end],
                &mut actual_right[start..end],
            );
            start = end;
        }
        let silence = vec![0.0; partition];
        render(
            &mut partitioned,
            &silence,
            &mut actual_left[input.len()..],
            &mut actual_right[input.len()..],
        );
        for frame in 0..input.len() {
            let left_error = (actual_left[frame + partition] - expected_left[frame]).abs();
            let right_error = (actual_right[frame + partition] - expected_right[frame]).abs();
            assert!(left_error <= 2.0e-5, "left frame {frame}: {left_error}");
            assert!(right_error <= 2.0e-5, "right frame {frame}: {right_error}");
        }
        assert!(actual_left[..partition]
            .iter()
            .all(|sample| sample.abs() < 1.0e-6));
        assert!(actual_right[..partition]
            .iter()
            .all(|sample| sample.abs() < 1.0e-6));
    }
}

#[test]
fn partitioned_reset_and_mode_switch_clear_state() {
    let input = vec![0.25; 128];
    let silence = vec![0.0; 128];
    let mut renderer = renderer(128);
    renderer
        .set_convolution_mode(ConvolutionMode::Partitioned)
        .unwrap();
    let mut left = vec![0.0; 128];
    let mut right = vec![0.0; 128];
    render(&mut renderer, &input, &mut left, &mut right);
    renderer.reset();
    render(&mut renderer, &silence, &mut left, &mut right);
    assert!(left.iter().all(|sample| *sample == 0.0));
    assert!(right.iter().all(|sample| *sample == 0.0));

    render(&mut renderer, &input, &mut left, &mut right);
    renderer
        .set_convolution_mode(ConvolutionMode::Time)
        .unwrap();
    render(&mut renderer, &silence, &mut left, &mut right);
    assert!(left.iter().all(|sample| *sample == 0.0));
    assert!(right.iter().all(|sample| *sample == 0.0));
}

#[test]
fn low_latency_partition_is_below_five_ms_at_supported_rates() {
    for sample_rate in [44_100_u32, 48_000, 96_000] {
        assert!(RenderProfile::LowLatency.partition_size() * 1_000 < sample_rate as usize * 5);
    }
}

#[test]
fn room_off_and_zero_amount_are_bit_identical_bypasses() {
    let input = [1.0, 0.25, -0.5, 0.0, 0.75, -0.25, 0.1, 0.0];
    let mut off = renderer(input.len());
    let mut zero = renderer(input.len());
    zero.set_room_preset(Some(RoomPreset::Studio)).unwrap();
    zero.set_room_amount(0.0).unwrap();
    let mut off_left = [0.0; 8];
    let mut off_right = [0.0; 8];
    let mut zero_left = [0.0; 8];
    let mut zero_right = [0.0; 8];
    render(&mut off, &input, &mut off_left, &mut off_right);
    render(&mut zero, &input, &mut zero_left, &mut zero_right);
    assert_eq!(zero_left, off_left);
    assert_eq!(zero_right, off_right);
}

#[test]
fn room_processing_is_block_invariant_and_reset_reproducible() {
    let mut input = [0.0; 640];
    input[0] = 1.0;
    input[17] = -0.25;
    let mut whole = renderer(input.len());
    whole.set_room_preset(Some(RoomPreset::Studio)).unwrap();
    whole.set_room_amount(0.5).unwrap();
    let mut split = renderer(128);
    split.set_room_preset(Some(RoomPreset::Studio)).unwrap();
    split.set_room_amount(0.5).unwrap();
    let mut whole_left = [0.0; 640];
    let mut whole_right = [0.0; 640];
    render(&mut whole, &input, &mut whole_left, &mut whole_right);
    let mut split_left = [0.0; 640];
    let mut split_right = [0.0; 640];
    for start in (0..input.len()).step_by(128) {
        let end = (start + 128).min(input.len());
        render(
            &mut split,
            &input[start..end],
            &mut split_left[start..end],
            &mut split_right[start..end],
        );
    }
    assert_eq!(split_left, whole_left);
    assert_eq!(split_right, whole_right);

    whole.reset();
    let mut reset_left = [0.0; 640];
    let mut reset_right = [0.0; 640];
    render(&mut whole, &input, &mut reset_left, &mut reset_right);
    assert_eq!(reset_left, whole_left);
    assert_eq!(reset_right, whole_right);
    assert!(whole_left.iter().skip(180).any(|sample| *sample != 0.0));
}

#[test]
fn neutral_object_effects_are_bit_identical_to_legacy_rendering() {
    let input = [1.0, -0.5, 0.25, 0.0, 0.75, -0.25, 0.1, 0.0];
    let object = ObjectInput {
        slot: 0,
        mono: &input,
        azimuth_deg: 17.0,
        elevation_deg: 8.0,
        distance: 1.0,
        gain: 1.0,
    };
    let mut legacy = renderer(input.len());
    let mut extended = renderer(input.len());
    let mut legacy_left = [0.0; 8];
    let mut legacy_right = [0.0; 8];
    let mut extended_left = [0.0; 8];
    let mut extended_right = [0.0; 8];
    legacy
        .process(&[object], &mut legacy_left, &mut legacy_right, input.len())
        .unwrap();
    extended
        .process_with_effects(
            &[object],
            &[ObjectEffects::default()],
            &mut extended_left,
            &mut extended_right,
            input.len(),
        )
        .unwrap();
    assert_eq!(extended_left, legacy_left);
    assert_eq!(extended_right, legacy_right);
}

#[test]
fn effects_apply_non_unity_gain_once_with_velocity_and_occlusion() {
    let input: Vec<f32> = (0..64)
        .map(|index| ((index as f32 * 0.17).sin() + 0.25) * 0.4)
        .collect();
    for configure in [
        |renderer: &mut BinauralRenderer| {
            renderer
                .set_listener_velocity(Some(Vec3 {
                    x: 0.0,
                    y: 0.0,
                    z: 12.0,
                }))
                .unwrap();
        },
        |renderer: &mut BinauralRenderer| renderer.set_occlusion(0.6).unwrap(),
    ] {
        let render_gain = |gain: f32| {
            let mut renderer = renderer(input.len());
            configure(&mut renderer);
            let mut left = vec![0.0; input.len()];
            let mut right = vec![0.0; input.len()];
            renderer
                .process_with_effects(
                    &[ObjectInput {
                        slot: 0,
                        mono: &input,
                        azimuth_deg: 25.0,
                        elevation_deg: 10.0,
                        distance: 2.0,
                        gain,
                    }],
                    &[ObjectEffects::default()],
                    &mut left,
                    &mut right,
                    input.len(),
                )
                .unwrap();
            (left, right)
        };
        let unity = render_gain(1.0);
        let half = render_gain(0.5);
        for (actual, reference) in half
            .0
            .iter()
            .zip(&unity.0)
            .chain(half.1.iter().zip(&unity.1))
        {
            assert!((*actual - *reference * 0.5).abs() <= 1.0e-6);
        }
    }
}

#[test]
fn doppler_occlusion_and_size_are_stateful_resettable_and_short_block_safe() {
    let input: Vec<f32> = (0..96)
        .map(|index| ((index as f32 * 0.19).sin() * 0.5) + 0.1)
        .collect();
    let run = |renderer: &mut BinauralRenderer, chunks: &[usize]| {
        let mut left = vec![0.0; input.len()];
        let mut right = vec![0.0; input.len()];
        let mut start = 0;
        for &length in chunks {
            let end = (start + length).min(input.len());
            let object = ObjectInput {
                slot: 0,
                mono: &input[start..end],
                azimuth_deg: 25.0,
                elevation_deg: 10.0,
                distance: 2.0,
                gain: 0.8,
            };
            renderer
                .process_with_effects(
                    &[object],
                    &[ObjectEffects {
                        size: 0.75,
                        spread_slot: Some(1),
                    }],
                    &mut left[start..end],
                    &mut right[start..end],
                    end - start,
                )
                .unwrap();
            start = end;
        }
        assert_eq!(start, input.len());
        (left, right)
    };

    let configure = |renderer: &mut BinauralRenderer| {
        renderer
            .set_listener_velocity(Some(Vec3 {
                x: 0.0,
                y: 0.0,
                z: 12.0,
            }))
            .unwrap();
        renderer.set_occlusion(0.6).unwrap();
    };
    let mut whole = renderer_with_objects(2, input.len());
    configure(&mut whole);
    let expected = run(&mut whole, &[input.len()]);

    let mut split = renderer_with_objects(2, 31);
    configure(&mut split);
    let actual = run(&mut split, &[7, 31, 5, 17, 29, 7]);
    assert_eq!(actual, expected);

    split.reset();
    assert_eq!(run(&mut split, &[7, 31, 5, 17, 29, 7]), expected);

    let mut neutral = renderer(input.len());
    let mut neutral_left = vec![0.0; input.len()];
    let mut neutral_right = vec![0.0; input.len()];
    render(&mut neutral, &input, &mut neutral_left, &mut neutral_right);
    assert_ne!(expected.0, neutral_left);
    assert_ne!(expected.1, neutral_right);
}

#[test]
fn object_effects_reject_reused_spread_slots() {
    let input = [0.25; 4];
    let objects = [
        ObjectInput {
            slot: 0,
            mono: &input,
            azimuth_deg: -20.0,
            elevation_deg: 0.0,
            distance: 1.0,
            gain: 1.0,
        },
        ObjectInput {
            slot: 1,
            mono: &input,
            azimuth_deg: 20.0,
            elevation_deg: 0.0,
            distance: 1.0,
            gain: 1.0,
        },
    ];
    let effects = [
        ObjectEffects {
            size: 0.5,
            spread_slot: Some(2),
        },
        ObjectEffects {
            size: 0.5,
            spread_slot: Some(2),
        },
    ];
    let mut renderer = renderer_with_objects(4, input.len());
    let mut left = [0.0; 4];
    let mut right = [0.0; 4];
    assert_eq!(
        renderer.process_with_effects(&objects, &effects, &mut left, &mut right, input.len()),
        Err(hrtf_core::ProcessError::DuplicateSlot { slot: 2 })
    );
}
