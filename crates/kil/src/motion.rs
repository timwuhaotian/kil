//! Retargetable tweens and the motion spec (DESIGN.md §7).

use std::time::{Duration, Instant};

pub const SPLIT: Duration = Duration::from_millis(200);
pub const CLOSE_PANE: Duration = Duration::from_millis(160);
pub const PILL: Duration = Duration::from_millis(160);
pub const ROW_IN: Duration = Duration::from_millis(180);
pub const ROW_OUT: Duration = Duration::from_millis(140);
pub const TAB_SWITCH: Duration = Duration::from_millis(140);
pub const FOCUS_DIM: Duration = Duration::from_millis(120);

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
            duration,
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
    fn is_animating_should_be_false_once_settled() {
        let start = Instant::now();
        let mut tween = Tween::settled(0.);
        tween.retarget(1., SPLIT, Ease::OutQuint, start);
        assert!(!tween.is_animating(start + SPLIT));
    }
}
