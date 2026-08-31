//! `hse-core` 的 wasm-bindgen 边界。
//!
//! JS 通过导出的指针直接读写预分配的 planar 缓冲；`process` 仅创建精确帧数的切片，
//! 不扩容、不复制、不分配。此 crate 不依赖 Web API、Node builtin 或 TS 核心。

use hrtf_core::{load_sofa_bytes, HrtfGrid, SofaGridOptions};
use hse_core::{
    biquad::BiquadStage,
    engine_chain::{EngineChainParams, EngineChainStage},
    Stage,
};
use serde_json::{json, Value};
use wasm_bindgen::prelude::*;

pub mod spatial_abi;

const FILTER_TYPES: [&str; 8] = [
    "peaking",
    "lowshelf",
    "highshelf",
    "lowpass",
    "highpass",
    "bandpass",
    "notch",
    "allpass",
];

fn validate_params(
    sample_rate: f64,
    filter_type: &str,
    f0: f64,
    q: f64,
    gain_db: f64,
) -> Result<(), String> {
    if !(sample_rate.is_finite() && sample_rate > 0.0) {
        return Err("sampleRate must be positive and finite".into());
    }
    if !FILTER_TYPES.contains(&filter_type) {
        return Err(format!("unsupported biquad type: {filter_type}"));
    }
    if !(f0.is_finite() && q.is_finite() && gain_db.is_finite()) {
        return Err("f0, q, and gainDb must be finite".into());
    }
    Ok(())
}

fn error(code: &str, message: impl Into<String>) -> String {
    json!({ "code": code, "message": message.into() }).to_string()
}

fn validate_engine_limits(sample_rate: f64, max_frames: u32) -> Result<usize, String> {
    if !(sample_rate.is_finite() && sample_rate > 0.0) {
        return Err(error(
            "invalid-sample-rate",
            "sampleRate must be positive and finite",
        ));
    }
    if max_frames == 0 {
        return Err(error(
            "invalid-capacity",
            "maxFrames must be greater than zero",
        ));
    }
    Ok(max_frames as usize)
}

fn build_engine(
    sample_rate: f64,
    capacity: usize,
    params_json: Option<&str>,
    hrtf_grid: Option<HrtfGrid>,
) -> Result<EngineChainStage, String> {
    let overrides = match params_json {
        None | Some("") => Value::Object(Default::default()),
        Some(params_json) => serde_json::from_str::<Value>(params_json).map_err(|err| {
            error(
                "invalid-params-json",
                format!("params JSON is invalid: {err}"),
            )
        })?,
    };
    if !overrides.is_object() {
        return Err(error(
            "invalid-params",
            "params JSON must contain an object",
        ));
    }

    let params = EngineChainParams::from_overrides(sample_rate, &overrides)
        .map_err(|err| error("invalid-params", err))?;
    let mut stage = EngineChainStage::from_params_with_hrtf_grid(sample_rate, params, hrtf_grid)
        .map_err(|err| error("engine-build-failed", err))?;
    stage.prepare(capacity);
    Ok(stage)
}

fn build_hrtf_grid(
    grid_sample_rate: u32,
    azimuths: &[f32],
    elevations: &[f32],
    hrir_length: u32,
    left: &[f32],
    right: &[f32],
) -> Result<HrtfGrid, String> {
    HrtfGrid::new(
        grid_sample_rate,
        azimuths.to_vec(),
        elevations.to_vec(),
        hrir_length as usize,
        left.to_vec(),
        right.to_vec(),
    )
    .map_err(|err| error("invalid-hrtf-grid", err.to_string()))
}

#[wasm_bindgen]
pub struct HseEngine {
    stage: EngineChainStage,
    left: Vec<f32>,
    right: Vec<f32>,
    sidechain_left: Vec<f32>,
    sidechain_right: Vec<f32>,
}

#[wasm_bindgen]
impl HseEngine {
    #[wasm_bindgen(constructor)]
    pub fn new(
        sample_rate: f64,
        max_frames: u32,
        params_json: Option<String>,
    ) -> Result<HseEngine, String> {
        let capacity = validate_engine_limits(sample_rate, max_frames)?;
        let stage = build_engine(sample_rate, capacity, params_json.as_deref(), None)?;
        Ok(Self::from_stage(stage, capacity))
    }

