//! Control-path loading of user-provided SimpleFreeFieldHRIR SOFA data.

use std::{error::Error, fmt, path::Path};

use sofar::reader::{Filter, OpenOptions, Sofar};

use crate::{GridError, HrtfGrid};

const SAMPLE_RATE_TOLERANCE_HZ: f32 = 0.1;
const INTEGER_DELAY_TOLERANCE_SAMPLES: f64 = 1.0e-4;
const SUPPORTED_SAMPLE_RATES: [u32; 3] = [44_100, 48_000, 96_000];
const SINC_HALF_WIDTH: i64 = 64;
const KAISER_BETA: f64 = 10.0;
const CUTOFF_MARGIN: f64 = 0.96;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SofaRegularAxis {
    pub minimum_deg: f32,
    pub maximum_deg: f32,
    pub step_deg: f32,
}

impl SofaRegularAxis {
    fn values(self, kind: AxisKind) -> Result<Vec<f32>, SofaError> {
        if !self.minimum_deg.is_finite()
            || !self.maximum_deg.is_finite()
            || !self.step_deg.is_finite()
            || self.step_deg <= 0.0
            || self.maximum_deg < self.minimum_deg
        {
            return Err(SofaError::InvalidAxis { axis: kind.name() });
        }

        let (minimum, maximum, maximum_exclusive) = match kind {
            AxisKind::Azimuth => (-180.0, 180.0, true),
            AxisKind::Elevation => (-90.0, 90.0, false),
        };
        if self.minimum_deg < minimum
            || self.maximum_deg > maximum
            || (maximum_exclusive && self.maximum_deg >= maximum)
        {
            return Err(SofaError::InvalidAxis { axis: kind.name() });
        }

        let span = self.maximum_deg - self.minimum_deg;
        let steps = span / self.step_deg;
        if !steps.is_finite() || steps > usize::MAX as f32 {
            return Err(SofaError::InvalidAxis { axis: kind.name() });
        }
        let rounded_steps = steps.round();
        if (steps - rounded_steps).abs() > 1.0e-4 {
            return Err(SofaError::IrregularAxis { axis: kind.name() });
        }

        let count = (rounded_steps as usize)
            .checked_add(1)
            .ok_or(SofaError::CapacityOverflow)?;
        let mut values = Vec::with_capacity(count);
        for index in 0..count {
            values.push(self.minimum_deg + index as f32 * self.step_deg);
        }
        if let Some(last) = values.last_mut() {
            *last = self.maximum_deg;
        }
        Ok(values)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SofaLookupMode {
    Nearest,
    Interpolated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SofaDelayStrategy {
    /// Prefix each ear's IR with its integer `Data.Delay` value.
    Embed { maximum_samples: usize },
    /// Accept only SOFA data whose queried filters have zero delay.
    RejectNonZero,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SofaGridOptions {
    pub sample_rate: u32,
    pub azimuth: SofaRegularAxis,
    pub elevation: SofaRegularAxis,
    pub lookup: SofaLookupMode,
    pub delay: SofaDelayStrategy,
}

impl Default for SofaGridOptions {
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            azimuth: SofaRegularAxis {
                minimum_deg: -180.0,
                maximum_deg: 175.0,
                step_deg: 5.0,
            },
            elevation: SofaRegularAxis {
                minimum_deg: -90.0,
                maximum_deg: 90.0,
                step_deg: 5.0,
            },
            lookup: SofaLookupMode::Interpolated,
            delay: SofaDelayStrategy::Embed {
                maximum_samples: 4_096,
            },
        }
    }
}

#[derive(Debug)]
pub enum SofaError {
    Io(std::io::Error),
    Parse(sofar::reader::Error),
    InvalidSampleRate,
    UnsupportedSampleRate {
        rate: u32,
    },
    InvalidAxis {
        axis: &'static str,
    },
    IrregularAxis {
        axis: &'static str,
    },
    UnsupportedConvention,
    UnsupportedDataType,
    InvalidReceiverCount {
        actual: u32,
    },
    InvalidFilterLength,
    InvalidArrayLength {
        name: &'static str,
    },
    NonFiniteValue {
        name: &'static str,
        index: usize,
    },
    SampleRateMismatch {
        source: f32,
        target: u32,
    },
    NonIntegerDelay {
        samples: f32,
    },
    DelayTooLong {
        samples: usize,
        maximum: usize,
    },
    NonZeroDelayRejected {
        samples: usize,
    },
    EmptyQuery {
        azimuth_index: usize,
        elevation_index: usize,
    },
    CapacityOverflow,
    Grid(GridError),
}

impl fmt::Display for SofaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "failed to read SOFA file: {error}"),
            Self::Parse(error) => write!(f, "failed to parse SOFA data: {error}"),
            Self::InvalidSampleRate => write!(f, "SOFA sample rate must be positive and finite"),
            Self::UnsupportedSampleRate { rate } => write!(
                f,
                "SOFA sample rate {rate} Hz is unsupported; expected 44100, 48000, or 96000 Hz"
            ),
            Self::InvalidAxis { axis } => write!(f, "invalid SOFA {axis} axis"),
            Self::IrregularAxis { axis } => {
                write!(f, "SOFA {axis} axis span must be divisible by its step")
            }
            Self::UnsupportedConvention => {
                write!(f, "SOFA file must use the SimpleFreeFieldHRIR convention")
            }
            Self::UnsupportedDataType => write!(f, "SOFA file must contain FIR data"),
            Self::InvalidReceiverCount { actual } => {
                write!(f, "SOFA file must contain exactly 2 receivers, found {actual}")
            }
            Self::InvalidFilterLength => write!(f, "SOFA filter length must be non-zero"),
            Self::InvalidArrayLength { name } => write!(f, "invalid SOFA {name} array length"),
            Self::NonFiniteValue { name, index } => {
                write!(f, "SOFA {name}[{index}] is not finite")
            }
            Self::SampleRateMismatch { source, target } => write!(
                f,
                "SOFA sample rate {source} Hz cannot be converted to target {target} Hz; supported rates are 44100, 48000, and 96000 Hz"
            ),
            Self::NonIntegerDelay { samples } => {
                write!(f, "SOFA Data.Delay value {samples} is not an integer sample delay")
            }
            Self::DelayTooLong { samples, maximum } => write!(
                f,
                "SOFA Data.Delay value {samples} exceeds configured maximum {maximum} samples"
            ),
            Self::NonZeroDelayRejected { samples } => {
                write!(f, "SOFA Data.Delay value {samples} is rejected by policy")
            }
            Self::EmptyQuery {
                azimuth_index,
                elevation_index,
            } => write!(
                f,
                "SOFA query at grid index ({azimuth_index}, {elevation_index}) returned an all-zero filter"
            ),
            Self::CapacityOverflow => write!(f, "SOFA grid capacity overflow"),
            Self::Grid(error) => error.fmt(f),
        }
    }
}

