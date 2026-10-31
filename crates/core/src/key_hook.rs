//! The Windows-key decisions of the low-level keyboard hook, kept pure so every rule is
//! unit-tested without installing a hook.
//!
//! The hook thread feeds each physical (not injected) key event to [`HookState::on_event`]
//! and gets back what to do with it ([`HookDecision`]) and, at most, one thing for the UI
//! thread to do ([`HookOutput`]). Nothing here keeps key data beyond the state needed for the
//! decision in progress: which Windows key is held, whether something else was pressed with
//! it, which held keys were swallowed, and which modifiers are down. No history is kept.
//!
//! Rules (spec Task 8):
//! - A lone Windows-key tap opens Vitrobar's Start. Its key-up is let through after the mask
//!   key `VK 0xE8` (unassigned), so Windows sees a "combination" and its own Start stays shut.
//! - Win+1…9 launch / activate the n-th bar app, Win+T / Win+B enter the bar's keyboard focus
//!   mode. Those keys are swallowed (down and up); then the Windows-key up is masked too, or
//!   Windows would see a lone tap and open its Start.
//! - Every other combination (Win+E, Win+Shift+S, Ctrl+Win+D, Shift+Win+3, …) passes
//!   untouched, as does everything while disabled.
//! - Win+T / Win+B enter focus mode at once (the UI thread turns it off if there is nothing
//!   to focus). In focus mode the navigation keys are swallowed and reported; any other key
//!   leaves it.
//! - Stuck-key safety: a key-up is swallowed exactly when its key-down was, unless that
//!   down is older than [`SWALLOW_TTL_MS`] (its up was lost) or the session switched.
//! - A mouse button anywhere ends focus mode ([`HookState::on_mouse_down`]).
//! - Hung-UI fallback: when an output has waited more than [`UI_STALL_MS`] for the UI thread
//!   to drain it, the hook behaves as disabled (Windows' own Start and shortcuts, no focus
//!   mode) until the UI drains again ([`HookState::ui_drained`]). Pairs already in flight are
//!   finished first: a swallowed down still gets its repeats and its up swallowed, and a
//!   Windows-key combination we swallowed part of still has its up masked.

/// Virtual-key codes the decisions use (Win32 values; `vitro-core` has no `windows` crate).
pub const VK_TAB: u32 = 0x09;
pub const VK_RETURN: u32 = 0x0D;
pub const VK_SHIFT: u32 = 0x10;
pub const VK_CONTROL: u32 = 0x11;
pub const VK_MENU: u32 = 0x12;
pub const VK_ESCAPE: u32 = 0x1B;
pub const VK_SPACE: u32 = 0x20;
pub const VK_END: u32 = 0x23;
pub const VK_HOME: u32 = 0x24;
pub const VK_LEFT: u32 = 0x25;
pub const VK_UP: u32 = 0x26;
pub const VK_RIGHT: u32 = 0x27;
pub const VK_DOWN: u32 = 0x28;
pub const VK_LWIN: u32 = 0x5B;
pub const VK_RWIN: u32 = 0x5C;
pub const VK_APPS: u32 = 0x5D;
pub const VK_LSHIFT: u32 = 0xA0;
pub const VK_RSHIFT: u32 = 0xA1;
pub const VK_LCONTROL: u32 = 0xA2;
pub const VK_RCONTROL: u32 = 0xA3;
pub const VK_LMENU: u32 = 0xA4;
pub const VK_RMENU: u32 = 0xA5;
/// The mask key: unassigned by Windows, injected around a Windows-key up so the shell sees
/// a combination instead of a lone tap.
pub const VK_MASK: u32 = 0xE8;

/// One physical key event as the hook sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyEv {
    Down(u32),
    Up(u32),
}

/// What the hook does with the event it is deciding on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookDecision {
    /// Let it through (`CallNextHookEx`).
    Pass,
    /// Eat it (return 1).
    Swallow,
    /// Inject the mask key (`VK_MASK` down + up) first, then let this event through.
    PassAndMask,
}

/// Work for the UI thread (queued, then a payload-free wake-up).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookOutput {
    /// With the Windows-key up's event time (ms, the `GetTickCount` clock).
    OpenStart(u32),
    /// Win+n, `n` in 1..=9.
    LaunchNth(u8),
    FocusApps,
    FocusTray,
    /// A navigation key went down while in focus mode.
    FocusKey(u32),
    /// A non-navigation key went down while in focus mode (it was let through).
    ExitFocus,
}

