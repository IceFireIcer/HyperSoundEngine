//! hse-parity —— HyperSoundEngine 对拍 harness（开发专用命令行工具）。
//!
//! 读取仓库根 `specs/dsp/vectors` 音频向量与 `specs/spatial/vectors` 结构化空间夹具：
//!
//! - 流式用例（moduleKind 缺省/'stream'）：把输入按 blockSize 分块驱动被测阶段，
//!   与期望输出按两支线统一的相对容差逐样本比对；
//! - 计量型用例（moduleKind='meter'，specs/dsp/lufs-meter.md §三）：把两段输入
//!   分块馈入计量模块（就地分析），全部块馈入完成后一次性读取六项读数，与
//!   readings 逐项判定（绝对容差 + NaN/±Infinity 哨兵等值）。
//! - 空间结构化用例：调用 `hrtf-core` world-listener 几何核，按字段绝对容差判定。
//!
//! 打印音频与空间用例各自的 PASS/FAIL 和最大误差汇总。
//!
//! 用法：
//!
//! ```text
//! cargo run -p hse-parity                 # 自动定位向量目录（缺省）
//! cargo run -p hse-parity -- <specs/dsp/vectors 目录>
//! ```
//!
//! 退出码约定：
//!
//! - `0`：音频与空间夹具全部通过；
//! - `1`：任一用例失败、夹具缺失或夹具结构无效。
//!
//! 说明：未落地的流式模块回退直通假实现（输出=输入），有向量时出现成片 FAIL
//! 属预期，恰证比对逻辑有效；hse-core 真实模块落地后同一命令应转绿。

mod runner;
mod segments;
mod spatial_runner;
mod spatial_vector;
mod stages;
mod tolerance;
mod vector;

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use vector::VectorCase;

/// 正常结束：音频与空间夹具全部通过。
const EXIT_ALL_GREEN: i32 = 0;
/// 存在任一失败用例。
const EXIT_HAS_FAILURES: i32 = 1;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args
        .iter()
        .any(|arg| arg.as_str() == "-h" || arg.as_str() == "--help")
    {
        print_usage();
        flush_stdout();
        return;
    }
    let explicit_dir = args.first().map(PathBuf::from);
    let exit_code = run(explicit_dir.as_deref());
    flush_stdout();
    std::process::exit(exit_code);
}

fn print_usage() {
    println!("hse-parity —— HyperSoundEngine 对拍 harness（开发专用）");
    println!();
    println!("用法：hse-parity [specs/dsp/vectors 目录]");
    println!();
    println!("不带参数时，按以下顺序自动定位 specs/dsp/vectors：");
    println!("  1. 从编译期记录的本 crate 路径（CARGO_MANIFEST_DIR）逐级向上查找；");
    println!("  2. 从当前工作目录逐级向上查找。");
    println!();
    println!("退出码：0 = 音频与空间夹具全绿；1 = 用例失败、夹具缺失或夹具无效。");
}

fn flush_stdout() {
    let _ = std::io::stdout().flush();
}

/// 选择向量目录：显式参数优先；否则两级向上推导。找不到时返回 None。
fn resolve_vectors_dir(explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(dir) = explicit {
        return Some(dir.to_path_buf());
    }
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    find_vectors_dir_upwards(&manifest_dir).or_else(|| {
        std::env::current_dir()
            .ok()
            .and_then(|cwd| find_vectors_dir_upwards(&cwd))
    })
}

/// 从 start 逐级向上找第一个包含 specs/dsp/vectors 的祖先目录。
fn find_vectors_dir_upwards(start: &Path) -> Option<PathBuf> {
    let mut cursor = Some(start);
    while let Some(dir) = cursor {
        let candidate = dir.join("specs").join("dsp").join("vectors");
        if candidate.is_dir() {
            return Some(candidate);
        }
        cursor = dir.parent();
    }
    None
}

/// 收集目录下的 *.json 用例文件，按路径排序保证输出顺序稳定。
fn collect_json_files(dir: &Path) -> Vec<PathBuf> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => {
            println!(
                "警告：无法枚举目录 {}（{err}），按无向量处理。",
                dir.display()
            );
            return Vec::new();
        }
    };
    let mut json_paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
        })
        .collect();
    json_paths.sort();
    json_paths
}