    #[wasm_bindgen(js_name = withSofaBytes)]
    pub fn with_sofa_bytes(
        sample_rate: f64,
        max_frames: u32,
        params_json: Option<String>,
        sofa_bytes: &[u8],
    ) -> Result<HseEngine, String> {
        let capacity = validate_engine_limits(sample_rate, max_frames)?;
        let options = SofaGridOptions {
            sample_rate: sample_rate.round() as u32,
            ..SofaGridOptions::default()
        };
        let grid = load_sofa_bytes(sofa_bytes, &options)
            .map_err(|err| error("hrtf-load-failed", err.to_string()))?;
        let stage = build_engine(sample_rate, capacity, params_json.as_deref(), Some(grid))?;
        Ok(Self::from_stage(stage, capacity))
    }

    #[wasm_bindgen(js_name = withHrtfGrid)]
    #[allow(clippy::too_many_arguments)]
    pub fn with_hrtf_grid(
        sample_rate: f64,
        max_frames: u32,
        params_json: Option<String>,
        grid_sample_rate: u32,
        azimuths: &[f32],
        elevations: &[f32],
        hrir_length: u32,
        left: &[f32],
        right: &[f32],
    ) -> Result<HseEngine, String> {
        let capacity = validate_engine_limits(sample_rate, max_frames)?;
        let grid = build_hrtf_grid(
            grid_sample_rate,
            azimuths,
            elevations,
            hrir_length,
            left,
            right,
        )?;
        let stage = build_engine(sample_rate, capacity, params_json.as_deref(), Some(grid))?;
        Ok(Self::from_stage(stage, capacity))
    }

    fn from_stage(stage: EngineChainStage, capacity: usize) -> Self {
        Self {
            stage,
            left: vec![0.0; capacity],
            right: vec![0.0; capacity],
            sidechain_left: vec![0.0; capacity],
            sidechain_right: vec![0.0; capacity],
        }
    }

    pub fn left_ptr(&self) -> *const f32 {
        self.left.as_ptr()
    }

    pub fn right_ptr(&self) -> *const f32 {
        self.right.as_ptr()
    }

    pub fn sidechain_left_ptr(&self) -> *const f32 {
        self.sidechain_left.as_ptr()
    }

    pub fn sidechain_right_ptr(&self) -> *const f32 {
        self.sidechain_right.as_ptr()
    }

    pub fn capacity(&self) -> usize {
        self.left.len()
    }

    pub fn process(&mut self, frames: u32) -> Result<(), String> {
        let frames = frames as usize;
        if frames > self.left.len() {
            return Err(error(
                "frames-exceed-capacity",
                format!(
                    "frames {frames} exceeds preallocated capacity {}",
                    self.left.len()
                ),
            ));
        }
        self.stage.process_with_sidechain(
            &mut self.left[..frames],
            &mut self.right[..frames],
            &self.sidechain_left[..frames],
            &self.sidechain_right[..frames],
        );
        Ok(())
    }

    pub fn reset(&mut self) {
        self.stage.reset();
        self.left.fill(0.0);
        self.right.fill(0.0);
        self.sidechain_left.fill(0.0);
        self.sidechain_right.fill(0.0);
    }
}

#[wasm_bindgen]
pub struct HseBiquad {
    sample_rate: f64,
    stage: BiquadStage,
    left: Vec<f32>,
    right: Vec<f32>,
}

#[wasm_bindgen]
impl HseBiquad {
    #[wasm_bindgen(constructor)]
    pub fn new(
        sample_rate: f64,
        filter_type: &str,
        f0: f64,
        q: f64,
        gain_db: f64,
        max_frames: u32,
    ) -> Result<HseBiquad, String> {
        validate_params(sample_rate, filter_type, f0, q, gain_db)?;
        if max_frames == 0 {
            return Err("maxFrames must be greater than zero".into());
        }

        let capacity = max_frames as usize;
        let mut stage = BiquadStage::new(sample_rate, filter_type, f0, q, gain_db)?;
        stage.prepare(capacity);
        Ok(Self {
            sample_rate,
            stage,
            left: vec![0.0; capacity],
            right: vec![0.0; capacity],
        })
    }