impl Error for SofaError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Parse(error) => Some(error),
            Self::Grid(error) => Some(error),
            _ => None,
        }
    }
}

impl From<GridError> for SofaError {
    fn from(value: GridError) -> Self {
        Self::Grid(value)
    }
}

/// Loads a user SOFA file on the non-realtime control path.
///
/// A real SimpleFreeFieldHRIR fixture is intentionally not bundled with this crate;
/// acceptance against a representative user file remains an asset-dependent test.
pub fn load_sofa_file(
    path: impl AsRef<Path>,
    options: &SofaGridOptions,
) -> Result<HrtfGrid, SofaError> {
    let bytes = std::fs::read(path).map_err(SofaError::Io)?;
    load_sofa_bytes(&bytes, options)
}

/// Parses user-provided SimpleFreeFieldHRIR bytes into the renderer's regular grid.
pub fn load_sofa_bytes(bytes: &[u8], options: &SofaGridOptions) -> Result<HrtfGrid, SofaError> {
    if options.sample_rate == 0 {
        return Err(SofaError::InvalidSampleRate);
    }
    validate_supported_sample_rate(options.sample_rate)?;
    let azimuths = options.azimuth.values(AxisKind::Azimuth)?;
    let elevations = options.elevation.values(AxisKind::Elevation)?;

    // NaN suppresses sofar's implicit resampling. We compare the parsed source
    // rate ourselves and fail explicitly because this crate disables resampling.
    let mut open_options = OpenOptions::new();
    open_options.sample_rate(f32::NAN).normalized(false);
    let sofa = open_options.open_data(bytes).map_err(SofaError::Parse)?;
    validate_sofa(&sofa, options.sample_rate)?;

    build_grid(&sofa, options, azimuths, elevations)
}

