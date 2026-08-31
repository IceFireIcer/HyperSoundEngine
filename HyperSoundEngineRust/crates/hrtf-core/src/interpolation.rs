use std::{error::Error, f64::consts::PI, fmt};

use crate::HrtfGrid;

const SH_BASIS_COUNT: usize = 16;
const SINGULAR_EPSILON: f64 = 1.0e-12;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InterpolationMode {
    #[default]
    Nearest,
    Spherical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterpolationError {
    DegenerateGrid,
    CapacityOverflow,
    IncompatibleConvolutionMode,
}

impl fmt::Display for InterpolationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "failed to prepare HRTF interpolation: {self:?}")
    }
}

impl Error for InterpolationError {}

#[derive(Debug)]
pub(crate) struct SphericalInterpolator {
    coeffs_left: Vec<f64>,
    coeffs_right: Vec<f64>,
    hrir_length: usize,
}

impl SphericalInterpolator {
    pub(crate) fn fit(grid: &HrtfGrid) -> Result<Self, InterpolationError> {
        let direction_count = grid
            .azimuths()
            .len()
            .checked_mul(grid.elevations().len())
            .ok_or(InterpolationError::CapacityOverflow)?;
        let matrix_len = direction_count
            .checked_mul(SH_BASIS_COUNT)
            .ok_or(InterpolationError::CapacityOverflow)?;
        let mut design = vec![0.0; matrix_len];
        let mut basis = [0.0; SH_BASIS_COUNT];
        let mut direction = 0;
        for &elevation in grid.elevations() {
            for &azimuth in grid.azimuths() {
                sh_basis(azimuth as f64, elevation as f64, &mut basis);
                design[direction * SH_BASIS_COUNT..(direction + 1) * SH_BASIS_COUNT]
                    .copy_from_slice(&basis);
                direction += 1;
            }
        }

        let mut gram = vec![0.0; SH_BASIS_COUNT * SH_BASIS_COUNT];
        for k in 0..SH_BASIS_COUNT {
            for m in 0..SH_BASIS_COUNT {
                let mut sum = 0.0;
                for d in 0..direction_count {
                    sum += design[d * SH_BASIS_COUNT + k] * design[d * SH_BASIS_COUNT + m];
                }
                gram[k * SH_BASIS_COUNT + m] = sum;
            }
        }
        invert_gauss_jordan(&mut gram)?;

        let pinv_len = SH_BASIS_COUNT
            .checked_mul(direction_count)
            .ok_or(InterpolationError::CapacityOverflow)?;
        let mut pinv = vec![0.0; pinv_len];
        for k in 0..SH_BASIS_COUNT {
            for d in 0..direction_count {
                let mut sum = 0.0;
                for m in 0..SH_BASIS_COUNT {
                    sum += gram[k * SH_BASIS_COUNT + m] * design[d * SH_BASIS_COUNT + m];
                }
                pinv[k * direction_count + d] = sum;
            }
        }

        let (left, right) = grid.planes();
        Ok(Self {
            coeffs_left: fit_ear(&pinv, left, direction_count, grid.hrir_length())?,
            coeffs_right: fit_ear(&pinv, right, direction_count, grid.hrir_length())?,
            hrir_length: grid.hrir_length(),
        })
    }

    pub(crate) fn evaluate(
        &self,
        grid: &HrtfGrid,
        azimuth_deg: f32,
        elevation_deg: f32,
        output_left: &mut [f32],
        output_right: &mut [f32],
    ) {
        let azimuth = (azimuth_deg + 180.0).rem_euclid(360.0) - 180.0;
        let elevation = elevation_deg.clamp(
            grid.elevations()[0],
            grid.elevations()[grid.elevations().len() - 1],
        );
        let mut basis = [0.0; SH_BASIS_COUNT];
        sh_basis(azimuth as f64, elevation as f64, &mut basis);
        evaluate_ear(&self.coeffs_left, &basis, self.hrir_length, output_left);
        evaluate_ear(&self.coeffs_right, &basis, self.hrir_length, output_right);
    }
}

fn fit_ear(
    pinv: &[f64],
    plane: &[f32],
    direction_count: usize,
    hrir_length: usize,
) -> Result<Vec<f64>, InterpolationError> {
    let length = SH_BASIS_COUNT
        .checked_mul(hrir_length)
        .ok_or(InterpolationError::CapacityOverflow)?;
    let mut coefficients = vec![0.0; length];
    for k in 0..SH_BASIS_COUNT {
        for tap in 0..hrir_length {
            let mut sum = 0.0;
            for direction in 0..direction_count {
                sum += pinv[k * direction_count + direction]
                    * plane[direction * hrir_length + tap] as f64;
            }
            coefficients[k * hrir_length + tap] = sum;
        }
    }
    Ok(coefficients)
}

fn evaluate_ear(coefficients: &[f64], basis: &[f64; 16], hrir_length: usize, out: &mut [f32]) {
    for tap in 0..hrir_length {
        let mut sum = 0.0;
        for k in 0..SH_BASIS_COUNT {
            sum += coefficients[k * hrir_length + tap] * basis[k];
        }
        out[tap] = sum as f32;
    }
}

