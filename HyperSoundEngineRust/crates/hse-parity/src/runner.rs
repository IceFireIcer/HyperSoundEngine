//! 用例执行器：按契约分块驱动被测阶段，并按统一容差汇总比对结论。
//!
//! 两类驱动形态（specs/ 向量格式契约）：
//! - 流式（moduleKind 缺省/'stream'）：[`run_case`] 按块驱动 [`Stage`]，把逐块
//!   输出拼接后与期望输出逐样本比对（相对容差）；
//! - 计量型（moduleKind='meter'，specs/dsp/lufs-meter.md §三）：[`run_meter_case`]
//!   将两段输入按 blockSize 切块馈入计量模块（就地分析、无输出），全部块馈入
//!   完成后一次性读取六项读数，与 readings 逐项判定（绝对容差 + 哨兵等值）。

use crate::segments::{split_meter, split_planar};
use crate::tolerance::{within_tolerance, ReadingWant};
use crate::vector::VectorCase;
use hse_core::lufs_meter::LufsMeter;
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

/// 单条 readings 读数的失配结论（计量型用例；仅记录未通过判定的读数）。
#[derive(Debug, Clone)]
pub struct ReadingOutcome {
    pub name: String,
    pub got: f64,
}

/// 计量型用例的比对结论：readings 逐项判定汇总（无音频段可比）。
#[derive(Debug, Clone)]
pub struct MeterCaseOutcome {
    pub passed: bool,
    pub frames: usize,
    /// 实际执行的块数（含末尾短块）。
    pub blocks_run: usize,
    /// 参与判定的读数条数。
    pub checked: usize,
    /// 失配读数（按向量声明顺序）。
    pub failures: Vec<ReadingOutcome>,
    /// 有限数读数的最大 |got − want|（哨兵判定不参与统计）。
    pub max_abs_deviation: f64,
}

