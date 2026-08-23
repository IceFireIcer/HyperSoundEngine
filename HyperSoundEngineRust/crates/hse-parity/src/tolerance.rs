//! 统一容差判定公式——两支线共用的对拍口径（见 `specs/` 向量格式契约）。
//!
//! 对每个样本判定：`|got - want| <= value * max(|want|, floor)`。
//! 其中 `value` 为相对容差，`floor` 为小信号地板：当 `|want|` 低于地板时
//! 改用地板作基准，避免零附近的相对误差发散。

/// 判定单个样本是否落在容差带内。
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
}
