//! 服务数据面使用的 HyperSoundEngine 1-21 级完整链。
//!
//! wire 参数兼容层位于 `params`；本模块只接收已经投影为 canonical
//! `HyperSoundEngineParams` 的完整快照。构造和 `prepare` 均发生在控制面线程，
//! DSP 线程只做原位处理。HseStretch 是链外能力，不在此处装配。

use hse_core::engine_chain::{EngineChainParams, EngineChainStage};
use hse_core::Stage;
use serde_json::Value;

/// 已构造并完成预分配的服务完整链。
pub struct ServiceEngineChain {
    inner: EngineChainStage,
}

impl ServiceEngineChain {
    pub fn build(canonical: &Value, sample_rate: f64, max_block: usize) -> Result<Self, String> {
        let params = EngineChainParams::from_overrides(sample_rate, canonical)?;
        let mut inner = EngineChainStage::from_params(sample_rate, params)?;
        inner.prepare(max_block);
        Ok(Self { inner })
    }

    pub fn stage_ids(&self) -> &[&'static str] {
        self.inner.stage_ids()
    }

    /// planar 左右声道原位处理。空间级固定为 off；HseStretch 不在主链内。
    pub fn process_planar(&mut self, left: &mut [f32], right: &mut [f32]) {
        self.inner.process(left, right);
    }

    pub fn reset(&mut self) {
        self.inner.reset();
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
    use serde_json::{json, Value};

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
