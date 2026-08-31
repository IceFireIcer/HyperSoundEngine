use std::{error::Error, fmt};

use crate::{
    air_absorption_coefficient, interpolation::SphericalInterpolator,
    partitioned::PartitionedConvolver, room::RoomState, ConvolutionMode, DistanceModel,
    DistanceParams, HrtfGrid, InterpolationError, InterpolationMode, ModelError, RoomError,
    RoomParams, RoomPreset, Vec3,
};

const SPEED_OF_SOUND: f32 = 343.0;
const DOPPLER_RATE_MIN: f32 = 0.5;
const DOPPLER_RATE_MAX: f32 = 2.0;
const RESAMPLE_LINE: usize = 1024;
const RESAMPLE_START_DELAY: f32 = 512.0;
const RESAMPLE_MIN_DELAY: f32 = 1.0;
const RESAMPLE_MAX_DELAY: f32 = (RESAMPLE_LINE - 2) as f32;
const DECORRELATION_LINE: usize = 16;
const OCCLUSION_GAIN_FACTOR: f32 = 0.8;
const OCCLUSION_FC_BASE: f32 = 12_000.0;
const OCCLUSION_FC_MIN: f32 = 1.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ObjectEffects {
    pub size: f32,
    /// Dedicated stable state slot for the second blurred direction when `size > 0`.
    pub spread_slot: Option<usize>,
}

impl Default for ObjectEffects {
    fn default() -> Self {
        Self {
            size: 0.0,
            spread_slot: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderProfile {
    /// Allows hosts to budget up to 5 ms of renderer latency.
    Compatibility,
    /// Uses 64-sample partitions and remains strictly below 5 ms at supported rates.
    LowLatency,
}

impl RenderProfile {
    pub const fn partition_size(self) -> usize {
        match self {
            Self::Compatibility => 128,
            Self::LowLatency => 64,
        }
    }

    pub fn maximum_latency_samples(self, sample_rate: u32) -> usize {
        (sample_rate as usize * 5).div_ceil(1000).saturating_sub(1)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ObjectInput<'a> {
    /// Stable state slot in `0..max_objects`; independent of this block's object order.
    pub slot: usize,
    pub mono: &'a [f32],
    pub azimuth_deg: f32,
    pub elevation_deg: f32,
    pub distance: f32,
    pub gain: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrepareError {
    ZeroMaxObjects,
    ZeroMaxFrames,
    CapacityOverflow,
    IncompatibleInterpolationMode,
    Interpolation(InterpolationError),
    Room(RoomError),
}

impl fmt::Display for PrepareError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "failed to prepare binaural renderer: {self:?}")
    }
}

impl Error for PrepareError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessError {
    NotPrepared,
    TooManyObjects { provided: usize, maximum: usize },
    SlotOutOfRange { slot: usize, maximum: usize },
    DuplicateSlot { slot: usize },
    FrameCountExceedsPrepared { provided: usize, maximum: usize },
    OutputTooShort,
    InputTooShort { object: usize },
    NonFiniteObject { object: usize },
    InvalidDistanceModel(ModelError),
}

impl fmt::Display for ProcessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "failed to render binaural block: {self:?}")
    }
}

impl Error for ProcessError {}

#[derive(Debug)]
pub struct BinauralRenderer {
    grid: HrtfGrid,
    profile: RenderProfile,
    distance_model: DistanceModel,
    distance_params: DistanceParams,
    max_objects: usize,
    max_frames: usize,
    history: Vec<f32>,
    write_positions: Vec<usize>,
    air_states: Vec<f32>,
    effect_convolution_states: Vec<f32>,
    slot_seen: Vec<bool>,
    convolution_mode: ConvolutionMode,
    partitioned: Option<PartitionedConvolver>,
    interpolation_mode: InterpolationMode,
    spherical: Option<SphericalInterpolator>,
    interpolated_left: Vec<f32>,
    interpolated_right: Vec<f32>,
    room_params: Option<RoomParams>,
    room_amount: f32,
    room: Option<RoomState>,
    listener_velocity: Option<Vec3>,
    occlusion: f32,
    effect_input: Vec<f32>,
    effect_left: Vec<f32>,
    effect_right: Vec<f32>,
    occlusion_states: Vec<f32>,
    resample_ring: Vec<f32>,
    resample_positions: Vec<usize>,
    resample_delays: Vec<f32>,
    decorrelation_ring: Vec<f32>,
    decorrelation_positions: Vec<usize>,
}

pub type HrtfRenderer = BinauralRenderer;

impl BinauralRenderer {
    pub fn new(
        grid: HrtfGrid,
        profile: RenderProfile,
        distance_model: DistanceModel,
        distance_params: DistanceParams,
    ) -> Result<Self, ModelError> {
        distance_params.validate()?;
        Ok(Self {
            grid,
            profile,
            distance_model,
            distance_params,
            max_objects: 0,
            max_frames: 0,
            history: Vec::new(),
            write_positions: Vec::new(),
            air_states: Vec::new(),
            effect_convolution_states: Vec::new(),
            slot_seen: Vec::new(),
            convolution_mode: ConvolutionMode::Time,
            partitioned: None,
            interpolation_mode: InterpolationMode::Nearest,
            spherical: None,
            interpolated_left: Vec::new(),
            interpolated_right: Vec::new(),
            room_params: None,
            room_amount: 0.0,
            room: None,
            listener_velocity: None,
            occlusion: 0.0,
            effect_input: Vec::new(),
            effect_left: Vec::new(),
            effect_right: Vec::new(),
            occlusion_states: Vec::new(),
            resample_ring: Vec::new(),
            resample_positions: Vec::new(),
            resample_delays: Vec::new(),
            decorrelation_ring: Vec::new(),
            decorrelation_positions: Vec::new(),
        })
    }

