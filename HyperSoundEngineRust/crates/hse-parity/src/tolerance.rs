//! 统一容差判定公式——两支线共用的对拍口径（见 `specs/` 向量格式契约）。
//!
//! 两套并立、互不混用的判定制（specs/dsp/lufs-meter.md §三.2/§五）：
//!
//! - **音频段（流式）**：逐样本相对容差
//!   `|got - want| <= value * max(|want|, floor)`，其中 `value` 为相对容差，
//!   `floor` 为小信号地板：当 `|want|` 低于地板时改用地板作基准，避免零附近的
//!   相对误差发散；
//! - **readings 标量读数（计量型）**：want 为有限数 → **绝对容差**
//!   `|got − want| ≤ tol`；want 为非有限哨兵（NaN / ±Infinity）→ **等值判定**
//!   （got 必须同为 NaN / 同号无穷大；tol 不参与）。

/// readings 期望读数的两种形态（计量型用例 JSON 的 `want` 字段；JSON 数值无法
/// 表达非有限值，故以字符串哨兵 `"NaN"` / `"+Infinity"` / `"-Infinity"` 冻结）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ReadingWant {
    /// 有限数：绝对容差判定 `|got − want| ≤ tol`。
    Finite(f64),
    /// 哨兵 `"NaN"`：got 必须同为 NaN。
    Nan,
    /// 哨兵 `"+Infinity"`：got 必须为 +∞。
    PositiveInfinity,
    /// 哨兵 `"-Infinity"`：got 必须为 −∞。
    NegativeInfinity,
}

impl ReadingWant {
    /// 判定实际读数是否满足期望（specs/dsp/lufs-meter.md §三.2）。
    ///
    /// 哨兵等值判定下 tol 不参与；有限数判定下 got 必须同为有限数（NaN/±∞
    /// 对有限 want 一律失配——读数哨兵语义不可被有限 want 容忍）。
    pub fn matches(&self, got: f64, tol: f64) -> bool {
        match *self {
            ReadingWant::Finite(want) => got.is_finite() && (got - want).abs() <= tol,
            ReadingWant::Nan => got.is_nan(),
            ReadingWant::PositiveInfinity => got == f64::INFINITY,
            ReadingWant::NegativeInfinity => got == f64::NEG_INFINITY,
        }
    }

    /// 期望值的诊断显示形态（与向量 JSON 中的冻结表示一致）。
    pub fn as_label(&self) -> String {
        match *self {
            ReadingWant::Finite(v) => format!("{v}"),
            ReadingWant::Nan => "\"NaN\"".to_string(),
            ReadingWant::PositiveInfinity => "\"+Infinity\"".to_string(),
            ReadingWant::NegativeInfinity => "\"-Infinity\"".to_string(),
        }
    }
}

/// 实际读数的诊断显示形态（Rust 的 `inf`/`-inf`/`NaN` 统一为 JSON 哨兵拼写）。
pub fn format_reading(got: f64) -> String {
    if got.is_nan() {
        "NaN".to_string()
    } else if got == f64::INFINITY {
        "+Infinity".to_string()
    } else if got == f64::NEG_INFINITY {
        "-Infinity".to_string()
    } else {
        format!("{got}")
    }
}