fn validate_sofa(sofa: &Sofar, target_sample_rate: u32) -> Result<(), SofaError> {
    let hrtf = sofa.hrtf();
    if hrtf.get_attribute("Conventions") != Some("SOFA")
        || hrtf.get_attribute("SOFAConventions") != Some("SimpleFreeFieldHRIR")
    {
        return Err(SofaError::UnsupportedConvention);
    }
    if hrtf.get_attribute("DataType") != Some("FIR") {
        return Err(SofaError::UnsupportedDataType);
    }
    if hrtf.r() != 2 {
        return Err(SofaError::InvalidReceiverCount { actual: hrtf.r() });
    }
    if hrtf.filter_len() == 0 {
        return Err(SofaError::InvalidFilterLength);
    }

    let measurements = hrtf.m() as usize;
    let filter_length = hrtf.filter_len();
    let expected_ir = measurements
        .checked_mul(2)
        .and_then(|value| value.checked_mul(filter_length))
        .ok_or(SofaError::CapacityOverflow)?;
    let expected_positions = measurements
        .checked_mul(3)
        .ok_or(SofaError::CapacityOverflow)?;
    validate_array_length("ReceiverPosition", hrtf.receiver_position.len(), 6)?;
    validate_array_length(
        "SourcePosition",
        hrtf.source_position.len(),
        expected_positions,
    )?;
    validate_array_length("Data.IR", hrtf.data_ir.len(), expected_ir)?;
    if hrtf.data_sampling_rate.len() != 1 {
        return Err(SofaError::InvalidArrayLength {
            name: "Data.SamplingRate",
        });
    }
    let delay_len = hrtf.data_delay.len();
    let per_receiver_delay_len = measurements
        .checked_mul(2)
        .ok_or(SofaError::CapacityOverflow)?;
    if !matches!(delay_len, 0 | 2) && delay_len != per_receiver_delay_len {
        return Err(SofaError::InvalidArrayLength { name: "Data.Delay" });
    }

    validate_finite("ListenerPosition", &hrtf.listener_position.values)?;
    validate_finite("ReceiverPosition", &hrtf.receiver_position.values)?;
    validate_finite("SourcePosition", &hrtf.source_position.values)?;
    validate_finite("EmitterPosition", &hrtf.emitter_position.values)?;
    validate_finite("ListenerUp", &hrtf.listener_up.values)?;
    validate_finite("ListenerView", &hrtf.listener_view.values)?;
    validate_finite("Data.IR", &hrtf.data_ir.values)?;
    validate_finite("Data.SamplingRate", &hrtf.data_sampling_rate.values)?;
    validate_finite("Data.Delay", &hrtf.data_delay.values)?;

    let source_rate = sofa.sample_rate();
    if !source_rate.is_finite() || source_rate <= 0.0 {
        return Err(SofaError::InvalidSampleRate);
    }
    parse_supported_sample_rate(source_rate, target_sample_rate)?;
    validate_supported_sample_rate(target_sample_rate)?;
    Ok(())
}

