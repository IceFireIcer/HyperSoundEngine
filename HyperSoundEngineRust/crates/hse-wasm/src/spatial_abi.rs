//! Stable C-style spatial ABI exported by the wasm boundary crate.
//!
//! Control calls may allocate. `spatial_render_objects` only borrows caller memory and uses
//! storage prepared by `spatial_load_hrtf`; it does not allocate, lock, or format strings.

use std::cell::RefCell;

use hrtf_core::{
    load_sofa_bytes, BinauralRenderer, ConvolutionMode, DistanceModel, DistanceParams,
    InterpolationMode, RenderProfile, RoomParams, RoomPreset, SofaGridOptions,
};

const MAX_HANDLES: usize = 64;
const ERROR_CAPACITY: usize = 512;
const OBJECT_PARAM_WIDTH: usize = 4;
const SPATIAL_ABI_VERSION: u32 = 1;

/// Returns the stable spatial ABI version supported by this binary.
#[no_mangle]
pub extern "C" fn spatial_abi_version() -> u32 {
    SPATIAL_ABI_VERSION
}

pub const SPATIAL_OK: i32 = 0;
pub const SPATIAL_INVALID_HANDLE: i32 = -1;
pub const SPATIAL_INVALID_ARGUMENT: i32 = -2;
pub const SPATIAL_BUFFER_TOO_SMALL: i32 = -3;
pub const SPATIAL_PARSE_ERROR: i32 = -4;
pub const SPATIAL_CAPACITY_EXCEEDED: i32 = -5;
pub const SPATIAL_UNSUPPORTED: i32 = -6;
pub const SPATIAL_INTERNAL_ERROR: i32 = -7;

#[derive(Clone)]
struct LastError {
    code: i32,
    length: usize,
    bytes: [u8; ERROR_CAPACITY],
}

impl Default for LastError {
    fn default() -> Self {
        Self {
            code: SPATIAL_OK,
            length: 0,
            bytes: [0; ERROR_CAPACITY],
        }
    }
}

impl LastError {
    fn clear(&mut self) {
        self.code = SPATIAL_OK;
        self.length = 0;
    }

    fn set(&mut self, code: i32, message: &str) {
        self.code = code;
        self.length = message.len().min(ERROR_CAPACITY - 1);
        self.bytes[..self.length].copy_from_slice(&message.as_bytes()[..self.length]);
        self.bytes[self.length] = 0;
    }
}

struct SpatialInstance {
    renderer: BinauralRenderer,
    error: LastError,
}

struct Slot {
    generation: u16,
    instance: Option<SpatialInstance>,
}

struct Registry {
    slots: Vec<Slot>,
}

impl Registry {
    fn new() -> Self {
        let mut slots = Vec::with_capacity(MAX_HANDLES);
        for _ in 0..MAX_HANDLES {
            slots.push(Slot {
                generation: 1,
                instance: None,
            });
        }
        Self { slots }
    }

    fn insert(&mut self, instance: SpatialInstance) -> Option<u32> {
        let (index, slot) = self
            .slots
            .iter_mut()
            .enumerate()
            .find(|(_, slot)| slot.instance.is_none())?;
        slot.instance = Some(instance);
        Some(make_handle(index, slot.generation))
    }

    fn get_mut(&mut self, handle: u32) -> Option<&mut SpatialInstance> {
        let (index, generation) = split_handle(handle)?;
        let slot = self.slots.get_mut(index)?;
        if slot.generation != generation {
            return None;
        }
        slot.instance.as_mut()
    }

    fn remove(&mut self, handle: u32) -> bool {
        let Some((index, generation)) = split_handle(handle) else {
            return false;
        };
        let Some(slot) = self.slots.get_mut(index) else {
            return false;
        };
        if slot.generation != generation || slot.instance.is_none() {
            return false;
        }
        slot.instance = None;
        slot.generation = slot.generation.wrapping_add(1).max(1);
        true
    }
}

