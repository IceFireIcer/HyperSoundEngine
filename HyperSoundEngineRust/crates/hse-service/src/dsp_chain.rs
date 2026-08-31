//! 服务数据面使用的 HyperSoundEngine 1-22 级完整链。
//!
//! wire 参数兼容层位于 `params`；本模块只接收已经投影为 canonical
//! `HyperSoundEngineParams` 的完整快照。构造和 `prepare` 均发生在控制面线程，
//! DSP 线程只做原位处理。HseStretch 是链外能力，不在此处装配。

use hrtf_core::HrtfGrid;
use hse_core::engine_chain::{EngineChainParams, EngineChainStage};
use hse_core::Stage;
use serde_json::Value;

/// 已构造并完成预分配的服务完整链。
pub struct ServiceEngineChain {
    inner: EngineChainStage,
}

impl ServiceEngineChain {
    pub fn build(canonical: &Value, sample_rate: f64, max_block: usize) -> Result<Self, String> {
        Self::build_with_hrtf_grid(canonical, sample_rate, max_block, None)
    }

    pub fn build_with_hrtf_grid(
        canonical: &Value,
        sample_rate: f64,
        max_block: usize,
        hrtf_grid: Option<HrtfGrid>,
    ) -> Result<Self, String> {
        Self::build_with_hrtf_grid_and_previous(canonical, sample_rate, max_block, hrtf_grid, None)
    }

    pub fn build_with_hrtf_grid_and_previous(
        canonical: &Value,
        sample_rate: f64,
        max_block: usize,
        hrtf_grid: Option<HrtfGrid>,
        previous: Option<&Value>,
    ) -> Result<Self, String> {
        let params = EngineChainParams::from_overrides(sample_rate, canonical)?;
        let mut inner = EngineChainStage::from_params_with_hrtf_grid_and_previous(
            sample_rate,
            params,
            hrtf_grid,
            previous,
        )?;
        inner.prepare(max_block);
        Ok(Self { inner })
    }

    pub fn stage_ids(&self) -> &[&'static str] {
        self.inner.stage_ids()
    }

    /// planar 左右声道原位处理。空间级是否启用由参数与预载 grid 决定；HseStretch 不在主链内。
    pub fn process_planar(&mut self, left: &mut [f32], right: &mut [f32]) {
        self.inner.process(left, right);
    }

    pub fn reset(&mut self) {
        self.inner.reset();
    }

    #[cfg(test)]
    fn spatial_listener_velocity(&self) -> Option<hrtf_core::Vec3> {
        self.inner.spatial_listener_velocity()
    }
}

/// 交错立体声 -> planar 双声道（src 长度必须为偶数）。
pub fn deinterleave(src: &[f32], left: &mut [f32], right: &mut [f32]) {
    debug_assert_eq!(src.len(), left.len() + right.len());
    for (frame, pair) in src.chunks_exact(2).enumerate() {
        left[frame] = pair[0];
        right[frame] = pair[1];
    }
}