fn build_grid(
    sofa: &Sofar,
    options: &SofaGridOptions,
    azimuths: Vec<f32>,
    elevations: Vec<f32>,
) -> Result<HrtfGrid, SofaError> {
    let source_sample_rate = parse_supported_sample_rate(sofa.sample_rate(), options.sample_rate)?;
    let target_sample_rate = options.sample_rate;
    let source_filter_length = sofa.filter_len();
    let resampled_filter_length =
        resampled_length(source_filter_length, source_sample_rate, target_sample_rate)?;
    let entry_count = azimuths
        .len()
        .checked_mul(elevations.len())
        .ok_or(SofaError::CapacityOverflow)?;
    let mut queried = Vec::with_capacity(entry_count);
    let mut maximum_delay = 0usize;

    for (elevation_index, elevation) in elevations.iter().copied().enumerate() {
        for (azimuth_index, azimuth) in azimuths.iter().copied().enumerate() {
            let [front, left, up] = hse_angles_to_sofar_cartesian(azimuth, elevation);
            let mut filter = Filter::new(sofa.filter_len());
            match options.lookup {
                SofaLookupMode::Nearest => sofa.filter_nointerp(front, left, up, &mut filter),
                SofaLookupMode::Interpolated => sofa.filter(front, left, up, &mut filter),
            }
            validate_finite("queried left IR", &filter.left)?;
            validate_finite("queried right IR", &filter.right)?;
            if filter.left.iter().all(|sample| *sample == 0.0)
                && filter.right.iter().all(|sample| *sample == 0.0)
            {
                return Err(SofaError::EmptyQuery {
                    azimuth_index,
                    elevation_index,
                });
            }

            let left_delay = delay_samples(
                filter.ldelay,
                source_sample_rate,
                target_sample_rate,
                options.delay,
            )?;
            let right_delay = delay_samples(
                filter.rdelay,
                source_sample_rate,
                target_sample_rate,
                options.delay,
            )?;
            maximum_delay = maximum_delay.max(left_delay).max(right_delay);
            let left = resample_ir(&filter.left, source_sample_rate, target_sample_rate)?;
            let right = resample_ir(&filter.right, source_sample_rate, target_sample_rate)?;
            if left.len() != resampled_filter_length || right.len() != resampled_filter_length {
                return Err(SofaError::InvalidFilterLength);
            }
            validate_finite("resampled left IR", &left)?;
            validate_finite("resampled right IR", &right)?;
            queried.push((left, right, left_delay, right_delay));
        }
    }

    let hrir_length = resampled_filter_length
        .checked_add(maximum_delay)
        .ok_or(SofaError::CapacityOverflow)?;
    let sample_count = entry_count
        .checked_mul(hrir_length)
        .ok_or(SofaError::CapacityOverflow)?;
    let mut left = vec![0.0; sample_count];
    let mut right = vec![0.0; sample_count];
    for (index, (filter_left, filter_right, left_delay, right_delay)) in
        queried.into_iter().enumerate()
    {
        let start = index * hrir_length;
        left[start + left_delay..start + left_delay + filter_left.len()]
            .copy_from_slice(&filter_left);
        right[start + right_delay..start + right_delay + filter_right.len()]
            .copy_from_slice(&filter_right);
    }

    HrtfGrid::new(
        options.sample_rate,
        azimuths,
        elevations,
        hrir_length,
        left,
        right,
    )
    .map_err(SofaError::Grid)
}

fn validate_array_length(
    name: &'static str,
    actual: usize,
    expected: usize,
) -> Result<(), SofaError> {
    if actual != expected {
        return Err(SofaError::InvalidArrayLength { name });
    }
    Ok(())
}

fn validate_finite(name: &'static str, values: &[f32]) -> Result<(), SofaError> {
    if let Some((index, _)) = values
        .iter()
        .enumerate()
        .find(|(_, value)| !value.is_finite())
    {
        return Err(SofaError::NonFiniteValue { name, index });
    }
    Ok(())
}

fn validate_supported_sample_rate(rate: u32) -> Result<(), SofaError> {
    if SUPPORTED_SAMPLE_RATES.contains(&rate) {
        Ok(())
    } else {
        Err(SofaError::UnsupportedSampleRate { rate })
    }
}

fn parse_supported_sample_rate(rate: f32, target_rate: u32) -> Result<u32, SofaError> {
    if !rate.is_finite() || rate <= 0.0 {
        return Err(SofaError::InvalidSampleRate);
    }
    for supported in SUPPORTED_SAMPLE_RATES {
        if (rate - supported as f32).abs() <= SAMPLE_RATE_TOLERANCE_HZ {
            return Ok(supported);
        }
    }
    Err(SofaError::SampleRateMismatch {
        source: rate,
        target: target_rate,
    })
}

fn resampled_length(
    source_length: usize,
    source_rate: u32,
    target_rate: u32,
) -> Result<usize, SofaError> {
    if source_length == 0 {
        return Err(SofaError::InvalidFilterLength);
    }
    validate_supported_sample_rate(source_rate)?;
    validate_supported_sample_rate(target_rate)?;
    let numerator = (source_length as u128)
        .checked_mul(u128::from(target_rate))
        .ok_or(SofaError::CapacityOverflow)?;
    let length = numerator
        .checked_add(u128::from(source_rate) - 1)
        .ok_or(SofaError::CapacityOverflow)?
        / u128::from(source_rate);
    usize::try_from(length).map_err(|_| SofaError::CapacityOverflow)
}