    pub fn left_ptr(&self) -> *const f32 {
        self.left.as_ptr()
    }

    pub fn right_ptr(&self) -> *const f32 {
        self.right.as_ptr()
    }

    pub fn capacity(&self) -> usize {
        self.left.len()
    }

    pub fn process(&mut self, frames: u32) -> Result<(), String> {
        let frames = frames as usize;
        if frames > self.left.len() {
            return Err(format!(
                "frames {frames} exceeds preallocated capacity {}",
                self.left.len()
            ));
        }
        self.stage
            .process(&mut self.left[..frames], &mut self.right[..frames]);
        Ok(())
    }

    pub fn configure(
        &mut self,
        filter_type: &str,
        f0: f64,
        q: f64,
        gain_db: f64,
    ) -> Result<(), String> {
        validate_params(self.sample_rate, filter_type, f0, q, gain_db)?;
        let mut next = BiquadStage::new(self.sample_rate, filter_type, f0, q, gain_db)?;
        next.prepare(self.left.len());
        self.stage = next;
        Ok(())
    }

    pub fn reset(&mut self) {
        self.stage.reset();
        self.left.fill(0.0);
        self.right.fill(0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        alloc::{GlobalAlloc, Layout, System},
        cell::Cell,
    };

    struct CountingAllocator;

    thread_local! {
        static COUNTING: Cell<bool> = const { Cell::new(false) };
        static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
        static DEALLOCATIONS: Cell<usize> = const { Cell::new(0) };
    }

    unsafe impl GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            COUNTING.with(|enabled| {
                if enabled.get() {
                    ALLOCATIONS.with(|count| count.set(count.get() + 1));
                }
            });
            unsafe { System.alloc(layout) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            COUNTING.with(|enabled| {
                if enabled.get() {
                    DEALLOCATIONS.with(|count| count.set(count.get() + 1));
                }
            });
            unsafe { System.dealloc(ptr, layout) }
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            COUNTING.with(|enabled| {
                if enabled.get() {
                    ALLOCATIONS.with(|count| count.set(count.get() + 1));
                }
            });
            unsafe { System.realloc(ptr, layout, new_size) }
        }
    }

    #[global_allocator]
    static ALLOCATOR: CountingAllocator = CountingAllocator;

    fn allocator_operations_during(run: impl FnOnce()) -> (usize, usize) {
        ALLOCATIONS.with(|count| count.set(0));
        DEALLOCATIONS.with(|count| count.set(0));
        COUNTING.with(|enabled| enabled.set(true));
        run();
        COUNTING.with(|enabled| enabled.set(false));
        (ALLOCATIONS.with(Cell::get), DEALLOCATIONS.with(Cell::get))
    }

    fn buffers_mut(instance: &mut HseBiquad) -> (&mut [f32], &mut [f32]) {
        let len = instance.capacity();
        let left = instance.left_ptr() as *mut f32;
        let right = instance.right_ptr() as *mut f32;
        // The pointers refer to separate, fixed-size Vec allocations owned by `instance`.
        unsafe {
            (
                std::slice::from_raw_parts_mut(left, len),
                std::slice::from_raw_parts_mut(right, len),
            )
        }
    }

    fn engine_buffers_mut(
        instance: &mut HseEngine,
    ) -> (&mut [f32], &mut [f32], &mut [f32], &mut [f32]) {
        let len = instance.capacity();
        let left = instance.left_ptr() as *mut f32;
        let right = instance.right_ptr() as *mut f32;
        let sidechain_left = instance.sidechain_left_ptr() as *mut f32;
        let sidechain_right = instance.sidechain_right_ptr() as *mut f32;
        // The pointers refer to four separate fixed-size Vec allocations owned by `instance`.
        unsafe {
            (
                std::slice::from_raw_parts_mut(left, len),
                std::slice::from_raw_parts_mut(right, len),
                std::slice::from_raw_parts_mut(sidechain_left, len),
                std::slice::from_raw_parts_mut(sidechain_right, len),
            )
        }
    }