thread_local! {
    static REGISTRY: RefCell<Registry> = RefCell::new(Registry::new());
    static GLOBAL_ERROR: RefCell<LastError> = RefCell::new(LastError::default());
}

fn make_handle(index: usize, generation: u16) -> u32 {
    ((generation as u32) << 16) | (index as u32 + 1)
}

fn split_handle(handle: u32) -> Option<(usize, u16)> {
    let slot = (handle & 0xffff) as usize;
    let generation = (handle >> 16) as u16;
    if slot == 0 || generation == 0 {
        return None;
    }
    Some((slot - 1, generation))
}

fn set_global_error(code: i32, message: &str) -> i32 {
    GLOBAL_ERROR.with(|error| error.borrow_mut().set(code, message));
    code
}

fn with_instance(handle: u32, operation: impl FnOnce(&mut SpatialInstance) -> i32) -> i32 {
    REGISTRY.with(|registry| {
        let mut registry = registry.borrow_mut();
        let Some(instance) = registry.get_mut(handle) else {
            return set_global_error(SPATIAL_INVALID_HANDLE, "invalid or stale spatial handle");
        };
        instance.error.clear();
        operation(instance)
    })
}

fn control_error(instance: &mut SpatialInstance, code: i32, message: impl AsRef<str>) -> i32 {
    instance.error.set(code, message.as_ref());
    code
}

fn render_error(instance: &mut SpatialInstance, code: i32, message: &'static str) -> i32 {
    instance.error.set(code, message);
    code
}

fn checked_bytes<T>(length: usize) -> Option<usize> {
    length.checked_mul(std::mem::size_of::<T>())
}

fn valid_pointer<T>(pointer: *const T, length: usize) -> bool {
    !pointer.is_null()
        && (pointer as usize) % std::mem::align_of::<T>() == 0
        && checked_bytes::<T>(length)
            .filter(|bytes| *bytes <= isize::MAX as usize)
            .and_then(|bytes| (pointer as usize).checked_add(bytes))
            .is_some()
}

fn valid_mut_pointer<T>(pointer: *mut T, length: usize) -> bool {
    valid_pointer(pointer.cast_const(), length)
}

fn ranges_overlap<T, U>(
    left: *const T,
    left_len: usize,
    right: *const U,
    right_len: usize,
) -> bool {
    if left_len == 0 || right_len == 0 {
        return false;
    }
    let Some(left_bytes) = checked_bytes::<T>(left_len) else {
        return true;
    };
    let Some(right_bytes) = checked_bytes::<U>(right_len) else {
        return true;
    };
    let left_start = left as usize;
    let right_start = right as usize;
    let Some(left_end) = left_start.checked_add(left_bytes) else {
        return true;
    };
    let Some(right_end) = right_start.checked_add(right_bytes) else {
        return true;
    };
    left_start < right_end && right_start < left_end
}