fn run(explicit_dir: Option<&Path>) -> i32 {
    let vectors_dir = match resolve_vectors_dir(explicit_dir) {
        Some(dir) => dir,
        None => {
            println!("未能定位 specs 向量目录：未通过参数指定，且从编译期路径与当前目录都找不到 specs/dsp/vectors。");
            return EXIT_HAS_FAILURES;
        }
    };

    let source_note = if explicit_dir.is_some() {
        "（命令行指定）"
    } else {
        "（自动推导）"
    };
    println!("向量目录：{}{source_note}", vectors_dir.display());

    if !vectors_dir.is_dir() {
        println!("该目录不存在：没有可对拍的音频向量。");
        return EXIT_HAS_FAILURES;
    }

    let json_files = collect_json_files(&vectors_dir);
    if json_files.is_empty() {
        println!("目录中没有 *.json 音频向量用例。");
        return EXIT_HAS_FAILURES;
    }

    println!(
        "发现 {} 个向量用例；被测实现：hse-core 真实模块（未落地流式模块回退直通占位）。开始对拍……",
        json_files.len()
    );
    println!();

    let mut pass_count = 0_usize;
    let mut fail_count = 0_usize;
    let mut worst_abs_diff = 0.0_f64;

    for json_path in &json_files {
        let label = json_path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "<未命名>".to_string());
        match evaluate_case(json_path) {
            Ok((case, outcome)) => {
                let name = case.display_name();
                if name != label {
                    println!(
                        "提示：文件 {label} 内声明的用例名是 {name}（两者不一致，按文件名继续）。"
                    );
                }
                let frames = match &outcome {
                    EvaluatedOutcome::Stream(stream) => stream.frames,
                    EvaluatedOutcome::Meter(meter_outcome) => meter_outcome.frames,
                };
                let shape = format!(
                    "module={name} blockSize={} sampleRate={} channels={} frames={}",
                    case.block_size, case.sample_rate, case.channels, frames
                );
                match outcome {
                    EvaluatedOutcome::Stream(stream) => {
                        if stream.passed {
                            pass_count += 1;
                            println!(
                                "[PASS] {label}  {shape} blocks={} maxAbsDiff={:.3e}",
                                stream.blocks_run, stream.max_abs_diff
                            );
                        } else {
                            fail_count += 1;
                            let mut line = format!(
                                "[FAIL] {label}  {shape} 数值失配：失配样本 {} 个，maxAbsDiff={:.3e}",
                                stream.mismatch_count, stream.max_abs_diff
                            );
                            if let Some(first) = &stream.first_mismatch {
                                line.push_str(&format!(
                                    "；首个失配 @{}[{}] got={:.9e} want={:.9e}",
                                    first.channel.as_label(),
                                    first.frame_index,
                                    first.got,
                                    first.want
                                ));
                            }
                            println!("{line}");
                        }
                        if stream.max_abs_diff > worst_abs_diff {
                            worst_abs_diff = stream.max_abs_diff;
                        }
                    }
                    EvaluatedOutcome::Meter(meter_outcome) => {
                        let pass_count_readings =
                            meter_outcome.checked - meter_outcome.failures.len();
                        if meter_outcome.passed {
                            pass_count += 1;
                            println!(
                                "[PASS] {label}  {shape} meter blocks={} readings={pass_count_readings}/{} maxAbsDev={:.3e}",
                                meter_outcome.blocks_run, meter_outcome.checked, meter_outcome.max_abs_deviation
                            );
                        } else {
                            fail_count += 1;
                            let mut line = format!(
                                "[FAIL] {label}  {shape} meter readings 失配 {}/{}（maxAbsDev={:.3e}）：",
                                meter_outcome.failures.len(),
                                meter_outcome.checked,
                                meter_outcome.max_abs_deviation
                            );
                            for failure in &meter_outcome.failures {
                                let spec = case
                                    .readings
                                    .as_ref()
                                    .and_then(|readings| {
                                        readings
                                            .iter()
                                            .find(|(read_name, _)| read_name == &failure.name)
                                    })
                                    .map(|(_, spec)| spec);
                                line.push_str(&format!(
                                    " {} got={} want={} tol={}",
                                    failure.name,
                                    tolerance::format_reading(failure.got),
                                    spec.map(|s| s.want.as_label())
                                        .unwrap_or_else(|| "?".to_string()),
                                    spec.map(|s| s.tol).unwrap_or(0.0),
                                ));
                            }
                            println!("{line}");
                        }
                        if meter_outcome.max_abs_deviation > worst_abs_diff {
                            worst_abs_diff = meter_outcome.max_abs_deviation;
                        }
                    }
                }
            }
            Err(reason) => {
                fail_count += 1;
                println!("[FAIL] {label}  夹具/运行错误：{reason}");
            }
        }
    }

    println!();
    println!("==== 对拍汇总 ====");
    println!(
        "总计 {} 个用例：PASS {pass_count} 个，FAIL {fail_count} 个；全程最大 |got-want| = {worst_abs_diff:.3e}",
        pass_count + fail_count
    );
    println!(
        "说明：流式用例按音频段相对容差逐样本比对；计量型（moduleKind='meter'）用例按 readings"
    );
    println!("      标量读数判定（绝对容差 + NaN/±Infinity 哨兵等值，specs/dsp/lufs-meter.md §三/§五）；");
    println!("      全部转绿即双支线门禁的 Rust 半边通过（TS 冻结向量全 PASS = exit 0）。");

    let spatial_path = vectors_dir.parent().and_then(Path::parent).map(|specs| {
        specs
            .join("spatial")
            .join("vectors")
            .join("world-listener.v1.json")
    });
    let spatial_failed = match spatial_path {
        Some(path) if path.is_file() => match spatial_vector::load_fixture(&path) {
            Ok(fixture) => {
                let outcome = spatial_runner::run_fixture(&fixture);
                println!();
                println!("==== Spatial world-listener 汇总 ====");
                println!(
                    "总计 {} 个用例：PASS {} 个，FAIL {} 个；最大绝对偏差 = {:.3e}",
                    outcome.checked,
                    outcome.passed_cases,
                    outcome.failed_cases,
                    outcome.max_abs_deviation
                );
                for failure in &outcome.failures {
                    println!("[FAIL] {failure}");
                }
                !outcome.passed
            }
            Err(reason) => {
                println!("[FAIL] Spatial world-listener 夹具错误：{reason}");
                true
            }
        },
        Some(path) => {
            println!(
                "[FAIL] 缺少 Spatial world-listener 夹具：{}",
                path.display()
            );
            true
        }
        None => {
            println!("[FAIL] 无法从 DSP 向量目录定位 specs/spatial/vectors");
            true
        }
    };

    if fail_count > 0 || spatial_failed {
        EXIT_HAS_FAILURES
    } else {
        EXIT_ALL_GREEN
    }
}

