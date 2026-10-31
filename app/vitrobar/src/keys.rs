//! The Windows-key glue: owns the keyboard hook ([`KeyHook`], its own thread) and the bar's
//! keyboard focus mode (Win+T on the apps, Win+B on the tray).
//!
//! The decisions about which keys are ours live in `vitro_core::key_hook`; this module only
//! holds the focus-mode navigation (pure, tested here) and the hook's lifetime. The app
//! (`app.rs`) drains the hook's outputs and acts on them:
//! - `OpenStart` toggles the Start panel on the monitor of the foreground window;
//! - `LaunchNth(n)` acts on that bar's n-th app ([`launch_action`]);
//! - `FocusApps` / `FocusTray` put a focus ring on the first app / tray icon; the arrows,
//!   Home / End and Tab move it, Enter / Space click, the Apps key opens the menu, and Esc,
//!   any other key, any click or a foreground change leave focus mode.
//!
//! While in focus mode the bar still never takes the foreground: the hook swallows the
//! navigation keys before they reach the app that has it.

use vitro_core::key_hook::{
    HookOutput, VK_APPS, VK_DOWN, VK_END, VK_ESCAPE, VK_HOME, VK_LEFT, VK_RETURN, VK_RIGHT,
    VK_SPACE, VK_TAB, VK_UP,
};
use vitro_core::windows_model::{click_action, AppGroup, ClickAction, Hwnd};
use vitro_render::bar_visual::BarFocus;
use vitro_win::key_hook::KeyHook;
use windows::Win32::Foundation::HWND;

/// Which part of a bar focus mode moves in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zone {
    Apps,
    Tray,
}

impl Zone {
    fn other(self) -> Self {
        match self {
            Zone::Apps => Zone::Tray,
            Zone::Tray => Zone::Apps,
        }
    }
}

/// The focused item: on the bar of `monitor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Focus {
    pub monitor: u64,
    pub zone: Zone,
    pub item: BarFocus,
}

/// What a bar shows that focus mode can land on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    /// App items shown (the "more" button excluded).
    pub apps: usize,
    /// Promoted tray icons.
    pub tray: usize,
    /// The overflow chevron shows.
    pub chevron: bool,
}

/// The focusable items of `zone`, left to right as the bar draws them.
pub fn zone_items(zone: Zone, c: Counts) -> Vec<BarFocus> {
    match zone {
        Zone::Apps => (0..c.apps).map(BarFocus::App).collect(),
        Zone::Tray => c
            .chevron
            .then_some(BarFocus::Chevron)
            .into_iter()
            .chain((0..c.tray).map(BarFocus::Tray))
            .collect(),
    }
}

/// Where Win+T / Win+B (and Tab) land: the first app; the first tray icon, or the chevron
/// when no icon is promoted.
pub fn first_in(zone: Zone, c: Counts) -> Option<BarFocus> {
    match zone {
        Zone::Apps => (c.apps > 0).then_some(BarFocus::App(0)),
        Zone::Tray if c.tray > 0 => Some(BarFocus::Tray(0)),
        Zone::Tray => c.chevron.then_some(BarFocus::Chevron),
    }
}

/// What a navigation key does in focus mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Move(Zone, BarFocus),
    /// Enter / Space: click the focused item (focus mode ends).
    Click(BarFocus),
    /// The Apps (context-menu) key: the item's menu (focus mode ends).
    Menu(BarFocus),
    /// Tab with nothing in the other zone.
    Stay,
    Exit,
}

pub fn step(zone: Zone, item: BarFocus, vk: u32, c: Counts) -> Step {
    let items = zone_items(zone, c);
    let Some(pos) = items.iter().position(|&i| i == item) else {
        // The item went away (the bar changed meanwhile): start over in this zone.
        return match (vk, first_in(zone, c)) {
            (VK_ESCAPE, _) | (_, None) => Step::Exit,
            (_, Some(first)) => Step::Move(zone, first),
        };
    };
    let n = items.len();
    match vk {
        VK_LEFT | VK_UP => Step::Move(zone, items[(pos + n - 1) % n]),
        VK_RIGHT | VK_DOWN => Step::Move(zone, items[(pos + 1) % n]),
        VK_HOME => Step::Move(zone, items[0]),
        VK_END => Step::Move(zone, items[n - 1]),
        VK_RETURN | VK_SPACE => Step::Click(item),
        VK_APPS => Step::Menu(item),
        VK_TAB => match first_in(zone.other(), c) {
            Some(first) => Step::Move(zone.other(), first),
            None => Step::Stay,
        },
        _ => Step::Exit,
    }
}

/// A Win-key `OpenStart` this much older than now (ms) is dropped: the UI thread was too
/// busy, and a Start that opens long after the tap would surprise.
pub const OPEN_START_MAX_AGE_MS: u32 = 500;

/// Whether an `OpenStart` stamped `event_ms` is still worth acting on at `now_ms` (both on
/// the `GetTickCount` clock, which wraps).
pub fn open_start_fresh(event_ms: u32, now_ms: u32) -> bool {
    (now_ms.wrapping_sub(event_ms) as i32) <= OPEN_START_MAX_AGE_MS as i32
}