/// Loads SOFA bytes, prepares all realtime storage, and returns a non-zero owned handle.
/// Returns zero on failure; use `spatial_last_error_code(0)` and `spatial_last_error_copy`.
#[no_mangle]
pub unsafe extern "C" fn spatial_load_hrtf(
    data_ptr: *const u8,
    data_len: usize,
    sample_rate: u32,
    max_objects: u32,
    max_frames: u32,
) -> u32 {
    GLOBAL_ERROR.with(|error| error.borrow_mut().clear());
    if data_len == 0
        || !valid_pointer(data_ptr, data_len)
        || sample_rate == 0
        || max_objects == 0
        || max_frames == 0
    {
        set_global_error(
            SPATIAL_INVALID_ARGUMENT,
            "SOFA pointer/length, sample rate, max objects, and max frames must be valid",
        );
        return 0;
    }
    let bytes = unsafe { std::slice::from_raw_parts(data_ptr, data_len) };
    let options = SofaGridOptions {
        sample_rate,
        ..SofaGridOptions::default()
    };
    let grid = match load_sofa_bytes(bytes, &options) {
        Ok(grid) => grid,
        Err(error) => {
            set_global_error(SPATIAL_PARSE_ERROR, &error.to_string());
            return 0;
        }
    };
    let mut renderer = match BinauralRenderer::new(
        grid,
        RenderProfile::LowLatency,
        DistanceModel::Inverse,
        DistanceParams::default(),
    ) {
        Ok(renderer) => renderer,
        Err(error) => {
            set_global_error(SPATIAL_INVALID_ARGUMENT, &error.to_string());
            return 0;
        }
    };
    if let Err(error) = renderer.prepare(max_objects as usize, max_frames as usize) {
        set_global_error(SPATIAL_CAPACITY_EXCEEDED, &error.to_string());
        return 0;
    }
    let instance = SpatialInstance {
        renderer,
        error: LastError::default(),
    };
    match REGISTRY.with(|registry| registry.borrow_mut().insert(instance)) {
        Some(handle) => handle,
        None => {
            set_global_error(SPATIAL_CAPACITY_EXCEEDED, "spatial handle table is full");
            0
        }
    }
}

/// Returns the per-ear HRIR length in `f32` elements, or zero for an invalid handle.
#[no_mangle]
pub extern "C" fn spatial_hrir_length(handle: u32) -> usize {
    REGISTRY.with(|registry| {
        registry
            .borrow_mut()
            .get_mut(handle)
            .map(|instance| instance.renderer.hrir_length())
            .unwrap_or_else(|| {
                set_global_error(SPATIAL_INVALID_HANDLE, "invalid or stale spatial handle");
                0
            })
    })
}

/// Copies one HRIR pair. Output lengths are counts of `f32` elements, not bytes.
#[no_mangle]
pub unsafe extern "C" fn spatial_get_hrir(
    handle: u32,
    azimuth_deg: f32,
    elevation_deg: f32,
    output_left_ptr: *mut f32,
    output_left_len: usize,
    output_right_ptr: *mut f32,
    output_right_len: usize,
) -> i32 {
    with_instance(handle, |instance| {
        if !azimuth_deg.is_finite() || !elevation_deg.is_finite() {
            return control_error(instance, SPATIAL_INVALID_ARGUMENT, "angles must be finite");
        }
        let required = instance.renderer.hrir_length();
        if output_left_len < required || output_right_len < required {
            return control_error(
                instance,
                SPATIAL_BUFFER_TOO_SMALL,
                "HRIR output buffers are shorter than the loaded HRIR",
            );
        }
        if !valid_mut_pointer(output_left_ptr, required)
            || !valid_mut_pointer(output_right_ptr, required)
            || ranges_overlap(
                output_left_ptr.cast_const(),
                required,
                output_right_ptr.cast_const(),
                required,
            )
        {
            return control_error(
                instance,
                SPATIAL_INVALID_ARGUMENT,
                "invalid or overlapping HRIR output pointer",
            );
        }
        let left = unsafe { std::slice::from_raw_parts_mut(output_left_ptr, required) };
        let right = unsafe { std::slice::from_raw_parts_mut(output_right_ptr, required) };
        match instance
            .renderer
            .get_hrir(azimuth_deg, elevation_deg, left, right)
        {
            Ok(length) => i32::try_from(length).unwrap_or(SPATIAL_CAPACITY_EXCEEDED),
            Err(error) => control_error(instance, SPATIAL_INTERNAL_ERROR, error.to_string()),
        }
    })
}