/// The physical state the hook thread reads back (`GetAsyncKeyState`) before a decision, so
/// a key-up the hook never saw (the secure desktop after Win+L or Ctrl+Alt+Del eats them)
/// cannot leave a Windows key or a modifier "held" forever.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Held {
    pub win: bool,
    pub modifier: bool,
}

/// An output the UI thread has not drained for this long (ms) means the UI is hung or busy:
/// the hook falls back to stock behaviour.
pub const UI_STALL_MS: u32 = 500;

/// A swallowed key whose up has not come within this long (no auto-repeat refreshed it) is
/// forgotten.
pub const SWALLOW_TTL_MS: u32 = 2000;

/// The navigation keys focus mode swallows.
pub const FOCUS_KEYS: [u32; 11] = [
    VK_LEFT, VK_RIGHT, VK_UP, VK_DOWN, VK_HOME, VK_END, VK_RETURN, VK_SPACE, VK_TAB, VK_ESCAPE,
    VK_APPS,
];

fn is_win(vk: u32) -> bool {
    vk == VK_LWIN || vk == VK_RWIN
}

fn is_modifier(vk: u32) -> bool {
    matches!(
        vk,
        VK_SHIFT
            | VK_CONTROL
            | VK_MENU
            | VK_LSHIFT
            | VK_RSHIFT
            | VK_LCONTROL
            | VK_RCONTROL
            | VK_LMENU
            | VK_RMENU
    )
}

/// A set of virtual-key codes (0..=255).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct VkSet([u64; 4]);

impl VkSet {
    fn insert(&mut self, vk: u32) {
        if vk < 256 {
            self.0[(vk / 64) as usize] |= 1 << (vk % 64);
        }
    }
    fn remove(&mut self, vk: u32) -> bool {
        let had = self.contains(vk);
        if vk < 256 {
            self.0[(vk / 64) as usize] &= !(1 << (vk % 64));
        }
        had
    }
    fn contains(&self, vk: u32) -> bool {
        vk < 256 && self.0[(vk / 64) as usize] & (1 << (vk % 64)) != 0
    }
    fn clear(&mut self) {
        self.0 = [0; 4];
    }
    fn is_empty(&self) -> bool {
        self.0 == [0; 4]
    }
}

#[derive(Debug, Clone)]
pub struct HookState {
    enabled: bool,
    focus_mode: bool,
    /// Windows keys currently held (only these two codes are ever stored).
    wins: VkSet,
    /// Something else went down while a Windows key was held.
    other_pressed: bool,
    /// We swallowed a key of the current Windows-key combination: its up must be masked.
    swallowed_combo: bool,
    /// Keys whose down we swallowed and whose up we have not seen yet ...
    swallowed: VkSet,
    /// ... and when (event time, ms) each last went down: older than [`SWALLOW_TTL_MS`],
    /// an entry is forgotten (its up was lost; the next press must not be eaten).
    swallowed_at: [u32; 256],
    /// Shift / Ctrl / Alt held (any side).
    mods: VkSet,
    /// Event time of the oldest output the UI thread has not drained yet.
    pending_since: Option<u32>,
}