    fn error_code(error_json: &str) -> String {
        serde_json::from_str::<Value>(error_json).unwrap()["code"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    fn delta_grid() -> HrtfGrid {
        HrtfGrid::new(
            48_000,
            vec![-30.0, 30.0],
            vec![0.0],
            1,
            vec![1.0, 0.0],
            vec![0.0, 1.0],
        )
        .unwrap()
    }

    fn grid_planes(grid: &HrtfGrid) -> (Vec<f32>, Vec<f32>) {
        let mut left = Vec::new();
        let mut right = Vec::new();
        for elevation in 0..grid.elevations().len() {
            for azimuth in 0..grid.azimuths().len() {
                let pair = grid.hrir(hrtf_core::NearestIndex { azimuth, elevation });
                left.extend_from_slice(pair.left);
                right.extend_from_slice(pair.right);
            }
        }
        (left, right)
    }

    #[test]
    fn engine_constructor_exposes_four_stable_preallocated_buffers() {
        let engine = HseEngine::new(48_000.0, 128, None).unwrap();
        assert_eq!(engine.capacity(), 128);
        let pointers = [
            engine.left_ptr(),
            engine.right_ptr(),
            engine.sidechain_left_ptr(),
            engine.sidechain_right_ptr(),
        ];
        assert!(pointers.iter().all(|pointer| !pointer.is_null()));
        for (index, pointer) in pointers.iter().enumerate() {
            assert!(!pointers[..index].contains(pointer));
        }
    }

    #[test]
    fn engine_accepts_preparsed_grid_for_stage22() {
        let grid = delta_grid();
        let (left, right) = grid_planes(&grid);
        let params = r#"{
            "eq":{"enabled":false},
            "limiter":{"enabled":false},
            "stereoWidth":1,
            "spatial":{"mode":"instant","masterGain":1,"convolution":"time","instant":{"spreadDeg":60,"amount":1}}
        }"#;
        let mut engine = HseEngine::with_hrtf_grid(
            48_000.0,
            8,
            Some(params.into()),
            grid.sample_rate(),
            grid.azimuths(),
            grid.elevations(),
            grid.hrir_length() as u32,
            &left,
            &right,
        )
        .unwrap();
        let (input_left, input_right, side_l, side_r) = engine_buffers_mut(&mut engine);
        input_left[0] = 1.0;
        input_right[0] = 1.0;
        side_l.fill(0.0);
        side_r.fill(0.0);

        engine.process(8).unwrap();

        assert!(unsafe { std::slice::from_raw_parts(engine.left_ptr(), 8) }
            .iter()
            .any(|sample| *sample != 0.0));
        assert!(unsafe { std::slice::from_raw_parts(engine.right_ptr(), 8) }
            .iter()
            .any(|sample| *sample != 0.0));
    }

    #[test]
    fn engine_constructs_world_and_stage_modes_with_preparsed_grid() {
        let grid = delta_grid();
        let (grid_left, grid_right) = grid_planes(&grid);
        let params = [
            r#"{
                "eq":{"enabled":false},"limiter":{"enabled":false},
                "spatial":{"mode":"world","convolution":"time","masterGain":1,
                    "instant":{"amount":1,"room":"off","roomAmount":0},
                    "world":{
                        "listener":{"position":{"x":0,"y":1.6,"z":0},"yaw":10,"pitch":5,"roll":-2},
                        "sources":[{"id":"lead","position":{"x":-1,"y":1.6,"z":3},"gain":1,"size":0.5}],
                        "playhead":1,"trajectories":[],"occlusion":0.3
                    },
                    "ambience":{"enabled":true,"amount":0.2}
                }
            }"#,
            r#"{
                "eq":{"enabled":false},"limiter":{"enabled":false},
                "spatial":{"mode":"stage","convolution":"time","masterGain":1,
                    "instant":{"amount":1},
                    "stage":{"preset":"piano","seat":"front","roomSize":0.8,"reverbAmount":0.4,"customSources":[]}
                }
            }"#,
        ];
        for params in params {
            let mut engine = HseEngine::with_hrtf_grid(
                48_000.0,
                17,
                Some(params.into()),
                grid.sample_rate(),
                grid.azimuths(),
                grid.elevations(),
                grid.hrir_length() as u32,
                &grid_left,
                &grid_right,
            )
            .unwrap();
            let (left, right, side_left, side_right) = engine_buffers_mut(&mut engine);
            left.fill(0.0);
            right.fill(0.0);
            left[0] = 1.0;
            side_left.fill(0.0);
            side_right.fill(0.0);
            engine.process(7).unwrap();
            let first_left = unsafe { std::slice::from_raw_parts(engine.left_ptr(), 7) }.to_vec();
            let first_right = unsafe { std::slice::from_raw_parts(engine.right_ptr(), 7) }.to_vec();
            assert!(first_left
                .iter()
                .chain(&first_right)
                .all(|sample| sample.is_finite()));

            engine.reset();
            let (left, right, side_left, side_right) = engine_buffers_mut(&mut engine);
            left.fill(0.0);
            right.fill(0.0);
            left[0] = 1.0;
            side_left.fill(0.0);
            side_right.fill(0.0);
            engine.process(7).unwrap();
            assert_eq!(
                unsafe { std::slice::from_raw_parts(engine.left_ptr(), 7) },
                first_left
            );
            assert_eq!(
                unsafe { std::slice::from_raw_parts(engine.right_ptr(), 7) },
                first_right
            );
        }
    }

    #[test]
    fn engine_rejects_invalid_preparsed_grid_before_building_stage22() {
        let error = HseEngine::with_hrtf_grid(
            48_000.0,
            128,
            Some(r#"{"spatial":{"mode":"instant"}}"#.into()),
            48_000,
            &[0.0],
            &[0.0],
            2,
            &[1.0],
            &[1.0, 0.0],
        )
        .err()
        .unwrap();
        assert_eq!(error_code(&error), "invalid-hrtf-grid");
    }

    #[test]
    fn engine_process_is_allocation_free_after_prepare() {
        let mut engine = HseEngine::new(48_000.0, 128, None).unwrap();
        engine.process(128).unwrap();
        engine.reset();

        let operations = allocator_operations_during(|| engine.process(128).unwrap());

        assert_eq!(operations, (0, 0));
    }

    #[test]
    fn engine_process_uses_exact_frame_slice_and_preserves_tail() {
        let params = r#"{
            "eq":{"enabled":false},
            "limiter":{"enabled":false},
            "stereoWidth":0
        }"#;
        let mut engine = HseEngine::new(48_000.0, 8, Some(params.into())).unwrap();
        let pointers = (engine.left_ptr(), engine.right_ptr());
        let (left, right, side_l, side_r) = engine_buffers_mut(&mut engine);
        left.copy_from_slice(&[1.0, 0.5, -1.0, -0.5, 9.0, 9.0, 9.0, 9.0]);
        right.copy_from_slice(&[-1.0, -0.5, 1.0, 0.5, 8.0, 8.0, 8.0, 8.0]);
        side_l.fill(0.0);
        side_r.fill(0.0);

        engine.process(4).unwrap();

        assert_eq!(
            unsafe { std::slice::from_raw_parts(engine.left_ptr(), 8) },
            [0.0, 0.0, 0.0, 0.0, 9.0, 9.0, 9.0, 9.0]
        );
        assert_eq!(
            unsafe { std::slice::from_raw_parts(engine.right_ptr(), 8) },
            [0.0, 0.0, 0.0, 0.0, 8.0, 8.0, 8.0, 8.0]
        );
        assert_eq!((engine.left_ptr(), engine.right_ptr()), pointers);
    }

    #[test]
    fn engine_sidechain_buffers_drive_enabled_compressor() {
        let params = r#"{
            "eq":{"enabled":false},
            "limiter":{"enabled":false},
            "compressor":{
                "enabled":true,
                "thresholdDb":-40,
                "ratio":20,
                "kneeDb":0,
                "attackMs":0,
                "releaseMs":100,
                "makeupDb":0,
                "outputGain":1,
                "sidechainEnabled":true
            }
        }"#;
        let mut quiet = HseEngine::new(48_000.0, 128, Some(params.into())).unwrap();
        let mut driven = HseEngine::new(48_000.0, 128, Some(params.into())).unwrap();
        for engine in [&mut quiet, &mut driven] {
            let (left, right, side_l, side_r) = engine_buffers_mut(engine);
            left.fill(0.5);
            right.fill(0.5);
            side_l.fill(0.0);
            side_r.fill(0.0);
        }
        let (_, _, side_l, side_r) = engine_buffers_mut(&mut driven);
        side_l.fill(1.0);
        side_r.fill(1.0);

        quiet.process(128).unwrap();
        driven.process(128).unwrap();

        assert!(unsafe { *driven.left_ptr().add(127) } < unsafe { *quiet.left_ptr().add(127) });
    }

    #[test]
    fn engine_reset_clears_all_buffers_without_replacing_storage() {
        let mut engine = HseEngine::new(
            48_000.0,
            8,
            Some(r#"{"eq":{"enabled":false},"limiter":{"enabled":false},"stereoWidth":0}"#.into()),
        )
        .unwrap();
        let pointers = (
            engine.left_ptr(),
            engine.right_ptr(),
            engine.sidechain_left_ptr(),
            engine.sidechain_right_ptr(),
        );
        let (left, right, side_l, side_r) = engine_buffers_mut(&mut engine);
        left.fill(1.0);
        right.fill(-1.0);
        side_l.fill(0.25);
        side_r.fill(-0.25);
        engine.process(8).unwrap();
        assert_eq!(unsafe { *engine.left_ptr() }, 0.0);

        engine.reset();
        assert_eq!(
            (
                engine.left_ptr(),
                engine.right_ptr(),
                engine.sidechain_left_ptr(),
                engine.sidechain_right_ptr(),
            ),
            pointers
        );
        for pointer in [
            engine.left_ptr(),
            engine.right_ptr(),
            engine.sidechain_left_ptr(),
            engine.sidechain_right_ptr(),
        ] {
            assert!(unsafe { std::slice::from_raw_parts(pointer, 8) }
                .iter()
                .all(|sample| *sample == 0.0));
        }
    }

    #[test]
    fn engine_errors_are_structured_and_failed_construction_keeps_existing_engine_usable() {
        assert_eq!(
            error_code(&HseEngine::new(0.0, 128, None).err().unwrap()),
            "invalid-sample-rate"
        );
        assert_eq!(
            error_code(&HseEngine::new(48_000.0, 0, None).err().unwrap()),
            "invalid-capacity"
        );
        assert_eq!(
            error_code(
                &HseEngine::new(48_000.0, 128, Some("[1]".into()))
                    .err()
                    .unwrap()
            ),
            "invalid-params"
        );
        assert_eq!(
            error_code(
                &HseEngine::new(48_000.0, 128, Some("{".into()))
                    .err()
                    .unwrap()
            ),
            "invalid-params-json"
        );

        let mut engine = HseEngine::new(
            48_000.0,
            4,
            Some(r#"{"eq":{"enabled":false},"limiter":{"enabled":false},"stereoWidth":0}"#.into()),
        )
        .unwrap();
        assert_eq!(
            error_code(
                &HseEngine::new(
                    48_000.0,
                    4,
                    Some(r#"{"spatial":{"mode":"instant"}}"#.into()),
                )
                .err()
                .unwrap()
            ),
            "engine-build-failed"
        );
        assert_eq!(
            error_code(&engine.process(5).err().unwrap()),
            "frames-exceed-capacity"
        );
        let (left, right, _, _) = engine_buffers_mut(&mut engine);
        left.fill(1.0);
        right.fill(-1.0);
        engine.process(4).unwrap();
        assert_eq!(unsafe { *engine.left_ptr() }, 0.0);
    }

    #[test]
    fn constructor_exposes_stable_preallocated_buffers() {
        let instance = HseBiquad::new(48_000.0, "peaking", 1_000.0, 1.2, 4.0, 128).unwrap();
        assert_eq!(instance.capacity(), 128);
        assert!(!instance.left_ptr().is_null());
        assert!(!instance.right_ptr().is_null());
        assert_ne!(instance.left_ptr(), instance.right_ptr());
    }

    #[test]
    fn pointer_buffers_are_processed_in_place() {
        let mut instance = HseBiquad::new(48_000.0, "lowpass", 1_000.0, 0.707, 0.0, 8).unwrap();
        let left_ptr = instance.left_ptr();
        let right_ptr = instance.right_ptr();
        let (left, right) = buffers_mut(&mut instance);
        left[..4].copy_from_slice(&[1.0, 0.0, 0.0, 0.0]);
        right[..4].copy_from_slice(&[0.5, 0.0, 0.0, 0.0]);
        instance.process(4).unwrap();
        assert_ne!(unsafe { *left_ptr }, 1.0);
        assert_ne!(unsafe { *right_ptr }, 0.5);
        assert_eq!(instance.left_ptr(), left_ptr);
        assert_eq!(instance.right_ptr(), right_ptr);
    }

    #[test]
    fn state_crosses_blocks_and_reset_reproduces_first_block() {
        let mut instance = HseBiquad::new(48_000.0, "notch", 60.0, 8.0, 0.0, 4).unwrap();
        let input = [1.0, -0.5, 0.25, -0.125];
        let (left, right) = buffers_mut(&mut instance);
        left.copy_from_slice(&input);
        right.copy_from_slice(&input);
        instance.process(4).unwrap();
        let first = unsafe { std::slice::from_raw_parts(instance.left_ptr(), 4) }.to_vec();

        let (left, right) = buffers_mut(&mut instance);
        left.copy_from_slice(&input);
        right.copy_from_slice(&input);
        instance.process(4).unwrap();
        let second = unsafe { std::slice::from_raw_parts(instance.left_ptr(), 4) }.to_vec();
        assert_ne!(
            first, second,
            "recursive state must survive between process calls"
        );

        instance.reset();
        let (left, right) = buffers_mut(&mut instance);
        left.copy_from_slice(&input);
        right.copy_from_slice(&input);
        instance.process(4).unwrap();
        assert_eq!(
            unsafe { std::slice::from_raw_parts(instance.left_ptr(), 4) },
            first
        );
    }

    #[test]
    fn invalid_parameters_and_oversized_process_are_rejected() {
        assert!(HseBiquad::new(0.0, "lowpass", 1_000.0, 1.0, 0.0, 128).is_err());
        assert!(HseBiquad::new(48_000.0, "unknown", 1_000.0, 1.0, 0.0, 128).is_err());
        assert!(HseBiquad::new(48_000.0, "lowpass", f64::NAN, 1.0, 0.0, 128).is_err());
        assert!(HseBiquad::new(48_000.0, "lowpass", 1_000.0, 1.0, 0.0, 0).is_err());

        let mut instance = HseBiquad::new(48_000.0, "lowpass", 1_000.0, 1.0, 0.0, 128).unwrap();
        assert!(instance
            .process(129)
            .err()
            .unwrap()
            .contains("exceeds preallocated capacity"));
        assert!(instance.configure("unknown", 1_000.0, 1.0, 0.0).is_err());
    }

    #[test]
    fn configure_replaces_coefficients_and_resets_filter_state() {
        let mut instance = HseBiquad::new(48_000.0, "lowpass", 500.0, 0.707, 0.0, 4).unwrap();
        let (left, right) = buffers_mut(&mut instance);
        left.fill(1.0);
        right.fill(1.0);
        instance.process(4).unwrap();

        instance.configure("highpass", 2_000.0, 0.707, 0.0).unwrap();
        let (left, right) = buffers_mut(&mut instance);
        left.fill(0.0);
        right.fill(0.0);
        instance.process(4).unwrap();
        assert_eq!(
            unsafe { std::slice::from_raw_parts(instance.left_ptr(), 4) },
            [0.0; 4]
        );
    }
}