/// Renders object-major planar mono input to planar stereo output.
///
/// `input_len`, `object_params_len`, and output lengths are counts of `f32` elements;
/// `object_slots_len` is a count of `u32` elements. Each object
/// starts at `input_ptr + object_index * input_stride`; the first `frame_count` samples are read.
/// `object_slots` contains one stable renderer state slot per object. Parameters are
/// `[azimuth_deg, elevation_deg, distance, gain]` repeated per object.
#[no_mangle]
pub unsafe extern "C" fn spatial_render_objects(
    handle: u32,
    input_ptr: *const f32,
    input_len: usize,
    input_stride: usize,
    object_slots_ptr: *const u32,
    object_slots_len: usize,
    object_params_ptr: *const f32,
    object_params_len: usize,
    object_count: u32,
    output_left_ptr: *mut f32,
    output_left_len: usize,
    output_right_ptr: *mut f32,
    output_right_len: usize,
    frame_count: u32,
) -> i32 {
    with_instance(handle, |instance| {
        let object_count = object_count as usize;
        let frame_count = frame_count as usize;
        if frame_count == 0 || input_stride < frame_count {
            return render_error(
                instance,
                SPATIAL_INVALID_ARGUMENT,
                "frame count must be non-zero and no greater than input stride",
            );
        }
        if object_count > instance.renderer.max_objects()
            || frame_count > instance.renderer.max_frames()
        {
            return render_error(
                instance,
                SPATIAL_CAPACITY_EXCEEDED,
                "render capacity exceeded",
            );
        }
        let Some(required_input) = object_count.checked_mul(input_stride) else {
            return render_error(instance, SPATIAL_INVALID_ARGUMENT, "input length overflow");
        };
        let Some(required_params) = object_count.checked_mul(OBJECT_PARAM_WIDTH) else {
            return render_error(
                instance,
                SPATIAL_INVALID_ARGUMENT,
                "parameter length overflow",
            );
        };
        if input_len < required_input
            || object_slots_len < object_count
            || object_params_len < required_params
            || output_left_len < frame_count
            || output_right_len < frame_count
        {
            return render_error(
                instance,
                SPATIAL_BUFFER_TOO_SMALL,
                "render buffer is too short",
            );
        }
        let invalid_pointer = (required_input > 0 && !valid_pointer(input_ptr, required_input))
            || (object_count > 0 && !valid_pointer(object_slots_ptr, object_count))
            || (required_params > 0 && !valid_pointer(object_params_ptr, required_params))
            || !valid_mut_pointer(output_left_ptr, frame_count)
            || !valid_mut_pointer(output_right_ptr, frame_count);
        let overlapping_output = ranges_overlap(
            output_left_ptr.cast_const(),
            frame_count,
            output_right_ptr.cast_const(),
            frame_count,
        );
        let input_overlaps_output = required_input > 0
            && (ranges_overlap(
                input_ptr,
                required_input,
                output_left_ptr.cast_const(),
                frame_count,
            ) || ranges_overlap(
                input_ptr,
                required_input,
                output_right_ptr.cast_const(),
                frame_count,
            ));
        let slots_overlap_output = object_count > 0
            && (ranges_overlap(
                object_slots_ptr,
                object_count,
                output_left_ptr.cast_const(),
                frame_count,
            ) || ranges_overlap(
                object_slots_ptr,
                object_count,
                output_right_ptr.cast_const(),
                frame_count,
            ));
        let params_overlap_output = required_params > 0
            && (ranges_overlap(
                object_params_ptr,
                required_params,
                output_left_ptr.cast_const(),
                frame_count,
            ) || ranges_overlap(
                object_params_ptr,
                required_params,
                output_right_ptr.cast_const(),
                frame_count,
            ));
        if invalid_pointer
            || overlapping_output
            || input_overlaps_output
            || slots_overlap_output
            || params_overlap_output
        {
            return render_error(
                instance,
                SPATIAL_INVALID_ARGUMENT,
                "invalid or overlapping render pointer",
            );
        }
        let input = if required_input == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(input_ptr, required_input) }
        };
        let slots = if object_count == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(object_slots_ptr, object_count) }
        };
        let params = if required_params == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(object_params_ptr, required_params) }
        };
        let left = unsafe { std::slice::from_raw_parts_mut(output_left_ptr, frame_count) };
        let right = unsafe { std::slice::from_raw_parts_mut(output_right_ptr, frame_count) };
        match instance.renderer.process_planar(
            input,
            input_stride,
            slots,
            params,
            object_count,
            left,
            right,
            frame_count,
        ) {
            Ok(()) => SPATIAL_OK,
            Err(_) => render_error(
                instance,
                SPATIAL_INVALID_ARGUMENT,
                "renderer rejected the block",
            ),
        }
    })
}