fn invert_gauss_jordan(matrix: &mut [f64]) -> Result<(), InterpolationError> {
    let n = SH_BASIS_COUNT;
    let width = n * 2;
    let mut augmented = vec![0.0; n * width];
    for row in 0..n {
        for column in 0..n {
            augmented[row * width + column] = matrix[row * n + column];
        }
        augmented[row * width + n + row] = 1.0;
    }
    for column in 0..n {
        let mut pivot = column;
        let mut best = augmented[column * width + column].abs();
        for row in column + 1..n {
            let candidate = augmented[row * width + column].abs();
            if candidate > best {
                best = candidate;
                pivot = row;
            }
        }
        if pivot != column {
            for index in 0..width {
                augmented.swap(column * width + index, pivot * width + index);
            }
        }
        let divisor = augmented[column * width + column];
        if divisor.abs() < SINGULAR_EPSILON {
            return Err(InterpolationError::DegenerateGrid);
        }
        for index in 0..width {
            augmented[column * width + index] /= divisor;
        }
        for row in 0..n {
            if row == column {
                continue;
            }
            let factor = augmented[row * width + column];
            if factor == 0.0 {
                continue;
            }
            for index in 0..width {
                augmented[row * width + index] -= factor * augmented[column * width + index];
            }
        }
    }
    for row in 0..n {
        for column in 0..n {
            matrix[row * n + column] = augmented[row * width + n + column];
        }
    }
    Ok(())
}

fn sh_basis(azimuth_deg: f64, elevation_deg: f64, out: &mut [f64; SH_BASIS_COUNT]) {
    let sqrt2 = 2.0_f64.sqrt();
    let k0 = 0.5 / PI.sqrt();
    let k1 = (3.0 / (4.0 * PI)).sqrt();
    let k2 = (5.0 / (16.0 * PI)).sqrt();
    let k3 = (7.0 / (16.0 * PI)).sqrt();
    let c21 = 3.0 * sqrt2 * (15.0 / (8.0 * PI)).sqrt();
    let c22 = 3.0 * sqrt2 * (15.0 / (32.0 * PI)).sqrt();
    let c31 = 1.5 * sqrt2 * (21.0 / (32.0 * PI)).sqrt();
    let c32 = 15.0 * sqrt2 * (105.0 / (32.0 * PI)).sqrt();
    let c33 = 15.0 * sqrt2 * (35.0 / (64.0 * PI)).sqrt();
    let phi = azimuth_deg * PI / 180.0;
    let theta = elevation_deg * PI / 180.0;
    let u = theta.cos();
    let v = theta.sin();
    let ca = phi.cos();
    let sa = phi.sin();
    let c2 = ca * ca - sa * sa;
    let s2 = 2.0 * sa * ca;
    let c3 = c2 * ca - s2 * sa;
    let s3 = s2 * ca + c2 * sa;
    let u2 = u * u;
    let u3 = u2 * u;
    let v2 = v * v;
    let v3 = v2 * v;

    out[0] = k0;
    out[1] = -k1 * sa * u;
    out[2] = k1 * v;
    out[3] = -k1 * ca * u;
    out[4] = c22 * s2 * u2;
    out[5] = -c21 * sa * v * u;
    out[6] = k2 * (3.0 * v2 - 1.0) * 0.5;
    out[7] = -c21 * ca * v * u;
    out[8] = c22 * c2 * u2;
    out[9] = -c33 * s3 * u3;
    out[10] = c32 * s2 * v * u2;
    out[11] = -c31 * sa * (5.0 * v2 - 1.0) * u;
    out[12] = k3 * (5.0 * v3 - 3.0 * v) * 0.5;
    out[13] = -c31 * ca * (5.0 * v2 - 1.0) * u;
    out[14] = c32 * c2 * v * u2;
    out[15] = -c33 * c3 * u3;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn harmonic_grid() -> HrtfGrid {
        let azimuths = vec![-157.5, -112.5, -67.5, -22.5, 22.5, 67.5, 112.5, 157.5];
        let elevations = vec![-70.0, -35.0, -10.0, 15.0, 45.0, 75.0];
        let mut left = Vec::new();
        let mut right = Vec::new();
        let mut basis = [0.0; SH_BASIS_COUNT];
        for &elevation in &elevations {
            for &azimuth in &azimuths {
                sh_basis(azimuth as f64, elevation as f64, &mut basis);
                left.extend([basis[0] as f32, basis[3] as f32]);
                right.extend([basis[2] as f32, basis[8] as f32]);
            }
        }
        HrtfGrid::new(48_000, azimuths, elevations, 2, left, right).unwrap()
    }

    #[test]
    fn spherical_fit_recovers_l3_fields_between_grid_points() {
        let grid = harmonic_grid();
        let fit = SphericalInterpolator::fit(&grid).unwrap();
        let mut left = [0.0; 2];
        let mut right = [0.0; 2];
        fit.evaluate(&grid, 12.5, 7.5, &mut left, &mut right);
        let mut expected = [0.0; SH_BASIS_COUNT];
        sh_basis(12.5, 7.5, &mut expected);
        assert!((left[0] - expected[0] as f32).abs() < 1.0e-5);
        assert!((left[1] - expected[3] as f32).abs() < 1.0e-5);
        assert!((right[0] - expected[2] as f32).abs() < 1.0e-5);
        assert!((right[1] - expected[8] as f32).abs() < 1.0e-5);
    }

    #[test]
    fn spherical_fit_rejects_rank_deficient_grids() {
        let grid = HrtfGrid::new(48_000, vec![0.0], vec![0.0], 1, vec![1.0], vec![1.0]).unwrap();
        assert!(matches!(
            SphericalInterpolator::fit(&grid),
            Err(InterpolationError::DegenerateGrid)
        ));
    }
}
