use hse_core::engine_chain::{EngineChainParams, EngineChainStage};
use hse_core::Stage;

use crate::param_scan_vector::{ParamScanCase, ParamScanFixture};

#[derive(Debug)]
pub struct ParamScanOutcome {
    pub passed: bool,
    pub passed_cases: usize,
    pub failed_cases: usize,
    pub max_relative_error: f64,
    pub failures: Vec<String>,
}

#[derive(Clone, Copy)]
struct Lcg(u32);

impl Lcg {
    fn next_u32(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        self.0
    }

    fn sample(&mut self) -> f32 {
        (-0.95 + 1.9 * (f64::from(self.next_u32()) / 4_294_967_296.0)) as f32
    }
}

pub fn run_fixture(fixture: &ParamScanFixture) -> ParamScanOutcome {
    let mut outcome = ParamScanOutcome {
        passed: true,
        passed_cases: 0,
        failed_cases: 0,
        max_relative_error: 0.0,
        failures: Vec::new(),
    };
    for case in &fixture.cases {
        match run_case(case, fixture) {
            Ok(max_relative_error) => {
                outcome.passed_cases += 1;
                outcome.max_relative_error = outcome.max_relative_error.max(max_relative_error);
            }
            Err(failure) => {
                outcome.failed_cases += 1;
                outcome.failures.push(failure);
            }
        }
    }
    outcome.passed = outcome.failed_cases == 0;
    outcome
}

fn run_case(case: &ParamScanCase, fixture: &ParamScanFixture) -> Result<f64, String> {
    let params = EngineChainParams::from_overrides(case.sample_rate, &case.overrides)
        .map_err(|err| format!("{} 参数无效：{err}", case.id))?;
    let mut engine = EngineChainStage::from_params(case.sample_rate, params)
        .map_err(|err| format!("{} 构链失败：{err}", case.id))?;
    engine.prepare(case.block_size);

    let mut rng = Lcg(case.input_seed);
    let mut left = vec![0.0_f32; case.frames];
    let mut right = vec![0.0_f32; case.frames];
    for index in 0..case.frames {
        left[index] = rng.sample();
        right[index] = rng.sample();
    }

    for (left_block, right_block) in left
        .chunks_mut(case.block_size)
        .zip(right.chunks_mut(case.block_size))
    {
        engine.set_next_frame_count(left_block.len());
        engine.process(left_block, right_block);
    }

    let mut max_relative_error = 0.0_f64;
    for (channel, got, want_summary) in [
        ("left", left.as_slice(), case.expected_left),
        ("right", right.as_slice(), case.expected_right),
    ] {
        let got_summary = summarize(got);
        for (metric, got_value, want_value) in [
            ("finiteRatio", got_summary.0, want_summary.finite_ratio),
            ("nonZeroRatio", got_summary.1, want_summary.non_zero_ratio),
            ("peakOrder", got_summary.2, want_summary.peak_order),
            ("rmsOrder", got_summary.3, want_summary.rms_order),
        ] {
            let diff = (got_value - want_value).abs();
            let scale = want_value.abs().max(fixture.tolerance_floor);
            let relative_error = diff / scale;
            max_relative_error = max_relative_error.max(relative_error);
            if diff > fixture.tolerance_value * scale {
                return Err(format!(
                    "{} {channel} {metric} 超差：got={got_value:.9e} want={want_value:.9e} relative={relative_error:.3e}",
                    case.id
                ));
            }
        }
    }
    Ok(max_relative_error)
}

fn summarize(samples: &[f32]) -> (f64, f64, f64, f64) {
    let mut finite_count = 0_usize;
    let mut non_zero_count = 0_usize;
    let mut peak_abs = 0.0_f64;
    let mut sum_squares = 0.0_f64;
    for &sample in samples {
        if sample.is_finite() {
            finite_count += 1;
        }
        if sample != 0.0 {
            non_zero_count += 1;
        }
        let sample = f64::from(sample);
        peak_abs = peak_abs.max(sample.abs());
        sum_squares += sample * sample;
    }
    let length = samples.len() as f64;
    let rms = (sum_squares / length).sqrt();
    (
        finite_count as f64 / length,
        non_zero_count as f64 / length,
        magnitude_order(peak_abs),
        magnitude_order(rms),
    )
}

fn magnitude_order(value: f64) -> f64 {
    if value > 0.0 && value.is_finite() {
        value.log10().floor()
    } else {
        0.0
    }
}
