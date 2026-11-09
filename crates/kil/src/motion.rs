//! Retargetable tweens and the motion spec (DESIGN.md §7).

use std::{
    cell::Cell,
    time::{Duration, Instant},
};

pub const SPLIT: Duration = Duration::from_millis(200);
pub const CLOSE_PANE: Duration = Duration::from_millis(160);
pub const PILL: Duration = Duration::from_millis(160);
pub const ROW_IN: Duration = Duration::from_millis(180);
pub const ROW_OUT: Duration = Duration::from_millis(140);
/// A row making room for the one being dragged past it.
pub const ROW_SHIFT: Duration = Duration::from_millis(140);
/// A dropped row settling from under the pointer into its slot.
pub const ROW_DROP: Duration = Duration::from_millis(180);
pub const TAB_SWITCH: Duration = Duration::from_millis(140);
pub const FOCUS_DIM: Duration = Duration::from_millis(120);
/// Longer than the update poll, so the download bar glides between readings.
pub const PROGRESS: Duration = Duration::from_millis(240);
/// One pass of the update bar while the size of the work isn't known.
pub const SWEEP: Duration = Duration::from_millis(1200);
/// One cursor blink fade (DESIGN.md §7: `ease_in_out`, 530 ms period).
pub const BLINK_FADE: Duration = Duration::from_millis(100);

/// The global motion scale: the reduce-motion hook (DESIGN.md §7). A scale of 0 turns
/// animation off. Thread-local so tests can't flake each other.
#[derive(Debug)]
pub struct Motion;

thread_local! {
    static SCALE: Cell<f32> = const { Cell::new(1.0) };
}

impl Motion {
    pub fn scale() -> f32 {
        SCALE.with(Cell::get)
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "tests set the scale; reading OS Reduce Motion is a non-goal"
        )
    )]
    pub fn set_scale(scale: f32) {
        SCALE.with(|scale_cell| scale_cell.set(scale.clamp(0., 1.)));
    }

    pub fn scaled(duration: Duration) -> Duration {
        duration.mul_f32(Self::scale())
    }

    pub fn enabled() -> bool {
        Self::scale() > 0.
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ease {
    OutQuint,
    InOut,
}

impl Ease {
    pub fn apply(self, t: f32) -> f32 {
        let t = t.clamp(0., 1.);
        match self {
            Self::OutQuint => 1. - (1. - t).powi(5),
            Self::InOut => {
                if t < 0.5 {
                    2. * t * t
                } else {
                    1. - (-2. * t + 2.).powi(2) / 2.
                }
            }
        }
    }
}

/// A value easing toward a target. Retargeting starts from the *current* value, so rapid
/// repeated actions never make the UI jump.
#[derive(Clone, Copy, Debug)]
pub struct Tween {
    from: f32,
    to: f32,
    start: Instant,
    duration: Duration,
    ease: Ease,
}

impl Tween {
    pub fn settled(value: f32) -> Self {
        Self {
            from: value,
            to: value,
            start: Instant::now(),
            duration: Duration::ZERO,
            ease: Ease::OutQuint,
        }
    }

    pub fn value(&self, now: Instant) -> f32 {
        if self.duration.is_zero() {
            return self.to;
        }
        let t =
            now.saturating_duration_since(self.start).as_secs_f32() / self.duration.as_secs_f32();
        self.from + (self.to - self.from) * self.ease.apply(t)
    }

    pub fn target(&self) -> f32 {
        self.to
    }

    pub fn is_animating(&self, now: Instant) -> bool {
        now < self.start + self.duration
    }

    pub fn retarget(&mut self, to: f32, duration: Duration, ease: Ease, now: Instant) {
        *self = Self {
            from: self.value(now),
            to,
            start: now,
            duration: Motion::scaled(duration),
            ease,
        };
    }

    /// Jumps to `value` (e.g. while dragging).
    pub fn set(&mut self, value: f32) {
        *self = Self::settled(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_should_reach_target_after_duration() {
        let start = Instant::now();
        let mut tween = Tween::settled(1.);
        tween.retarget(0.5, SPLIT, Ease::OutQuint, start);
        assert!((tween.value(start + SPLIT) - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn retarget_should_continue_from_current_value() {
        let start = Instant::now();
        let mut tween = Tween::settled(0.);
        tween.retarget(1., SPLIT, Ease::InOut, start);
        let midway = start + SPLIT / 2;
        let current = tween.value(midway);
        tween.retarget(0., SPLIT, Ease::InOut, midway);
        assert!((tween.value(midway) - current).abs() < f32::EPSILON);
    }

    #[test]
    fn scale_should_be_one_on_a_fresh_thread() {
        let scale = std::thread::spawn(Motion::scale).join().unwrap();
        assert!((scale - 1.).abs() < f32::EPSILON);
    }

    #[test]
    fn set_scale_should_clamp_to_zero_to_one() {
        Motion::set_scale(-1.);
        assert!((Motion::scale() - 0.).abs() < f32::EPSILON);
        Motion::set_scale(2.);
        assert!((Motion::scale() - 1.).abs() < f32::EPSILON);
        Motion::set_scale(0.5);
        assert!((Motion::scale() - 0.5).abs() < f32::EPSILON);
        Motion::set_scale(1.);
    }

    #[test]
    fn scaled_should_shorten_duration_when_scale_is_reduced() {
        Motion::set_scale(0.5);
        assert_eq!(
            Motion::scaled(Duration::from_millis(200)),
            Duration::from_millis(100)
        );
        Motion::set_scale(1.);
    }

    #[test]
    fn enabled_should_be_false_when_scale_is_zero() {
        Motion::set_scale(0.);
        assert!(!Motion::enabled());
        Motion::set_scale(0.5);
        assert!(Motion::enabled());
        Motion::set_scale(1.);
    }

    #[test]
    fn retarget_should_settle_immediately_when_scale_is_zero() {
        Motion::set_scale(0.);
        let now = Instant::now();
        let mut tween = Tween::settled(0.);
        tween.retarget(1., SPLIT, Ease::OutQuint, now);
        assert!((tween.value(now) - 1.).abs() < f32::EPSILON);
        assert!(!tween.is_animating(now));
        Motion::set_scale(1.);
    }

    #[test]
    fn retarget_should_use_scaled_duration_when_scale_is_reduced() {
        Motion::set_scale(0.5);
        let start = Instant::now();
        let mut tween = Tween::settled(0.);
        tween.retarget(1., SPLIT, Ease::OutQuint, start);
        assert!(tween.is_animating(start + SPLIT / 4));
        assert!(!tween.is_animating(start + SPLIT));
        assert!((tween.value(start + SPLIT / 2) - 1.).abs() < f32::EPSILON);
        Motion::set_scale(1.);
    }

    #[test]
    fn is_animating_should_be_false_once_settled() {
        let start = Instant::now();
        let mut tween = Tween::settled(0.);
        tween.retarget(1., SPLIT, Ease::OutQuint, start);
        assert!(!tween.is_animating(start + SPLIT));
    }
}