/// 执行一个计量型用例（specs/dsp/lufs-meter.md §三.3）：
///
/// 1. 校验 `.f32` 为两段输入布局（总长恰 2 × frames 个样本）；
/// 2. 将 `inL`/`inR` 按 `blockSize` 自头至尾顺序切块（末块允许短于 blockSize），
///    逐块调用 `process_stereo`（就地分析，输入缓冲不被改写）；
/// 3. 全部块馈入完成后，一次性读取六项读数，与 `case.readings` 逐项判定：
///    want 为有限数 → 绝对容差 `|got − want| ≤ tol`；want 为哨兵 → 等值判定
///    （tol 不参与）。readings 中未声明的读数不判定。
pub fn run_meter_case(
    case: &VectorCase,
    planar_data: &[f32],
    meter: &mut LufsMeter,
) -> Result<MeterCaseOutcome, String> {
    let segments = split_meter(planar_data, case.frames).ok_or_else(|| {
        format!(
            ".f32 数据长度 {} 与 frames={}（计量型两段布局需要 {} 个样本）不符",
            planar_data.len(),
            case.frames,
            case.frames.saturating_mul(2)
        )
    })?;
    let readings = case
        .readings
        .as_ref()
        .ok_or_else(|| "计量型用例缺少 readings".to_string())?;

    let block_size = case.block_size.max(1);
    let mut blocks_run = 0_usize;
    let mut offset = 0_usize;
    while offset < case.frames {
        let chunk = (case.frames - offset).min(block_size);
        meter.process_stereo(
            &segments.input_left[offset..offset + chunk],
            &segments.input_right[offset..offset + chunk],
        );
        offset += chunk;
        blocks_run += 1;
    }

    // 全部块馈入完成后一次性读取六项读数（读数与 blockSize 无关，GWT-LUFSMETER-05）。
    let got: [(&str, f64); 6] = [
        ("integratedLufs", meter.get_integrated_lufs()),
        ("momentaryLufs", meter.get_momentary_lufs()),
        ("shortTermLufs", meter.get_short_term_lufs()),
        ("lra", meter.get_lra()),
        ("peakDb", meter.get_peak_db()),
        ("truePeakDb", meter.get_true_peak_db()),
    ];

    let mut outcome = MeterCaseOutcome {
        passed: true,
        frames: case.frames,
        blocks_run,
        checked: 0,
        failures: Vec::new(),
        max_abs_deviation: 0.0,
    };
    for (name, spec) in readings {
        let got_value = got
            .iter()
            .find(|(got_name, _)| got_name == name)
            .map(|(_, value)| *value)
            .ok_or_else(|| format!("内部错误：读数名 {name} 不在六项读数表中"))?;
        outcome.checked += 1;
        let passed = spec.want.matches(got_value, spec.tol);
        if let ReadingWant::Finite(want) = spec.want {
            if got_value.is_finite() {
                let dev = (got_value - want).abs();
                if dev > outcome.max_abs_deviation {
                    outcome.max_abs_deviation = dev;
                }
            }
        }
        if !passed {
            outcome.failures.push(ReadingOutcome {
                name: name.clone(),
                got: got_value,
            });
        }
    }
    outcome.passed = outcome.failures.is_empty();
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector::{ReadingSpec, ToleranceSpec};

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
            params: serde_json::Value::Null,
            module_kind: None,
            readings: None,
        }
    }

    /// 计量型用例构造：readings 直接按 (want, tol) 声明。
    fn make_meter_case(
        frames: usize,
        block_size: usize,
        readings: Vec<(&str, ReadingWant, f64)>,
    ) -> VectorCase {
        VectorCase {
            schema_version: 1,
            module: "lufs-meter".to_string(),
            case: "unit".to_string(),
            sample_rate: 48_000.0,
            block_size,
            channels: 2,
            frames,
            tolerance: ToleranceSpec {
                kind: "relative".to_string(),
                value: 1.0e-6,
                floor: 1.0e-9,
            },
            params: serde_json::Value::Null,
            module_kind: Some("meter".to_string()),
            readings: Some(
                readings
                    .into_iter()
                    .map(|(name, want, tol)| (name.to_string(), ReadingSpec { want, tol }))
                    .collect(),
            ),
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

    // ---------------- 计量型（meter）回放与 readings 判定 ----------------

    #[test]
    fn 计量型静音用例_哨兵判定通过() {
        // 0.5 s 全零（2 个完整分析块，全为静音块）→ 六项读数全哨兵。
        let frames = 24_000;
        let case = make_meter_case(
            frames,
            256,
            vec![
                ("integratedLufs", ReadingWant::Nan, 0.1),
                ("momentaryLufs", ReadingWant::Nan, 0.1),
                ("shortTermLufs", ReadingWant::Nan, 0.1),
                ("lra", ReadingWant::Nan, 0.5),
                ("peakDb", ReadingWant::NegativeInfinity, 0.05),
                ("truePeakDb", ReadingWant::NegativeInfinity, 0.1),
            ],
        );
        let data = vec![0.0_f32; frames * 2]; // 两段输入布局：8 × frames 字节
        let mut meter = LufsMeter::new(case.sample_rate).expect("合法采样率");
        let outcome = run_meter_case(&case, &data, &mut meter).expect("合法数据");
        assert!(outcome.passed);
        assert_eq!(outcome.checked, 6);
        assert!(outcome.failures.is_empty());
        // 24000 / 256 = 93.75 → 94 块（末块 96 帧短块）
        assert_eq!(outcome.blocks_run, 94);
        assert_eq!(outcome.frames, frames);
        assert_eq!(outcome.max_abs_deviation, 0.0, "哨兵判定不参与偏差统计");
    }

    #[test]
    fn 计量型用例_有限期望与错号哨兵失配并给出诊断() {
        // 0.25 满幅常量输入：peakDb = 20·log10(0.25) ≈ -12.04（want -6 失配）；
        // 非静音输入的 truePeakDb 有限（want -Infinity 哨兵失配）。
        let frames = 1_024;
        let case = make_meter_case(
            frames,
            256,
            vec![
                ("peakDb", ReadingWant::Finite(-6.0), 0.05),
                ("truePeakDb", ReadingWant::NegativeInfinity, 0.1),
            ],
        );
        let data = vec![0.25_f32; frames * 2];
        let mut meter = LufsMeter::new(case.sample_rate).expect("合法采样率");
        let outcome = run_meter_case(&case, &data, &mut meter).expect("合法数据");
        assert!(!outcome.passed);
        assert_eq!(outcome.checked, 2);
        assert_eq!(outcome.failures.len(), 2);
        assert_eq!(outcome.failures[0].name, "peakDb");
        let got = outcome.failures[0].got;
        assert!((got - 20.0 * 0.25_f64.log10()).abs() < 1e-9, "got={got}");
        assert!(
            (outcome.max_abs_deviation - (got - -6.0).abs()).abs() < 1e-12,
            "偏差统计 = |got − want|：{}",
            outcome.max_abs_deviation
        );
        assert_eq!(outcome.failures[1].name, "truePeakDb");
    }

    #[test]
    fn 计量型用例_容差带内通过带外失配() {
        let frames = 1_024;
        // 0.25 常量输入 peakDb ≈ -12.0412：带内（±0.5）通过，带外（±0.005）失配。
        let peak_db = 20.0 * 0.25_f64.log10();
        let pass_case = make_meter_case(
            frames,
            256,
            vec![("peakDb", ReadingWant::Finite(peak_db - 0.4), 0.5)],
        );
        let fail_case = make_meter_case(
            frames,
            256,
            vec![("peakDb", ReadingWant::Finite(peak_db - 0.02), 0.005)],
        );
        let data = vec![0.25_f32; frames * 2];
        let mut meter = LufsMeter::new(pass_case.sample_rate).expect("合法采样率");
        assert!(run_meter_case(&pass_case, &data, &mut meter).expect("合法数据").passed);
        let outcome = run_meter_case(&fail_case, &data, &mut meter).expect("合法数据");
        assert!(!outcome.passed);
        // 绝对容差不随 |want| 缩放（与音频段相对制的本质差异）。
        assert!(outcome.max_abs_deviation > 0.015);
    }

    #[test]
    fn 计量型用例_数据长度不符报结构性错误() {
        let case = make_meter_case(4, 2, vec![("peakDb", ReadingWant::Finite(0.0), 0.05)]);
        // 四段布局长度（16 个样本）对两段布局（需要 8 个）不符。
        let data = vec![0.0_f32; 16];
        let mut meter = LufsMeter::new(48_000.0).expect("合法采样率");
        assert!(run_meter_case(&case, &data, &mut meter).is_err());
    }

    #[test]
    fn 计量型用例_读数与分块无关() {
        // GWT-LUFSMETER-05 投影：want 取自整段单块馈入的读数，任意分块重放必须
        // 全部命中（want 以 1e-9 冻结——若块边界/逐样本次序被分块破坏立即暴露）。
        let frames = 20_000;
        let fs = 48_000.0;
        let input: Vec<f32> = (0..frames)
            .map(|i| ((i % 97) as f32) * 0.001_f32 + 0.1)
            .collect();
        let data: Vec<f32> = input.iter().chain(input.iter()).copied().collect();

        let want = {
            let mut reference = LufsMeter::new(fs).expect("合法采样率");
            reference.process_stereo(&input, &input);
            vec![
                ("integratedLufs", reference.get_integrated_lufs()),
                ("momentaryLufs", reference.get_momentary_lufs()),
                ("peakDb", reference.get_peak_db()),
                ("truePeakDb", reference.get_true_peak_db()),
            ]
        };
        let make = |block_size: usize| {
            let mut readings: Vec<(String, ReadingSpec)> = want
                .iter()
                .map(|(name, value)| {
                    (
                        name.to_string(),
                        ReadingSpec { want: ReadingWant::Finite(*value), tol: 1e-9 },
                    )
                })
                .collect();
            // 静音块/块数不足路径：1 个分析块 → shortTerm/lra 必为 NaN。
            readings.push(("shortTermLufs".to_string(), ReadingSpec { want: ReadingWant::Nan, tol: 0.1 }));
            readings.push(("lra".to_string(), ReadingSpec { want: ReadingWant::Nan, tol: 0.5 }));
            let mut case = make_meter_case(frames, block_size, Vec::new());
            case.readings = Some(readings);
            case
        };

        for block_size in [1_usize, 997, 4093] {
            let case = make(block_size);
            let mut meter = LufsMeter::new(fs).expect("合法采样率");
            let outcome = run_meter_case(&case, &data, &mut meter).expect("合法数据");
            assert!(
                outcome.passed,
                "blockSize={block_size} 读数偏离整段参考：{:?}",
                outcome.failures
            );
            assert_eq!(outcome.max_abs_deviation, 0.0, "blockSize={block_size} 必须逐位命中参考");
        }
    }
}
