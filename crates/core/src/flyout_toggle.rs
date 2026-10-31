//! Which stock flyout (Quick Settings, notification centre) is open, inferred from our own
//! launches and the foreground changes that follow them. Pure; the app feeds it the clock.
//!
//! The stock flyouts have no API: the only evidence is that the first window to take the
//! foreground shortly after we launched one is that flyout. A second click on the same button
//! then closes it; a click on the other button, or after the flyout lost the foreground, opens.

/// How long after a launch the first foreground change still counts as the flyout appearing.
pub const CLAIM_MS: u64 = 1500;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StockFlyout {
    QuickSettings,
    Notifications,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ToggleAction {
    Open,
    Close,
}

#[derive(Default)]
pub struct FlyoutToggle {
    /// Launched, window not seen yet: the flyout and when.
    pending: Option<(StockFlyout, u64)>,
    /// The flyout and the window that claimed to be it.
    open: Option<(StockFlyout, u64)>,
}

impl FlyoutToggle {
    pub fn new() -> Self {
        Self::default()
    }

    /// A launch of `which` was just requested.
    pub fn launched(&mut self, which: StockFlyout, now_ms: u64) {
        self.open = None;
        self.pending = Some((which, now_ms));
    }

    /// The foreground window is now `fg`.
    pub fn foreground_changed(&mut self, fg: u64, now_ms: u64) {
        if let Some((which, at)) = self.pending.take() {
            if now_ms.saturating_sub(at) <= CLAIM_MS {
                self.open = Some((which, fg));
                return;
            }
        }
        if self.open.is_some_and(|(_, w)| w != fg) {
            self.open = None;
        }
    }

    /// A click on `which`'s button while `fg` is the foreground window. `Close` only when that
    /// flyout is recorded open and still in front; the caller then dismisses it.
    pub fn on_click(&mut self, which: StockFlyout, fg: u64, _now_ms: u64) -> ToggleAction {
        if self.open == Some((which, fg)) {
            self.open = None;
            self.pending = None;
            ToggleAction::Close
        } else {
            self.open = None;
            ToggleAction::Open
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use StockFlyout::*;

    #[test]
    fn second_click_closes() {
        let mut t = FlyoutToggle::new();
        assert_eq!(t.on_click(QuickSettings, 1, 0), ToggleAction::Open);
        t.launched(QuickSettings, 0);
        t.foreground_changed(77, 200);
        assert_eq!(t.on_click(QuickSettings, 77, 3000), ToggleAction::Close);
        assert_eq!(t.on_click(QuickSettings, 1, 3100), ToggleAction::Open);
    }

    #[test]
    fn other_button_switches() {
        let mut t = FlyoutToggle::new();
        t.launched(QuickSettings, 0);
        t.foreground_changed(77, 200);
        assert_eq!(t.on_click(Notifications, 77, 900), ToggleAction::Open);
    }

    #[test]
    fn dismissed_elsewhere_reopens() {
        let mut t = FlyoutToggle::new();
        t.launched(Notifications, 0);
        t.foreground_changed(77, 200);
        t.foreground_changed(5, 4000);
        assert_eq!(t.on_click(Notifications, 5, 4100), ToggleAction::Open);
    }

    #[test]
    fn late_foreground_is_not_claimed() {
        let mut t = FlyoutToggle::new();
        t.launched(QuickSettings, 0);
        t.foreground_changed(77, 2000);
        assert_eq!(t.on_click(QuickSettings, 77, 2100), ToggleAction::Open);
    }
}