/// 判定单个样本是否落在容差带内（音频段相对制）。
///
/// 任何一侧出现非有限值（NaN / 无穷大）一律判为失配：
/// 冻结基线中不应出现非有限样本，出现即视为实现错误。
pub fn within_tolerance(got: f32, want: f32, value: f64, floor: f64) -> bool {
    if !got.is_finite() || !want.is_finite() {
        return false;
    }
    let diff = (f64::from(got) - f64::from(want)).abs();
    let reference = f64::from(want).abs().max(floor);
    diff <= value * reference
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALUE: f64 = 1.0e-6;
    const FLOOR: f64 = 1.0e-9;

    #[test]
    fn 完全相等在双零容差下也通过() {
        // diff = 0 <= value * max(|want|, floor) 的下边界必须成立。
        assert!(within_tolerance(0.25, 0.25, 0.0, 0.0));
        assert!(within_tolerance(-123.5, -123.5, 0.0, 0.0));
    }

    #[test]
    fn 相对带内通过带外失配() {
        // want = 1.0 时带宽为 value = 1e-6：5e-7 在带内，2e-3 远超带外。
        assert!(within_tolerance(1.0 + 5.0e-7_f32, 1.0, VALUE, FLOOR));
        assert!(!within_tolerance(1.0 + 2.0e-3_f32, 1.0, VALUE, FLOOR));
    }

    #[test]
    fn 负样本对称判定() {
        assert!(within_tolerance(-1.0 - 5.0e-7_f32, -1.0, VALUE, FLOOR));
        assert!(!within_tolerance(-1.0 - 2.0e-3_f32, -1.0, VALUE, FLOOR));
    }

    #[test]
    fn 地板主导小信号基准() {
        // want = 0 时基准退化为 floor = 1e-6，带宽 = value*floor = 1e-9。
        assert!(within_tolerance(9.0e-10_f32, 0.0, 1.0e-3, 1.0e-6));
        assert!(!within_tolerance(2.0e-9_f32, 0.0, 1.0e-3, 1.0e-6));
    }

    #[test]
    fn 非有限值一律失配() {
        assert!(!within_tolerance(f32::NAN, 1.0, VALUE, FLOOR));
        assert!(!within_tolerance(1.0, f32::NAN, VALUE, FLOOR));
        assert!(!within_tolerance(f32::INFINITY, 1.0, VALUE, FLOOR));
        assert!(!within_tolerance(1.0, f32::NEG_INFINITY, VALUE, FLOOR));
    }

    // ---------------- readings 标量读数（计量型：绝对容差 + 哨兵等值） ----------------

    #[test]
    fn 有限期望走绝对容差() {
        let want = ReadingWant::Finite(-22.0);
        assert!(want.matches(-22.0, 0.5));
        // |got − want| = 0.5 恰在带上（二进制精确值，避免十进制边界噪声）。
        assert!(want.matches(-21.5, 0.5));
        assert!(want.matches(-22.5, 0.5));
        assert!(!want.matches(-21.4, 0.5));
        // 小数值期望下绝对容差不因 |want| 小而收窄（与相对制的本质差异）。
        let small = ReadingWant::Finite(0.003);
        assert!(small.matches(0.004, 0.005));
    }

    #[test]
    fn 有限期望对非有限实际一律失配() {
        let want = ReadingWant::Finite(-1.0);
        assert!(!want.matches(f64::NAN, 1.0));
        assert!(!want.matches(f64::INFINITY, 1.0));
        assert!(!want.matches(f64::NEG_INFINITY, 1.0));
    }

    #[test]
    fn 哨兵_nan_按等值判定_tol_不参与() {
        let want = ReadingWant::Nan;
        assert!(want.matches(f64::NAN, 0.0), "NaN == NaN 哨兵等值必须匹配");
        assert!(want.matches(f64::NAN, 100.0), "哨兵判定与 tol 无关");
        assert!(!want.matches(0.0, 0.1), "有限值不满足 NaN 哨兵");
        assert!(!want.matches(f64::INFINITY, 0.1));
        assert!(!want.matches(f64::NEG_INFINITY, 0.1));
    }

    #[test]
    fn 哨兵_正负无穷_按同号等值判定() {
        assert!(ReadingWant::PositiveInfinity.matches(f64::INFINITY, 0.0));
        assert!(
            !ReadingWant::PositiveInfinity.matches(f64::NEG_INFINITY, 0.0),
            "错号无穷大必须失配"
        );
        assert!(!ReadingWant::PositiveInfinity.matches(f64::NAN, 0.0));
        assert!(!ReadingWant::PositiveInfinity.matches(1.0, 0.0));
        assert!(ReadingWant::NegativeInfinity.matches(f64::NEG_INFINITY, 0.0));
        assert!(!ReadingWant::NegativeInfinity.matches(f64::INFINITY, 0.0));
    }

    #[test]
    fn 诊断显示与向量哨兵拼写一致() {
        assert_eq!(ReadingWant::Nan.as_label(), "\"NaN\"");
        assert_eq!(ReadingWant::PositiveInfinity.as_label(), "\"+Infinity\"");
        assert_eq!(ReadingWant::NegativeInfinity.as_label(), "\"-Infinity\"");
        assert_eq!(ReadingWant::Finite(-23.5).as_label(), "-23.5");
        assert_eq!(format_reading(f64::NAN), "NaN");
        assert_eq!(format_reading(f64::INFINITY), "+Infinity");
        assert_eq!(format_reading(f64::NEG_INFINITY), "-Infinity");
        assert_eq!(format_reading(-0.1777729621775159), "-0.1777729621775159");
    }
}
