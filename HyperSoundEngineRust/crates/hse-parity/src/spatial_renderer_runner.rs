use hrtf_core::{
    air_absorption_coefficient, BinauralRenderer, DistanceModel, DistanceParams, HrtfGrid,
    NearestIndex, RenderProfile, RoomPreset,
};

use crate::spatial_renderer_vector::{
    DistanceModelSpec, RendererCase, RendererFixture, RoomModeSpec,
};

#[derive(Debug)]
pub struct RendererOutcome {
    pub passed: bool,
    pub checked: usize,
    pub passed_cases: usize,
    pub failures: Vec<String>,
    pub max_abs_deviation: f64,
}

pub fn run_fixture(fixture: &RendererFixture) -> RendererOutcome {
    let grid = match build_grid(fixture) {
        Ok(grid) => grid,
        Err(reason) => return failed_outcome(reason),
    };
    let mut failures = Vec::new();
    let mut passed_cases = 0;
    let mut max_abs_deviation = 0.0_f64;

    for case in &fixture.nearest_cases {
        let before = failures.len();
        let index = grid.nearest_index(case.azimuth_deg, case.elevation_deg);
        if index.azimuth != case.azimuth_index || index.elevation != case.elevation_index {
            failures.push(format!(
                "{}.index: got=({}, {}) want=({}, {})",
                case.id, index.azimuth, index.elevation, case.azimuth_index, case.elevation_index
            ));
        }
        let hrir = grid.hrir(NearestIndex {
            azimuth: case.azimuth_index,
            elevation: case.elevation_index,
        });
        compare_slice(
            &case.id,
            "left",
            hrir.left,
            &case.left,
            fixture,
            &mut max_abs_deviation,
            &mut failures,
        );
        compare_slice(
            &case.id,
            "right",
            hrir.right,
            &case.right,
            fixture,
            &mut max_abs_deviation,
            &mut failures,
        );
        if failures.len() == before {
            passed_cases += 1;
        }
    }

    let params = DistanceParams {
        reference_distance: fixture.distance_params.reference_distance,
        maximum_distance: fixture.distance_params.maximum_distance,
        rolloff_factor: fixture.distance_params.rolloff_factor,
    };
    for case in &fixture.distance_cases {
        let before = failures.len();
        let got_gain = rust_model(case.model).gain(case.distance, params).unwrap();
        compare_value(
            &case.id,
            "gain",
            got_gain,
            case.expected_gain,
            fixture,
            &mut max_abs_deviation,
            &mut failures,
        );
        let got_air =
            air_absorption_coefficient(fixture.grid.sample_rate as f32, case.distance).unwrap();
        compare_value(
            &case.id,
            "air",
            got_air,
            case.expected_air_coefficient,
            fixture,
            &mut max_abs_deviation,
            &mut failures,
        );
        if failures.len() == before {
            passed_cases += 1;
        }
    }

    for case in &fixture.renderer_cases {
        let before = failures.len();
        match render_case(fixture, case) {
            Ok((left, right)) => {
                compare_slice(
                    &case.id,
                    "left",
                    &left,
                    &case.expected_left,
                    fixture,
                    &mut max_abs_deviation,
                    &mut failures,
                );
                compare_slice(
                    &case.id,
                    "right",
                    &right,
                    &case.expected_right,
                    fixture,
                    &mut max_abs_deviation,
                    &mut failures,
                );
                if case.id == "delta-right-asymmetric" && left == right {
                    failures.push(format!("{}: 左右输出不应相同", case.id));
                }
            }
            Err(reason) => failures.push(format!("{}: {reason}", case.id)),
        }
        if failures.len() == before {
            passed_cases += 1;
        }
    }

    let checked =
        fixture.nearest_cases.len() + fixture.distance_cases.len() + fixture.renderer_cases.len();
    RendererOutcome {
        passed: failures.is_empty(),
        checked,
        passed_cases,
        failures,
        max_abs_deviation,
    }
}

fn build_grid(fixture: &RendererFixture) -> Result<HrtfGrid, String> {
    let mut left = Vec::with_capacity(fixture.grid.directions.len() * fixture.grid.hrir_length);
    let mut right = Vec::with_capacity(left.capacity());
    for (index, direction) in fixture.grid.directions.iter().enumerate() {
        let expected_azimuth = fixture.grid.azimuths[index % fixture.grid.azimuths.len()];
        let expected_elevation = fixture.grid.elevations[index / fixture.grid.azimuths.len()];
        if direction.azimuth_deg != expected_azimuth
            || direction.elevation_deg != expected_elevation
        {
            return Err(format!(
                "grid direction {index} 不符合 elevation-major 顺序"
            ));
        }
        left.extend_from_slice(&direction.left);
        right.extend_from_slice(&direction.right);
    }
    HrtfGrid::new(
        fixture.grid.sample_rate,
        fixture.grid.azimuths.clone(),
        fixture.grid.elevations.clone(),
        fixture.grid.hrir_length,
        left,
        right,
    )
    .map_err(|error| error.to_string())
}

