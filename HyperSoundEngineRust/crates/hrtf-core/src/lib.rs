//! Platform-independent HRTF spatial geometry primitives.

pub mod world;

pub use world::{relative_direction, wrap_azimuth_deg, RelativeDirection, Vec3, WorldListener};
