//! 引擎子链：midSide → biquad → compressor → reverb-simple → bass-enhancer → limiter
//! （交错进、planar 过链、交错出）。
//!
//! 对应全链中六者的相对出现顺序（第 3 级 M/S → 第 4 级 Pre-EQ → 第 6 级 Compressor →
//! 第 13 级 混响 → 第 14 级 BassEnhancer → 第 21 级 Limiter）。构造只在控制面线程
//! 发生（分配合法）；process_planar 在 DSP 线程稳态零分配零锁零系统调用。
//! 参数热更换经 rtrb 命令环移交整条新链的所有权（见 pipeline）。

use hse_core::bass_enhancer::BassEnhancerStage;
use hse_core::biquad::BiquadStage;
use hse_core::compressor::CompressorStage;
use hse_core::limiter::LimiterStage;
use hse_core::mid_side::MidSideStage;
use hse_core::reverb_simple::ReverbSimpleStage;
use hse_core::Stage;

use crate::params::{MidSideParams, PilotParams};

/// 已装配就绪的引擎子链（所有权可跨线程移交，rtrb 元素要求 Move）。
pub struct PilotSubchain {
    mid_side: MidSideStage,
    biquad: Option<BiquadStage>,
    compressor: CompressorStage,
    reverb: ReverbSimpleStage,
    bass: BassEnhancerStage,
    limiter: LimiterStage,
}

impl PilotSubchain {
    /// 按参数快照构造并对 max_block 完成预分配（控制面线程调用）。
    pub fn build(params: &PilotParams, sample_rate: f64, max_block: usize) -> Result<Self, String> {
        let MidSideParams { width, voice_balance } = params.mid_side;
        let mut mid_side = MidSideStage::new();
        mid_side.set_params(width, voice_balance);
        let biquad = match &params.biquad {
            Some(spec) => Some(BiquadStage::new(
                sample_rate,
                &spec.filter_type,
                spec.f0,
                spec.q,
                spec.gain_db,
            )?),
            None => None, // TS 构造默认即恒等直通
        };
        let compressor = CompressorStage::from_settings(sample_rate, params.compressor.clone())?;
        let reverb = ReverbSimpleStage::from_params(sample_rate, params.reverb_simple.clone())?;
        let bass = BassEnhancerStage::from_settings(sample_rate, params.bass_enhancer.clone())?;
        let limiter = LimiterStage::from_settings(sample_rate, params.limiter.clone())?;
        let mut chain = Self { mid_side, biquad, compressor, reverb, bass, limiter };
        chain.prepare(max_block);
        Ok(chain)
    }

    fn prepare(&mut self, max_block: usize) {
        Stage::prepare(&mut self.mid_side, max_block);
        if let Some(b) = self.biquad.as_mut() {
            Stage::prepare(b, max_block);
        }
        Stage::prepare(&mut self.compressor, max_block);
        Stage::prepare(&mut self.reverb, max_block);
        Stage::prepare(&mut self.bass, max_block);
        Stage::prepare(&mut self.limiter, max_block);
    }

    /// planar 就地过链：左右声道长度恒等（Stage 契约）。
    pub fn process_planar(&mut self, left: &mut [f32], right: &mut [f32]) {
        self.mid_side.process(left, right);
        if let Some(b) = self.biquad.as_mut() {
            b.process(left, right);
        }
        self.compressor.process(left, right);
        self.reverb.process(left, right);
        self.bass.process(left, right);
        self.limiter.process(left, right);
    }

    /// 复位全部阶段状态（换链/重启时在非实时侧调用）。
    pub fn reset(&mut self) {
        self.mid_side.reset();
        if let Some(b) = self.biquad.as_mut() {
            b.reset();
        }
        self.compressor.reset();
        self.reverb.reset();
        self.bass.reset();
        self.limiter.reset();
    }
}

/// 交错立体声 → planar 双声道（src 长度必须为偶数）。
pub fn deinterleave(src: &[f32], left: &mut [f32], right: &mut [f32]) {
    debug_assert_eq!(src.len(), left.len() + right.len());
    for (f, pair) in src.chunks_exact(2).enumerate() {
        left[f] = pair[0];
        right[f] = pair[1];
    }
}

/// planar 双声道 → 交错立体声。
pub fn interleave(left: &[f32], right: &[f32], dst: &mut [f32]) {
    debug_assert_eq!(dst.len(), left.len() + right.len());
    for f in 0..left.len() {
        dst[f * 2] = left[f];
        dst[f * 2 + 1] = right[f];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::PilotParams;

    #[test]
    fn 交错往返无损() {
        let src: Vec<f32> = (0..16).map(|i| i as f32 * 0.25).collect();
        let mut l = vec![0.0; 8];
        let mut r = vec![0.0; 8];
        deinterleave(&src, &mut l, &mut r);
        assert_eq!(l, [0.0, 0.5, 1.0, 1.5, 2.0, 2.5, 3.0, 3.5]);
        assert_eq!(r, [0.25, 0.75, 1.25, 1.75, 2.25, 2.75, 3.25, 3.75]);
        let mut dst = vec![0.0; 16];
        interleave(&l, &r, &mut dst);
        assert_eq!(dst, src);
    }

    #[test]
    fn 直通链逐位等于输入() {
        // biquad 关闭 + reverb 全干(wet=0,dry=1) + limiter 禁用 ⇒ 逐位直通。
        let mut p = PilotParams::default();
        p.reverb_simple.wet = 0.0;
        p.reverb_simple.dry = 1.0;
        p.reverb_simple.pre_delay_ms = 0.0;
        p.limiter.enabled = false;
        let mut chain = PilotSubchain::build(&p, 48000.0, 64).unwrap();
        let mut left: Vec<f32> = (-32..32).map(|i| i as f32 * 0.01).collect();
        let mut right: Vec<f32> = left.clone();
        let want_l = left.clone();
        chain.process_planar(&mut left, &mut right);
        assert_eq!(left, want_l);
        assert_eq!(right, want_l);
    }

    #[test]
    fn 静音输入不产生非有限值() {
        let mut chain = PilotSubchain::build(&PilotParams::default(), 48000.0, 128).unwrap();
        let mut l = vec![0.0_f32; 128];
        let mut r = vec![0.0_f32; 128];
        for _ in 0..8 {
            chain.process_planar(&mut l, &mut r);
        }
        assert!(l.iter().all(|x| x.is_finite()));
        assert!(r.iter().all(|x| x.is_finite()));
        assert!(l.iter().all(|x| *x == 0.0));
    }

    #[test]
    fn 正弦输入经默认链能量受限且有限() {
        let mut chain = PilotSubchain::build(&PilotParams::default(), 48000.0, 256).unwrap();
        let n = 256 * 40;
        let mut l = Vec::with_capacity(256);
        let mut r = Vec::with_capacity(256);
        let mut peak: f32 = 0.0;
        for blk in 0..(n / 256) {
            l.clear();
            r.clear();
            for i in 0..256 {
                let t = ((blk * 256 + i) as f64) / 48000.0;
                let s = (2.0 * std::f64::consts::PI * 440.0 * t).sin() as f32 * 0.5;
                l.push(s);
                r.push(s);
            }
            chain.process_planar(&mut l, &mut r);
            peak = peak.max(l.iter().fold(0.0_f32, |m, x| m.max(x.abs())));
            assert!(l.iter().all(|x| x.is_finite()));
        }
        assert!(peak <= 1.0, "限幅后峰值应不超过 1.0，实际 {}", peak);
    }
}