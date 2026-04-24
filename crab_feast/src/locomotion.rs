//! Speed / input → locomotion clip selection (tests cover the pure mapping).

/// Horizontal speed (m/s) and optional movement input force in \[0, 1\].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LocomotionInput {
    pub speed_horizontal: f32,
    /// `None` when not moving (idle input).
    pub move_force: Option<f32>,
}

#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocomotionAnim {
    #[default]
    Idle,
    Walk,
    Run,
}

/// Idle if almost no motion; run if speed is high or player is pushing hard on the stick.
pub fn locomotion_anim_from_speed_and_force(
    input: LocomotionInput,
    speed_idle_max: f32,
    speed_run_min: f32,
    force_run_threshold: f32,
) -> LocomotionAnim {
    let v = input.speed_horizontal;
    let force = input.move_force.unwrap_or(0.0);
    if v < speed_idle_max && input.move_force.is_none() {
        return LocomotionAnim::Idle;
    }
    if v < speed_idle_max && force < 0.01 {
        return LocomotionAnim::Idle;
    }
    if v >= speed_run_min || force >= force_run_threshold {
        LocomotionAnim::Run
    } else {
        LocomotionAnim::Walk
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_speed_idle_input_is_idle() {
        let s = locomotion_anim_from_speed_and_force(
            LocomotionInput {
                speed_horizontal: 0.0,
                move_force: None,
            },
            0.15,
            4.0,
            0.9,
        );
        assert_eq!(s, LocomotionAnim::Idle);
    }

    #[test]
    fn slow_speed_active_low_force_is_walk() {
        let s = locomotion_anim_from_speed_and_force(
            LocomotionInput {
                speed_horizontal: 1.0,
                move_force: Some(0.5),
            },
            0.15,
            4.0,
            0.9,
        );
        assert_eq!(s, LocomotionAnim::Walk);
    }

    #[test]
    fn high_speed_is_run() {
        let s = locomotion_anim_from_speed_and_force(
            LocomotionInput {
                speed_horizontal: 5.0,
                move_force: Some(0.5),
            },
            0.15,
            4.0,
            0.9,
        );
        assert_eq!(s, LocomotionAnim::Run);
    }

    #[test]
    fn full_force_is_run() {
        let s = locomotion_anim_from_speed_and_force(
            LocomotionInput {
                speed_horizontal: 0.2,
                move_force: Some(1.0),
            },
            0.15,
            4.0,
            0.9,
        );
        assert_eq!(s, LocomotionAnim::Run);
    }
}
