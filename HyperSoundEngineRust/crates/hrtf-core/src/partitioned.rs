use rustfft::{num_complex::Complex32, Fft, FftPlanner};
use std::sync::Arc;

use crate::{HrirPair, HrtfGrid, NearestIndex, PrepareError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConvolutionMode {
    #[default]
    Time,
    Partitioned,
}

pub(crate) struct PartitionedConvolver {
    partition_size: usize,
    fft_size: usize,
    partition_count: usize,
    direction_count: usize,
    forward: Arc<dyn Fft<f32>>,
    inverse: Arc<dyn Fft<f32>>,
    hrir_left: Vec<Complex32>,
    hrir_right: Vec<Complex32>,
    input_blocks: Vec<f32>,
    input_fill: Vec<usize>,
    input_spectra: Vec<Complex32>,
    spectrum_heads: Vec<usize>,
    output_left: Vec<f32>,
    output_right: Vec<f32>,
    output_positions: Vec<usize>,
    overlap_left: Vec<f32>,
    overlap_right: Vec<f32>,
    fft_input: Vec<Complex32>,
    fft_left: Vec<Complex32>,
    fft_right: Vec<Complex32>,
    scratch: Vec<Complex32>,
}

impl std::fmt::Debug for PartitionedConvolver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PartitionedConvolver")
            .field("partition_size", &self.partition_size)
            .field("fft_size", &self.fft_size)
            .field("partition_count", &self.partition_count)
            .field("direction_count", &self.direction_count)
            .finish_non_exhaustive()
    }
}

impl PartitionedConvolver {
    pub(crate) fn new(
        grid: &HrtfGrid,
        max_objects: usize,
        partition_size: usize,
    ) -> Result<Self, PrepareError> {
        let fft_size = partition_size
            .checked_mul(2)
            .ok_or(PrepareError::CapacityOverflow)?;
        let partition_count = grid.hrir_length().div_ceil(partition_size);
        let direction_count = grid
            .azimuths()
            .len()
            .checked_mul(grid.elevations().len())
            .ok_or(PrepareError::CapacityOverflow)?;
        let spectra_per_direction = partition_count
            .checked_mul(fft_size)
            .ok_or(PrepareError::CapacityOverflow)?;
        let hrir_spectra_len = direction_count
            .checked_mul(spectra_per_direction)
            .ok_or(PrepareError::CapacityOverflow)?;
        let object_spectra_len = max_objects
            .checked_mul(spectra_per_direction)
            .ok_or(PrepareError::CapacityOverflow)?;
        let object_partition_len = max_objects
            .checked_mul(partition_size)
            .ok_or(PrepareError::CapacityOverflow)?;

        let mut planner = FftPlanner::new();
        let forward = planner.plan_fft_forward(fft_size);
        let inverse = planner.plan_fft_inverse(fft_size);
        let scratch_len = forward
            .get_inplace_scratch_len()
            .max(inverse.get_inplace_scratch_len());
        let mut state = Self {
            partition_size,
            fft_size,
            partition_count,
            direction_count,
            forward,
            inverse,
            hrir_left: vec![Complex32::ZERO; hrir_spectra_len],
            hrir_right: vec![Complex32::ZERO; hrir_spectra_len],
            input_blocks: vec![0.0; object_partition_len],
            input_fill: vec![0; max_objects],
            input_spectra: vec![Complex32::ZERO; object_spectra_len],
            spectrum_heads: vec![0; max_objects],
            output_left: vec![0.0; object_partition_len],
            output_right: vec![0.0; object_partition_len],
            output_positions: vec![0; max_objects],
            overlap_left: vec![0.0; object_partition_len],
            overlap_right: vec![0.0; object_partition_len],
            fft_input: vec![Complex32::ZERO; fft_size],
            fft_left: vec![Complex32::ZERO; fft_size],
            fft_right: vec![Complex32::ZERO; fft_size],
            scratch: vec![Complex32::ZERO; scratch_len],
        };
        state.precompute_hrirs(grid);
        Ok(state)
    }

    fn precompute_hrirs(&mut self, grid: &HrtfGrid) {
        for elevation in 0..grid.elevations().len() {
            for azimuth in 0..grid.azimuths().len() {
                let direction = elevation * grid.azimuths().len() + azimuth;
                self.precompute_direction(
                    grid.hrir(NearestIndex { azimuth, elevation }),
                    direction,
                );
            }
        }
    }

