//! Platform-independent HRTF spatial geometry and rendering primitives.

pub mod grid;
pub mod interpolation;
pub mod model;
mod partitioned;
pub mod renderer;
pub mod room;
pub mod sofa;
pub mod world;

pub use grid::{GridError, HrirPair, HrtfGrid, NearestIndex};
pub use interpolation::{InterpolationError, InterpolationMode};
pub use model::{air_absorption_coefficient, DistanceModel, DistanceParams, ModelError};
pub use partitioned::ConvolutionMode;
pub use renderer::{
    BinauralRenderer, HrtfRenderer, ObjectEffects, ObjectInput, PrepareError, ProcessError,
    RenderProfile,
};
pub use room::{RoomError, RoomParams, RoomPreset};
pub use sofa::{
    load_sofa_bytes, load_sofa_file, SofaDelayStrategy, SofaError, SofaGridOptions, SofaLookupMode,
    SofaRegularAxis,
};
pub use world::{
    relative_direction, relative_direction_pose, wrap_azimuth_deg, RelativeDirection, Vec3,
    WorldListener, WorldListenerPose,
};
