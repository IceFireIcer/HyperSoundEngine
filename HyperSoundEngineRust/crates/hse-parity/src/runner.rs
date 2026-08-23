//! 用例执行器：按契约分块驱动被测阶段，并按统一容差汇总比对结论。

use crate::segments::split_planar;
use crate::tolerance::within_tolerance;
use crate::vector::VectorCase;
use hse_core::Stage;

/// 直通假实现：输出恒等于输入（process 为就地恒等操作）。
///
/// Phase 0 的用途是跑通 harness 全流程；由于它不做任何 DSP，
/// 只要冻结基线的期望输出不等于输入，对拍就必然 FAIL——这属于预期行为，
/// 恰好证明比对逻辑在工作。真实模块于后续阶段按 specs/ 规格替换此实现。
pub struct PassthroughStage;

impl Stage for PassthroughStage {
    fn prepare(&mut self, _max_block_size: usize) {}
    fn process(&mut self, _left: &mut [f32], _right: &mut [f32]) {}
    fn reset(&mut self) {}
}

/// 声道标记。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelTag {
    Left,
    Right,
}

impl ChannelTag {
    pub fn as_label(self) -> &'static str {
        match self {
            ChannelTag::Left => "左声道",
            ChannelTag::Right => "右声道",
        }
    }
}

/// 首个失配样本的位置与取值，用于快速定位问题。
#[derive(Debug, Clone)]
pub struct MismatchSample {
    pub channel: ChannelTag,
    pub frame_index: usize,
    pub got: f32,
    pub want: f32,
}

/// 单个用例的比对结论。
#[derive(Debug, Clone)]
pub struct CaseOutcome {
    pub passed: bool,
    pub frames: usize,
    /// 实际执行的块数（含末尾短块）。
    pub blocks_run: usize,
    /// 全部样本上最大的 |got - want|（无论是否越界都参与统计）。
    pub max_abs_diff: f64,
    pub mismatch_count: usize,
    pub first_mismatch: Option<MismatchSample>,
}

/// 执行一个用例：把输入按 `blockSize` 顺序分块送入被测阶段
/// （末块可短，状态跨块保持），把逐块输出拼接后与期望输出逐样本比对。
///
/// 返回 `Err` 表示结构性错误（数据长度与 frames 不符等）；
/// `Ok(CaseOutcome)` 的 `passed` 才反映数值对拍结果。
pub fn run_case(
    case: &VectorCase,
    planar_data: &[f32],
    stage: &mut dyn Stage,
) -> Result<CaseOutcome, String> {
    let segments = split_planar(planar_data, case.frames).ok_or_else(|| {
        format!(
            ".f32 数据长度 {} 与 frames={}（需要 {} 个样本，四段布局）不符",
            planar_data.len(),
            case.frames,
            case.frames.saturating_mul(4)
        )
    })?;

    let block_size = case.block_size.max(1);
    stage.prepare(block_size);

    // 离线 harness 不受音频线程铁律约束，但仍在循环外一次分配，避免反复分配。
    let mut got_left = vec![0.0_f32; case.frames];
    let mut got_right = vec![0.0_f32; case.frames];
    let mut work_left = vec![0.0_f32; block_size];
    let mut work_right = vec![0.0_f32; block_size];

    let mut blocks_run = 0_usize;
    let mut offset = 0_usize;
    while offset < case.frames {
        let chunk = (case.frames - offset).min(block_size);
        work_left[..chunk].copy_from_slice(&segments.input_left[offset..offset + chunk]);
        work_right[..chunk].copy_from_slice(&segments.input_right[offset..offset + chunk]);
        stage.process(&mut work_left[..chunk], &mut work_right[..chunk]);
        got_left[offset..offset + chunk].copy_from_slice(&work_left[..chunk]);
        got_right[offset..offset + chunk].copy_from_slice(&work_right[..chunk]);
        offset += chunk;
        blocks_run += 1;
    }

    let mut outcome = CaseOutcome {
        passed: true,
        frames: case.frames,
        blocks_run,
        max_abs_diff: 0.0,
        mismatch_count: 0,
        first_mismatch: None,
    };
    compare_channel(
        case,
        ChannelTag::Left,
        segments.expected_left,
        &got_left,
        &mut outcome,
    );
    compare_channel(
        case,
        ChannelTag::Right,
        segments.expected_right,
        &got_right,
        &mut outcome,
    );
    outcome.passed = outcome.mismatch_count == 0;
    Ok(outcome)
}