    pub fn prepare(&mut self, max_objects: usize, max_frames: usize) -> Result<(), PrepareError> {
        if max_objects == 0 {
            return Err(PrepareError::ZeroMaxObjects);
        }
        if max_frames == 0 {
            return Err(PrepareError::ZeroMaxFrames);
        }
        let history_len = max_objects
            .checked_mul(self.grid.hrir_length())
            .ok_or(PrepareError::CapacityOverflow)?;

        let interpolation_len = max_objects
            .checked_mul(self.grid.hrir_length())
            .ok_or(PrepareError::CapacityOverflow)?;
        self.history = vec![0.0; history_len];
        self.write_positions = vec![0; max_objects];
        self.air_states = vec![0.0; max_objects];
        self.effect_convolution_states = vec![0.0; max_objects];
        self.slot_seen = vec![false; max_objects];
        self.interpolated_left = vec![0.0; interpolation_len];
        self.interpolated_right = vec![0.0; interpolation_len];
        self.effect_input = vec![0.0; max_frames];
        self.effect_left = vec![0.0; max_frames];
        self.effect_right = vec![0.0; max_frames];
        self.occlusion_states = vec![0.0; max_objects];
        self.resample_ring = vec![0.0; max_objects * RESAMPLE_LINE];
        self.resample_positions = vec![0; max_objects];
        self.resample_delays = vec![RESAMPLE_START_DELAY; max_objects];
        self.decorrelation_ring = vec![0.0; max_objects * DECORRELATION_LINE];
        self.decorrelation_positions = vec![0; max_objects];
        self.max_objects = max_objects;
        self.max_frames = max_frames;
        self.rebuild_partitioned()?;
        self.rebuild_room().map_err(PrepareError::Room)?;
        Ok(())
    }