/// Whether a foreground change ends focus mode. A change away from one of our own windows
/// does not: entering focus mode closes an open Start panel or overflow, and the foreground
/// then goes back to the app that had it before, which must not cancel the mode just
/// entered.
pub fn activation_ends_focus(changed: bool, previous_was_ours: bool) -> bool {
    changed && !previous_was_ours
}

/// Win+n on an app: as a click, except that a group with several windows activates the
/// next one (after the foreground one, or the first) instead of showing the thumbnails.
pub fn launch_action(g: &AppGroup, foreground: Option<Hwnd>) -> ClickAction {
    match click_action(g, foreground) {
        ClickAction::ShowThumbnails => {
            let next = match foreground.and_then(|f| g.windows.iter().position(|&h| h == f)) {
                Some(i) => g.windows[(i + 1) % g.windows.len()],
                None => g.windows[0],
            };
            ClickAction::Activate(next)
        }
        other => other,
    }
}

/// The hook (when it could be installed) and the focus mode.
#[derive(Default)]
pub struct Keys {
    hook: Option<KeyHook>,
    focus: Option<Focus>,
}

impl Keys {
    /// Installs the hook; without it the Windows key simply stays stock.
    pub fn start(&mut self, notify: HWND, enabled: bool) {
        if self.hook.is_some() {
            return;
        }
        self.hook = KeyHook::start(notify, enabled)
            .inspect_err(|e| {
                eprintln!("vitrobar: no keyboard hook, the Windows key stays stock: {e}")
            })
            .ok();
    }

    /// `Settings.win_key_opens_start` changed (the settings window): off, the hook stays
    /// installed but lets every key through. Returns the monitor whose bar showed the focus
    /// ring (it must be redrawn without it).
    #[allow(dead_code)] // Called by the settings window (Task 9) through `App::set_win_key`.
    pub fn set_enabled(&mut self, on: bool) -> Option<u64> {
        let ringed = if on {
            None
        } else {
            self.focus.take().map(|f| f.monitor)
        };
        if let Some(h) = &self.hook {
            h.set_enabled(on);
        }
        ringed
    }

    /// The session or the input desktop switched: key-ups may have been lost.
    pub fn forget_swallowed(&self) {
        if let Some(h) = &self.hook {
            h.forget_swallowed();
        }
    }

    pub fn drain(&self) -> Vec<HookOutput> {
        self.hook.as_ref().map(KeyHook::drain).unwrap_or_default()
    }

    pub fn focus(&self) -> Option<Focus> {
        self.focus
    }

    /// Enters, moves or (`None`) leaves focus mode; the hook swallows the navigation keys
    /// exactly while it is on.
    pub fn set_focus(&mut self, focus: Option<Focus>) {
        self.focus = focus;
        if let Some(h) = &self.hook {
            h.set_focus_mode(focus.is_some());
        }
    }