#[no_mangle]
pub extern "C" fn spatial_set_room(
    handle: u32,
    width: f32,
    height: f32,
    depth: f32,
    reflectivity: f32,
    early_orders: u32,
    rt60: f32,
    amount: f32,
) -> i32 {
    with_instance(handle, |instance| {
        if !amount.is_finite() || !(0.0..=1.0).contains(&amount) {
            return control_error(
                instance,
                SPATIAL_INVALID_ARGUMENT,
                "room amount must be finite and between zero and one",
            );
        }
        let Ok(early_orders) = u8::try_from(early_orders) else {
            return control_error(instance, SPATIAL_INVALID_ARGUMENT, "early orders exceed u8");
        };
        let params = RoomParams {
            width,
            height,
            depth,
            reflectivity,
            early_orders,
            rt60,
        };
        if let Err(error) = instance.renderer.set_room(Some(params)) {
            return control_error(instance, SPATIAL_INVALID_ARGUMENT, error.to_string());
        }
        match instance.renderer.set_room_amount(amount) {
            Ok(()) => SPATIAL_OK,
            Err(error) => control_error(instance, SPATIAL_INVALID_ARGUMENT, error.to_string()),
        }
    })
}

#[no_mangle]
pub extern "C" fn spatial_set_room_preset(handle: u32, preset: u32, amount: f32) -> i32 {
    with_instance(handle, |instance| {
        if !amount.is_finite() || !(0.0..=1.0).contains(&amount) {
            return control_error(
                instance,
                SPATIAL_INVALID_ARGUMENT,
                "room amount must be finite and between zero and one",
            );
        }
        let preset = match preset {
            0 => RoomPreset::Studio,
            1 => RoomPreset::Hall,
            2 => RoomPreset::Stage,
            3 => RoomPreset::Church,
            4 => RoomPreset::Outdoor,
            5 => RoomPreset::Bathroom,
            6 => RoomPreset::Corridor,
            _ => return control_error(instance, SPATIAL_INVALID_ARGUMENT, "unknown room preset"),
        };
        if let Err(error) = instance.renderer.set_room_preset(Some(preset)) {
            return control_error(instance, SPATIAL_INVALID_ARGUMENT, error.to_string());
        }
        match instance.renderer.set_room_amount(amount) {
            Ok(()) => SPATIAL_OK,
            Err(error) => control_error(instance, SPATIAL_INVALID_ARGUMENT, error.to_string()),
        }
    })
}

#[no_mangle]
pub extern "C" fn spatial_set_hrtf_interp_mode(handle: u32, mode: u32) -> i32 {
    with_instance(handle, |instance| {
        let mode = match mode {
            0 => InterpolationMode::Nearest,
            1 => InterpolationMode::Spherical,
            _ => {
                return control_error(
                    instance,
                    SPATIAL_INVALID_ARGUMENT,
                    "unknown HRTF interpolation mode",
                )
            }
        };
        match instance.renderer.set_interpolation_mode(mode) {
            Ok(()) => SPATIAL_OK,
            Err(error) => control_error(instance, SPATIAL_INVALID_ARGUMENT, error.to_string()),
        }
    })
}