/// planar 双声道 -> 交错立体声。
pub fn interleave(left: &[f32], right: &[f32], dst: &mut [f32]) {
    debug_assert_eq!(dst.len(), left.len() + right.len());
    for frame in 0..left.len() {
        dst[frame * 2] = left[frame];
        dst[frame * 2 + 1] = right[frame];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::parse_pilot_params;
    use hrtf_core::HrtfGrid;
    use serde_json::{json, Value};

    fn test_grid() -> HrtfGrid {
        HrtfGrid::new(
            48_000,
            vec![-30.0, 30.0],
            vec![0.0],
            3,
            vec![1.0, 0.5, 0.0, 0.25, 0.0, 0.0],
            vec![0.25, 0.0, 0.0, 1.0, 0.5, 0.0],
        )
        .unwrap()
    }

    const COMPLETE_STAGE_IDS: [&str; 22] = [
        "loudness-normalization",
        "surround3d",
        "mid-side",
        "pre-eq",
        "deesser",
        "compressor",
        "night-mode",
        "delay",
        "chorus",
        "flanger",
        "phaser",
        "tremolo",
        "reverb",
        "bass-enhancer",
        "loudness-compensation",
        "ieq-post",
        "analysis",
        "dynamic-eq",
        "lufs",
        "mod-master-gain",
        "limiter",
        "spatial",
    ];

    fn build_wire(value: &Value, block: usize) -> ServiceEngineChain {
        let (wire, warnings) = parse_pilot_params(value).unwrap();
        assert!(warnings.is_empty());
        let canonical = wire.to_canonical_json(value, 48_000.0).unwrap();
        ServiceEngineChain::build(&canonical, 48_000.0, block).unwrap()
    }

    #[test]
    fn 交错往返无损() {
        let src: Vec<f32> = (0..16).map(|i| i as f32 * 0.25).collect();
        let mut left = vec![0.0; 8];
        let mut right = vec![0.0; 8];
        deinterleave(&src, &mut left, &mut right);
        let mut dst = vec![0.0; 16];
        interleave(&left, &right, &mut dst);
        assert_eq!(dst, src);
    }

    #[test]
    fn 服务链暴露完整级序且历史缺失级已进入链() {
        let chain = build_wire(
            &json!({"reverbRoute":"off","limiter":{"enabled":false}}),
            128,
        );
        assert_eq!(chain.stage_ids(), COMPLETE_STAGE_IDS);
        for formerly_missing in [
            "loudness-normalization",
            "surround3d",
            "night-mode",
            "ieq-post",
            "analysis",
            "lufs",
        ] {
            assert!(chain.stage_ids().contains(&formerly_missing));
        }
        assert_eq!(chain.stage_ids()[..21].len(), 21);
        assert_eq!(chain.stage_ids()[21], "spatial");
    }

    #[test]
    fn 非off空间参数不被覆盖且无grid明确失败() {
        let value = json!({"spatial":{"mode":"instant"}});
        let (wire, warnings) = parse_pilot_params(&value).unwrap();
        assert!(warnings.is_empty());
        let canonical = wire.to_canonical_json(&value, 48_000.0).unwrap();
        assert_eq!(canonical["spatial"]["mode"], "instant");
        let error = ServiceEngineChain::build(&canonical, 48_000.0, 128)
            .err()
            .expect("服务未配置 HRTF grid 时必须拒绝非 off 空间模式");
        assert!(error.contains("HRTF grid"), "实际错误：{error}");
    }

    #[test]
    fn 非off空间参数可使用控制路径预载grid构链() {
        let value = json!({"spatial":{"mode":"instant"}});
        let (wire, warnings) = parse_pilot_params(&value).unwrap();
        assert!(warnings.is_empty());
        let canonical = wire.to_canonical_json(&value, 48_000.0).unwrap();
        let chain =
            ServiceEngineChain::build_with_hrtf_grid(&canonical, 48_000.0, 128, Some(test_grid()))
                .expect("预载 grid 后 stage 22 应可构建");
        assert_eq!(chain.stage_ids()[21], "spatial");
    }

    #[test]
    fn world与stage可使用控制路径预载grid构链() {
        for value in [
            json!({"spatial":{"mode":"world","convolution":"time","world":{
                "listener":{"position":{"x":0,"y":1.6,"z":0},"yaw":0,"pitch":5,"roll":-2},
                "sources":[{"id":"lead","position":{"x":-2,"y":1.6,"z":4},"gain":1,"size":0.5}],
                "playhead":1,"trajectories":[],"occlusion":0.3
            }}}),
            json!({"spatial":{"mode":"stage","convolution":"time","stage":{
                "preset":"piano","seat":"front","roomSize":0.8,"reverbAmount":0.4,
                "customSources":[]
            }}}),
        ] {
            let (wire, warnings) = parse_pilot_params(&value).unwrap();
            assert!(warnings.is_empty());
            let canonical = wire.to_canonical_json(&value, 48_000.0).unwrap();
            let mut chain = ServiceEngineChain::build_with_hrtf_grid(
                &canonical,
                48_000.0,
                64,
                Some(test_grid()),
            )
            .unwrap();
            let mut left = [0.0; 17];
            let mut right = [0.0; 17];
            left[0] = 1.0;
            chain.process_planar(&mut left, &mut right);
            chain.reset();
        }
    }

    #[test]
    fn world相邻快照推导确定速度() {
        let previous = json!({"spatial":{"mode":"world","world":{
            "listener":{"position":{"x":0,"y":1.6,"z":0},"yaw":0,"pitch":0,"roll":0},
            "sources":[{"id":"lead","position":{"x":0,"y":1.6,"z":4},"gain":1,"size":0}],
            "playhead":1,"trajectories":[],"occlusion":0
        }}});
        let current = json!({"spatial":{"mode":"world","convolution":"time","world":{
            "listener":{"position":{"x":2,"y":2.6,"z":-1},"yaw":0,"pitch":0,"roll":0},
            "sources":[{"id":"lead","position":{"x":0,"y":1.6,"z":4},"gain":1,"size":0}],
            "playhead":3,"trajectories":[],"occlusion":0
        }}});
        let (previous_params, _) = parse_pilot_params(&previous).unwrap();
        let previous_canonical = previous_params
            .to_canonical_json(&previous, 48_000.0)
            .unwrap();
        let (current_params, _) = parse_pilot_params(&current).unwrap();
        let current_canonical = current_params
            .to_canonical_json(&current, 48_000.0)
            .unwrap();
        let chain = ServiceEngineChain::build_with_hrtf_grid_and_previous(
            &current_canonical,
            48_000.0,
            64,
            Some(test_grid()),
            Some(&previous_canonical),
        )
        .unwrap();
        assert_eq!(
            chain.spatial_listener_velocity(),
            Some(hrtf_core::Vec3 {
                x: 1.0,
                y: 0.5,
                z: -0.5,
            })
        );
    }

    #[test]
    fn 服务wire投影命中all_bypass冻结向量() {
        let metadata: Value = serde_json::from_str(include_str!(
            "../../../../specs/dsp/vectors/engine-chain.all-bypass-bitexact.json"
        ))
        .unwrap();
        let frames = metadata["frames"].as_u64().unwrap() as usize;
        let block = metadata["blockSize"].as_u64().unwrap() as usize;
        let bytes =
            include_bytes!("../../../../specs/dsp/vectors/engine-chain.all-bypass-bitexact.f32");
        let samples: Vec<f32> = bytes
            .chunks_exact(4)
            .map(|chunk| f32::from_le_bytes(chunk.try_into().unwrap()))
            .collect();
        assert_eq!(samples.len(), frames * 4);
        let mut left = samples[..frames].to_vec();
        let mut right = samples[frames..frames * 2].to_vec();
        let want_left = &samples[frames * 2..frames * 3];
        let want_right = &samples[frames * 3..frames * 4];

        // 旧 wire 的等价旁路形态，内部投影到完整 canonical 快照。
        let mut chain = build_wire(
            &json!({
                "reverbRoute":"off",
                "limiter":{"enabled":false}
            }),
            block,
        );
        for start in (0..frames).step_by(block) {
            let end = (start + block).min(frames);
            chain.process_planar(&mut left[start..end], &mut right[start..end]);
        }
        assert_eq!(left, want_left);
        assert_eq!(right, want_right);
    }

    #[test]
    fn 三源混后进入完整链并确定性到达render缓冲() {
        let wire = json!({
            "eqChain":{"bands":[{"frequency":1000,"gain":3,"q":1}],"bandCount":1},
            "compressor":{"enabled":true,"thresholdDb":-18,"ratio":3},
            "reverbRoute":"off",
            "limiter":{"enabled":false}
        });
        let loopback: Vec<f32> = (0..128).map(|i| (i as f32 - 64.0) / 512.0).collect();
        let source_a = vec![0.125_f32; 128];
        let source_b: Vec<f32> = (0..128).map(|i| (i % 7) as f32 * 0.01).collect();
        let mixed: Vec<f32> = loopback
            .iter()
            .zip(&source_a)
            .zip(&source_b)
            .map(|((&loopback, &a), &b)| loopback + a + b)
            .collect();

        let run = || {
            let mut chain = build_wire(&wire, 64);
            let mut render = mixed.clone();
            let mut left = vec![0.0; 64];
            let mut right = vec![0.0; 64];
            deinterleave(&render, &mut left, &mut right);
            chain.process_planar(&mut left, &mut right);
            interleave(&left, &right, &mut render);
            render
        };
        let first = run();
        let second = run();
        assert_eq!(first, second, "loopback+A+B 经完整链到 render 必须确定");
        assert_ne!(first, mixed, "完整链中的显式 EQ/压缩必须实际生效");
        assert!(first.iter().all(|sample| sample.is_finite()));
    }
}