    pub fn process(
        &mut self,
        objects: &[ObjectInput<'_>],
        output_left: &mut [f32],
        output_right: &mut [f32],
        frame_count: usize,
    ) -> Result<(), ProcessError> {
        self.validate_process(objects, output_left, output_right, frame_count)?;
        output_left[..frame_count].fill(0.0);
        output_right[..frame_count].fill(0.0);

        let hrir_length = self.grid.hrir_length();
        let sample_rate = self.grid.sample_rate() as f32;
        for object in objects {
            let slot = object.slot;
            let distance_gain = self
                .distance_model
                .gain(object.distance, self.distance_params)
                .map_err(ProcessError::InvalidDistanceModel)?;
            let gain = object.gain * distance_gain;
            let air_coefficient = air_absorption_coefficient(sample_rate, object.distance)
                .map_err(ProcessError::InvalidDistanceModel)?;
            let air_state = &mut self.air_states[slot];

            if self.convolution_mode == ConvolutionMode::Partitioned
                && self.interpolation_mode == InterpolationMode::Nearest
            {
                let index = self
                    .grid
                    .nearest_index(object.azimuth_deg, object.elevation_deg);
                let direction = index.elevation * self.grid.azimuths().len() + index.azimuth;
                self.partitioned
                    .as_mut()
                    .expect("partitioned mode is prepared on the control path")
                    .process_object(
                        slot,
                        direction,
                        &object.mono[..frame_count],
                        gain,
                        air_coefficient,
                        air_state,
                        &mut output_left[..frame_count],
                        &mut output_right[..frame_count],
                    );
                continue;
            }

            let history_start = slot * hrir_length;
            let history = &mut self.history[history_start..history_start + hrir_length];
            let write_position = &mut self.write_positions[slot];
            match self.interpolation_mode {
                InterpolationMode::Nearest => {
                    let hrir = self.grid.nearest(object.azimuth_deg, object.elevation_deg);
                    render_object(
                        object.mono,
                        hrir.left,
                        hrir.right,
                        gain,
                        air_coefficient,
                        history,
                        write_position,
                        air_state,
                        output_left,
                        output_right,
                        frame_count,
                    );
                }
                InterpolationMode::Spherical => {
                    let ir_start = slot * hrir_length;
                    let ir_left = &mut self.interpolated_left[ir_start..ir_start + hrir_length];
                    let ir_right = &mut self.interpolated_right[ir_start..ir_start + hrir_length];
                    self.spherical
                        .as_ref()
                        .expect("spherical mode is fitted on the control path")
                        .evaluate(
                            &self.grid,
                            object.azimuth_deg,
                            object.elevation_deg,
                            ir_left,
                            ir_right,
                        );
                    render_object(
                        object.mono,
                        ir_left,
                        ir_right,
                        gain,
                        air_coefficient,
                        history,
                        write_position,
                        air_state,
                        output_left,
                        output_right,
                        frame_count,
                    );
                }
            }
        }
        if self.room_amount > 0.0 {
            if let Some(room) = &mut self.room {
                room.process(
                    &mut output_left[..frame_count],
                    &mut output_right[..frame_count],
                    objects.len(),
                );
            }
        }
        Ok(())
    }

    /// Renders objects with the optional world-mode effects used by engine stage 22.
    ///
    /// All scratch and per-slot state is allocated by `prepare`; neutral settings delegate to the
    /// legacy path so existing renderer vectors retain their exact arithmetic.
    pub fn process_with_effects(
        &mut self,
        objects: &[ObjectInput<'_>],
        effects: &[ObjectEffects],
        output_left: &mut [f32],
        output_right: &mut [f32],
        frame_count: usize,
    ) -> Result<(), ProcessError> {
        if effects.len() < objects.len() {
            return Err(ProcessError::InputTooShort {
                object: effects.len(),
            });
        }
        if self.listener_velocity.is_none()
            && self.occlusion == 0.0
            && effects[..objects.len()]
                .iter()
                .all(|effect| effect.size == 0.0)
        {
            return self.process(objects, output_left, output_right, frame_count);
        }
        self.validate_process(objects, output_left, output_right, frame_count)?;
        self.slot_seen.fill(false);
        for object in objects {
            self.slot_seen[object.slot] = true;
        }
        for (index, effect) in effects[..objects.len()].iter().enumerate() {
            if !effect.size.is_finite() || !(0.0..=1.0).contains(&effect.size) {
                return Err(ProcessError::NonFiniteObject { object: index });
            }
            if effect.size > 0.0 {
                let slot = effect.spread_slot.ok_or(ProcessError::DuplicateSlot {
                    slot: objects[index].slot,
                })?;
                if slot >= self.max_objects {
                    return Err(ProcessError::SlotOutOfRange {
                        slot,
                        maximum: self.max_objects,
                    });
                }
                if self.slot_seen[slot] {
                    return Err(ProcessError::DuplicateSlot { slot });
                }
                self.slot_seen[slot] = true;
            }
        }

        output_left[..frame_count].fill(0.0);
        output_right[..frame_count].fill(0.0);
        let mut effect_input = std::mem::take(&mut self.effect_input);
        let mut effect_left = std::mem::take(&mut self.effect_left);
        let mut effect_right = std::mem::take(&mut self.effect_right);
        let render_result = (|| {
            for (index, object) in objects.iter().enumerate() {
                effect_input[..frame_count].copy_from_slice(&object.mono[..frame_count]);
                let slot = object.slot;
                let distance_gain = self
                    .distance_model
                    .gain(object.distance, self.distance_params)
                    .map_err(ProcessError::InvalidDistanceModel)?;
                let gain = object.gain * distance_gain;
                let air_coefficient =
                    air_absorption_coefficient(self.grid.sample_rate() as f32, object.distance)
                        .map_err(ProcessError::InvalidDistanceModel)?;
                let mut air_state = self.air_states[slot];
                for sample in &mut effect_input[..frame_count] {
                    air_state += air_coefficient * (*sample - air_state);
                    *sample = air_state * gain;
                }
                self.air_states[slot] = air_state;
                if self.occlusion > 0.0 {
                    let gain = 1.0 - OCCLUSION_GAIN_FACTOR * self.occlusion;
                    let cutoff = (OCCLUSION_FC_BASE * (1.0 - self.occlusion)).max(OCCLUSION_FC_MIN);
                    let coefficient = 1.0
                        - (-2.0 * std::f32::consts::PI * cutoff / self.grid.sample_rate() as f32)
                            .exp();
                    let mut state = self.occlusion_states[slot];
                    for sample in &mut effect_input[..frame_count] {
                        state += coefficient * (*sample * gain - state);
                        *sample = state;
                    }
                    self.occlusion_states[slot] = state;
                }
                if let Some(velocity) = self.listener_velocity {
                    let azimuth = object.azimuth_deg.to_radians();
                    let elevation = object.elevation_deg.to_radians();
                    let cos_elevation = elevation.cos();
                    let direction = Vec3 {
                        x: (azimuth.sin() * cos_elevation) as f64,
                        y: elevation.sin() as f64,
                        z: (azimuth.cos() * cos_elevation) as f64,
                    };
                    let projection = velocity.x as f32 * direction.x as f32
                        + velocity.y as f32 * direction.y as f32
                        + velocity.z as f32 * direction.z as f32;
                    let rate = (SPEED_OF_SOUND / (SPEED_OF_SOUND - projection))
                        .clamp(DOPPLER_RATE_MIN, DOPPLER_RATE_MAX);
                    if rate != 1.0 {
                        resample_slot(
                            &mut effect_input[..frame_count],
                            slot,
                            rate,
                            &mut self.resample_ring,
                            &mut self.resample_positions,
                            &mut self.resample_delays,
                        );
                    }
                }

                effect_left[..frame_count].fill(0.0);
                effect_right[..frame_count].fill(0.0);
                let effect = effects[index];
                if effect.size > 0.0 {
                    let spread = effect.size * 30.0;
                    self.render_planar_object(
                        slot,
                        &effect_input[..frame_count],
                        object.azimuth_deg - spread,
                        object.elevation_deg,
                        object.distance,
                        0.5,
                        true,
                        &mut effect_left[..frame_count],
                        &mut effect_right[..frame_count],
                        frame_count,
                    )?;
                    self.render_planar_object(
                        effect.spread_slot.expect("validated spread slot"),
                        &effect_input[..frame_count],
                        object.azimuth_deg + spread,
                        object.elevation_deg,
                        object.distance,
                        0.5,
                        true,
                        &mut effect_left[..frame_count],
                        &mut effect_right[..frame_count],
                        frame_count,
                    )?;
                    decorrelate_slot(
                        &mut effect_right[..frame_count],
                        slot,
                        effect.size * 6.0,
                        &mut self.decorrelation_ring,
                        &mut self.decorrelation_positions,
                    );
                } else {
                    self.render_planar_object(
                        slot,
                        &effect_input[..frame_count],
                        object.azimuth_deg,
                        object.elevation_deg,
                        object.distance,
                        1.0,
                        true,
                        &mut effect_left[..frame_count],
                        &mut effect_right[..frame_count],
                        frame_count,
                    )?;
                }
                for frame in 0..frame_count {
                    output_left[frame] += effect_left[frame];
                    output_right[frame] += effect_right[frame];
                }
            }
            Ok(())
        })();
        self.effect_input = effect_input;
        self.effect_left = effect_left;
        self.effect_right = effect_right;
        render_result?;
        if self.room_amount > 0.0 {
            if let Some(room) = &mut self.room {
                room.process(
                    &mut output_left[..frame_count],
                    &mut output_right[..frame_count],
                    objects.len(),
                );
            }
        }
        Ok(())
    }

    /// Sets deterministic listener velocity in metres per second. `None` disables Doppler.
    pub fn set_listener_velocity(&mut self, velocity: Option<Vec3>) -> Result<(), ModelError> {
        if velocity.is_some_and(|value| {
            !value.x.is_finite() || !value.y.is_finite() || !value.z.is_finite()
        }) {
            return Err(ModelError::InvalidDistance);
        }
        self.listener_velocity = velocity;
        Ok(())
    }

    pub fn listener_velocity(&self) -> Option<Vec3> {
        self.listener_velocity
    }

    /// Sets the global world occlusion amount in `0..=1`.
    pub fn set_occlusion(&mut self, amount: f32) -> Result<(), ModelError> {
        if !amount.is_finite() || !(0.0..=1.0).contains(&amount) {
            return Err(ModelError::InvalidDistance);
        }
        self.occlusion = amount;
        Ok(())
    }

    /// Renders caller-owned planar object audio without constructing per-object descriptors.
    ///
    /// `input` is `object_count` planes of `input_stride` samples. `slots` identifies the stable
    /// renderer state slot for each plane. `object_params` contains azimuth degrees, elevation
    /// degrees, distance, and gain for each object.
    #[allow(clippy::too_many_arguments)]
    pub fn process_planar(
        &mut self,
        input: &[f32],
        input_stride: usize,
        slots: &[u32],
        object_params: &[f32],
        object_count: usize,
        output_left: &mut [f32],
        output_right: &mut [f32],
        frame_count: usize,
    ) -> Result<(), ProcessError> {
        if self.max_objects == 0 || self.max_frames == 0 {
            return Err(ProcessError::NotPrepared);
        }
        if object_count > self.max_objects {
            return Err(ProcessError::TooManyObjects {
                provided: object_count,
                maximum: self.max_objects,
            });
        }
        if frame_count > self.max_frames {
            return Err(ProcessError::FrameCountExceedsPrepared {
                provided: frame_count,
                maximum: self.max_frames,
            });
        }
        if input_stride < frame_count {
            return Err(ProcessError::InputTooShort { object: 0 });
        }
        let input_length = object_count
            .checked_mul(input_stride)
            .ok_or(ProcessError::InputTooShort { object: 0 })?;
        let params_length = object_count
            .checked_mul(4)
            .ok_or(ProcessError::InputTooShort { object: 0 })?;
        if input.len() < input_length
            || slots.len() < object_count
            || object_params.len() < params_length
        {
            return Err(ProcessError::InputTooShort { object: 0 });
        }
        self.validate_slots(slots[..object_count].iter().map(|slot| *slot as usize))?;
        if output_left.len() < frame_count || output_right.len() < frame_count {
            return Err(ProcessError::OutputTooShort);
        }
        for object_index in 0..object_count {
            let params = &object_params[object_index * 4..object_index * 4 + 4];
            if params.iter().any(|value| !value.is_finite()) || params[2] < 0.0 {
                return Err(ProcessError::NonFiniteObject {
                    object: object_index,
                });
            }
        }

        output_left[..frame_count].fill(0.0);
        output_right[..frame_count].fill(0.0);
        for object_index in 0..object_count {
            let input_start = object_index * input_stride;
            let params = &object_params[object_index * 4..object_index * 4 + 4];
            self.render_planar_object(
                slots[object_index] as usize,
                &input[input_start..input_start + frame_count],
                params[0],
                params[1],
                params[2],
                params[3],
                false,
                output_left,
                output_right,
                frame_count,
            )?;
        }
        if self.room_amount > 0.0 {
            if let Some(room) = &mut self.room {
                room.process(
                    &mut output_left[..frame_count],
                    &mut output_right[..frame_count],
                    object_count,
                );
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn render_planar_object(
        &mut self,
        slot: usize,
        mono: &[f32],
        azimuth_deg: f32,
        elevation_deg: f32,
        distance: f32,
        object_gain: f32,
        prefiltered: bool,
        output_left: &mut [f32],
        output_right: &mut [f32],
        frame_count: usize,
    ) -> Result<(), ProcessError> {
        let (gain, air_coefficient) = if prefiltered {
            (object_gain, 1.0)
        } else {
            let distance_gain = self
                .distance_model
                .gain(distance, self.distance_params)
                .map_err(ProcessError::InvalidDistanceModel)?;
            (
                object_gain * distance_gain,
                air_absorption_coefficient(self.grid.sample_rate() as f32, distance)
                    .map_err(ProcessError::InvalidDistanceModel)?,
            )
        };
        let air_state = if prefiltered {
            &mut self.effect_convolution_states[slot]
        } else {
            &mut self.air_states[slot]
        };
        if self.convolution_mode == ConvolutionMode::Partitioned
            && self.interpolation_mode == InterpolationMode::Nearest
        {
            let index = self.grid.nearest_index(azimuth_deg, elevation_deg);
            let direction = index.elevation * self.grid.azimuths().len() + index.azimuth;
            self.partitioned
                .as_mut()
                .expect("partitioned mode is prepared on the control path")
                .process_object(
                    slot,
                    direction,
                    mono,
                    gain,
                    air_coefficient,
                    air_state,
                    output_left,
                    output_right,
                );
            return Ok(());
        }
        let hrir_length = self.grid.hrir_length();
        let history_start = slot * hrir_length;
        let history = &mut self.history[history_start..history_start + hrir_length];
        let write_position = &mut self.write_positions[slot];
        match self.interpolation_mode {
            InterpolationMode::Nearest => {
                let hrir = self.grid.nearest(azimuth_deg, elevation_deg);
                render_object(
                    mono,
                    hrir.left,
                    hrir.right,
                    gain,
                    air_coefficient,
                    history,
                    write_position,
                    air_state,
                    output_left,
                    output_right,
                    frame_count,
                );
            }
            InterpolationMode::Spherical => {
                let ir_start = slot * hrir_length;
                let ir_left = &mut self.interpolated_left[ir_start..ir_start + hrir_length];
                let ir_right = &mut self.interpolated_right[ir_start..ir_start + hrir_length];
                self.spherical
                    .as_ref()
                    .expect("spherical mode is fitted on the control path")
                    .evaluate(&self.grid, azimuth_deg, elevation_deg, ir_left, ir_right);
                render_object(
                    mono,
                    ir_left,
                    ir_right,
                    gain,
                    air_coefficient,
                    history,
                    write_position,
                    air_state,
                    output_left,
                    output_right,
                    frame_count,
                );
            }
        }
        Ok(())
    }

    pub fn reset(&mut self) {
        self.history.fill(0.0);
        self.write_positions.fill(0);
        self.air_states.fill(0.0);
        self.effect_convolution_states.fill(0.0);
        self.effect_input.fill(0.0);
        self.effect_left.fill(0.0);
        self.effect_right.fill(0.0);
        self.occlusion_states.fill(0.0);
        self.resample_ring.fill(0.0);
        self.resample_positions.fill(0);
        self.resample_delays.fill(RESAMPLE_START_DELAY);
        self.decorrelation_ring.fill(0.0);
        self.decorrelation_positions.fill(0);
        if let Some(partitioned) = &mut self.partitioned {
            partitioned.reset();
        }
        if let Some(room) = &mut self.room {
            room.reset();
        }
    }

    /// Clears convolution and air-filter state for one stable object slot.
    pub fn reset_slot(&mut self, slot: usize) -> Result<(), ProcessError> {
        if self.max_objects == 0 || self.max_frames == 0 {
            return Err(ProcessError::NotPrepared);
        }
        if slot >= self.max_objects {
            return Err(ProcessError::SlotOutOfRange {
                slot,
                maximum: self.max_objects,
            });
        }
        let hrir_length = self.grid.hrir_length();
        let history_start = slot * hrir_length;
        self.history[history_start..history_start + hrir_length].fill(0.0);
        self.write_positions[slot] = 0;
        self.air_states[slot] = 0.0;
        self.effect_convolution_states[slot] = 0.0;
        self.occlusion_states[slot] = 0.0;
        self.resample_ring[slot * RESAMPLE_LINE..(slot + 1) * RESAMPLE_LINE].fill(0.0);
        self.resample_positions[slot] = 0;
        self.resample_delays[slot] = RESAMPLE_START_DELAY;
        self.decorrelation_ring[slot * DECORRELATION_LINE..(slot + 1) * DECORRELATION_LINE]
            .fill(0.0);
        self.decorrelation_positions[slot] = 0;
        if let Some(partitioned) = &mut self.partitioned {
            partitioned.reset_slot(slot);
        }
        Ok(())
    }

    pub fn interpolation_mode(&self) -> InterpolationMode {
        self.interpolation_mode
    }

    pub fn hrir_length(&self) -> usize {
        self.grid.hrir_length()
    }

    /// Copies the HRIR selected by the current interpolation mode into caller-owned buffers.
    pub fn get_hrir(
        &mut self,
        azimuth_deg: f32,
        elevation_deg: f32,
        output_left: &mut [f32],
        output_right: &mut [f32],
    ) -> Result<usize, InterpolationError> {
        let length = self.grid.hrir_length();
        assert!(output_left.len() >= length && output_right.len() >= length);
        match self.interpolation_mode {
            InterpolationMode::Nearest => {
                let hrir = self.grid.nearest(azimuth_deg, elevation_deg);
                output_left[..length].copy_from_slice(hrir.left);
                output_right[..length].copy_from_slice(hrir.right);
            }
            InterpolationMode::Spherical => {
                if self.spherical.is_none() {
                    self.spherical = Some(SphericalInterpolator::fit(&self.grid)?);
                }
                self.spherical.as_ref().unwrap().evaluate(
                    &self.grid,
                    azimuth_deg,
                    elevation_deg,
                    &mut output_left[..length],
                    &mut output_right[..length],
                );
            }
        }
        Ok(length)
    }

    pub fn set_interpolation_mode(
        &mut self,
        mode: InterpolationMode,
    ) -> Result<(), InterpolationError> {
        if mode == InterpolationMode::Spherical
            && self.convolution_mode == ConvolutionMode::Partitioned
        {
            return Err(InterpolationError::IncompatibleConvolutionMode);
        }
        if mode == InterpolationMode::Spherical && self.spherical.is_none() {
            self.spherical = Some(SphericalInterpolator::fit(&self.grid)?);
        }
        if mode != self.interpolation_mode {
            self.interpolation_mode = mode;
            self.reset();
        }
        Ok(())
    }

    pub fn convolution_mode(&self) -> ConvolutionMode {
        self.convolution_mode
    }

    pub fn set_convolution_mode(&mut self, mode: ConvolutionMode) -> Result<(), PrepareError> {
        if mode == ConvolutionMode::Partitioned
            && self.interpolation_mode == InterpolationMode::Spherical
        {
            return Err(PrepareError::IncompatibleInterpolationMode);
        }
        if mode != self.convolution_mode {
            let next_partitioned = match (mode, self.max_objects) {
                (ConvolutionMode::Partitioned, max_objects) if max_objects > 0 => {
                    Some(PartitionedConvolver::new(
                        &self.grid,
                        max_objects,
                        self.profile.partition_size(),
                    )?)
                }
                _ => None,
            };
            self.convolution_mode = mode;
            self.partitioned = next_partitioned;
            self.reset();
        }
        Ok(())
    }

    pub fn room_params(&self) -> Option<RoomParams> {
        self.room_params
    }

    pub fn room_amount(&self) -> f32 {
        self.room_amount
    }

    pub fn set_room(&mut self, params: Option<RoomParams>) -> Result<(), RoomError> {
        if let Some(params) = params {
            params.validate()?;
        }
        self.room_params = params;
        self.rebuild_room()
    }

    pub fn set_room_preset(&mut self, preset: Option<RoomPreset>) -> Result<(), RoomError> {
        self.set_room(preset.map(RoomPreset::params))
    }

    pub fn set_room_amount(&mut self, amount: f32) -> Result<(), RoomError> {
        if !amount.is_finite() || !(0.0..=1.0).contains(&amount) {
            return Err(RoomError::InvalidAmount);
        }
        self.room_amount = amount;
        if let Some(room) = &mut self.room {
            room.set_amount(amount)?;
        } else if amount > 0.0 {
            self.rebuild_room()?;
        }
        Ok(())
    }

    pub fn set_distance_model(
        &mut self,
        model: DistanceModel,
        params: DistanceParams,
    ) -> Result<(), ModelError> {
        self.distance_params = params.validate()?;
        self.distance_model = model;
        Ok(())
    }

    fn rebuild_partitioned(&mut self) -> Result<(), PrepareError> {
        self.partitioned = match (self.convolution_mode, self.max_objects) {
            (ConvolutionMode::Partitioned, max_objects) if max_objects > 0 => Some(
                PartitionedConvolver::new(&self.grid, max_objects, self.profile.partition_size())?,
            ),
            _ => None,
        };
        Ok(())
    }

    fn rebuild_room(&mut self) -> Result<(), RoomError> {
        self.room = match (self.room_params, self.max_frames) {
            (Some(params), max_frames) if max_frames > 0 => Some(RoomState::new(
                self.grid.sample_rate(),
                params,
                self.room_amount,
                max_frames,
            )?),
            _ => None,
        };
        Ok(())
    }

    pub fn profile(&self) -> RenderProfile {
        self.profile
    }

    pub fn latency_samples(&self) -> usize {
        match self.convolution_mode {
            ConvolutionMode::Time => 0,
            ConvolutionMode::Partitioned => self.profile.partition_size(),
        }
    }

    pub fn maximum_latency_samples(&self) -> usize {
        self.profile
            .maximum_latency_samples(self.grid.sample_rate())
    }

    pub fn max_objects(&self) -> usize {
        self.max_objects
    }

    pub fn max_frames(&self) -> usize {
        self.max_frames
    }

    fn validate_process(
        &mut self,
        objects: &[ObjectInput<'_>],
        output_left: &[f32],
        output_right: &[f32],
        frame_count: usize,
    ) -> Result<(), ProcessError> {
        if self.max_objects == 0 || self.max_frames == 0 {
            return Err(ProcessError::NotPrepared);
        }
        if objects.len() > self.max_objects {
            return Err(ProcessError::TooManyObjects {
                provided: objects.len(),
                maximum: self.max_objects,
            });
        }
        if frame_count > self.max_frames {
            return Err(ProcessError::FrameCountExceedsPrepared {
                provided: frame_count,
                maximum: self.max_frames,
            });
        }
        if output_left.len() < frame_count || output_right.len() < frame_count {
            return Err(ProcessError::OutputTooShort);
        }
        self.validate_slots(objects.iter().map(|object| object.slot))?;
        for (index, object) in objects.iter().enumerate() {
            if object.mono.len() < frame_count {
                return Err(ProcessError::InputTooShort { object: index });
            }
            if !object.azimuth_deg.is_finite()
                || !object.elevation_deg.is_finite()
                || !object.distance.is_finite()
                || object.distance < 0.0
                || !object.gain.is_finite()
            {
                return Err(ProcessError::NonFiniteObject { object: index });
            }
        }
        Ok(())
    }

    fn validate_slots(
        &mut self,
        slots: impl IntoIterator<Item = usize>,
    ) -> Result<(), ProcessError> {
        self.slot_seen.fill(false);
        for slot in slots {
            if slot >= self.max_objects {
                return Err(ProcessError::SlotOutOfRange {
                    slot,
                    maximum: self.max_objects,
                });
            }
            if self.slot_seen[slot] {
                return Err(ProcessError::DuplicateSlot { slot });
            }
            self.slot_seen[slot] = true;
        }
        Ok(())
    }
}

fn resample_slot(
    samples: &mut [f32],
    slot: usize,
    rate: f32,
    ring: &mut [f32],
    positions: &mut [usize],
    delays: &mut [f32],
) {
    let base = slot * RESAMPLE_LINE;
    let mut write = positions[slot];
    let mut delay = delays[slot];
    for sample in samples {
        ring[base + write] = *sample;
        write = (write + 1) % RESAMPLE_LINE;
        delay = (delay + 1.0 - rate).clamp(RESAMPLE_MIN_DELAY, RESAMPLE_MAX_DELAY);
        let position = (write + RESAMPLE_LINE - 1) as f32 - (delay - 1.0);
        let floor = position.floor();
        let fraction = position - floor;
        let first = (floor as isize).rem_euclid(RESAMPLE_LINE as isize) as usize;
        let second = (first + 1) % RESAMPLE_LINE;
        *sample = ring[base + first] * (1.0 - fraction) + ring[base + second] * fraction;
    }
    positions[slot] = write;
    delays[slot] = delay;
}

fn decorrelate_slot(
    samples: &mut [f32],
    slot: usize,
    delay: f32,
    ring: &mut [f32],
    positions: &mut [usize],
) {
    let base = slot * DECORRELATION_LINE;
    let mut write = positions[slot];
    for sample in samples {
        ring[base + write] = *sample;
        write = (write + 1) % DECORRELATION_LINE;
        let newest = (write + DECORRELATION_LINE - 1) % DECORRELATION_LINE;
        let position = newest as f32 - delay;
        let floor = position.floor();
        let fraction = position - floor;
        let first = (floor as isize).rem_euclid(DECORRELATION_LINE as isize) as usize;
        let second = (first + 1) % DECORRELATION_LINE;
        *sample = ring[base + first] * (1.0 - fraction) + ring[base + second] * fraction;
    }
    positions[slot] = write;
}

#[allow(clippy::too_many_arguments)] // Keeps caller-owned realtime buffers explicit and allocation-free.
fn render_object(
    mono: &[f32],
    hrir_left: &[f32],
    hrir_right: &[f32],
    gain: f32,
    air_coefficient: f32,
    history: &mut [f32],
    write_position: &mut usize,
    air_state: &mut f32,
    output_left: &mut [f32],
    output_right: &mut [f32],
    frame_count: usize,
) {
    let hrir_length = history.len();
    let mut position = *write_position;
    let mut filtered = *air_state;
    for frame in 0..frame_count {
        filtered += air_coefficient * (mono[frame] - filtered);
        history[position] = filtered * gain;
        let mut left = 0.0;
        let mut right = 0.0;
        let mut read_position = position;
        for tap in 0..hrir_length {
            let sample = history[read_position];
            left += sample * hrir_left[tap];
            right += sample * hrir_right[tap];
            read_position = if read_position == 0 {
                hrir_length - 1
            } else {
                read_position - 1
            };
        }
        output_left[frame] += left;
        output_right[frame] += right;
        position += 1;
        if position == hrir_length {
            position = 0;
        }
    }
    *write_position = position;
    *air_state = filtered;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_grid() -> HrtfGrid {
        HrtfGrid::new(
            48_000,
            vec![-90.0, 0.0, 90.0],
            vec![0.0],
            3,
            vec![1.0, 0.5, 0.25, 1.0, 0.25, 0.0, 0.5, 0.25, 0.0],
            vec![0.5, 0.25, 0.0, 1.0, 0.25, 0.0, 1.0, 0.5, 0.25],
        )
        .unwrap()
    }

    fn renderer(max_objects: usize, max_frames: usize) -> BinauralRenderer {
        let mut renderer = BinauralRenderer::new(
            test_grid(),
            RenderProfile::LowLatency,
            DistanceModel::Inverse,
            DistanceParams::default(),
        )
        .unwrap();
        renderer.prepare(max_objects, max_frames).unwrap();
        renderer
    }

    #[test]
    fn profile_exposes_latency_constraints() {
        assert_eq!(
            RenderProfile::Compatibility.maximum_latency_samples(48_000),
            239
        );
        assert_eq!(
            RenderProfile::LowLatency.maximum_latency_samples(48_000),
            239
        );
        let renderer = renderer(1, 8);
        assert_eq!(renderer.latency_samples(), 0);
        assert!(renderer.latency_samples() <= renderer.maximum_latency_samples());
    }

    #[test]
    fn rendering_is_deterministic_and_reset_reproduces_initial_output() {
        let input = [1.0, 0.25, -0.5, 0.0, 0.75, -0.25];
        let object = ObjectInput {
            slot: 0,
            mono: &input,
            azimuth_deg: 0.0,
            elevation_deg: 0.0,
            distance: 1.0,
            gain: 0.8,
        };
        let mut first = renderer(1, input.len());
        let mut second = renderer(1, input.len());
        let mut first_left = [0.0; 6];
        let mut first_right = [0.0; 6];
        let mut second_left = [0.0; 6];
        let mut second_right = [0.0; 6];
        first
            .process(&[object], &mut first_left, &mut first_right, input.len())
            .unwrap();
        second
            .process(&[object], &mut second_left, &mut second_right, input.len())
            .unwrap();
        assert_eq!(first_left, second_left);
        assert_eq!(first_right, second_right);

        first.reset();
        let mut reset_left = [0.0; 6];
        let mut reset_right = [0.0; 6];
        first
            .process(&[object], &mut reset_left, &mut reset_right, input.len())
            .unwrap();
        assert_eq!(reset_left, first_left);
        assert_eq!(reset_right, first_right);
    }

    #[test]
    fn split_blocks_match_a_single_block() {
        let input = [1.0, 0.5, 0.25, 0.0, -0.25, -0.5, 0.75, 0.1];
        let mut whole = renderer(1, input.len());
        let mut split = renderer(1, 5);
        let mut whole_left = [0.0; 8];
        let mut whole_right = [0.0; 8];
        whole
            .process(
                &[ObjectInput {
                    slot: 0,
                    mono: &input,
                    azimuth_deg: 90.0,
                    elevation_deg: 0.0,
                    distance: 2.0,
                    gain: 1.0,
                }],
                &mut whole_left,
                &mut whole_right,
                input.len(),
            )
            .unwrap();

        let mut split_left = [0.0; 8];
        let mut split_right = [0.0; 8];
        split
            .process(
                &[ObjectInput {
                    slot: 0,
                    mono: &input[..3],
                    azimuth_deg: 90.0,
                    elevation_deg: 0.0,
                    distance: 2.0,
                    gain: 1.0,
                }],
                &mut split_left[..3],
                &mut split_right[..3],
                3,
            )
            .unwrap();
        split
            .process(
                &[ObjectInput {
                    slot: 0,
                    mono: &input[3..],
                    azimuth_deg: 90.0,
                    elevation_deg: 0.0,
                    distance: 2.0,
                    gain: 1.0,
                }],
                &mut split_left[3..],
                &mut split_right[3..],
                5,
            )
            .unwrap();
        assert_eq!(split_left, whole_left);
        assert_eq!(split_right, whole_right);
    }

    #[test]
    fn rejects_unprepared_and_out_of_capacity_calls() {
        let input = [0.0; 4];
        let object = ObjectInput {
            slot: 0,
            mono: &input,
            azimuth_deg: 0.0,
            elevation_deg: 0.0,
            distance: 1.0,
            gain: 1.0,
        };
        let mut renderer = BinauralRenderer::new(
            test_grid(),
            RenderProfile::LowLatency,
            DistanceModel::Inverse,
            DistanceParams::default(),
        )
        .unwrap();
        let mut left = [0.0; 4];
        let mut right = [0.0; 4];
        assert_eq!(
            renderer.process(&[object], &mut left, &mut right, 4),
            Err(ProcessError::NotPrepared)
        );

        renderer.prepare(1, 3).unwrap();
        assert_eq!(
            renderer.process(&[object, object], &mut left, &mut right, 3),
            Err(ProcessError::TooManyObjects {
                provided: 2,
                maximum: 1
            })
        );
        assert_eq!(
            renderer.process(&[object], &mut left, &mut right, 4),
            Err(ProcessError::FrameCountExceedsPrepared {
                provided: 4,
                maximum: 3
            })
        );
    }

    #[test]
    fn multiple_objects_sum_independent_slots() {
        let a = [1.0, 0.0, 0.0, 0.0];
        let b = [0.0, 1.0, 0.0, 0.0];
        let objects = [
            ObjectInput {
                slot: 0,
                mono: &a,
                azimuth_deg: -90.0,
                elevation_deg: 0.0,
                distance: 1.0,
                gain: 1.0,
            },
            ObjectInput {
                slot: 1,
                mono: &b,
                azimuth_deg: 90.0,
                elevation_deg: 0.0,
                distance: 1.0,
                gain: 0.5,
            },
        ];
        let mut combined = renderer(2, 4);
        let mut left = [0.0; 4];
        let mut right = [0.0; 4];
        combined
            .process(&objects, &mut left, &mut right, 4)
            .unwrap();

        let mut expected_left = [0.0; 4];
        let mut expected_right = [0.0; 4];
        for object in objects {
            let object = ObjectInput { slot: 0, ..object };
            let mut single = renderer(1, 4);
            let mut object_left = [0.0; 4];
            let mut object_right = [0.0; 4];
            single
                .process(&[object], &mut object_left, &mut object_right, 4)
                .unwrap();
            for frame in 0..4 {
                expected_left[frame] += object_left[frame];
                expected_right[frame] += object_right[frame];
            }
        }
        assert_eq!(left, expected_left);
        assert_eq!(right, expected_right);
    }

    fn object<'a>(slot: usize, mono: &'a [f32]) -> ObjectInput<'a> {
        ObjectInput {
            slot,
            mono,
            azimuth_deg: 0.0,
            elevation_deg: 0.0,
            distance: 1.0,
            gain: 1.0,
        }
    }

    #[test]
    fn stable_slots_survive_deletion_reordering_and_temporary_absence() {
        let impulse_a = [1.0, 0.0];
        let impulse_b = [0.5, 0.0];
        let silence = [0.0; 2];
        let signal_a = [0.25, -0.5];
        let signal_b = [-0.75, 0.25];
        let mut subject = renderer(2, 2);
        let mut scratch_left = [0.0; 2];
        let mut scratch_right = [0.0; 2];
        subject
            .process(
                &[object(0, &impulse_a), object(1, &impulse_b)],
                &mut scratch_left,
                &mut scratch_right,
                2,
            )
            .unwrap();
        subject
            .process(
                &[object(0, &silence)],
                &mut scratch_left,
                &mut scratch_right,
                2,
            )
            .unwrap();
        let mut actual_left = [0.0; 2];
        let mut actual_right = [0.0; 2];
        subject
            .process(
                &[object(1, &signal_b), object(0, &signal_a)],
                &mut actual_left,
                &mut actual_right,
                2,
            )
            .unwrap();

        let mut expected_left = [0.0; 2];
        let mut expected_right = [0.0; 2];
        for (impulse, middle, signal) in [
            (&impulse_a[..], Some(&silence[..]), &signal_a[..]),
            (&impulse_b[..], None, &signal_b[..]),
        ] {
            let mut reference = renderer(1, 2);
            reference
                .process(
                    &[object(0, impulse)],
                    &mut scratch_left,
                    &mut scratch_right,
                    2,
                )
                .unwrap();
            if let Some(middle) = middle {
                reference
                    .process(
                        &[object(0, middle)],
                        &mut scratch_left,
                        &mut scratch_right,
                        2,
                    )
                    .unwrap();
            }
            let mut left = [0.0; 2];
            let mut right = [0.0; 2];
            reference
                .process(&[object(0, signal)], &mut left, &mut right, 2)
                .unwrap();
            for frame in 0..2 {
                expected_left[frame] += left[frame];
                expected_right[frame] += right[frame];
            }
        }
        assert_eq!(actual_left, expected_left);
        assert_eq!(actual_right, expected_right);
    }

    #[test]
    fn rejects_duplicate_and_out_of_range_slots() {
        let input = [0.0; 2];
        let mut renderer = renderer(2, 2);
        let mut left = [0.0; 2];
        let mut right = [0.0; 2];
        assert_eq!(
            renderer.process(
                &[object(1, &input), object(1, &input)],
                &mut left,
                &mut right,
                2,
            ),
            Err(ProcessError::DuplicateSlot { slot: 1 })
        );
        assert_eq!(
            renderer.process(&[object(2, &input)], &mut left, &mut right, 2),
            Err(ProcessError::SlotOutOfRange {
                slot: 2,
                maximum: 2,
            })
        );
    }

    #[test]
    fn reset_slot_clears_only_the_selected_object_state() {
        let impulse = [1.0, 0.0];
        let continuation = [0.25, 0.0];
        let mut subject = renderer(2, 2);
        let mut left = [0.0; 2];
        let mut right = [0.0; 2];
        subject
            .process(
                &[object(0, &impulse), object(1, &impulse)],
                &mut left,
                &mut right,
                2,
            )
            .unwrap();
        subject.reset_slot(0).unwrap();

        let mut reset_left = [0.0; 2];
        let mut reset_right = [0.0; 2];
        subject
            .process(
                &[object(0, &continuation)],
                &mut reset_left,
                &mut reset_right,
                2,
            )
            .unwrap();
        let mut fresh = renderer(1, 2);
        let mut fresh_left = [0.0; 2];
        let mut fresh_right = [0.0; 2];
        fresh
            .process(
                &[object(0, &continuation)],
                &mut fresh_left,
                &mut fresh_right,
                2,
            )
            .unwrap();
        assert_eq!(reset_left, fresh_left);
        assert_eq!(reset_right, fresh_right);

        let mut preserved_left = [0.0; 2];
        let mut preserved_right = [0.0; 2];
        subject
            .process(
                &[object(1, &continuation)],
                &mut preserved_left,
                &mut preserved_right,
                2,
            )
            .unwrap();
        assert_ne!(preserved_left, fresh_left);
        assert_ne!(preserved_right, fresh_right);
        assert_eq!(
            subject.reset_slot(2),
            Err(ProcessError::SlotOutOfRange {
                slot: 2,
                maximum: 2,
            })
        );
    }
}