fn resample_ir(input: &[f32], source_rate: u32, target_rate: u32) -> Result<Vec<f32>, SofaError> {
    validate_finite("source IR", input)?;
    let output_length = resampled_length(input.len(), source_rate, target_rate)?;
    if source_rate == target_rate {
        return Ok(input.to_vec());
    }

    let rate_ratio = target_rate as f64 / source_rate as f64;
    let cutoff = CUTOFF_MARGIN * rate_ratio.min(1.0);
    let window_denominator = modified_bessel_zero(KAISER_BETA);
    let mut output = Vec::with_capacity(output_length);

    for output_index in 0..output_length {
        let source_position = output_index as f64 / rate_ratio;
        let center = source_position.floor() as i64;
        let mut weighted_sum = 0.0;
        let mut weight_sum = 0.0;

        for source_index in center - SINC_HALF_WIDTH..=center + SINC_HALF_WIDTH {
            if source_index < 0 || source_index >= input.len() as i64 {
                continue;
            }
            let distance = source_position - source_index as f64;
            let normalized = distance / (SINC_HALF_WIDTH as f64 + 1.0);
            let window = if normalized.abs() < 1.0 {
                modified_bessel_zero(KAISER_BETA * (1.0 - normalized * normalized).sqrt())
                    / window_denominator
            } else {
                0.0
            };
            let argument = std::f64::consts::PI * cutoff * distance;
            let sinc = if argument.abs() < f64::EPSILON {
                cutoff
            } else {
                cutoff * argument.sin() / argument
            };
            let weight = sinc * window;
            weighted_sum += f64::from(input[source_index as usize]) * weight;
            weight_sum += weight;
        }

        if !weight_sum.is_finite() || weight_sum.abs() <= f64::EPSILON {
            return Err(SofaError::NonFiniteValue {
                name: "resampler weight",
                index: output_index,
            });
        }
        let sample = weighted_sum / weight_sum;
        if !sample.is_finite() || sample > f32::MAX as f64 || sample < f32::MIN as f64 {
            return Err(SofaError::NonFiniteValue {
                name: "resampled IR",
                index: output_index,
            });
        }
        output.push(sample as f32);
    }

    Ok(output)
}

fn modified_bessel_zero(value: f64) -> f64 {
    let quarter_square = value * value * 0.25;
    let mut sum = 1.0;
    let mut term = 1.0;
    for order in 1..=32 {
        term *= quarter_square / (order as f64 * order as f64);
        sum += term;
        if term <= sum * f64::EPSILON {
            break;
        }
    }
    sum
}

fn delay_samples(
    seconds: f32,
    source_sample_rate: u32,
    target_sample_rate: u32,
    strategy: SofaDelayStrategy,
) -> Result<usize, SofaError> {
    let source_samples = f64::from(seconds) * f64::from(source_sample_rate);
    if !source_samples.is_finite() || source_samples < 0.0 {
        return Err(SofaError::NonIntegerDelay {
            samples: source_samples as f32,
        });
    }
    let rounded_source_samples = source_samples.round();
    if (source_samples - rounded_source_samples).abs() > INTEGER_DELAY_TOLERANCE_SAMPLES {
        return Err(SofaError::NonIntegerDelay {
            samples: source_samples as f32,
        });
    }
    let target_samples =
        rounded_source_samples * f64::from(target_sample_rate) / f64::from(source_sample_rate);
    let rounded = target_samples.round();
    if !rounded.is_finite() || rounded > usize::MAX as f64 {
        return Err(SofaError::NonIntegerDelay {
            samples: target_samples as f32,
        });
    }
    let samples = rounded as usize;
    match strategy {
        SofaDelayStrategy::Embed { maximum_samples } if samples > maximum_samples => {
            Err(SofaError::DelayTooLong {
                samples,
                maximum: maximum_samples,
            })
        }
        SofaDelayStrategy::RejectNonZero if samples != 0 => {
            Err(SofaError::NonZeroDelayRejected { samples })
        }
        _ => Ok(samples),
    }
}

fn hse_angles_to_sofar_cartesian(azimuth_deg: f32, elevation_deg: f32) -> [f32; 3] {
    let azimuth = (-azimuth_deg).to_radians();
    let elevation = elevation_deg.to_radians();
    let horizontal = elevation.cos();
    [
        azimuth.cos() * horizontal,
        azimuth.sin() * horizontal,
        elevation.sin(),
    ]
}

#[derive(Debug, Clone, Copy)]
enum AxisKind {
    Azimuth,
    Elevation,
}

