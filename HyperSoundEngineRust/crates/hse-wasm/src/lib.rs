//! `hse-core` 的最小 wasm-bindgen 边界。
//!
//! JS 通过导出的指针直接读写预分配的 planar 左右声道缓冲；`process` 仅创建切片，
//! 不扩容、不复制、不分配。此 crate 不依赖 Web API、Node builtin 或 TS 核心。

use hse_core::{biquad::BiquadStage, Stage};
use wasm_bindgen::prelude::*;

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
            .unwrap_err()
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
