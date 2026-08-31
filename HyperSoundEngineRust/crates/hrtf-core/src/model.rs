use std::{error::Error, f32::consts::TAU, fmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DistanceModel {
    Inverse,
    Linear,
    Exponential,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DistanceParams {
    pub reference_distance: f32,
    pub maximum_distance: f32,
    pub rolloff_factor: f32,
}

impl Default for DistanceParams {
    fn default() -> Self {
        Self {
            reference_distance: 1.0,
            maximum_distance: 50.0,
            rolloff_factor: 1.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelError {
    InvalidReferenceDistance,
    InvalidMaximumDistance,
    InvalidRolloffFactor,
    InvalidDistance,
    InvalidSampleRate,
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid spatial model parameter: {self:?}")
    }
}

impl Error for ModelError {}

impl DistanceParams {
    pub fn validate(self) -> Result<Self, ModelError> {
        if !self.reference_distance.is_finite() || self.reference_distance <= 0.0 {
            return Err(ModelError::InvalidReferenceDistance);
        }
        if !self.maximum_distance.is_finite() || self.maximum_distance <= self.reference_distance {
            return Err(ModelError::InvalidMaximumDistance);
        }
        if !self.rolloff_factor.is_finite() || self.rolloff_factor < 0.0 {
            return Err(ModelError::InvalidRolloffFactor);
        }
        Ok(self)
    }
}

impl DistanceModel {
    pub fn gain(self, distance: f32, params: DistanceParams) -> Result<f32, ModelError> {
        let params = params.validate()?;
        if !distance.is_finite() || distance < 0.0 {
            return Err(ModelError::InvalidDistance);
        }
        if distance <= params.reference_distance {
            return Ok(1.0);
        }

        let distance = distance.min(params.maximum_distance);
        let gain = match self {
            Self::Inverse => {
                params.reference_distance
                    / (params.reference_distance
                        + params.rolloff_factor * (distance - params.reference_distance))
            }
            Self::Linear => {
                1.0 - params.rolloff_factor * (distance - params.reference_distance)
                    / (params.maximum_distance - params.reference_distance)
            }
            Self::Exponential => {
                (distance / params.reference_distance).powf(-params.rolloff_factor)
            }
        };
        Ok(gain.clamp(0.0, 1.0))
    }
}

/// Returns the one-pole low-pass feed coefficient for `fc = 4000 / (1 + distance)`.
pub fn air_absorption_coefficient(sample_rate: f32, distance: f32) -> Result<f32, ModelError> {
    if !sample_rate.is_finite() || sample_rate <= 0.0 {
        return Err(ModelError::InvalidSampleRate);
    }
    if !distance.is_finite() || distance < 0.0 {
        return Err(ModelError::InvalidDistance);
    }
    let cutoff = (4000.0 / (1.0 + distance)).min(sample_rate * 0.5);
    Ok(1.0 - (-TAU * cutoff / sample_rate).exp())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_models_are_unity_inside_reference_distance() {
        for model in [
            DistanceModel::Inverse,
            DistanceModel::Linear,
            DistanceModel::Exponential,
        ] {
            assert_eq!(model.gain(0.25, DistanceParams::default()), Ok(1.0));
            assert_eq!(model.gain(1.0, DistanceParams::default()), Ok(1.0));
        }
    }

    #[test]
    fn models_follow_their_attenuation_curves() {
        let params = DistanceParams {
            reference_distance: 1.0,
            maximum_distance: 5.0,
            rolloff_factor: 1.0,
        };
        assert_eq!(DistanceModel::Inverse.gain(3.0, params), Ok(1.0 / 3.0));
        assert_eq!(DistanceModel::Linear.gain(3.0, params), Ok(0.5));
        assert_eq!(DistanceModel::Linear.gain(5.0, params), Ok(0.0));
        assert_eq!(DistanceModel::Exponential.gain(3.0, params), Ok(1.0 / 3.0));
    }

    #[test]
    fn air_absorption_is_finite_and_stronger_at_distance() {
        let near = air_absorption_coefficient(48_000.0, 0.0).unwrap();
        let far = air_absorption_coefficient(48_000.0, 20.0).unwrap();
        assert!(near > far && far > 0.0 && near <= 1.0);
    }
}