fn render_case(
    fixture: &RendererFixture,
    case: &RendererCase,
) -> Result<(Vec<f32>, Vec<f32>), String> {
    let params = DistanceParams {
        reference_distance: fixture.distance_params.reference_distance,
        maximum_distance: fixture.distance_params.maximum_distance,
        rolloff_factor: fixture.distance_params.rolloff_factor,
    };
    let max_frames = *case.block_sizes.iter().max().ok_or("blockSizes 为空")?;
    if case.input_stride < max_frames {
        return Err("inputStride 小于最大块长".into());
    }
    let mut renderer = BinauralRenderer::new(
        build_grid(fixture)?,
        RenderProfile::LowLatency,
        rust_model(case.distance_model),
        params,
    )
    .map_err(|error| error.to_string())?;
    if case.room_mode == RoomModeSpec::ConfiguredZero {
        renderer
            .set_room_preset(Some(RoomPreset::Studio))
            .map_err(|error| error.to_string())?;
        renderer
            .set_room_amount(0.0)
            .map_err(|error| error.to_string())?;
    }
    renderer
        .prepare(1, max_frames)
        .map_err(|error| error.to_string())?;
    let first = render_blocks(&mut renderer, case)?;
    if case.reset_replay {
        renderer.reset();
        let replay = render_blocks(&mut renderer, case)?;
        if replay != first {
            return Err("reset 后重放未复现初始输出".into());
        }
    }
    Ok(first)
}

fn render_blocks(
    renderer: &mut BinauralRenderer,
    case: &RendererCase,
) -> Result<(Vec<f32>, Vec<f32>), String> {
    let mut left = vec![0.0; case.input.len()];
    let mut right = vec![0.0; case.input.len()];
    let params = [
        case.azimuth_deg,
        case.elevation_deg,
        case.distance,
        case.gain,
    ];
    let mut offset = 0;
    for block_size in &case.block_sizes {
        let mut planar = vec![0.0; case.input_stride];
        planar[..*block_size].copy_from_slice(&case.input[offset..offset + block_size]);
        renderer
            .process_planar(
                &planar,
                case.input_stride,
                &case.object_slots,
                &params,
                1,
                &mut left[offset..offset + block_size],
                &mut right[offset..offset + block_size],
                *block_size,
            )
            .map_err(|error| error.to_string())?;
        offset += block_size;
    }
    Ok((left, right))
}

fn rust_model(model: DistanceModelSpec) -> DistanceModel {
    match model {
        DistanceModelSpec::Inverse => DistanceModel::Inverse,
        DistanceModelSpec::Linear => DistanceModel::Linear,
        DistanceModelSpec::Exponential => DistanceModel::Exponential,
    }
}

fn compare_slice(
    id: &str,
    field: &str,
    got: &[f32],
    want: &[f32],
    fixture: &RendererFixture,
    max_abs: &mut f64,
    failures: &mut Vec<String>,
) {
    if got.len() != want.len() {
        failures.push(format!(
            "{id}.{field}: 长度 got={} want={}",
            got.len(),
            want.len()
        ));
        return;
    }
    for (index, (&got, &want)) in got.iter().zip(want).enumerate() {
        if !within(got, want, fixture, max_abs) {
            failures.push(format!(
                "{id}.{field}[{index}]: got={got:.9e} want={want:.9e}"
            ));
            return;
        }
    }
}

fn compare_value(
    id: &str,
    field: &str,
    got: f32,
    want: f32,
    fixture: &RendererFixture,
    max_abs: &mut f64,
    failures: &mut Vec<String>,
) {
    if !within(got, want, fixture, max_abs) {
        failures.push(format!("{id}.{field}: got={got:.9e} want={want:.9e}"));
    }
}

fn within(got: f32, want: f32, fixture: &RendererFixture, max_abs: &mut f64) -> bool {
    let deviation = f64::from((got - want).abs());
    *max_abs = max_abs.max(deviation);
    got.is_finite()
        && deviation <= fixture.tolerance_value * f64::from(want.abs()).max(fixture.tolerance_floor)
}

fn failed_outcome(reason: String) -> RendererOutcome {
    RendererOutcome {
        passed: false,
        checked: 0,
        passed_cases: 0,
        failures: vec![reason],
        max_abs_deviation: 0.0,
    }
}