/// 单个用例的比对结论（按驱动形态二选一）。
enum EvaluatedOutcome {
    /// 流式：音频段逐样本相对容差比对。
    Stream(runner::CaseOutcome),
    /// 计量型：readings 标量读数比对（绝对容差 + 哨兵等值）。
    Meter(runner::MeterCaseOutcome),
}

/// 单个用例的完整执行产物：解析出的用例描述 + 比对结论。
type EvaluatedCase = (VectorCase, EvaluatedOutcome);

/// 解析并执行单个用例；Err 为夹具缺陷或运行期结构错误。
fn evaluate_case(json_path: &Path) -> Result<EvaluatedCase, String> {
    let case: VectorCase = vector::load_case(json_path)?;
    let f32_path = json_path.with_extension("f32");
    if !f32_path.is_file() {
        return Err(format!(
            "缺少同名 .f32 数据文件（期望路径：{}）",
            f32_path.display()
        ));
    }
    let planar_data = segments::decode_file(&f32_path)?;
    let outcome = if case.is_meter() {
        // 计量型：两段输入布局（8 × frames 字节，无期望输出段），走 readings 读数
        // 判定路径（specs/dsp/lufs-meter.md §三.3 驱动语义）。
        let mut meter = stages::make_meter(&case)?;
        EvaluatedOutcome::Meter(runner::run_meter_case(&case, &planar_data, &mut meter)?)
    } else {
        let mut stage = stages::make_stage(&case)?;
        EvaluatedOutcome::Stream(runner::run_case(&case, &planar_data, &mut *stage)?)
    };
    Ok((case, outcome))
}