impl HookState {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            focus_mode: false,
            wins: VkSet::default(),
            other_pressed: false,
            swallowed_combo: false,
            swallowed: VkSet::default(),
            swallowed_at: [0; 256],
            mods: VkSet::default(),
            pending_since: None,
        }
    }

    /// The UI thread took every queued output: the hung-UI fallback (if on) ends.
    pub fn ui_drained(&mut self) {
        self.pending_since = None;
    }

    /// An output has waited for the UI thread longer than [`UI_STALL_MS`] at `now`.
    pub fn ui_stalled(&self, now: u32) -> bool {
        // Signed: event times can come very slightly out of order.
        self.pending_since
            .is_some_and(|t| now.wrapping_sub(t) as i32 > UI_STALL_MS as i32)
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn focus_mode(&self) -> bool {
        self.focus_mode
    }

    /// Disabling also leaves focus mode; keys already swallowed still get their ups eaten.
    pub fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
        if !on {
            self.focus_mode = false;
        }
    }

    pub fn set_focus_mode(&mut self, on: bool) {
        self.focus_mode = on && self.enabled;
    }

    /// Corrects the held state from the physical one before deciding on a key-down.
    /// Only ever forgets: a key the hook saw go up stays up, and a key it never saw go down
    /// is not invented.
    pub fn sync_held(&mut self, held: Held) {
        if !held.win {
            self.wins.clear();
            self.other_pressed = false;
            self.swallowed_combo = false;
        }
        if !held.modifier {
            self.mods.clear();
        }
    }

    /// The session or the input desktop changed: ups may have been lost.
    pub fn forget_swallowed(&mut self) {
        self.swallowed.clear();
    }

    /// A mouse button went down anywhere (the mouse hook runs only in focus mode): focus
    /// mode ends at once, so the next key reaches the app that was clicked.
    pub fn on_mouse_down(&mut self) -> Option<HookOutput> {
        std::mem::take(&mut self.focus_mode).then_some(HookOutput::ExitFocus)
    }

    fn swallow(&mut self, vk: u32, now: u32) {
        self.swallowed.insert(vk);
        if let Some(t) = self.swallowed_at.get_mut(vk as usize) {
            *t = now;
        }
    }

    /// `vk` was swallowed and that is still current (a stale entry is dropped here).
    fn still_swallowed(&mut self, vk: u32, now: u32) -> bool {
        if !self.swallowed.contains(vk) {
            return false;
        }
        let at = self.swallowed_at.get(vk as usize).copied().unwrap_or(now);
        // Signed: event times can come very slightly out of order.
        if now.wrapping_sub(at) as i32 > SWALLOW_TTL_MS as i32 {
            self.swallowed.remove(vk);
            return false;
        }
        true
    }

    /// [`HookState::on_event_at`] with no clock (tests: nothing ever ages).
    pub fn on_event(&mut self, ev: KeyEv) -> (HookDecision, Option<HookOutput>) {
        self.on_event_at(ev, 0)
    }

    /// Decides on one event; `now` is its time stamp (ms, `KBDLLHOOKSTRUCT::time`). The
    /// caller queues the output (if any) for the UI thread and calls [`Self::ui_drained`]
    /// whenever the UI takes the queue.
    pub fn on_event_at(&mut self, ev: KeyEv, now: u32) -> (HookDecision, Option<HookOutput>) {
        if self.ui_stalled(now) {
            return self.on_event_stalled(ev, now);
        }
        let r = self.decide(ev, now);
        if r.1.is_some() && self.pending_since.is_none() {
            self.pending_since = Some(now);
        }
        r
    }

    /// The UI is not draining: decide as if disabled (stock), but finish in-flight pairs.
    fn on_event_stalled(&mut self, ev: KeyEv, now: u32) -> (HookDecision, Option<HookOutput>) {
        // Focus mode would eat the navigation keys for a UI that cannot act on them.
        let left_focus = std::mem::take(&mut self.focus_mode);
        let enabled = std::mem::replace(&mut self.enabled, false);
        let (d, _) = self.decide(ev, now);
        self.enabled = enabled;
        (d, left_focus.then_some(HookOutput::ExitFocus))
    }

    fn decide(&mut self, ev: KeyEv, now: u32) -> (HookDecision, Option<HookOutput>) {
        use HookDecision::*;
        match ev {
            KeyEv::Up(vk) => {
                // Whatever else changed: a swallowed down gets its up swallowed, nothing else.
                if self.still_swallowed(vk, now) {
                    self.swallowed.remove(vk);
                    return (Swallow, None);
                }
                if is_modifier(vk) {
                    self.mods.remove(vk);
                } else if is_win(vk) && self.wins.remove(vk) {
                    if !self.wins.is_empty() {
                        // The other Windows key is still held: a combination.
                        self.other_pressed = true;
                        return (Pass, None);
                    }
                    let lone = !std::mem::take(&mut self.other_pressed);
                    if std::mem::take(&mut self.swallowed_combo) {
                        // Windows saw nothing between Win down and up: mask, or its Start opens.
                        return (PassAndMask, None);
                    }
                    if self.enabled && lone {
                        return (PassAndMask, Some(HookOutput::OpenStart(now)));
                    }
                }
                (Pass, None)
            }
            KeyEv::Down(vk) => {
                if is_win(vk) {
                    if self.wins.is_empty() {
                        self.other_pressed = false;
                        self.swallowed_combo = false;
                    } else if !self.wins.contains(vk) {
                        self.other_pressed = true;
                    }
                    self.wins.insert(vk);
                    return self.pass_or_exit_focus();
                }
                if self.still_swallowed(vk, now) {
                    // Auto-repeat of a key we swallowed: still ours. Arrows repeat the move.
                    self.swallow(vk, now);
                    let out = (self.focus_mode && FOCUS_KEYS.contains(&vk))
                        .then_some(HookOutput::FocusKey(vk));
                    return (Swallow, out);
                }
                let win_held = !self.wins.is_empty();
                if win_held {
                    self.other_pressed = true;
                }
                if is_modifier(vk) {
                    self.mods.insert(vk);
                    return self.pass_or_exit_focus();
                }
                if !self.enabled {
                    return (Pass, None);
                }
                if win_held {
                    let out = match vk {
                        0x31..=0x39 if self.mods.is_empty() => {
                            Some(HookOutput::LaunchNth((vk - 0x30) as u8))
                        }
                        0x54 if self.mods.is_empty() => Some(HookOutput::FocusApps),
                        0x42 if self.mods.is_empty() => Some(HookOutput::FocusTray),
                        _ => None,
                    };
                    if out.is_some() {
                        self.swallow(vk, now);
                        self.swallowed_combo = true;
                        // Focus mode starts now, not when the UI thread gets to it.
                        self.focus_mode |=
                            matches!(out, Some(HookOutput::FocusApps | HookOutput::FocusTray));
                        return (Swallow, out);
                    }
                    // Win+Arrow, Win+Tab, Win+D, ...: Windows' own, and the end of focus mode.
                    return self.pass_or_exit_focus();
                }
                if self.focus_mode {
                    if FOCUS_KEYS.contains(&vk) {
                        self.swallow(vk, now);
                        return (Swallow, Some(HookOutput::FocusKey(vk)));
                    }
                    return self.pass_or_exit_focus();
                }
                (Pass, None)
            }
        }
    }

    /// A key that is not a navigation key: let it through, leaving focus mode if in it.
    fn pass_or_exit_focus(&mut self) -> (HookDecision, Option<HookOutput>) {
        if std::mem::take(&mut self.focus_mode) {
            (HookDecision::Pass, Some(HookOutput::ExitFocus))
        } else {
            (HookDecision::Pass, None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::HookDecision::*;
    use super::HookOutput::*;
    use super::KeyEv::*;
    use super::*;

    fn tap(h: &mut HookState, vk: u32) -> [(HookDecision, Option<HookOutput>); 2] {
        [h.on_event(Down(vk)), h.on_event(Up(vk))]
    }

    #[test]
    fn lone_win_tap_opens_start_with_mask() {
        let mut h = HookState::new(true);
        assert_eq!(h.on_event(Down(VK_LWIN)).0, Pass);
        assert_eq!(h.on_event(Up(VK_LWIN)), (PassAndMask, Some(OpenStart(0))));
    }

    #[test]
    fn win_e_passes_untouched() {
        let mut h = HookState::new(true);
        h.on_event(Down(VK_LWIN));
        assert_eq!(h.on_event(Down(0x45)), (Pass, None));
        assert_eq!(h.on_event(Up(0x45)), (Pass, None));
        assert_eq!(h.on_event(Up(VK_LWIN)), (Pass, None));
    }

    #[test]
    fn win_digit_launches_and_swallows_both_edges() {
        let mut h = HookState::new(true);
        h.on_event(Down(VK_LWIN));
        assert_eq!(h.on_event(Down(0x33)), (Swallow, Some(LaunchNth(3))));
        assert_eq!(h.on_event(Up(0x33)), (Swallow, None));
        // Windows saw Win down and up with nothing between: masked, or its Start would open.
        assert_eq!(h.on_event(Up(VK_LWIN)), (PassAndMask, None));
        for (vk, n) in [(0x31, 1), (0x39, 9)] {
            h.on_event(Down(VK_LWIN));
            assert_eq!(h.on_event(Down(vk)), (Swallow, Some(LaunchNth(n))));
            assert_eq!(h.on_event(Up(vk)), (Swallow, None));
            assert_eq!(h.on_event(Up(VK_LWIN)), (PassAndMask, None));
        }
        // Win+0 is not ours.
        h.on_event(Down(VK_LWIN));
        assert_eq!(tap(&mut h, 0x30), [(Pass, None), (Pass, None)]);
        assert_eq!(h.on_event(Up(VK_LWIN)), (Pass, None));
    }

    #[test]
    fn a_held_digit_repeats_without_relaunching() {
        let mut h = HookState::new(true);
        h.on_event(Down(VK_LWIN));
        assert_eq!(h.on_event(Down(0x32)), (Swallow, Some(LaunchNth(2))));
        assert_eq!(h.on_event(Down(0x32)), (Swallow, None));
        assert_eq!(h.on_event(Down(0x32)), (Swallow, None));
        assert_eq!(h.on_event(Up(0x32)), (Swallow, None));
    }

    #[test]
    fn digit_up_after_win_up_is_still_swallowed() {
        let mut h = HookState::new(true);
        h.on_event(Down(VK_LWIN));
        h.on_event(Down(0x34));
        assert_eq!(h.on_event(Up(VK_LWIN)), (PassAndMask, None));
        assert_eq!(h.on_event(Up(0x34)), (Swallow, None));
        // ... and a later plain 4 is a plain 4.
        assert_eq!(tap(&mut h, 0x34), [(Pass, None), (Pass, None)]);
    }

    #[test]
    fn win_t_and_b() {
        let mut h = HookState::new(true);
        h.on_event(Down(VK_LWIN));
        assert_eq!(h.on_event(Down(0x54)), (Swallow, Some(FocusApps)));
        assert_eq!(h.on_event(Up(0x54)), (Swallow, None));
        assert_eq!(h.on_event(Up(VK_LWIN)), (PassAndMask, None));
        h.on_event(Down(VK_RWIN));
        assert_eq!(h.on_event(Down(0x42)), (Swallow, Some(FocusTray)));
        assert_eq!(h.on_event(Up(0x42)), (Swallow, None));
        assert_eq!(h.on_event(Up(VK_RWIN)), (PassAndMask, None));
    }

    #[test]
    fn win_t_enters_focus_mode_at_once() {
        // The arrows typed right after Win+T must not race the UI thread's answer.
        let mut h = HookState::new(true);
        h.on_event(Down(VK_LWIN));
        h.on_event(Down(0x54));
        h.on_event(Up(0x54));
        h.on_event(Up(VK_LWIN));
        assert!(h.focus_mode());
        assert_eq!(
            h.on_event(Down(VK_RIGHT)),
            (Swallow, Some(FocusKey(VK_RIGHT)))
        );
        // The UI thread found nothing to focus: it turns focus mode off again.
        h.set_focus_mode(false);
        h.on_event(Up(VK_RIGHT));
        assert_eq!(tap(&mut h, VK_RIGHT), [(Pass, None), (Pass, None)]);
    }

    #[test]
    fn disabled_is_fully_stock() {
        let mut h = HookState::new(false);
        h.on_event(Down(VK_LWIN));
        assert_eq!(h.on_event(Up(VK_LWIN)), (Pass, None));
        h.on_event(Down(VK_LWIN));
        for vk in [0x31, 0x54, 0x42, 0x45] {
            assert_eq!(tap(&mut h, vk), [(Pass, None), (Pass, None)]);
        }
        assert_eq!(h.on_event(Up(VK_LWIN)), (Pass, None));
        h.set_focus_mode(true);
        assert!(!h.focus_mode(), "no focus mode while disabled");
        assert_eq!(tap(&mut h, VK_LEFT), [(Pass, None), (Pass, None)]);
    }

    #[test]
    fn disabling_mid_combo_still_eats_the_swallowed_up() {
        let mut h = HookState::new(true);
        h.on_event(Down(VK_LWIN));
        h.on_event(Down(0x35));
        h.set_enabled(false);
        assert_eq!(h.on_event(Up(0x35)), (Swallow, None));
        // The combination had a swallowed key: still masked, or the stock Start opens.
        assert_eq!(h.on_event(Up(VK_LWIN)), (PassAndMask, None));
        h.on_event(Down(VK_LWIN));
        assert_eq!(h.on_event(Up(VK_LWIN)), (Pass, None));
    }

    #[test]
    fn focus_mode_swallows_nav_and_exits_on_other() {
        let mut h = HookState::new(true);
        h.set_focus_mode(true);
        for vk in FOCUS_KEYS {
            assert_eq!(
                h.on_event(Down(vk)),
                (Swallow, Some(FocusKey(vk))),
                "{vk:#x}"
            );
            assert_eq!(h.on_event(Up(vk)), (Swallow, None), "{vk:#x}");
        }
        // Holding an arrow repeats the move.
        assert_eq!(
            h.on_event(Down(VK_RIGHT)),
            (Swallow, Some(FocusKey(VK_RIGHT)))
        );
        assert_eq!(
            h.on_event(Down(VK_RIGHT)),
            (Swallow, Some(FocusKey(VK_RIGHT)))
        );
        assert_eq!(h.on_event(Up(VK_RIGHT)), (Swallow, None));
        assert_eq!(h.on_event(Down(0x41)), (Pass, Some(ExitFocus)));
        assert!(
            !h.focus_mode(),
            "left at once, before the UI thread answers"
        );
        assert_eq!(h.on_event(Up(0x41)), (Pass, None));
        assert_eq!(tap(&mut h, VK_LEFT), [(Pass, None), (Pass, None)]);
    }

    #[test]
    fn leaving_focus_mode_keeps_eating_held_nav_ups() {
        let mut h = HookState::new(true);
        h.set_focus_mode(true);
        h.on_event(Down(VK_ESCAPE));
        h.set_focus_mode(false);
        assert_eq!(h.on_event(Up(VK_ESCAPE)), (Swallow, None));
    }

    #[test]
    fn a_windows_key_in_focus_mode_exits_and_still_works() {
        let mut h = HookState::new(true);
        h.set_focus_mode(true);
        assert_eq!(h.on_event(Down(VK_LWIN)), (Pass, Some(ExitFocus)));
        assert_eq!(h.on_event(Up(VK_LWIN)), (PassAndMask, Some(OpenStart(0))));
    }

    #[test]
    fn shift_win_s_passes() {
        let mut h = HookState::new(true);
        assert_eq!(h.on_event(Down(VK_LSHIFT)), (Pass, None));
        assert_eq!(h.on_event(Down(VK_LWIN)), (Pass, None));
        assert_eq!(h.on_event(Down(0x53)), (Pass, None));
        assert_eq!(h.on_event(Up(0x53)), (Pass, None));
        assert_eq!(h.on_event(Up(VK_LWIN)), (Pass, None));
        assert_eq!(h.on_event(Up(VK_LSHIFT)), (Pass, None));
    }

    #[test]
    fn modified_win_digits_stay_stock() {
        // Shift+Win+3 (new instance), Ctrl+Win+3, Alt+Win+3 belong to Windows.
        for m in [
            VK_LSHIFT,
            VK_RSHIFT,
            VK_LCONTROL,
            VK_RCONTROL,
            VK_LMENU,
            VK_RMENU,
        ] {
            let mut h = HookState::new(true);
            h.on_event(Down(VK_LWIN));
            h.on_event(Down(m));
            assert_eq!(tap(&mut h, 0x33), [(Pass, None), (Pass, None)], "{m:#x}");
            h.on_event(Up(m));
            assert_eq!(h.on_event(Up(VK_LWIN)), (Pass, None));
        }
    }

    #[test]
    fn win_modifier_tap_is_not_a_lone_tap() {
        // Win+Shift (released without another key) is a combination: no Start.
        let mut h = HookState::new(true);
        h.on_event(Down(VK_LWIN));
        h.on_event(Down(VK_LSHIFT));
        h.on_event(Up(VK_LSHIFT));
        assert_eq!(h.on_event(Up(VK_LWIN)), (Pass, None));
    }

    #[test]
    fn rwin_works_like_lwin() {
        let mut h = HookState::new(true);
        assert_eq!(h.on_event(Down(VK_RWIN)), (Pass, None));
        assert_eq!(h.on_event(Up(VK_RWIN)), (PassAndMask, Some(OpenStart(0))));
        h.on_event(Down(VK_RWIN));
        assert_eq!(h.on_event(Down(0x37)), (Swallow, Some(LaunchNth(7))));
        h.on_event(Up(0x37));
        assert_eq!(h.on_event(Up(VK_RWIN)), (PassAndMask, None));
    }

    #[test]
    fn win_auto_repeat_keeps_the_combination() {
        let mut h = HookState::new(true);
        h.on_event(Down(VK_LWIN));
        h.on_event(Down(0x45));
        h.on_event(Up(0x45));
        // Held long enough to auto-repeat: still the Win+E combination.
        assert_eq!(h.on_event(Down(VK_LWIN)), (Pass, None));
        assert_eq!(h.on_event(Up(VK_LWIN)), (Pass, None));
    }

    #[test]
    fn both_windows_keys_are_a_combination() {
        let mut h = HookState::new(true);
        h.on_event(Down(VK_LWIN));
        h.on_event(Down(VK_RWIN));
        assert_eq!(h.on_event(Up(VK_RWIN)), (Pass, None));
        assert_eq!(h.on_event(Up(VK_LWIN)), (Pass, None));
    }

    #[test]
    fn a_missed_windows_up_does_not_stick() {
        // Win+L: the up happens on the secure desktop and never reaches the hook.
        let mut h = HookState::new(true);
        h.on_event(Down(VK_LWIN));
        h.on_event(Down(0x4C));
        // After unlocking, a plain 3 must be a plain 3.
        h.sync_held(Held {
            win: false,
            modifier: false,
        });
        assert_eq!(tap(&mut h, 0x33), [(Pass, None), (Pass, None)]);
        // A stray Win up later is harmless.
        assert_eq!(h.on_event(Up(VK_LWIN)), (Pass, None));
    }

    #[test]
    fn a_missed_modifier_up_does_not_stick() {
        // Ctrl+Alt+Del: the ups happen on the secure desktop.
        let mut h = HookState::new(true);
        h.on_event(Down(VK_LCONTROL));
        h.on_event(Down(VK_LMENU));
        h.sync_held(Held {
            win: true,
            modifier: false,
        });
        h.on_event(Down(VK_LWIN));
        assert_eq!(h.on_event(Down(0x31)), (Swallow, Some(LaunchNth(1))));
    }

    #[test]
    fn a_win_up_without_a_down_passes() {
        let mut h = HookState::new(true);
        assert_eq!(h.on_event(Up(VK_LWIN)), (Pass, None));
        assert_eq!(h.on_event(Up(0x33)), (Pass, None));
    }

    #[test]
    fn open_start_carries_the_event_time() {
        let mut h = HookState::new(true);
        h.on_event_at(Down(VK_LWIN), 100);
        assert_eq!(
            h.on_event_at(Up(VK_LWIN), 180),
            (PassAndMask, Some(OpenStart(180)))
        );
    }

    #[test]
    fn a_lost_up_does_not_eat_the_next_press() {
        let mut h = HookState::new(true);
        h.on_event_at(Down(VK_LWIN), 0);
        assert_eq!(h.on_event_at(Down(0x33), 10), (Swallow, Some(LaunchNth(3))));
        // The Up of 3 never arrives (lost); Win goes up.
        assert_eq!(h.on_event_at(Up(VK_LWIN), 100), (PassAndMask, None));
        // 3 s later a plain 3 is a plain 3, down and up.
        assert_eq!(h.on_event_at(Down(0x33), 3000), (Pass, None));
        assert_eq!(h.on_event_at(Up(0x33), 3050), (Pass, None));
    }

    #[test]
    fn a_long_hold_keeps_its_up_swallowed() {
        let mut h = HookState::new(true);
        h.on_event_at(Down(VK_LWIN), 0);
        h.on_event_at(Down(0x32), 0);
        // Auto-repeat refreshes the entry.
        for t in (500..=3000).step_by(500) {
            assert_eq!(h.on_event_at(Down(0x32), t), (Swallow, None), "{t}");
        }
        assert_eq!(h.on_event_at(Up(0x32), 3030), (Swallow, None));
    }

    #[test]
    fn a_session_switch_forgets_swallowed_keys() {
        let mut h = HookState::new(true);
        h.on_event(Down(VK_LWIN));
        h.on_event(Down(0x35));
        h.forget_swallowed();
        assert_eq!(h.on_event(Up(0x35)), (Pass, None));
        // The combination still had a swallowed key: Win-up stays masked, no Start.
        assert_eq!(h.on_event(Up(VK_LWIN)), (PassAndMask, None));
    }

    #[test]
    fn win_shortcuts_in_focus_mode_pass_and_end_it() {
        for vk in [VK_RIGHT, VK_LEFT, VK_UP, VK_TAB, 0x44] {
            let mut h = HookState::new(true);
            h.on_event(Down(VK_LWIN));
            h.on_event(Down(0x54));
            h.on_event(Up(0x54));
            assert!(h.focus_mode());
            // Win still held: Win+Right snaps, Win+Tab is Task view, Win+D the desktop.
            assert_eq!(h.on_event(Down(vk)), (Pass, Some(ExitFocus)), "{vk:#x}");
            assert!(!h.focus_mode());
            assert_eq!(h.on_event(Up(vk)), (Pass, None));
            assert_eq!(h.on_event(Up(VK_LWIN)), (PassAndMask, None));
        }
    }

    #[test]
    fn a_mouse_button_ends_focus_mode_at_once() {
        let mut h = HookState::new(true);
        assert_eq!(h.on_mouse_down(), None);
        h.set_focus_mode(true);
        assert_eq!(h.on_mouse_down(), Some(ExitFocus));
        assert!(!h.focus_mode());
        // The very next key reaches the app that was clicked.
        assert_eq!(tap(&mut h, VK_RETURN), [(Pass, None), (Pass, None)]);
        assert_eq!(h.on_mouse_down(), None);
        // A nav key held across the click still gets its up swallowed.
        h.set_focus_mode(true);
        h.on_event(Down(VK_SPACE));
        assert_eq!(h.on_mouse_down(), Some(ExitFocus));
        assert_eq!(h.on_event(Up(VK_SPACE)), (Swallow, None));
    }

    /// Feeds `ev` at time `t` (ms).
    fn at(h: &mut HookState, ev: KeyEv, t: u32) -> (HookDecision, Option<HookOutput>) {
        h.on_event_at(ev, t)
    }

    #[test]
    fn a_hung_ui_makes_the_hook_stock_until_it_drains() {
        let mut h = HookState::new(true);
        // A lone tap: OpenStart is queued at t = 1000 and the UI never takes it.
        at(&mut h, Down(VK_LWIN), 990);
        assert_eq!(
            at(&mut h, Up(VK_LWIN), 1000),
            (PassAndMask, Some(OpenStart(1000)))
        );
        // Within 500 ms: still ours.
        assert!(!h.ui_stalled(1500));
        at(&mut h, Down(VK_LWIN), 1400);
        assert_eq!(at(&mut h, Up(VK_LWIN), 1450).0, PassAndMask);
        // Past 500 ms: a lone tap passes unmasked (Windows' Start opens), Win+3 is Windows'.
        assert!(h.ui_stalled(1501));
        at(&mut h, Down(VK_LWIN), 1600);
        assert_eq!(at(&mut h, Up(VK_LWIN), 1610), (Pass, None));
        at(&mut h, Down(VK_LWIN), 1700);
        assert_eq!(at(&mut h, Down(0x33), 1710), (Pass, None));
        assert_eq!(at(&mut h, Up(0x33), 1720), (Pass, None));
        assert_eq!(at(&mut h, Up(VK_LWIN), 1730), (Pass, None));
        // The UI drains: ours again.
        h.ui_drained();
        at(&mut h, Down(VK_LWIN), 1800);
        assert_eq!(at(&mut h, Down(0x33), 1810), (Swallow, Some(LaunchNth(3))));
        assert_eq!(at(&mut h, Up(0x33), 1820), (Swallow, None));
        assert_eq!(at(&mut h, Up(VK_LWIN), 1830), (PassAndMask, None));
    }

    #[test]
    fn a_stall_mid_pair_still_finishes_the_pair() {
        let mut h = HookState::new(true);
        // Win+2 swallowed at t = 0; the UI hangs before draining LaunchNth.
        at(&mut h, Down(VK_LWIN), 0);
        assert_eq!(at(&mut h, Down(0x32), 10), (Swallow, Some(LaunchNth(2))));
        // 600 ms later (auto-repeat, within the swallow TTL) the hook is stock, yet the
        // repeats and the up of the swallowed 2 are still eaten, and the Windows-key up is
        // still masked (or Windows' Start would open on a "lone" tap).
        assert!(h.ui_stalled(611));
        assert_eq!(at(&mut h, Down(0x32), 611), (Swallow, None));
        assert_eq!(at(&mut h, Up(0x32), 640), (Swallow, None));
        assert_eq!(at(&mut h, Up(VK_LWIN), 650), (PassAndMask, None));
        // The next press of 2 is a plain 2.
        assert_eq!(at(&mut h, Down(0x32), 700), (Pass, None));
        assert_eq!(at(&mut h, Up(0x32), 710), (Pass, None));
        assert!(h.enabled(), "the fallback never changes the setting");
    }

    #[test]
    fn a_stall_in_focus_mode_lets_the_navigation_keys_through() {
        let mut h = HookState::new(true);
        at(&mut h, Down(VK_LWIN), 0);
        assert_eq!(at(&mut h, Down(0x54), 5), (Swallow, Some(FocusApps)));
        at(&mut h, Up(0x54), 10);
        at(&mut h, Up(VK_LWIN), 15);
        assert!(h.focus_mode());
        // The UI never drained FocusApps: 600 ms on, an arrow passes and focus mode ends.
        assert_eq!(at(&mut h, Down(VK_LEFT), 606), (Pass, Some(ExitFocus)));
        assert!(!h.focus_mode());
        assert_eq!(at(&mut h, Up(VK_LEFT), 620), (Pass, None));
    }

    #[test]
    fn outputs_drained_in_time_never_stall() {
        let mut h = HookState::new(true);
        for i in 0..10u32 {
            let t = i * 1000;
            at(&mut h, Down(VK_LWIN), t);
            assert_eq!(at(&mut h, Up(VK_LWIN), t + 10).0, PassAndMask);
            h.ui_drained();
        }
        assert!(!h.ui_stalled(100_000));
    }

    #[test]
    fn other_keys_alone_pass() {
        let mut h = HookState::new(true);
        for vk in [0x41, 0x31, 0x54, VK_LEFT, VK_RETURN, VK_ESCAPE, VK_MASK] {
            assert_eq!(tap(&mut h, vk), [(Pass, None), (Pass, None)], "{vk:#x}");
        }
    }
}
