//! mid_side —— M/S 立体声编解码 + 宽度 / 人声比例。
//!
//! 行为事实标准：仓库根 `src/dsp/MidSide.ts`；规格：`specs/dsp/mid-side.md`。
//! 本文件是该 TS 类的逐行移植，关键对应关系：
//!
//! - M/S 变换与逆变换（规格 §4.1）：逐样本 `m = (li+ri)×0.5`、`s = (li−ri)×0.5`、
//!   `l = m·midGain + s·sideGain`、`r = m·midGain − s·sideGain`；
//! - 增益语义（规格 §4.2，对称衰减、电平安全）：
//!   `midGain = 1 + min(0, vb)`、`sideGain = w × (1 − max(0, vb))`——
//!   vb>0 衰减侧（人声突出）、vb<0 衰减中（伴奏突出）、width 只进入 sideGain；
//! - **无采样率概念**（规格 §一/§4.4）：构造无参、无内部滤波与状态、行为与 fs
//!   完全无关；向量驱动器不得把 sampleRate 传入模块；
//! - 无内部状态：`reset()` 为空操作，任意分块（含逐样本）结果一致（规格 §4.3）。
//!
//! # 数值精度铁律的落点
//!
//! - 中间量 m/s 与增益乘加全程 f64（TS Number），f32 落点仅输出样本写回两处；
//!   恒等条件（width=1、vb=0）下 `m·1 + s·1` 与 `m·1 − s·1` 精确还原 f32 输入
//!   （除 2 与乘 1 均为精确运算），构成逐位一致的最强精度锚点（GWT-MS-01）；
//! - `Math.min`/`Math.max` 的 NaN 传播语义以 [`js_min`]/[`js_max`] 显式复刻
//!   （理由同 biquad.rs）。

use crate::Stage;

/// 复刻 JS Math.min(a, b) 的 NaN 传播语义（理由见 biquad.rs 的同名函数）。
fn js_min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a < b {
        a
    } else {
        b
    }
}

/// 复刻 JS Math.max(a, b) 的 NaN 传播语义（理由同上）。
fn js_max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a > b {
        a
    } else {
        b
    }
}

/// 一个已配置的 M/S 立体声阶段（字段一一对应 TS `MidSide` 私有域）。
///
/// 构造内置 midGain=1、sideGain=1（未调 `set_params` 时恒等，规格 §三）。
#[derive(Debug, Clone)]
pub struct MidSideStage {
    mid_gain: f64,
    side_gain: f64,
}

impl MidSideStage {
    /// TS 无参构造：初始 midGain=1、sideGain=1（恒等）。
    pub fn new() -> Self {
        Self { mid_gain: 1.0, side_gain: 1.0 }
    }

    /// 对齐 TS 位置参数接口 `setParams(width, voiceBalance)`：
    /// width 0..2（1=原始宽度，越界双向钳制）、voiceBalance -1..1
    /// （-1=仅伴奏 / +1=仅人声，越界双向钳制）。
    pub fn set_params(&mut self, width: f64, voice_balance: f64) {
        // TS L26–L27：w = min(max(width, 0), 2)；vb = min(max(vb, -1), 1)。
        let w = js_min(js_max(width, 0.0), 2.0);
        let vb = js_min(js_max(voice_balance, -1.0), 1.0);
        // TS L29–L30：对称衰减语义（mg = 1 + min(0, vb)；sg = w × (1 − max(0, vb))）。
        let mg = 1.0 + js_min(0.0, vb);
        let sg = w * (1.0 - js_max(0.0, vb));
        self.mid_gain = mg;
        self.side_gain = sg;
    }

    /// 当前生效增益快照（诊断/测试用途）。
    pub fn gains(&self) -> (f64, f64) {
        (self.mid_gain, self.side_gain)
    }
}

impl Default for MidSideStage {
    fn default() -> Self {
        Self::new()
    }
}

impl Stage for MidSideStage {
    /// 本模块无状态无缓冲，无需预分配（保留形参以符合 Stage 契约调用时序）。
    fn prepare(&mut self, _max_block_size: usize) {}