#[no_mangle]
pub extern "C" fn spatial_set_convolution_mode(handle: u32, mode: u32) -> i32 {
    with_instance(handle, |instance| {
        let mode = match mode {
            0 => ConvolutionMode::Time,
            1 => ConvolutionMode::Partitioned,
            _ => {
                return control_error(
                    instance,
                    SPATIAL_INVALID_ARGUMENT,
                    "unknown convolution mode",
                )
            }
        };
        match instance.renderer.set_convolution_mode(mode) {
            Ok(()) => SPATIAL_OK,
            Err(error) => control_error(instance, SPATIAL_INVALID_ARGUMENT, error.to_string()),
        }
    })
}

#[no_mangle]
pub extern "C" fn spatial_set_distance_model(
    handle: u32,
    model: u32,
    reference_distance: f32,
    maximum_distance: f32,
    rolloff_factor: f32,
) -> i32 {
    with_instance(handle, |instance| {
        let model = match model {
            0 => DistanceModel::Inverse,
            1 => DistanceModel::Linear,
            2 => DistanceModel::Exponential,
            _ => {
                return control_error(instance, SPATIAL_INVALID_ARGUMENT, "unknown distance model")
            }
        };
        let params = DistanceParams {
            reference_distance,
            maximum_distance,
            rolloff_factor,
        };
        match instance.renderer.set_distance_model(model, params) {
            Ok(()) => SPATIAL_OK,
            Err(error) => control_error(instance, SPATIAL_INVALID_ARGUMENT, error.to_string()),
        }
    })
}

#[no_mangle]
pub extern "C" fn spatial_reset_slot(handle: u32, slot: u32) -> i32 {
    with_instance(handle, |instance| {
        match instance.renderer.reset_slot(slot as usize) {
            Ok(()) => SPATIAL_OK,
            Err(_) => control_error(
                instance,
                SPATIAL_INVALID_ARGUMENT,
                "slot is outside the prepared renderer capacity",
            ),
        }
    })
}

/// Releases a handle. All pointers previously associated with it become invalid.
#[no_mangle]
pub extern "C" fn spatial_destroy(handle: u32) -> i32 {
    if REGISTRY.with(|registry| registry.borrow_mut().remove(handle)) {
        SPATIAL_OK
    } else {
        set_global_error(SPATIAL_INVALID_HANDLE, "invalid or stale spatial handle")
    }
}

#[no_mangle]
pub extern "C" fn spatial_last_error_code(handle: u32) -> i32 {
    if handle == 0 {
        return GLOBAL_ERROR.with(|error| error.borrow().code);
    }
    REGISTRY.with(|registry| {
        registry
            .borrow_mut()
            .get_mut(handle)
            .map(|instance| instance.error.code)
            .unwrap_or(SPATIAL_INVALID_HANDLE)
    })
}

/// Copies a nul-terminated UTF-8 error message and returns the required byte count including nul.
#[no_mangle]
pub unsafe extern "C" fn spatial_last_error_copy(
    handle: u32,
    output_ptr: *mut u8,
    output_len: usize,
) -> usize {
    let error = if handle == 0 {
        GLOBAL_ERROR.with(|error| error.borrow().clone())
    } else {
        REGISTRY.with(|registry| {
            registry
                .borrow_mut()
                .get_mut(handle)
                .map(|instance| instance.error.clone())
                .unwrap_or_else(|| {
                    let mut error = LastError::default();
                    error.set(SPATIAL_INVALID_HANDLE, "invalid or stale spatial handle");
                    error
                })
        })
    };
    let required = error.length + 1;
    if output_len < required || (required > 0 && !valid_mut_pointer(output_ptr, required)) {
        return required;
    }
    let output = unsafe { std::slice::from_raw_parts_mut(output_ptr, required) };
    output[..error.length].copy_from_slice(&error.bytes[..error.length]);
    output[error.length] = 0;
    required
}

#[cfg(test)]
mod tests {
    use super::*;
    use hrtf_core::HrtfGrid;