    fn precompute_direction(&mut self, hrir: HrirPair<'_>, direction: usize) {
        for partition in 0..self.partition_count {
            let tap_start = partition * self.partition_size;
            let tap_end = (tap_start + self.partition_size).min(hrir.left.len());
            self.fft_input.fill(Complex32::ZERO);
            for (target, &sample) in self.fft_input[..tap_end - tap_start]
                .iter_mut()
                .zip(&hrir.left[tap_start..tap_end])
            {
                target.re = sample;
            }
            self.forward
                .process_with_scratch(&mut self.fft_input, &mut self.scratch);
            let offset = self.hrir_offset(direction, partition);
            self.hrir_left[offset..offset + self.fft_size].copy_from_slice(&self.fft_input);

            self.fft_input.fill(Complex32::ZERO);
            for (target, &sample) in self.fft_input[..tap_end - tap_start]
                .iter_mut()
                .zip(&hrir.right[tap_start..tap_end])
            {
                target.re = sample;
            }
            self.forward
                .process_with_scratch(&mut self.fft_input, &mut self.scratch);
            self.hrir_right[offset..offset + self.fft_size].copy_from_slice(&self.fft_input);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn process_object(
        &mut self,
        slot: usize,
        direction: usize,
        mono: &[f32],
        gain: f32,
        air_coefficient: f32,
        air_state: &mut f32,
        output_left: &mut [f32],
        output_right: &mut [f32],
    ) {
        debug_assert!(direction < self.direction_count);
        let block_start = slot * self.partition_size;
        let mut fill = self.input_fill[slot];
        let mut output_position = self.output_positions[slot];
        let mut filtered = *air_state;
        for frame in 0..mono.len() {
            output_left[frame] += self.output_left[block_start + output_position];
            output_right[frame] += self.output_right[block_start + output_position];
            self.output_left[block_start + output_position] = 0.0;
            self.output_right[block_start + output_position] = 0.0;
            output_position += 1;
            if output_position == self.partition_size {
                output_position = 0;
            }

            filtered += air_coefficient * (mono[frame] - filtered);
            self.input_blocks[block_start + fill] = filtered * gain;
            fill += 1;
            if fill == self.partition_size {
                self.process_partition(slot, direction);
                fill = 0;
            }
        }
        self.input_fill[slot] = fill;
        self.output_positions[slot] = output_position;
        *air_state = filtered;
    }

    fn process_partition(&mut self, slot: usize, direction: usize) {
        self.fft_input.fill(Complex32::ZERO);
        let block_start = slot * self.partition_size;
        for (target, &sample) in self.fft_input[..self.partition_size]
            .iter_mut()
            .zip(&self.input_blocks[block_start..block_start + self.partition_size])
        {
            target.re = sample;
        }
        self.forward
            .process_with_scratch(&mut self.fft_input, &mut self.scratch);

        let head = self.spectrum_heads[slot];
        let input_offset = self.input_offset(slot, head);
        self.input_spectra[input_offset..input_offset + self.fft_size]
            .copy_from_slice(&self.fft_input);
        self.fft_left.fill(Complex32::ZERO);
        self.fft_right.fill(Complex32::ZERO);
        for ir_partition in 0..self.partition_count {
            let history_partition =
                (head + self.partition_count - ir_partition) % self.partition_count;
            let input_offset = self.input_offset(slot, history_partition);
            let hrir_offset = self.hrir_offset(direction, ir_partition);
            for bin in 0..self.fft_size {
                let input = self.input_spectra[input_offset + bin];
                self.fft_left[bin] += input * self.hrir_left[hrir_offset + bin];
                self.fft_right[bin] += input * self.hrir_right[hrir_offset + bin];
            }
        }
        self.inverse
            .process_with_scratch(&mut self.fft_left, &mut self.scratch);
        self.inverse
            .process_with_scratch(&mut self.fft_right, &mut self.scratch);
        let scale = 1.0 / self.fft_size as f32;
        for frame in 0..self.partition_size {
            self.output_left[block_start + frame] =
                self.fft_left[frame].re * scale + self.overlap_left[block_start + frame];
            self.output_right[block_start + frame] =
                self.fft_right[frame].re * scale + self.overlap_right[block_start + frame];
            self.overlap_left[block_start + frame] =
                self.fft_left[frame + self.partition_size].re * scale;
            self.overlap_right[block_start + frame] =
                self.fft_right[frame + self.partition_size].re * scale;
        }
        self.spectrum_heads[slot] = (head + 1) % self.partition_count;
    }

    pub(crate) fn reset(&mut self) {
        self.input_blocks.fill(0.0);
        self.input_fill.fill(0);
        self.input_spectra.fill(Complex32::ZERO);
        self.spectrum_heads.fill(0);
        self.output_left.fill(0.0);
        self.output_right.fill(0.0);
        self.output_positions.fill(0);
        self.overlap_left.fill(0.0);
        self.overlap_right.fill(0.0);
    }

    pub(crate) fn reset_slot(&mut self, slot: usize) {
        let block_start = slot * self.partition_size;
        self.input_blocks[block_start..block_start + self.partition_size].fill(0.0);
        self.output_left[block_start..block_start + self.partition_size].fill(0.0);
        self.output_right[block_start..block_start + self.partition_size].fill(0.0);
        self.overlap_left[block_start..block_start + self.partition_size].fill(0.0);
        self.overlap_right[block_start..block_start + self.partition_size].fill(0.0);
        let spectra_start = slot * self.partition_count * self.fft_size;
        self.input_spectra[spectra_start..spectra_start + self.partition_count * self.fft_size]
            .fill(Complex32::ZERO);
        self.input_fill[slot] = 0;
        self.spectrum_heads[slot] = 0;
        self.output_positions[slot] = 0;
    }

    fn input_offset(&self, slot: usize, partition: usize) -> usize {
        (slot * self.partition_count + partition) * self.fft_size
    }

    fn hrir_offset(&self, direction: usize, partition: usize) -> usize {
        (direction * self.partition_count + partition) * self.fft_size
    }
}
