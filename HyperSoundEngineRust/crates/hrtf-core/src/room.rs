use std::{error::Error, f64::consts::PI, fmt};

const SPEED_OF_SOUND: f64 = 343.0;
const EARLY_LP_FC_BASE: f64 = 8_000.0;
const FDN_LP_FC: f64 = 4_000.0;
const FDN_DELAYS_48K: [usize; 8] = [179, 211, 251, 307, 359, 419, 467, 521];

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RoomParams {
    pub width: f32,
    pub height: f32,
    pub depth: f32,
    pub reflectivity: f32,
    pub early_orders: u8,
    pub rt60: f32,
}

impl RoomParams {
    pub fn validate(self) -> Result<Self, RoomError> {
        if !self.width.is_finite() || self.width <= 0.0 {
            return Err(RoomError::InvalidDimension("width"));
        }
        if !self.height.is_finite() || self.height <= 0.0 {
            return Err(RoomError::InvalidDimension("height"));
        }
        if !self.depth.is_finite() || self.depth <= 0.0 {
            return Err(RoomError::InvalidDimension("depth"));
        }
        if !self.reflectivity.is_finite() || !(0.0..=1.0).contains(&self.reflectivity) {
            return Err(RoomError::InvalidReflectivity);
        }
        if self.early_orders > 3 {
            return Err(RoomError::InvalidEarlyOrders);
        }
        if !self.rt60.is_finite() || self.rt60 <= 0.0 {
            return Err(RoomError::InvalidRt60);
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomPreset {
    Studio,
    Hall,
    Stage,
    Church,
    Outdoor,
    Bathroom,
    Corridor,
}

impl RoomPreset {
    pub const ALL: [Self; 7] = [
        Self::Studio,
        Self::Hall,
        Self::Stage,
        Self::Church,
        Self::Outdoor,
        Self::Bathroom,
        Self::Corridor,
    ];

    pub fn params(self) -> RoomParams {
        let (width, height, depth, reflectivity, rt60) = match self {
            Self::Studio => (5.0, 3.0, 4.0, 0.25, 0.45),
            Self::Hall => (25.0, 12.0, 18.0, 0.6, 2.2),
            Self::Stage => (18.0, 8.0, 14.0, 0.5, 1.4),
            Self::Church => (30.0, 18.0, 40.0, 0.75, 4.5),
            Self::Outdoor => (80.0, 30.0, 60.0, 0.15, 1.2),
            Self::Bathroom => (2.5, 2.6, 2.2, 0.9, 1.8),
            Self::Corridor => (2.2, 2.8, 18.0, 0.5, 1.6),
        };
        RoomParams {
            width,
            height,
            depth,
            reflectivity,
            early_orders: 2,
            rt60,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomError {
    InvalidDimension(&'static str),
    InvalidReflectivity,
    InvalidEarlyOrders,
    InvalidRt60,
    InvalidAmount,
    CapacityOverflow,
}

impl fmt::Display for RoomError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid room configuration: {self:?}")
    }
}

impl Error for RoomError {}

#[derive(Debug)]
struct EarlyTap {
    delay: usize,
    gain: f32,
    lp_coefficient: f32,
    state_left: f32,
    state_right: f32,
}

#[derive(Debug)]
struct FdnState {
    delays: [usize; 8],
    gains: [f32; 8],
    lp_coefficients: [f32; 8],
    lines_left: [Vec<f32>; 8],
    lines_right: [Vec<f32>; 8],
    positions_left: [usize; 8],
    positions_right: [usize; 8],
    states_left: [f32; 8],
    states_right: [f32; 8],
    matrix: [[f64; 8]; 8],
}

impl FdnState {
    fn new(sample_rate: u32, rt60: f32) -> Self {
        let scale = sample_rate as f64 / 48_000.0;
        let delays = std::array::from_fn(|index| {
            ((FDN_DELAYS_48K[index] as f64 * scale).round() as usize).max(1)
        });
        let gains = std::array::from_fn(|index| {
            10.0_f64.powf(-3.0 * (delays[index] as f64 / sample_rate as f64) / rt60 as f64) as f32
        });
        let coefficient = (-2.0 * PI * FDN_LP_FC / sample_rate as f64).exp() as f32;
        let lp_coefficients = [coefficient; 8];
        let lines_left = std::array::from_fn(|index| vec![0.0; delays[index]]);
        let lines_right = std::array::from_fn(|index| vec![0.0; delays[index]]);
        let inverse = 1.0 / 8.0_f64.sqrt();
        let matrix = std::array::from_fn(|row| {
            std::array::from_fn(|column| {
                if (row & column).count_ones() & 1 == 0 {
                    inverse
                } else {
                    -inverse
                }
            })
        });
        Self {
            delays,
            gains,
            lp_coefficients,
            lines_left,
            lines_right,
            positions_left: [0; 8],
            positions_right: [0; 8],
            states_left: [0.0; 8],
            states_right: [0.0; 8],
            matrix,
        }
    }

    fn process_left(&mut self, input: f64) -> f64 {
        process_fdn_ear(
            input,
            &self.delays,
            &self.gains,
            &self.lp_coefficients,
            &self.matrix,
            &mut self.lines_left,
            &mut self.positions_left,
            &mut self.states_left,
        )
    }

    fn process_right(&mut self, input: f64) -> f64 {
        process_fdn_ear(
            input,
            &self.delays,
            &self.gains,
            &self.lp_coefficients,
            &self.matrix,
            &mut self.lines_right,
            &mut self.positions_right,
            &mut self.states_right,
        )
    }

    fn reset(&mut self) {
        for line in &mut self.lines_left {
            line.fill(0.0);
        }
        for line in &mut self.lines_right {
            line.fill(0.0);
        }
        self.positions_left.fill(0);
        self.positions_right.fill(0);
        self.states_left.fill(0.0);
        self.states_right.fill(0.0);
    }
}

#[allow(clippy::too_many_arguments)]
fn process_fdn_ear(
    input: f64,
    delays: &[usize; 8],
    gains: &[f32; 8],
    lp_coefficients: &[f32; 8],
    matrix: &[[f64; 8]; 8],
    lines: &mut [Vec<f32>; 8],
    positions: &mut [usize; 8],
    states: &mut [f32; 8],
) -> f64 {
    let mut values = [0.0; 8];
    for index in 0..8 {
        let read = lines[index][positions[index]];
        let a = lp_coefficients[index];
        let low_pass = (1.0 - a) as f64 * read as f64 + a as f64 * states[index] as f64;
        states[index] = low_pass as f32;
        values[index] = low_pass;
    }
    for index in 0..8 {
        let mut mixed = 0.0;
        for column in 0..8 {
            mixed += matrix[index][column] * values[column];
        }
        let position = positions[index];
        lines[index][position] = (input + gains[index] as f64 * mixed) as f32;
        positions[index] = if position + 1 == delays[index] {
            0
        } else {
            position + 1
        };
    }
    values.iter().sum()
}

#[derive(Debug)]
pub(crate) struct RoomState {
    amount: f32,
    taps: Vec<EarlyTap>,
    history_left: Vec<f32>,
    history_right: Vec<f32>,
    position_left: usize,
    position_right: usize,
    early_left: Vec<f32>,
    early_right: Vec<f32>,
    fdn: FdnState,
}

impl RoomState {
    pub(crate) fn new(
        sample_rate: u32,
        params: RoomParams,
        amount: f32,
        max_frames: usize,
    ) -> Result<Self, RoomError> {
        let params = params.validate()?;
        if !amount.is_finite() || !(0.0..=1.0).contains(&amount) {
            return Err(RoomError::InvalidAmount);
        }
        let mut taps = Vec::new();
        let mut maximum_delay = 1;
        let center = [
            params.width as f64 * 0.5,
            params.height as f64 * 0.5,
            params.depth as f64 * 0.5,
        ];
        for order_x in 0..=params.early_orders {
            for image_x in axis_images(center[0], params.width as f64, order_x) {
                for order_y in 0..=params.early_orders {
                    for image_y in axis_images(center[1], params.height as f64, order_y) {
                        for order_z in 0..=params.early_orders {
                            for image_z in axis_images(center[2], params.depth as f64, order_z) {
                                let order = image_x.1 + image_y.1 + image_z.1;
                                if order == 0 || order > params.early_orders {
                                    continue;
                                }
                                let dx = image_x.0 - center[0];
                                let dy = image_y.0 - center[1];
                                let dz = image_z.0 - center[2];
                                let distance = (dx * dx + dy * dy + dz * dz).sqrt();
                                let delay = ((distance * sample_rate as f64 / SPEED_OF_SOUND)
                                    .round() as usize)
                                    .max(1);
                                maximum_delay = maximum_delay.max(delay);
                                let mut reflection = 1.0;
                                for _ in 0..order {
                                    reflection *= params.reflectivity as f64;
                                }
                                let cutoff = EARLY_LP_FC_BASE / (1 + order as usize) as f64;
                                taps.push(EarlyTap {
                                    delay,
                                    gain: (reflection / (distance * distance)) as f32,
                                    lp_coefficient: (-2.0 * PI * cutoff / sample_rate as f64).exp()
                                        as f32,
                                    state_left: 0.0,
                                    state_right: 0.0,
                                });
                            }
                        }
                    }
                }
            }
        }
        let history_length = maximum_delay
            .checked_add(1)
            .ok_or(RoomError::CapacityOverflow)?;
        Ok(Self {
            amount,
            taps,
            history_left: vec![0.0; history_length],
            history_right: vec![0.0; history_length],
            position_left: 0,
            position_right: 0,
            early_left: vec![0.0; max_frames],
            early_right: vec![0.0; max_frames],
            fdn: FdnState::new(sample_rate, params.rt60),
        })
    }

    pub(crate) fn set_amount(&mut self, amount: f32) -> Result<(), RoomError> {
        if !amount.is_finite() || !(0.0..=1.0).contains(&amount) {
            return Err(RoomError::InvalidAmount);
        }
        self.amount = amount;
        Ok(())
    }

    pub(crate) fn process(&mut self, left: &mut [f32], right: &mut [f32], object_count: usize) {
        self.early_left[..left.len()].fill(0.0);
        self.early_right[..right.len()].fill(0.0);
        process_early_ear(
            left,
            &mut self.history_left,
            &mut self.position_left,
            &mut self.taps,
            &mut self.early_left,
            true,
        );
        process_early_ear(
            right,
            &mut self.history_right,
            &mut self.position_right,
            &mut self.taps,
            &mut self.early_right,
            false,
        );
        let divisor = object_count.max(1) as f64;
        for (frame, sample) in left.iter_mut().enumerate() {
            let dry = *sample;
            let late = self.fdn.process_left(dry as f64 / divisor);
            *sample =
                (dry as f64 + self.amount as f64 * (self.early_left[frame] as f64 + late)) as f32;
        }
        for (frame, sample) in right.iter_mut().enumerate() {
            let dry = *sample;
            let late = self.fdn.process_right(dry as f64 / divisor);
            *sample =
                (dry as f64 + self.amount as f64 * (self.early_right[frame] as f64 + late)) as f32;
        }
    }

    pub(crate) fn reset(&mut self) {
        self.history_left.fill(0.0);
        self.history_right.fill(0.0);
        self.position_left = 0;
        self.position_right = 0;
        for tap in &mut self.taps {
            tap.state_left = 0.0;
            tap.state_right = 0.0;
        }
        self.fdn.reset();
    }
}

fn process_early_ear(
    input: &[f32],
    history: &mut [f32],
    position: &mut usize,
    taps: &mut [EarlyTap],
    output: &mut [f32],
    left: bool,
) {
    let history_length = history.len();
    let mut write_position = *position;
    for (frame, &sample) in input.iter().enumerate() {
        history[write_position] = sample;
        for tap in taps.iter_mut() {
            let read = history[(write_position + history_length - tap.delay) % history_length];
            let state = if left {
                &mut tap.state_left
            } else {
                &mut tap.state_right
            };
            let filtered = (1.0 - tap.lp_coefficient) as f64 * read as f64
                + tap.lp_coefficient as f64 * *state as f64;
            *state = filtered as f32;
            output[frame] += (tap.gain as f64 * filtered) as f32;
        }
        write_position += 1;
        if write_position == history_length {
            write_position = 0;
        }
    }
    *position = write_position;
}

fn axis_images(coordinate: f64, dimension: f64, order: u8) -> [(f64, u8); 2] {
    match order {
        0 => [(coordinate, 0), (coordinate, 4)],
        1 => [(-coordinate, 1), (2.0 * dimension - coordinate, 1)],
        2 => [
            (2.0 * dimension + coordinate, 2),
            (coordinate - 2.0 * dimension, 2),
        ],
        _ => [
            (4.0 * dimension - coordinate, 3),
            (-2.0 * dimension - coordinate, 3),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_match_the_seven_reference_rooms() {
        assert_eq!(RoomPreset::ALL.len(), 7);
        assert_eq!(RoomPreset::Studio.params().early_orders, 2);
        assert_eq!(RoomPreset::Church.params().rt60, 4.5);
        for preset in RoomPreset::ALL {
            preset.params().validate().unwrap();
        }
    }

    #[test]
    fn early_order_zero_builds_only_the_fdn() {
        let mut params = RoomPreset::Studio.params();
        params.early_orders = 0;
        let room = RoomState::new(48_000, params, 0.5, 128).unwrap();
        assert!(room.taps.is_empty());
    }

    #[test]
    fn every_early_tap_starts_at_its_declared_delay_including_the_maximum() {
        let params = RoomPreset::Studio.params();
        let reference = RoomState::new(48_000, params, 1.0, 1).unwrap();
        let delays: Vec<usize> = reference.taps.iter().map(|tap| tap.delay).collect();
        let maximum_delay = *delays.iter().max().unwrap();
        assert_eq!(reference.history_left.len(), maximum_delay + 1);

        for (target, &declared_delay) in delays.iter().enumerate() {
            let mut room = RoomState::new(48_000, params, 1.0, maximum_delay + 1).unwrap();
            for (index, tap) in room.taps.iter_mut().enumerate() {
                tap.gain = if index == target { 1.0 } else { 0.0 };
            }
            let mut input = vec![0.0; maximum_delay + 1];
            input[0] = 1.0;
            let mut output = vec![0.0; input.len()];
            process_early_ear(
                &input,
                &mut room.history_left,
                &mut room.position_left,
                &mut room.taps,
                &mut output,
                true,
            );
            let first_non_zero = output.iter().position(|sample| *sample != 0.0);
            assert_eq!(
                first_non_zero,
                Some(declared_delay),
                "tap {target}, delay {declared_delay}"
            );
        }

        assert!(delays.contains(&maximum_delay));
    }
}