    fn test_handle() -> u32 {
        let grid = HrtfGrid::new(
            48_000,
            vec![-90.0, 0.0, 90.0],
            vec![0.0],
            2,
            vec![1.0, 0.0, 0.5, 0.0, 0.25, 0.0],
            vec![0.25, 0.0, 0.5, 0.0, 1.0, 0.0],
        )
        .unwrap();
        let mut renderer = BinauralRenderer::new(
            grid,
            RenderProfile::LowLatency,
            DistanceModel::Inverse,
            DistanceParams::default(),
        )
        .unwrap();
        renderer.prepare(2, 128).unwrap();
        REGISTRY.with(|registry| {
            registry
                .borrow_mut()
                .insert(SpatialInstance {
                    renderer,
                    error: LastError::default(),
                })
                .unwrap()
        })
    }

    #[test]
    fn abi_version_is_stable() {
        assert_eq!(spatial_abi_version(), 1);
        assert!(include_str!("../include/hypersoundengine_spatial.h")
            .contains("#define HSE_SPATIAL_ABI_VERSION 1u"));
    }

    #[test]
    fn exports_render_and_hrir_layouts() {
        let handle = test_handle();
        assert_eq!(spatial_hrir_length(handle), 2);
        let input = [1.0, 0.0, 0.0, 0.0];
        let slots = [0_u32];
        let params = [0.0, 0.0, 1.0, 1.0];
        let mut left = [9.0; 4];
        let mut right = [9.0; 4];
        assert_eq!(
            unsafe {
                spatial_render_objects(
                    handle,
                    input.as_ptr(),
                    input.len(),
                    4,
                    slots.as_ptr(),
                    slots.len(),
                    params.as_ptr(),
                    params.len(),
                    1,
                    left.as_mut_ptr(),
                    left.len(),
                    right.as_mut_ptr(),
                    right.len(),
                    4,
                )
            },
            SPATIAL_OK
        );
        assert_eq!(left, right);
        assert!(left[0].is_finite() && left[0] > 0.0);
        assert!(left.windows(2).all(|window| window[1] < window[0]));

        let mut hrir_left = [0.0; 2];
        let mut hrir_right = [0.0; 2];
        assert_eq!(
            unsafe {
                spatial_get_hrir(
                    handle,
                    90.0,
                    0.0,
                    hrir_left.as_mut_ptr(),
                    2,
                    hrir_right.as_mut_ptr(),
                    2,
                )
            },
            2
        );
        assert_eq!(hrir_left, [0.25, 0.0]);
        assert_eq!(hrir_right, [1.0, 0.0]);
        assert_eq!(spatial_reset_slot(handle, 0), SPATIAL_OK);
        assert_eq!(spatial_reset_slot(handle, 2), SPATIAL_INVALID_ARGUMENT);
        assert_eq!(spatial_destroy(handle), SPATIAL_OK);
    }

    #[test]
    fn load_rejects_invalid_sofa_and_exposes_global_error() {
        let bytes = b"not a SOFA file";
        assert_eq!(
            unsafe { spatial_load_hrtf(bytes.as_ptr(), bytes.len(), 48_000, 2, 128) },
            0
        );
        assert_eq!(spatial_last_error_code(0), SPATIAL_PARSE_ERROR);
        let required = unsafe { spatial_last_error_copy(0, std::ptr::null_mut(), 0) };
        assert!(required > 1);
        let mut message = vec![0; required];
        assert_eq!(
            unsafe { spatial_last_error_copy(0, message.as_mut_ptr(), message.len()) },
            required
        );
        assert!(std::str::from_utf8(&message[..required - 1])
            .unwrap()
            .contains("SOFA"));
    }

