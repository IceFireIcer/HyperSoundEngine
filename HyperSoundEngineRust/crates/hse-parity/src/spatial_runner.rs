use hrtf_core::{relative_direction, Vec3, WorldListener};

use crate::spatial_vector::{SpatialCase, SpatialFixture};

#[derive(Debug)]
pub struct SpatialOutcome {
    pub passed: bool,
    pub checked: usize,
    pub passed_cases: usize,
    pub failed_cases: usize,
    pub failures: Vec<String>,
    pub max_abs_deviation: f64,
}

pub fn run_fixture(fixture: &SpatialFixture) -> SpatialOutcome {
    let mut failures = Vec::new();
    let mut failed_cases = 0usize;
    let mut max_abs_deviation = 0.0_f64;
    for case in &fixture.cases {
        let failures_before = failures.len();
        let got = relative(case);
        evaluate_field(
            case,
            "azimuthDeg",
            got.azimuth_deg,
            case.expected.azimuth_deg,
            fixture.angle_abs,
            &mut max_abs_deviation,
            &mut failures,
        );
        evaluate_field(
            case,
            "elevationDeg",
            got.elevation_deg,
            case.expected.elevation_deg,
            fixture.angle_abs,
            &mut max_abs_deviation,
            &mut failures,
        );
        evaluate_field(
            case,
            "distance",
            got.distance,
            case.expected.distance,
            fixture.distance_abs,
            &mut max_abs_deviation,
            &mut failures,
        );
        if failures.len() > failures_before {
            failed_cases += 1;
        }
    }
    SpatialOutcome {
        passed: failed_cases == 0,
        checked: fixture.cases.len(),
        passed_cases: fixture.cases.len() - failed_cases,
        failed_cases,
        failures,
        max_abs_deviation,
    }
}

fn relative(case: &SpatialCase) -> hrtf_core::RelativeDirection {
    relative_direction(
        WorldListener {
            position: Vec3 {
                x: case.listener.position.x,
                y: case.listener.position.y,
                z: case.listener.position.z,
            },
            yaw_deg: case.listener.yaw,
        },
        Vec3 {
            x: case.source.x,
            y: case.source.y,
            z: case.source.z,
        },
    )
}

fn evaluate_field(
    case: &SpatialCase,
    field: &str,
    got: f64,
    want: f64,
    tolerance: f64,
    max_abs_deviation: &mut f64,
    failures: &mut Vec<String>,
) {
    let deviation = (got - want).abs();
    *max_abs_deviation = max_abs_deviation.max(deviation);
    if !got.is_finite() || deviation > tolerance {
        failures.push(format!(
            "{}.{field}: got={got:.15e} want={want:.15e} tol={tolerance:.3e}",
            case.id
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spatial_vector::{Expected, Listener, Vec3 as FixtureVec3};

    fn fixture(expected_azimuth: f64) -> SpatialFixture {
        SpatialFixture {
            angle_abs: 1e-9,
            distance_abs: 1e-9,
            cases: vec![SpatialCase {
                id: "front".to_string(),
                listener: Listener {
                    position: FixtureVec3 {
                        x: 0.0,
                        y: 0.0,
                        z: 0.0,
                    },
                    yaw: 0.0,
                },
                source: FixtureVec3 {
                    x: 0.0,
                    y: 0.0,
                    z: 1.0,
                },
                expected: Expected {
                    azimuth_deg: expected_azimuth,
                    elevation_deg: 0.0,
                    distance: 1.0,
                },
            }],
        }
    }

    #[test]
    fn 合法夹具通过() {
        let outcome = run_fixture(&fixture(0.0));
        assert!(outcome.passed);
        assert_eq!(outcome.checked, 1);
        assert_eq!(outcome.passed_cases, 1);
        assert_eq!(outcome.failed_cases, 0);
        assert!(outcome.failures.is_empty());
    }

    #[test]
    fn 多字段失配仍只计一个失败case() {
        let mut fixture = fixture(1.0);
        fixture.cases[0].expected.elevation_deg = 1.0;
        fixture.cases[0].expected.distance = 2.0;
        let outcome = run_fixture(&fixture);
        assert_eq!(outcome.checked, 1);
        assert_eq!(outcome.passed_cases, 0);
        assert_eq!(outcome.failed_cases, 1);
        assert_eq!(outcome.failures.len(), 3);
        assert!(outcome
            .failures
            .iter()
            .any(|failure| failure.contains("front.azimuthDeg")));
        assert!(outcome
            .failures
            .iter()
            .any(|failure| failure.contains("front.elevationDeg")));
        assert!(outcome
            .failures
            .iter()
            .any(|failure| failure.contains("front.distance")));
    }
}