impl AxisKind {
    fn name(self) -> &'static str {
        match self {
            Self::Azimuth => "azimuth",
            Self::Elevation => "elevation",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    #[test]
    fn rejects_invalid_sofa_bytes() {
        let error = load_sofa_bytes(b"not an HDF5 SOFA file", &SofaGridOptions::default())
            .expect_err("invalid bytes must fail");
        assert!(matches!(error, SofaError::Parse(_)));
    }

    #[test]
    fn validates_grid_options_before_parsing() {
        let options = SofaGridOptions {
            sample_rate: 0,
            ..SofaGridOptions::default()
        };
        assert!(matches!(
            load_sofa_bytes(&[], &options),
            Err(SofaError::InvalidSampleRate)
        ));

        let defaults = SofaGridOptions::default();
        let options = SofaGridOptions {
            azimuth: SofaRegularAxis {
                step_deg: 0.0,
                ..defaults.azimuth
            },
            ..defaults
        };
        assert!(matches!(
            load_sofa_bytes(&[], &options),
            Err(SofaError::InvalidAxis { axis: "azimuth" })
        ));

        let options = SofaGridOptions {
            elevation: SofaRegularAxis {
                minimum_deg: -30.0,
                maximum_deg: 30.0,
                step_deg: 17.0,
            },
            ..SofaGridOptions::default()
        };
        assert!(matches!(
            load_sofa_bytes(&[], &options),
            Err(SofaError::IrregularAxis { axis: "elevation" })
        ));
    }

    #[test]
    fn converts_hse_right_positive_angles_to_sofar_front_left_up() {
        assert_cartesian(0.0, 0.0, [1.0, 0.0, 0.0]);
        assert_cartesian(90.0, 0.0, [0.0, -1.0, 0.0]);
        assert_cartesian(-90.0, 0.0, [0.0, 1.0, 0.0]);
        assert_cartesian(0.0, 90.0, [0.0, 0.0, 1.0]);
        assert_cartesian(-180.0, 0.0, [-1.0, 0.0, 0.0]);
    }

    #[test]
    fn delay_policy_requires_bounded_integer_samples() {
        assert_eq!(
            delay_samples(
                3.0 / 48_000.0,
                48_000,
                48_000,
                SofaDelayStrategy::Embed { maximum_samples: 3 }
            )
            .unwrap(),
            3
        );
        assert!(matches!(
            delay_samples(
                3.5 / 48_000.0,
                48_000,
                48_000,
                SofaDelayStrategy::Embed { maximum_samples: 8 }
            ),
            Err(SofaError::NonIntegerDelay { .. })
        ));
        assert!(matches!(
            delay_samples(
                4.0 / 48_000.0,
                48_000,
                48_000,
                SofaDelayStrategy::Embed { maximum_samples: 3 }
            ),
            Err(SofaError::DelayTooLong { .. })
        ));
        assert!(matches!(
            delay_samples(
                1.0 / 48_000.0,
                48_000,
                48_000,
                SofaDelayStrategy::RejectNonZero
            ),
            Err(SofaError::NonZeroDelayRejected { .. })
        ));
    }

    #[test]
    fn resampler_lengths_cover_supported_rate_pairs() {
        for (source_rate, target_rate, source_len, expected_len) in [
            (44_100, 48_000, 441, 480),
            (44_100, 96_000, 441, 960),
            (48_000, 44_100, 480, 441),
            (48_000, 96_000, 480, 960),
            (96_000, 44_100, 960, 441),
            (96_000, 48_000, 960, 480),
        ] {
            let input = vec![0.0; source_len];
            let output = resample_ir(&input, source_rate, target_rate).unwrap();
            assert_eq!(
                output.len(),
                expected_len,
                "unexpected {source_rate} -> {target_rate} length"
            );
            assert!(output.iter().all(|sample| sample.is_finite()));
        }
    }

    #[test]
    fn resampler_preserves_passband_sine_frequency_and_level() {
        const FREQUENCY_HZ: f64 = 4_000.0;
        const DURATION_SECONDS: f64 = 0.1;

        for (source_rate, target_rate) in [
            (44_100, 48_000),
            (44_100, 96_000),
            (48_000, 44_100),
            (48_000, 96_000),
            (96_000, 44_100),
            (96_000, 48_000),
        ] {
            let source_len = (source_rate as f64 * DURATION_SECONDS) as usize;
            let input: Vec<f32> = (0..source_len)
                .map(|index| (TAU * FREQUENCY_HZ * index as f64 / source_rate as f64).sin() as f32)
                .collect();
            let output = resample_ir(&input, source_rate, target_rate).unwrap();
            let margin = (target_rate / 200).max(64) as usize;
            let body = &output[margin..output.len() - margin];
            let rms = (body
                .iter()
                .map(|sample| f64::from(*sample).powi(2))
                .sum::<f64>()
                / body.len() as f64)
                .sqrt();
            let correlation = body
                .iter()
                .enumerate()
                .map(|(index, sample)| {
                    let absolute_index = index + margin;
                    f64::from(*sample)
                        * (TAU * FREQUENCY_HZ * absolute_index as f64 / target_rate as f64).sin()
                })
                .sum::<f64>()
                * 2.0
                / body.len() as f64;

            assert!((rms - 0.5_f64.sqrt()).abs() < 2.0e-3);
            assert!(
                correlation > 0.995,
                "{source_rate} -> {target_rate}: {correlation}"
            );
        }
    }

    #[test]
    fn resampler_keeps_an_impulse_finite_and_time_aligned() {
        for (source_rate, target_rate) in [(44_100, 96_000), (96_000, 44_100)] {
            let mut input = vec![0.0; source_rate as usize / 100];
            let source_peak = input.len() / 2;
            input[source_peak] = 1.0;
            let output = resample_ir(&input, source_rate, target_rate).unwrap();
            let expected_peak = ((source_peak as u64 * target_rate as u64)
                + source_rate as u64 / 2)
                / source_rate as u64;
            let actual_peak = output
                .iter()
                .enumerate()
                .max_by(|(_, left), (_, right)| left.abs().total_cmp(&right.abs()))
                .map(|(index, _)| index)
                .unwrap();

            assert!(output.iter().all(|sample| sample.is_finite()));
            assert!(actual_peak.abs_diff(expected_peak as usize) <= 1);
            let minimum_peak = (target_rate as f32 / source_rate as f32).min(1.0) * 0.5;
            assert!(
                output[actual_peak].abs() > minimum_peak,
                "{source_rate} -> {target_rate}: peak {} below {minimum_peak}",
                output[actual_peak].abs()
            );
        }
    }

    #[test]
    fn rejects_unsupported_resampler_rates_and_non_finite_input() {
        assert!(matches!(
            resample_ir(&[1.0], 48_000, 88_200),
            Err(SofaError::UnsupportedSampleRate { rate: 88_200 })
        ));
        assert!(matches!(
            resample_ir(&[f32::NAN], 48_000, 96_000),
            Err(SofaError::NonFiniteValue {
                name: "source IR",
                index: 0
            })
        ));
    }

    #[test]
    fn delay_is_converted_at_the_target_sample_rate() {
        let seconds = 10.0 / 44_100.0;
        assert_eq!(
            delay_samples(
                seconds,
                44_100,
                96_000,
                SofaDelayStrategy::Embed {
                    maximum_samples: 22
                }
            )
            .unwrap(),
            22
        );
    }

    #[test]
    #[ignore = "requires a user-supplied SimpleFreeFieldHRIR SOFA asset"]
    fn accepts_real_simple_free_field_hrir_asset() {
        let path = std::env::var_os("HSE_TEST_SOFA")
            .expect("set HSE_TEST_SOFA to a SimpleFreeFieldHRIR file");
        for sample_rate in SUPPORTED_SAMPLE_RATES {
            let options = SofaGridOptions {
                sample_rate,
                ..SofaGridOptions::default()
            };
            let grid = load_sofa_file(&path, &options).unwrap();
            assert_eq!(grid.sample_rate(), sample_rate);
            assert!(grid.hrir_length() > 0);
            let front = grid.nearest(0.0, 0.0);
            assert!(front.left.iter().all(|sample| sample.is_finite()));
            assert!(front.right.iter().all(|sample| sample.is_finite()));
        }
    }

    fn assert_cartesian(azimuth: f32, elevation: f32, expected: [f32; 3]) {
        let actual = hse_angles_to_sofar_cartesian(azimuth, elevation);
        for index in 0..3 {
            assert!(
                (actual[index] - expected[index]).abs() <= 1.0e-6,
                "component {index}: expected {}, got {}",
                expected[index],
                actual[index]
            );
        }
    }
}