    #[test]
    fn control_functions_validate_modes_and_parameters() {
        let handle = test_handle();
        assert_eq!(spatial_set_convolution_mode(handle, 0), SPATIAL_OK);
        assert_eq!(spatial_set_convolution_mode(handle, 1), SPATIAL_OK);
        assert_eq!(
            spatial_set_convolution_mode(handle, 99),
            SPATIAL_INVALID_ARGUMENT
        );
        assert_eq!(
            spatial_set_hrtf_interp_mode(handle, 99),
            SPATIAL_INVALID_ARGUMENT
        );
        assert_eq!(
            spatial_set_distance_model(handle, 1, 1.0, 10.0, 0.5),
            SPATIAL_OK
        );
        assert_eq!(
            spatial_set_distance_model(handle, 2, 1.0, 0.5, 1.0),
            SPATIAL_INVALID_ARGUMENT
        );
        assert_eq!(
            spatial_set_room_preset(handle, 0, f32::NAN),
            SPATIAL_INVALID_ARGUMENT
        );
        assert_eq!(
            spatial_set_room(handle, 5.0, 3.0, 4.0, 0.25, 2, 0.45, 0.5),
            SPATIAL_OK
        );
        assert_eq!(
            spatial_set_room(handle, -1.0, 3.0, 4.0, 0.25, 2, 0.45, 0.5),
            SPATIAL_INVALID_ARGUMENT
        );
        assert_eq!(spatial_destroy(handle), SPATIAL_OK);
    }

    #[test]
    fn render_rejects_duplicate_slots() {
        let handle = test_handle();
        let input = [1.0, 0.0, 0.5, 0.0];
        let slots = [1_u32, 1_u32];
        let params = [0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 1.0, 1.0];
        let mut left = [0.0; 2];
        let mut right = [0.0; 2];
        assert_eq!(
            unsafe {
                spatial_render_objects(
                    handle,
                    input.as_ptr(),
                    input.len(),
                    2,
                    slots.as_ptr(),
                    slots.len(),
                    params.as_ptr(),
                    params.len(),
                    2,
                    left.as_mut_ptr(),
                    left.len(),
                    right.as_mut_ptr(),
                    right.len(),
                    2,
                )
            },
            SPATIAL_INVALID_ARGUMENT
        );
        assert_eq!(spatial_destroy(handle), SPATIAL_OK);
    }

    #[test]
    fn rejects_bad_buffers_and_accepts_partitioned_mode() {
        let handle = test_handle();
        let mut output = [0.0; 4];
        assert_eq!(
            unsafe {
                spatial_render_objects(
                    handle,
                    std::ptr::null(),
                    4,
                    4,
                    std::ptr::null(),
                    1,
                    std::ptr::null(),
                    4,
                    1,
                    output.as_mut_ptr(),
                    4,
                    output.as_mut_ptr(),
                    4,
                    4,
                )
            },
            SPATIAL_INVALID_ARGUMENT
        );
        assert_eq!(spatial_last_error_code(handle), SPATIAL_INVALID_ARGUMENT);
        assert_eq!(spatial_set_convolution_mode(handle, 1), SPATIAL_OK);
        assert_eq!(spatial_last_error_code(handle), SPATIAL_OK);
        let mut input = [0.0_f32; 128];
        input[0] = 1.0;
        let slots = [0_u32];
        let params = [0.0, 0.0, 1.0, 1.0];
        let mut left = [0.0_f32; 128];
        let mut right = [0.0_f32; 128];
        assert_eq!(
            unsafe {
                spatial_render_objects(
                    handle,
                    input.as_ptr(),
                    input.len(),
                    input.len(),
                    slots.as_ptr(),
                    slots.len(),
                    params.as_ptr(),
                    params.len(),
                    1,
                    left.as_mut_ptr(),
                    left.len(),
                    right.as_mut_ptr(),
                    right.len(),
                    128,
                )
            },
            SPATIAL_OK
        );
        assert!(left[..64].iter().all(|sample| sample.abs() < 1.0e-6));
        assert!(right[..64].iter().all(|sample| sample.abs() < 1.0e-6));
        assert!(left[64] > 0.0 && right[64] > 0.0);
        assert_eq!(spatial_destroy(handle), SPATIAL_OK);
        assert_eq!(spatial_destroy(handle), SPATIAL_INVALID_HANDLE);
    }
}