    /// 就地 M/S 编解码一个立体声块；无跨样本状态，任意分块结果一致（GWT-MS-08）。
    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        debug_assert_eq!(left.len(), right.len(), "左右声道块长必须一致（Stage 契约）");
        // 局部缓存增益（对齐 TS L38–L39）。
        let mg = self.mid_gain;
        let sg = self.side_gain;
        for i in 0..left.len() {
            let li = f64::from(left[i]);
            let ri = f64::from(right[i]);
            // 中/侧信号（双精度中间量；×0.5 为精确运算）。
            let m = (li + ri) * 0.5;
            let s = (li - ri) * 0.5;
            // 逆变换写回（f32 落点；加/减与乘的结合顺序与 TS L45–L46 一致）。
            left[i] = (m * mg + s * sg) as f32;
            right[i] = (m * mg - s * sg) as f32;
        }
    }

    /// 无内部状态，空操作（接口一致性，对齐 TS reset）。
    fn reset(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 确定性伪噪声（LCG，无随机依赖），幅度 [-1, 1)。
    fn lcg_noise(n: usize, seed: u32) -> Vec<f32> {
        let mut u = seed;
        (0..n)
            .map(|_| {
                u = u.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((f64::from(u) / f64::from(u32::MAX)) * 2.0 - 1.0) as f32
            })
            .collect()
    }

    #[test]
    fn 构造默认为恒等增益() {
        let stage = MidSideStage::new();
        let (mg, sg) = stage.gains();
        assert_eq!(mg, 1.0);
        assert_eq!(sg, 1.0);
    }

    #[test]
    fn 恒等锚点_宽度1人声0_输出与输入逐位一致() {
        // GWT-MS-01：M/S 正逆变换在双精度中间量下精确还原 f32 输入。
        for n in [1_usize, 7, 256, 4096] {
            let l_in = lcg_noise(n, 5);
            let r_in = lcg_noise(n, 77);
            let mut stage = MidSideStage::new();
            stage.set_params(1.0, 0.0);
            stage.prepare(n);
            let mut l = l_in.clone();
            let mut r = r_in.clone();
            stage.process(&mut l, &mut r);
            assert_eq!(l, l_in, "n={n} 左声道必须逐位一致");
            assert_eq!(r, r_in, "n={n} 右声道必须逐位一致");
        }
    }

    #[test]
    fn 单声道塌缩_宽度0_左右输出为中信号且逐位相等() {
        // GWT-MS-02：sideGain=0 → 输出均为 M。
        let n = 4096;
        let l_in = lcg_noise(n, 9);
        let r_in = lcg_noise(n, 31);
        let mut stage = MidSideStage::new();
        stage.set_params(0.0, 0.0);
        stage.prepare(n);
        let mut l = l_in.clone();
        let mut r = r_in.clone();
        stage.process(&mut l, &mut r);
        for i in 0..n {
            let want = ((f64::from(l_in[i]) + f64::from(r_in[i])) * 0.5) as f32;
            assert_eq!(l[i].to_bits(), want.to_bits(), "left[{i}] 应为中信号 M");
            assert_eq!(l[i].to_bits(), r[i].to_bits(), "left/right[{i}] 应逐位相等");
        }
    }

    #[test]
    fn 宽度展宽_精确增益核对() {
        // GWT-MS-03：width=2 → sideGain=2，中不变。用 2 的幂输入做精确断言：
        // L=0.5, R=0.25 → m=0.375, s=0.125 → out=0.625 / 0.125。
        let mut stage = MidSideStage::new();
        stage.set_params(2.0, 0.0);
        stage.prepare(2);
        let mut l = [0.5_f32, -0.5];
        let mut r = [0.25_f32, -0.25];
        stage.process(&mut l, &mut r);
        assert_eq!(l[0], 0.625);
        assert_eq!(r[0], 0.125);
        assert_eq!(l[1], -0.625);
        assert_eq!(r[1], -0.125);
    }

    #[test]
    fn width越界双向钳制() {
        // GWT-MS-04：2.5 → 2；-1 → 0（负宽度语义与单声道塌缩一致）。
        let mut stage = MidSideStage::new();
        stage.set_params(2.5, 0.0);
        assert_eq!(stage.gains(), (1.0, 2.0));
        stage.set_params(-1.0, 0.0);
        assert_eq!(stage.gains(), (1.0, 0.0));
    }

    #[test]
    fn 人声路径_vb正_侧衰减中不变() {
        // GWT-MS-05：w=1、vb=0.75 → mg=1、sg=0.25。
        // L=0.5, R=0.25 → m=0.375, s=0.125 → out=0.40625 / 0.34375。
        let mut stage = MidSideStage::new();
        stage.set_params(1.0, 0.75);
        let (mg, sg) = stage.gains();
        assert_eq!(mg, 1.0);
        assert_eq!(sg, 0.25);
        stage.prepare(2);
        let mut l = [0.5_f32, 0.5];
        let mut r = [0.25_f32, 0.25];
        stage.process(&mut l, &mut r);
        assert_eq!(l[0], 0.40625);
        assert_eq!(r[0], 0.34375);
        // vb=+1 完全去侧：sg=0，输出为中信号。
        stage.set_params(1.0, 1.0);
        assert_eq!(stage.gains(), (1.0, 0.0));
    }

    #[test]
    fn 伴奏路径_vb负_中衰减侧不变() {
        // GWT-MS-06：vb=-0.5 → mg=0.5、sg=w。
        // L=0.5, R=0.25 → m=0.375, s=0.125 → out=0.3125 / 0.0625。
        let mut stage = MidSideStage::new();
        stage.set_params(1.0, -0.5);
        assert_eq!(stage.gains(), (0.5, 1.0));
        stage.prepare(2);
        let mut l = [0.5_f32, 0.5];
        let mut r = [0.25_f32, 0.25];
        stage.process(&mut l, &mut r);
        assert_eq!(l[0], 0.3125);
        assert_eq!(r[0], 0.0625);
        // vb=-1 完全去中：mg=0；vb 越界双向钳制。
        stage.set_params(1.0, -3.0);
        assert_eq!(stage.gains(), (0.0, 1.0));
        stage.set_params(1.0, 2.0);
        assert_eq!(stage.gains(), (1.0, 0.0));
    }

    #[test]
    fn 电平安全_vb非零时只衰减不提升() {
        // GWT-MS-07：mg ≤ 1 且 sg ≤ w（width 展宽除外）。
        for w in [0.0_f64, 0.5, 1.0, 2.0] {
            for vb in [-1.0_f64, -0.7, -0.01, 0.01, 0.5, 1.0] {
                let mut stage = MidSideStage::new();
                stage.set_params(w, vb);
                let (mg, sg) = stage.gains();
                assert!(mg <= 1.0, "w={w} vb={vb}: midGain {mg} 不得超过 1");
                assert!(sg <= w, "w={w} vb={vb}: sideGain {sg} 不得超过 width");
            }
        }
    }

    #[test]
    fn 任意分块一致_含逐样本与超长块() {
        // GWT-MS-08：无跨样本状态，blockSize=1 与整块结果逐位一致。
        let n = 1000;
        let l_in = lcg_noise(n, 13);
        let r_in = lcg_noise(n, 29);
        let mut whole = MidSideStage::new();
        whole.set_params(1.7, 0.4);
        whole.prepare(n);
        let mut wl = l_in.clone();
        let mut wr = r_in.clone();
        whole.process(&mut wl, &mut wr);

        let mut per_sample = MidSideStage::new();
        per_sample.set_params(1.7, 0.4);
        per_sample.prepare(1);
        let mut pl = l_in.clone();
        let mut pr = r_in.clone();
        for i in 0..n {
            per_sample.process(&mut pl[i..i + 1], &mut pr[i..i + 1]);
        }
        assert_eq!(wl, pl, "逐样本分块必须与整块逐位一致（左）");
        assert_eq!(wr, pr, "逐样本分块必须与整块逐位一致（右）");
    }

    #[test]
    fn reset_为空操作_重放逐位一致() {
        // GWT-MS-09：无状态可清。
        let n = 256;
        let l_in = lcg_noise(n, 3);
        let r_in = lcg_noise(n, 8);
        let mut stage = MidSideStage::new();
        stage.set_params(0.6, -0.3);
        stage.prepare(64);
        let mut l1 = l_in.clone();
        let mut r1 = r_in.clone();
        stage.process(&mut l1, &mut r1);
        stage.reset();
        let mut l2 = l_in.clone();
        let mut r2 = r_in.clone();
        stage.process(&mut l2, &mut r2);
        assert_eq!(l1, l2);
        assert_eq!(r1, r2);
    }
}
