use std::{error::Error, fmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NearestIndex {
    pub azimuth: usize,
    pub elevation: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct HrirPair<'a> {
    pub left: &'a [f32],
    pub right: &'a [f32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GridError {
    InvalidSampleRate,
    EmptyAzimuths,
    EmptyElevations,
    InvalidHrirLength,
    NonFiniteAxis {
        axis: &'static str,
        index: usize,
    },
    AxisOutOfRange {
        axis: &'static str,
        index: usize,
    },
    AxisNotStrictlyAscending {
        axis: &'static str,
        index: usize,
    },
    DataLengthOverflow,
    DataLength {
        expected: usize,
        left: usize,
        right: usize,
    },
    NonFiniteSample {
        ear: &'static str,
        index: usize,
    },
}

impl fmt::Display for GridError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid HRTF grid: {self:?}")
    }
}

impl Error for GridError {}

#[derive(Debug, Clone)]
pub struct HrtfGrid {
    sample_rate: u32,
    azimuths: Vec<f32>,
    elevations: Vec<f32>,
    hrir_length: usize,
    left: Vec<f32>,
    right: Vec<f32>,
}

impl HrtfGrid {
    pub fn new(
        sample_rate: u32,
        azimuths: Vec<f32>,
        elevations: Vec<f32>,
        hrir_length: usize,
        left: Vec<f32>,
        right: Vec<f32>,
    ) -> Result<Self, GridError> {
        if sample_rate == 0 {
            return Err(GridError::InvalidSampleRate);
        }
        validate_axis(&azimuths, "azimuth", -180.0, 180.0, true)?;
        validate_axis(&elevations, "elevation", -90.0, 90.0, false)?;
        if hrir_length == 0 {
            return Err(GridError::InvalidHrirLength);
        }

        let expected = azimuths
            .len()
            .checked_mul(elevations.len())
            .and_then(|count| count.checked_mul(hrir_length))
            .ok_or(GridError::DataLengthOverflow)?;
        if left.len() != expected || right.len() != expected {
            return Err(GridError::DataLength {
                expected,
                left: left.len(),
                right: right.len(),
            });
        }
        validate_samples(&left, "left")?;
        validate_samples(&right, "right")?;

        Ok(Self {
            sample_rate,
            azimuths,
            elevations,
            hrir_length,
            left,
            right,
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn azimuths(&self) -> &[f32] {
        &self.azimuths
    }

    pub fn elevations(&self) -> &[f32] {
        &self.elevations
    }

    pub fn hrir_length(&self) -> usize {
        self.hrir_length
    }

    pub(crate) fn planes(&self) -> (&[f32], &[f32]) {
        (&self.left, &self.right)
    }

    pub fn nearest_index(&self, azimuth_deg: f32, elevation_deg: f32) -> NearestIndex {
        assert!(azimuth_deg.is_finite(), "azimuth must be finite");
        assert!(elevation_deg.is_finite(), "elevation must be finite");

        let azimuth = wrap_azimuth(azimuth_deg);
        let mut azimuth_index = 0;
        let mut azimuth_distance = f32::INFINITY;
        for (index, candidate) in self.azimuths.iter().copied().enumerate() {
            let difference = (azimuth - candidate).abs();
            let distance = difference.min(360.0 - difference);
            if distance < azimuth_distance {
                azimuth_distance = distance;
                azimuth_index = index;
            }
        }

        let elevation = elevation_deg.clamp(
            self.elevations[0],
            self.elevations[self.elevations.len() - 1],
        );
        let mut elevation_index = 0;
        let mut elevation_distance = f32::INFINITY;
        for (index, candidate) in self.elevations.iter().copied().enumerate() {
            let distance = (elevation - candidate).abs();
            if distance < elevation_distance {
                elevation_distance = distance;
                elevation_index = index;
            }
        }

        NearestIndex {
            azimuth: azimuth_index,
            elevation: elevation_index,
        }
    }

    pub fn nearest(&self, azimuth_deg: f32, elevation_deg: f32) -> HrirPair<'_> {
        self.hrir(self.nearest_index(azimuth_deg, elevation_deg))
    }

    pub fn hrir(&self, index: NearestIndex) -> HrirPair<'_> {
        assert!(
            index.azimuth < self.azimuths.len(),
            "azimuth index out of range"
        );
        assert!(
            index.elevation < self.elevations.len(),
            "elevation index out of range"
        );
        let entry = index.elevation * self.azimuths.len() + index.azimuth;
        let start = entry * self.hrir_length;
        let end = start + self.hrir_length;
        HrirPair {
            left: &self.left[start..end],
            right: &self.right[start..end],
        }
    }
}

fn validate_axis(
    values: &[f32],
    axis: &'static str,
    minimum: f32,
    maximum: f32,
    maximum_is_exclusive: bool,
) -> Result<(), GridError> {
    if values.is_empty() {
        return Err(if axis == "azimuth" {
            GridError::EmptyAzimuths
        } else {
            GridError::EmptyElevations
        });
    }
    for (index, value) in values.iter().copied().enumerate() {
        if !value.is_finite() {
            return Err(GridError::NonFiniteAxis { axis, index });
        }
        let above_maximum = if maximum_is_exclusive {
            value >= maximum
        } else {
            value > maximum
        };
        if value < minimum || above_maximum {
            return Err(GridError::AxisOutOfRange { axis, index });
        }
        if index > 0 && value <= values[index - 1] {
            return Err(GridError::AxisNotStrictlyAscending { axis, index });
        }
    }
    Ok(())
}

fn validate_samples(values: &[f32], ear: &'static str) -> Result<(), GridError> {
    for (index, value) in values.iter().enumerate() {
        if !value.is_finite() {
            return Err(GridError::NonFiniteSample { ear, index });
        }
    }
    Ok(())
}

fn wrap_azimuth(angle: f32) -> f32 {
    (angle + 180.0).rem_euclid(360.0) - 180.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid() -> HrtfGrid {
        HrtfGrid::new(
            48_000,
            vec![-170.0, -90.0, 0.0, 90.0, 170.0],
            vec![-30.0, 0.0, 30.0],
            1,
            vec![0.0; 15],
            vec![0.0; 15],
        )
        .unwrap()
    }

    #[test]
    fn rejects_invalid_shapes_axes_and_samples() {
        assert!(matches!(
            HrtfGrid::new(0, vec![0.0], vec![0.0], 1, vec![0.0], vec![0.0]),
            Err(GridError::InvalidSampleRate)
        ));
        assert!(matches!(
            HrtfGrid::new(48_000, vec![], vec![0.0], 1, vec![], vec![]),
            Err(GridError::EmptyAzimuths)
        ));
        assert!(matches!(
            HrtfGrid::new(48_000, vec![0.0], vec![], 1, vec![], vec![]),
            Err(GridError::EmptyElevations)
        ));
        assert!(matches!(
            HrtfGrid::new(
                48_000,
                vec![0.0, 0.0],
                vec![0.0],
                1,
                vec![0.0; 2],
                vec![0.0; 2]
            ),
            Err(GridError::AxisNotStrictlyAscending { .. })
        ));
        assert!(matches!(
            HrtfGrid::new(48_000, vec![180.0], vec![0.0], 1, vec![0.0], vec![0.0]),
            Err(GridError::AxisOutOfRange { .. })
        ));
        assert!(matches!(
            HrtfGrid::new(48_000, vec![0.0], vec![91.0], 1, vec![0.0], vec![0.0]),
            Err(GridError::AxisOutOfRange { .. })
        ));
        assert!(matches!(
            HrtfGrid::new(48_000, vec![f32::NAN], vec![0.0], 1, vec![0.0], vec![0.0]),
            Err(GridError::NonFiniteAxis { .. })
        ));
        assert!(matches!(
            HrtfGrid::new(48_000, vec![0.0], vec![0.0], 0, vec![], vec![]),
            Err(GridError::InvalidHrirLength)
        ));
        assert!(matches!(
            HrtfGrid::new(48_000, vec![0.0], vec![0.0], 2, vec![0.0], vec![0.0; 2]),
            Err(GridError::DataLength { .. })
        ));
        assert!(matches!(
            HrtfGrid::new(
                48_000,
                vec![0.0],
                vec![0.0],
                1,
                vec![f32::INFINITY],
                vec![0.0]
            ),
            Err(GridError::NonFiniteSample { .. })
        ));
    }

    #[test]
    fn nearest_wraps_azimuth_and_clamps_elevation() {
        let grid = grid();
        assert_eq!(
            grid.nearest_index(181.0, 80.0),
            NearestIndex {
                azimuth: 0,
                elevation: 2
            }
        );
        assert_eq!(
            grid.nearest_index(-181.0, -80.0),
            NearestIndex {
                azimuth: 4,
                elevation: 0
            }
        );
        assert_eq!(
            grid.nearest_index(88.0, 1.0),
            NearestIndex {
                azimuth: 3,
                elevation: 1
            }
        );
    }
}
