//! hse-core —— HyperSoundEngine Rust 支线的 DSP 内核与引擎链（纯库）。
//!
//! 定位（《原生化双支线与Windows音频接入规划书》§2.1）：承接全部 DSP 模块与
//! 引擎链；平台相关代码（WASAPI 后端、服务进程等）一律不进入本 crate。
//!
//! 当前为 Phase 0 骨架：只定义处理阶段的统一抽象 [`Stage`] 与纯结构容器
//! [`StageChain`]。各 DSP 模块的真实实现自下一阶段起按仓库根 `specs/` 的
//! 共享规格逐个落地——规格先行双实现，TS 支线（仓库根 `src/`）是行为事实标准。
//!
//! # 全库铁律（对本 crate 及一切 [`Stage`] 实现生效）
//!
//! - **实时安全**：[`Stage::process`] 在稳态下零堆分配、零锁、零系统调用；
//!   所有工作缓冲必须在 [`Stage::prepare`] 中按最大块长一次性预分配。
//! - **确定性**：核心算法禁用随机数、系统时钟与任何形式的日志输出；
//!   同输入、同参数、同状态必得同输出。
//! - **就地处理**：[`Stage::process`] 直接改写传入的左右声道切片，
//!   不额外产出新缓冲（对齐 TS 支线 `processStereo` 的就地语义）。

pub mod bass_enhancer;
pub mod biquad;
pub mod compressor;
pub mod convolver;
pub mod deesser;
pub mod dynamic_eq;
pub mod engine_chain;
pub mod eq_chain;
pub mod fft;
pub mod fdn_reverb;
pub mod hse_stretch;
pub mod limiter;
pub mod loudness_comp;
pub mod lufs_meter;
pub mod mid_side;
pub mod midi;
pub mod mod_effects;
pub mod modulation_matrix;
pub mod reverb_simple;
pub mod share_codec;
pub mod wav;

/// 处理链中的单个阶段：立体声、就地、按块处理。
///
/// 语义对齐 TS 支线 `src/interfaces.ts` 的两个契约：
///
/// - `StereoProcessor`：`processStereo(left, right)` 的就地立体声处理形态与
///   `reset` 语义；参数快照更新由各具体模块自定义方法承担（整体替换、
///   内部深拷贝的语义与 TS 侧一致，不进本 trait）。
/// - `ProcessingStage`：引擎把阶段按固定顺序串成数组、逐块依次调用的链路
///   语义；激活/旁路判断属引擎链层职责，随引擎链一并落地。
///
/// # 实时安全约定
///
/// - [`Stage::prepare`]：只在音频回调之外调用（初始化或拓扑变化后），
///   内部按 `max_block_size` 完成全部预分配；
/// - [`Stage::process`]：会被实时音频线程按块调用，块长不超过最近一次
///   `prepare` 的值；稳态下禁止分配、加锁、读时钟、打日志；
/// - [`Stage::reset`]：清空内部状态回初始值，同样不允许分配。
///
/// 左右声道切片长度恒等于本块帧数；末块允许短于 `max_block_size`，
/// 状态跨块保持（滤波器历史、包络、延迟线等不得因分块而断裂）。
pub trait Stage {
    /// 预分配内部工作缓冲；`max_block_size` 为此后 `process` 可能收到的最大块长。
    fn prepare(&mut self, max_block_size: usize);

    /// 就地处理一个立体声块；`left`/`right` 长度相等，状态跨块保持。
    fn process(&mut self, left: &mut [f32], right: &mut [f32]);

    /// 复位全部内部状态到刚构造时的取值。
    fn reset(&mut self);
}

/// 固定顺序串联若干 [`Stage`] 的纯结构容器（引擎链雏形）。
///
/// 只做转发编排，不含任何 DSP 数学：`prepare`/`process`/`reset`
/// 依追加顺序作用于每个阶段。阶段所有权由链持有，装配发生在音频回调之外，
/// 因此这里的堆分配不违反实时铁律。
///
/// 用途：给后续引擎链与对拍 harness 提供统一组合单元，让"多阶段顺序即行为"
/// 这一 TS 侧语义在 Rust 支线有一致的落点。
pub struct StageChain {
    stages: Vec<Box<dyn Stage>>,
}

impl StageChain {
    /// 创建空链。
    pub fn new() -> Self {
        Self { stages: Vec::new() }
    }

    /// 追加一个阶段；处理顺序即追加顺序。
    pub fn push(&mut self, stage: Box<dyn Stage>) {
        self.stages.push(stage);
    }

    /// 当前链内的阶段数量。
    pub fn len(&self) -> usize {
        self.stages.len()
    }

    /// 链是否为空。
    pub fn is_empty(&self) -> bool {
        self.stages.is_empty()
    }
}

impl Default for StageChain {
    fn default() -> Self {
        Self::new()
    }
}

impl Stage for StageChain {
    fn prepare(&mut self, max_block_size: usize) {
        for stage in self.stages.iter_mut() {
            stage.prepare(max_block_size);
        }
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        debug_assert_eq!(left.len(), right.len(), "左右声道块长必须一致（Stage 契约）");
        for stage in self.stages.iter_mut() {
            stage.process(left, right);
        }
    }

    fn reset(&mut self) {
        for stage in self.stages.iter_mut() {
            stage.reset();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试用固定增益阶段：左右声道同乘一个系数（确定性、无分配）。
    struct FixedGain {
        gain: f32,
    }

    impl FixedGain {
        fn new(gain: f32) -> Self {
            Self { gain }
        }
    }

    impl Stage for FixedGain {
        fn prepare(&mut self, _max_block_size: usize) {}
        fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
            for sample in left.iter_mut() {
                *sample *= self.gain;
            }
            for sample in right.iter_mut() {
                *sample *= self.gain;
            }
        }
        fn reset(&mut self) {}
    }

    #[test]
    fn 空链为直通() {
        let mut chain = StageChain::new();
        assert!(chain.is_empty());
        let mut left = [0.5_f32, -0.25];
        let mut right = [0.125_f32, 2.0];
        chain.process(&mut left, &mut right);
        assert_eq!(left, [0.5, -0.25]);
        assert_eq!(right, [0.125, 2.0]);
    }

    #[test]
    fn 链按追加顺序级联且复位后可复用() {
        // 两级 0.5 增益级联等效 0.25（全部是二的幂，浮点结果精确）。
        let mut chain = StageChain::default();
        chain.push(Box::new(FixedGain::new(0.5)));
        chain.push(Box::new(FixedGain::new(0.5)));
        assert_eq!(chain.len(), 2);
        chain.prepare(2);

        let mut left = [1.0_f32, -2.0];
        let mut right = [4.0_f32, -8.0];
        chain.process(&mut left, &mut right);
        assert_eq!(left[0], 0.25);
        assert_eq!(left[1], -0.5);
        assert_eq!(right[0], 1.0);
        assert_eq!(right[1], -2.0);
        chain.reset();

        // reset 后链仍可用：同样的输入得到同样的输出。
        let mut left_again = [1.0_f32, -2.0];
        let mut right_again = [4.0_f32, -8.0];
        chain.process(&mut left_again, &mut right_again);
        assert_eq!(left_again[0], 0.25);
        assert_eq!(right_again[1], -2.0);
    }
}