fn compare_channel(
    case: &VectorCase,
    channel: ChannelTag,
    expected: &[f32],
    got: &[f32],
    outcome: &mut CaseOutcome,
) {
    for (frame_index, (want, got_sample)) in expected.iter().zip(got.iter()).enumerate() {
        let diff = (f64::from(*got_sample) - f64::from(*want)).abs();
        if diff > outcome.max_abs_diff {
            outcome.max_abs_diff = diff;
        }
        let ok = within_tolerance(
            *got_sample,
            *want,
            case.tolerance.value,
            case.tolerance.floor,
        );
        if !ok {
            outcome.mismatch_count += 1;
            if outcome.first_mismatch.is_none() {
                outcome.first_mismatch = Some(MismatchSample {
                    channel,
                    frame_index,
                    got: *got_sample,
                    want: *want,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector::ToleranceSpec;

    fn make_case(frames: usize, block_size: usize) -> VectorCase {
        VectorCase {
            schema_version: 1,
            module: "selfcheck".to_string(),
            case: "passthrough".to_string(),
            sample_rate: 48_000.0,
            block_size,
            channels: 2,
            frames,
            tolerance: ToleranceSpec {
                kind: "relative".to_string(),
                value: 1.0e-6,
                floor: 1.0e-9,
            },
        }
    }

    fn make_planar(
        input_l: &[f32],
        input_r: &[f32],
        expect_l: &[f32],
        expect_r: &[f32],
    ) -> Vec<f32> {
        let mut all = Vec::new();
        all.extend_from_slice(input_l);
        all.extend_from_slice(input_r);
        all.extend_from_slice(expect_l);
        all.extend_from_slice(expect_r);
        all
    }

    #[test]
    fn 期望等于输入时直通实现对拍通过() {
        let case = make_case(5, 2);
        let data = make_planar(
            &[0.1, -0.2, 0.3, -0.4, 0.5],
            &[0.05, 0.15, -0.25, 0.35, -0.45],
            &[0.1, -0.2, 0.3, -0.4, 0.5],
            &[0.05, 0.15, -0.25, 0.35, -0.45],
        );
        let mut stage = PassthroughStage;
        let outcome = run_case(&case, &data, &mut stage).expect("合法数据不应报结构性错误");
        assert!(outcome.passed);
        assert_eq!(outcome.blocks_run, 3); // 2 + 2 + 1，末块变短
        assert_eq!(outcome.mismatch_count, 0);
        assert_eq!(outcome.max_abs_diff, 0.0);
    }

    #[test]
    fn 期望偏离输入时直通实现对拍失败并给出定位() {
        let case = make_case(3, 4);
        let data = make_planar(
            &[1.0, -2.0, 3.0],
            &[0.5, -0.5, 0.25],
            &[1.5, -2.0, 3.0],  // 左声道第 0 帧偏了 0.5
            &[0.5, -1.0, 0.25], // 右声道第 1 帧偏了 0.5
        );
        let mut stage = PassthroughStage;
        let outcome = run_case(&case, &data, &mut stage).expect("合法数据不应报结构性错误");
        assert!(!outcome.passed);
        assert_eq!(outcome.mismatch_count, 2);
        assert!((outcome.max_abs_diff - 0.5).abs() < 1e-12);
        let first = outcome.first_mismatch.expect("必须记录首个失配");
        assert_eq!(first.channel, ChannelTag::Left);
        assert_eq!(first.frame_index, 0);
        assert_eq!(first.got, 1.0);
        assert_eq!(first.want, 1.5);
    }

    #[test]
    fn 数据长度不符报结构性错误() {
        let case = make_case(2, 2);
        let short_data = vec![0.0_f32; 6]; // 四段布局需要 8 个样本
        let mut stage = PassthroughStage;
        assert!(run_case(&case, &short_data, &mut stage).is_err());
    }

    #[test]
    fn 零帧用例平凡通过() {
        let case = make_case(0, 8);
        let mut stage = PassthroughStage;
        let outcome = run_case(&case, &[], &mut stage).expect("空数据是合法零帧布局");
        assert!(outcome.passed);
        assert_eq!(outcome.blocks_run, 0);
    }
}
