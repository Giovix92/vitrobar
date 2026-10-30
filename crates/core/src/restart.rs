//! When the watchdog restarts a crashed bar, and when it stops trying.

/// What to do after a crash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Restart,
    GiveUp,
}

/// Remembers recent crashes: the fourth one within [`RestartPolicy::WINDOW_MS`] gives up.
#[derive(Debug, Default, Clone)]
pub struct RestartPolicy {
    crashes: Vec<u64>,
}

impl RestartPolicy {
    /// Crashes this close together count towards giving up.
    pub const WINDOW_MS: u64 = 60_000;
    /// Restarts allowed within the window; the next crash gives up.
    pub const MAX_RESTARTS: usize = 3;

    pub fn new() -> Self {
        Self::default()
    }

    /// Records a crash at `now_ms` (any monotonic clock) and decides what to do.
    pub fn on_crash(&mut self, now_ms: u64) -> Decision {
        self.crashes
            .retain(|&t| now_ms.saturating_sub(t) < Self::WINDOW_MS);
        self.crashes.push(now_ms);
        if self.crashes.len() > Self::MAX_RESTARTS {
            Decision::GiveUp
        } else {
            Decision::Restart
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gives_up_on_fourth_crash_within_a_minute() {
        let mut p = RestartPolicy::new();
        assert_eq!(p.on_crash(0), Decision::Restart);
        assert_eq!(p.on_crash(10_000), Decision::Restart);
        assert_eq!(p.on_crash(20_000), Decision::Restart);
        assert_eq!(p.on_crash(30_000), Decision::GiveUp);
    }

    #[test]
    fn old_crashes_forgotten() {
        let mut p = RestartPolicy::new();
        for t in [0, 10_000, 20_000] {
            p.on_crash(t);
        }
        assert_eq!(p.on_crash(90_000), Decision::Restart);
    }

    #[test]
    fn window_is_sliding() {
        let mut p = RestartPolicy::new();
        for t in [0, 30_000, 50_000] {
            assert_eq!(p.on_crash(t), Decision::Restart);
        }
        // 0 is now more than a minute old: three in the window.
        assert_eq!(p.on_crash(61_000), Decision::Restart);
        assert_eq!(p.on_crash(62_000), Decision::GiveUp);
    }
}
