//! hse-parity —— HyperSoundEngine 对拍 harness（开发专用命令行工具）。
//!
//! 读取仓库根 `specs/dsp/vectors` 下的共享测试向量，用被测实现重放输入，
//! 按两支线统一的相对容差逐样本比对期望输出，打印每个用例的 PASS/FAIL
//! 与最大误差汇总。
//!
//! 用法：
//!
//! ```text
//! cargo run -p hse-parity                 # 自动定位向量目录（缺省）
//! cargo run -p hse-parity -- <specs 向量目录>
//! ```
//!
//! 退出码约定：
//!
//! - `0`：全部用例通过，或向量目录不存在/为空（Phase 0 允许空跑框架）；
//! - `1`：存在任一 FAIL（数值失配或夹具缺陷）。
//!
//! 说明：当前被测对象是直通假实现（输出=输入），因此有向量时出现成片 FAIL
//! 属预期，恰证比对逻辑有效；hse-core 真实模块落地后同一命令应转绿。

mod runner;
mod segments;
mod stages;
mod tolerance;
mod vector;

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use runner::CaseOutcome;
use vector::VectorCase;

/// 正常结束（含"无向量可跑"的空跑场景）。
const EXIT_ALL_GREEN: i32 = 0;
/// 存在任一失败用例。
const EXIT_HAS_FAILURES: i32 = 1;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg.as_str() == "-h" || arg.as_str() == "--help") {
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
    println!("用法：hse-parity [specs 向量目录]");
    println!();
    println!("不带参数时，按以下顺序自动定位 specs/dsp/vectors：");
    println!("  1. 从编译期记录的本 crate 路径（CARGO_MANIFEST_DIR）逐级向上查找；");
    println!("  2. 从当前工作目录逐级向上查找。");
    println!();
    println!("退出码：0 = 全绿或空跑（目录不存在/为空）；1 = 存在失败用例。");
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
    find_vectors_dir_upwards(&manifest_dir)
        .or_else(|| std::env::current_dir().ok().and_then(|cwd| find_vectors_dir_upwards(&cwd)))
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
            println!("警告：无法枚举目录 {}（{err}），按无向量处理。", dir.display());
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
            println!("Phase 0 允许空跑框架：本次不对拍任何向量，按通过结束（退出码 0）。");
            return EXIT_ALL_GREEN;
        }
    };

    let source_note = if explicit_dir.is_some() {
        "（命令行指定）"
    } else {
        "（自动推导）"
    };
    println!("向量目录：{}{source_note}", vectors_dir.display());

    if !vectors_dir.is_dir() {
        println!("该目录不存在——Phase 0 允许空跑框架：没有可对拍的向量，按通过结束（退出码 0）。");
        return EXIT_ALL_GREEN;
    }

    let json_files = collect_json_files(&vectors_dir);
    if json_files.is_empty() {
        println!("目录中没有 *.json 向量用例——Phase 0 允许空跑框架：没有可对拍的向量，按通过结束（退出码 0）。");
        return EXIT_ALL_GREEN;
    }

    println!(
        "发现 {} 个向量用例；被测实现：直通假实现（输出=输入）。开始对拍……",
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
                    println!("提示：文件 {label} 内声明的用例名是 {name}（两者不一致，按文件名继续）。");
                }
                let shape = format!(
                    "module={name} blockSize={} sampleRate={} channels={} frames={}",
                    case.block_size, case.sample_rate, case.channels, outcome.frames
                );
                if outcome.passed {
                    pass_count += 1;
                    println!(
                        "[PASS] {label}  {shape} blocks={} maxAbsDiff={:.3e}",
                        outcome.blocks_run, outcome.max_abs_diff
                    );
                } else {
                    fail_count += 1;
                    let mut line = format!(
                        "[FAIL] {label}  {shape} 数值失配：失配样本 {} 个，maxAbsDiff={:.3e}",
                        outcome.mismatch_count, outcome.max_abs_diff
                    );
                    if let Some(first) = &outcome.first_mismatch {
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
                if outcome.max_abs_diff > worst_abs_diff {
                    worst_abs_diff = outcome.max_abs_diff;
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
    println!("说明：直通假实现下的大面积数值 FAIL 属预期（尚无真实 DSP 实现），用于确认比对逻辑正常工作；");
    println!("      待 hse-core 模块按 specs/ 规格落地后，同一命令应当转绿（双支线门禁的 Rust 半边）。");

    if fail_count > 0 {
        EXIT_HAS_FAILURES
    } else {
        EXIT_ALL_GREEN
    }
}

/// 单个用例的完整执行产物：解析出的用例描述 + 比对结论。
type EvaluatedCase = (VectorCase, CaseOutcome);

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
    let mut stage = stages::make_stage(&case)?;
    let outcome = runner::run_case(&case, &planar_data, &mut *stage)?;
    Ok((case, outcome))
}