    /// The `"keys"` object of the state dump: counters only, never key codes.
    pub fn dump(&self) -> String {
        let s = self.hook.as_ref().map(KeyHook::stats).unwrap_or_default();
        let focus = match self.focus.map(|f| f.zone) {
            None => "none",
            Some(Zone::Apps) => "apps",
            Some(Zone::Tray) => "tray",
        };
        format!(
            "{{\"hook\":{},\"enabled\":{},\"focus\":\"{}\",\"mouse_hook\":{},\"calls\":{},\"max_us\":{}}}",
            s.installed, s.enabled, focus, s.mouse_hook, s.calls, s.max_us
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vitro_core::windows_model::GroupKey;

    const C: Counts = Counts {
        apps: 3,
        tray: 2,
        chevron: true,
    };

    #[test]
    fn tray_zone_is_chevron_then_icons() {
        use BarFocus::*;
        assert_eq!(zone_items(Zone::Tray, C), vec![Chevron, Tray(0), Tray(1)]);
        assert_eq!(zone_items(Zone::Apps, C), vec![App(0), App(1), App(2)]);
        assert_eq!(first_in(Zone::Tray, C), Some(Tray(0)));
        let only_chevron = Counts { tray: 0, ..C };
        assert_eq!(first_in(Zone::Tray, only_chevron), Some(Chevron));
        assert_eq!(first_in(Zone::Tray, Counts::default()), None);
        assert_eq!(first_in(Zone::Apps, Counts::default()), None);
    }

    #[test]
    fn arrows_wrap_home_end_jump() {
        use BarFocus::*;
        let a = Zone::Apps;
        assert_eq!(step(a, App(0), VK_RIGHT, C), Step::Move(a, App(1)));
        assert_eq!(step(a, App(2), VK_RIGHT, C), Step::Move(a, App(0)));
        assert_eq!(step(a, App(0), VK_LEFT, C), Step::Move(a, App(2)));
        assert_eq!(step(a, App(1), VK_UP, C), Step::Move(a, App(0)));
        assert_eq!(step(a, App(1), VK_DOWN, C), Step::Move(a, App(2)));
        assert_eq!(step(a, App(1), VK_HOME, C), Step::Move(a, App(0)));
        assert_eq!(step(a, App(0), VK_END, C), Step::Move(a, App(2)));
        let t = Zone::Tray;
        assert_eq!(step(t, Tray(0), VK_LEFT, C), Step::Move(t, Chevron));
        assert_eq!(step(t, Tray(1), VK_RIGHT, C), Step::Move(t, Chevron));
    }

    #[test]
    fn enter_space_click_apps_key_menu_esc_exits() {
        use BarFocus::*;
        assert_eq!(step(Zone::Apps, App(1), VK_RETURN, C), Step::Click(App(1)));
        assert_eq!(step(Zone::Tray, Chevron, VK_SPACE, C), Step::Click(Chevron));
        assert_eq!(step(Zone::Tray, Tray(1), VK_APPS, C), Step::Menu(Tray(1)));
        assert_eq!(step(Zone::Apps, App(0), VK_ESCAPE, C), Step::Exit);
        assert_eq!(step(Zone::Apps, App(0), 0x41, C), Step::Exit);
    }

    #[test]
    fn tab_toggles_zones() {
        use BarFocus::*;
        assert_eq!(
            step(Zone::Apps, App(2), VK_TAB, C),
            Step::Move(Zone::Tray, Tray(0))
        );
        assert_eq!(
            step(Zone::Tray, Chevron, VK_TAB, C),
            Step::Move(Zone::Apps, App(0))
        );
        // A secondary monitor's bar has no tray area.
        let no_tray = Counts {
            apps: 2,
            ..Counts::default()
        };
        assert_eq!(step(Zone::Apps, App(1), VK_TAB, no_tray), Step::Stay);
    }

    #[test]
    fn a_vanished_item_restarts_the_zone() {
        use BarFocus::*;
        assert_eq!(
            step(Zone::Apps, App(7), VK_RIGHT, C),
            Step::Move(Zone::Apps, App(0))
        );
        assert_eq!(step(Zone::Apps, App(7), VK_ESCAPE, C), Step::Exit);
        assert_eq!(
            step(Zone::Apps, App(0), VK_RIGHT, Counts::default()),
            Step::Exit
        );
    }

    fn group(windows: Vec<Hwnd>, pin: Option<usize>) -> AppGroup {
        AppGroup {
            key: GroupKey::Aumid("a".into()),
            pin,
            windows,
            active: false,
            attention: false,
        }
    }

    #[test]
    fn win_n_cycles_a_group_instead_of_thumbnails() {
        let g = group(vec![10, 20, 30], Some(0));
        assert_eq!(launch_action(&g, None), ClickAction::Activate(10));
        assert_eq!(launch_action(&g, Some(99)), ClickAction::Activate(10));
        assert_eq!(launch_action(&g, Some(10)), ClickAction::Activate(20));
        assert_eq!(launch_action(&g, Some(30)), ClickAction::Activate(10));
        // Otherwise exactly a click: launch, activate, minimize.
        assert_eq!(
            launch_action(&group(vec![], Some(2)), None),
            ClickAction::Launch(2)
        );
        assert_eq!(
            launch_action(&group(vec![5], None), None),
            ClickAction::Activate(5)
        );
        assert_eq!(
            launch_action(&group(vec![5], None), Some(5)),
            ClickAction::Minimize(5)
        );
    }

    #[test]
    fn closing_our_own_panel_does_not_cancel_focus_mode() {
        // Win+T with the Start panel open: the panel closes, the previous app comes back.
        assert!(!activation_ends_focus(true, true));
        // A real switch to another app (or a click into one) ends it.
        assert!(activation_ends_focus(true, false));
        assert!(!activation_ends_focus(false, false));
    }

    #[test]
    fn stale_open_start_is_dropped() {
        assert!(open_start_fresh(1000, 1000));
        assert!(open_start_fresh(1000, 1500));
        assert!(!open_start_fresh(1000, 1501));
        // Across the 49.7-day wrap of GetTickCount.
        assert!(open_start_fresh(u32::MAX - 100, 200));
        assert!(!open_start_fresh(u32::MAX - 100, 900));
        // An event stamped a little after "now" (clock skew) counts as fresh.
        assert!(open_start_fresh(1010, 1000));
    }

    #[test]
    fn disabling_returns_the_ringed_bar() {
        let mut k = Keys::default();
        k.set_focus(Some(Focus {
            monitor: 7,
            zone: Zone::Apps,
            item: BarFocus::App(0),
        }));
        assert_eq!(k.set_enabled(true), None);
        assert!(k.focus().is_some());
        assert_eq!(k.set_enabled(false), Some(7));
        assert_eq!(k.focus(), None);
        assert_eq!(k.set_enabled(false), None);
    }

    #[test]
    fn dump_without_hook_has_counts_only() {
        let k = Keys::default();
        assert_eq!(
            k.dump(),
            "{\"hook\":false,\"enabled\":false,\"focus\":\"none\",\"mouse_hook\":false,\"calls\":0,\"max_us\":0}"
        );
    }
}
