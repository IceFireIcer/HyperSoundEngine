//! Right-handed world-space to listener-relative direction conversion.

/// A position in the right-handed world coordinate system, in meters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vec3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

/// Listener position and horizontal orientation.
///
/// A yaw of zero faces `+Z`; positive yaw turns toward `+X`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorldListener {
    pub position: Vec3,
    pub yaw_deg: f64,
}

/// Direction from a listener to a source.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RelativeDirection {
    pub azimuth_deg: f64,
    pub elevation_deg: f64,
    pub distance: f64,
}

/// Wraps an angle in degrees to `[-180, 180)`.
pub fn wrap_azimuth_deg(angle: f64) -> f64 {
    assert!(angle.is_finite(), "azimuth angle must be finite");
    (angle + 180.0).rem_euclid(360.0) - 180.0
}

/// Converts a world-space source position into listener-relative direction.
///
/// # Panics
///
/// Panics when any listener or source component is not finite.
pub fn relative_direction(listener: WorldListener, source: Vec3) -> RelativeDirection {
    assert_finite(listener, source);

    let dx = source.x - listener.position.x;
    let dy = source.y - listener.position.y;
    let dz = source.z - listener.position.z;
    let distance = (dx * dx + dy * dy + dz * dz).sqrt();

    if distance == 0.0 {
        return RelativeDirection {
            azimuth_deg: 0.0,
            elevation_deg: 0.0,
            distance: 0.0,
        };
    }

    let world_azimuth_deg = dx.atan2(dz).to_degrees();
    let elevation_ratio = (dy / distance).clamp(-1.0, 1.0);

    RelativeDirection {
        azimuth_deg: wrap_azimuth_deg(world_azimuth_deg - listener.yaw_deg),
        elevation_deg: elevation_ratio.asin().to_degrees(),
        distance,
    }
}

fn assert_finite(listener: WorldListener, source: Vec3) {
    assert!(
        listener.position.x.is_finite()
            && listener.position.y.is_finite()
            && listener.position.z.is_finite()
            && listener.yaw_deg.is_finite()
            && source.x.is_finite()
            && source.y.is_finite()
            && source.z.is_finite(),
        "listener and source values must be finite"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPSILON: f64 = 1e-12;

    fn listener(position: Vec3, yaw_deg: f64) -> WorldListener {
        WorldListener { position, yaw_deg }
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() <= EPSILON,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn maps_six_axis_aligned_directions() {
        let origin = Vec3 {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        };
        let listener = listener(origin, 0.0);
        let cases = [
            (
                Vec3 {
                    x: 0.0,
                    y: 0.0,
                    z: 5.0,
                },
                0.0,
                0.0,
            ),
            (
                Vec3 {
                    x: 5.0,
                    y: 0.0,
                    z: 0.0,
                },
                90.0,
                0.0,
            ),
            (
                Vec3 {
                    x: -5.0,
                    y: 0.0,
                    z: 0.0,
                },
                -90.0,
                0.0,
            ),
            (
                Vec3 {
                    x: 0.0,
                    y: 0.0,
                    z: -5.0,
                },
                -180.0,
                0.0,
            ),
            (
                Vec3 {
                    x: 0.0,
                    y: 5.0,
                    z: 0.0,
                },
                0.0,
                90.0,
            ),
            (
                Vec3 {
                    x: 0.0,
                    y: -5.0,
                    z: 0.0,
                },
                0.0,
                -90.0,
            ),
        ];

        for (source, azimuth, elevation) in cases {
            let result = relative_direction(listener, source);
            assert_close(result.azimuth_deg, azimuth);
            assert_close(result.elevation_deg, elevation);
            assert_close(result.distance, 5.0);
        }
    }

    #[test]
    fn wraps_to_half_open_azimuth_range() {
        let cases = [
            (-540.0, -180.0),
            (-360.0, 0.0),
            (-181.0, 179.0),
            (-180.0, -180.0),
            (180.0, -180.0),
            (181.0, -179.0),
            (360.0, 0.0),
            (540.0, -180.0),
        ];

        for (angle, expected) in cases {
            assert_close(wrap_azimuth_deg(angle), expected);
        }
    }

    #[test]
    fn subtracts_listener_yaw_and_wraps_full_turns() {
        let source = Vec3 {
            x: -0.173_648_177_666_930_33,
            y: 0.0,
            z: -0.984_807_753_012_208,
        };
        let origin = Vec3 {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        };

        assert_close(
            relative_direction(listener(origin, 30.0), source).azimuth_deg,
            160.0,
        );
        assert_close(
            relative_direction(listener(origin, 390.0), source).azimuth_deg,
            160.0,
        );
    }

    #[test]
    fn is_invariant_under_shared_translation() {
        let base = relative_direction(
            listener(
                Vec3 {
                    x: 0.0,
                    y: 0.0,
                    z: 0.0,
                },
                25.0,
            ),
            Vec3 {
                x: 3.0,
                y: 4.0,
                z: 12.0,
            },
        );
        let translated = relative_direction(
            listener(
                Vec3 {
                    x: 10.0,
                    y: -2.0,
                    z: 7.0,
                },
                25.0,
            ),
            Vec3 {
                x: 13.0,
                y: 2.0,
                z: 19.0,
            },
        );

        assert_close(translated.azimuth_deg, base.azimuth_deg);
        assert_close(translated.elevation_deg, base.elevation_deg);
        assert_close(translated.distance, base.distance);
    }

    #[test]
    fn returns_zeroes_for_coincident_points() {
        let point = Vec3 {
            x: 1.0,
            y: 2.0,
            z: 3.0,
        };
        let result = relative_direction(listener(point, 725.0), point);

        assert_eq!(
            result,
            RelativeDirection {
                azimuth_deg: 0.0,
                elevation_deg: 0.0,
                distance: 0.0,
            }
        );
    }
}
