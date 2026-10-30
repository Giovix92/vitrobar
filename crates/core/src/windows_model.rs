//! Taskbar window model: which windows show up, how they group, what a click does.

use crate::settings::Pin;

pub type Hwnd = u64;
pub type MonitorId = u64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    pub hwnd: Hwnd,
    pub seq: u64,
    pub aumid: Option<String>,
    pub exe_path: Option<String>,
    pub title: String,
    pub visible: bool,
    pub tool_window: bool,
    pub app_window: bool,
    pub has_owner: bool,
    pub cloaked_by_app: bool,
    pub on_current_desktop: bool,
    pub monitor: MonitorId,
    pub minimized: bool,
    pub flashing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupKey {
    Aumid(String),
    /// Always lowercased.
    Path(String),
    Window(Hwnd),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppGroup {
    pub key: GroupKey,
    /// Index into the pins slice.
    pub pin: Option<usize>,
    pub windows: Vec<Hwnd>,
    pub active: bool,
    pub attention: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClickAction {
    Launch(usize),
    Activate(Hwnd),
    Minimize(Hwnd),
    ShowThumbnails,
}

pub fn is_taskbar_window(w: &WindowInfo) -> bool {
    w.visible && !w.cloaked_by_app && (w.app_window || (!w.tool_window && !w.has_owner))
}

pub fn group_key(w: &WindowInfo) -> GroupKey {
    if let Some(a) = &w.aumid {
        GroupKey::Aumid(a.clone())
    } else if let Some(p) = &w.exe_path {
        GroupKey::Path(p.to_lowercase())
    } else {
        GroupKey::Window(w.hwnd)
    }
}

fn pin_key(p: &Pin) -> GroupKey {
    match (&p.aumid, &p.path) {
        (Some(a), _) => GroupKey::Aumid(a.clone()),
        (None, Some(path)) => GroupKey::Path(path.to_lowercase()),
        // A pin with neither identifier can never match; keep it distinct anyway.
        (None, None) => GroupKey::Path(String::new()),
    }
}

/// When both sides have an AUMID it alone decides (a web app pinned from Chrome shares
/// chrome.exe with Chrome itself but not its AUMID); otherwise the executable does.
fn pin_matches(p: &Pin, w: &WindowInfo) -> bool {
    match (&p.aumid, &w.aumid) {
        (Some(a), Some(b)) => a == b,
        _ => matches!(
            (&p.path, &w.exe_path),
            (Some(a), Some(b)) if a.to_lowercase() == b.to_lowercase()
        ),
    }
}

fn new_group(key: GroupKey, pin: Option<usize>) -> AppGroup {
    AppGroup {
        key,
        pin,
        windows: Vec::new(),
        active: false,
        attention: false,
    }
}

pub fn build_groups(
    pins: &[Pin],
    windows: &[WindowInfo],
    monitor: MonitorId,
    foreground: Option<Hwnd>,
) -> Vec<AppGroup> {
    let mut groups: Vec<AppGroup> = pins
        .iter()
        .enumerate()
        .map(|(i, p)| new_group(pin_key(p), Some(i)))
        .collect();

    let mut shown: Vec<&WindowInfo> = windows
        .iter()
        .filter(|w| is_taskbar_window(w) && w.monitor == monitor && w.on_current_desktop)
        .collect();
    shown.sort_by_key(|w| w.seq);

    for w in shown {
        let idx = match pins.iter().position(|p| pin_matches(p, w)) {
            Some(i) => i,
            None => {
                let key = group_key(w);
                match groups.iter().skip(pins.len()).position(|g| g.key == key) {
                    Some(i) => pins.len() + i,
                    None => {
                        groups.push(new_group(key, None));
                        groups.len() - 1
                    }
                }
            }
        };
        let g = &mut groups[idx];
        g.windows.push(w.hwnd);
        g.active |= foreground == Some(w.hwnd);
        g.attention |= w.flashing;
    }
    groups
}

pub fn click_action(g: &AppGroup, foreground: Option<Hwnd>) -> ClickAction {
    match g.windows.as_slice() {
        [] => ClickAction::Launch(g.pin.unwrap_or(0)),
        [h] if foreground == Some(*h) => ClickAction::Minimize(*h),
        [h] => ClickAction::Activate(*h),
        _ => ClickAction::ShowThumbnails,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w() -> WindowInfo {
        WindowInfo {
            hwnd: 1,
            seq: 1,
            aumid: None,
            exe_path: None,
            title: String::new(),
            visible: true,
            tool_window: false,
            app_window: false,
            has_owner: false,
            cloaked_by_app: false,
            on_current_desktop: true,
            monitor: 1,
            minimized: false,
            flashing: false,
        }
    }

    impl WindowInfo {
        fn id(mut self, hwnd: Hwnd, seq: u64) -> Self {
            self.hwnd = hwnd;
            self.seq = seq;
            self
        }
        fn tool(mut self) -> Self {
            self.tool_window = true;
            self
        }
        fn app_window(mut self) -> Self {
            self.app_window = true;
            self
        }
        fn owned(mut self) -> Self {
            self.has_owner = true;
            self
        }
        fn cloaked(mut self) -> Self {
            self.cloaked_by_app = true;
            self
        }
        fn path(mut self, p: &str) -> Self {
            self.exe_path = Some(p.into());
            self
        }
        fn aumid(mut self, a: &str) -> Self {
            self.aumid = Some(a.into());
            self
        }
        fn monitor(mut self, m: MonitorId) -> Self {
            self.monitor = m;
            self
        }
        fn other_desktop(mut self) -> Self {
            self.on_current_desktop = false;
            self
        }
        fn flashing(mut self) -> Self {
            self.flashing = true;
            self
        }
    }

    fn g(hwnds: &[Hwnd]) -> AppGroup {
        AppGroup {
            key: GroupKey::Path("x".into()),
            pin: None,
            windows: hwnds.to_vec(),
            active: false,
            attention: false,
        }
    }

    impl AppGroup {
        fn pinned(mut self, i: usize) -> Self {
            self.pin = Some(i);
            self
        }
    }

    fn pin_path(name: &str, path: &str) -> Pin {
        Pin {
            aumid: None,
            path: Some(path.into()),
            name: name.into(),
            link: None,
        }
    }

    fn keys(gs: &[AppGroup]) -> Vec<GroupKey> {
        gs.iter().map(|g| g.key.clone()).collect()
    }

    #[test]
    fn tool_window_hidden_unless_appwindow() {
        assert!(!is_taskbar_window(&w().tool()));
        assert!(is_taskbar_window(&w().tool().app_window()));
    }

    #[test]
    fn owned_window_hidden() {
        assert!(!is_taskbar_window(&w().owned()));
        assert!(is_taskbar_window(&w().owned().app_window()));
    }

    #[test]
    fn cloaked_hidden() {
        assert!(!is_taskbar_window(&w().cloaked()));
        assert!(!is_taskbar_window(&w().app_window().cloaked()));
    }

    #[test]
    fn invisible_hidden() {
        let mut x = w();
        x.visible = false;
        assert!(!is_taskbar_window(&x));
    }

    #[test]
    fn plain_window_shown() {
        assert!(is_taskbar_window(&w()));
    }

    #[test]
    fn group_key_precedence() {
        assert_eq!(
            group_key(&w().aumid("A").path("C:\\X.exe")),
            GroupKey::Aumid("A".into())
        );
        assert_eq!(
            group_key(&w().path("C:\\X.EXE")),
            GroupKey::Path("c:\\x.exe".into())
        );
        assert_eq!(group_key(&w().id(42, 1)), GroupKey::Window(42));
    }

    #[test]
    fn groups_by_aumid_before_path() {
        let ws = [
            w().id(1, 1).path("C:\\chrome.exe").aumid("Chrome.A"),
            w().id(2, 2).path("C:\\chrome.exe").aumid("Chrome.B"),
        ];
        let gs = build_groups(&[], &ws, 1, None);
        assert_eq!(gs.len(), 2);
    }

    #[test]
    fn path_case_insensitive() {
        let ws = [
            w().id(1, 1).path("C:\\X\\A.EXE"),
            w().id(2, 2).path("c:\\x\\a.exe"),
        ];
        let gs = build_groups(&[], &ws, 1, None);
        assert_eq!(gs.len(), 1);
        assert_eq!(gs[0].windows, vec![1, 2]);
    }

    #[test]
    fn pins_first_then_by_seq() {
        let pins = [pin_path("B", "C:\\b.exe"), pin_path("A", "C:\\a.exe")];
        let ws = [
            w().id(1, 1).path("C:\\a.exe"),
            w().id(2, 2).path("C:\\c.exe"),
            w().id(3, 3).path("C:\\b.exe"),
        ];
        let gs = build_groups(&pins, &ws, 1, None);
        assert_eq!(
            keys(&gs),
            vec![
                GroupKey::Path("c:\\b.exe".into()),
                GroupKey::Path("c:\\a.exe".into()),
                GroupKey::Path("c:\\c.exe".into()),
            ]
        );
        assert_eq!(gs[0].pin, Some(0));
        assert_eq!(gs[1].pin, Some(1));
        assert_eq!(gs[2].pin, None);
    }

    #[test]
    fn pin_without_windows_is_kept() {
        let pins = [pin_path("D", "C:\\d.exe")];
        let gs = build_groups(&pins, &[], 1, None);
        assert_eq!(gs.len(), 1);
        assert!(gs[0].windows.is_empty());
        assert_eq!(gs[0].pin, Some(0));
    }

    #[test]
    fn pinned_group_key_prefers_aumid() {
        let pins = [Pin {
            aumid: Some("App.Id".into()),
            path: Some("C:\\A.exe".into()),
            name: "A".into(),
            link: None,
        }];
        let gs = build_groups(&pins, &[], 1, None);
        assert_eq!(gs[0].key, GroupKey::Aumid("App.Id".into()));
    }

    #[test]
    fn path_only_pin_absorbs_window_with_aumid() {
        let pins = [pin_path("A", "C:\\a.exe")];
        let ws = [w().id(1, 1).path("C:\\A.EXE").aumid("Some.Aumid")];
        let gs = build_groups(&pins, &ws, 1, None);
        assert_eq!(gs.len(), 1);
        assert_eq!(gs[0].key, GroupKey::Path("c:\\a.exe".into()));
        assert_eq!(gs[0].windows, vec![1]);
    }

    #[test]
    fn web_app_pin_does_not_take_its_browser_windows() {
        let pin = |a: &str| Pin {
            aumid: Some(a.into()),
            path: Some(r"C:\chrome.exe".into()),
            name: a.into(),
            link: None,
        };
        let pins = [pin("Chrome._crx_gmail"), pin("Chrome")];
        let ws = [
            w().id(1, 1).path(r"C:\chrome.exe").aumid("Chrome"),
            w().id(2, 2)
                .path(r"C:\chrome.exe")
                .aumid("Chrome._crx_gmail"),
            // No AUMID on the window: the executable decides (first pin).
            w().id(3, 3).path(r"C:\chrome.exe"),
        ];
        let gs = build_groups(&pins, &ws, 1, None);
        assert_eq!(gs.len(), 2);
        assert_eq!(gs[0].windows, vec![2, 3]);
        assert_eq!(gs[1].windows, vec![1]);
    }

    #[test]
    fn window_matches_first_pin_only() {
        let pins = [pin_path("A1", "C:\\a.exe"), pin_path("A2", "C:\\A.exe")];
        let ws = [w().id(1, 1).path("C:\\a.exe")];
        let gs = build_groups(&pins, &ws, 1, None);
        assert_eq!(gs.len(), 2);
        assert_eq!(gs[0].windows, vec![1]);
        assert!(gs[1].windows.is_empty());
    }

    #[test]
    fn windows_ordered_by_seq_and_flags() {
        let ws = [
            w().id(5, 9).path("C:\\a.exe"),
            w().id(6, 3).path("C:\\a.exe").flashing(),
        ];
        let gs = build_groups(&[], &ws, 1, Some(5));
        assert_eq!(gs[0].windows, vec![6, 5]);
        assert!(gs[0].active);
        assert!(gs[0].attention);
        let gs = build_groups(&[], &ws, 1, None);
        assert!(!gs[0].active);
    }

    #[test]
    fn unpinned_ordered_by_lowest_seq() {
        let ws = [
            w().id(1, 5).path("C:\\a.exe"),
            w().id(2, 2).path("C:\\b.exe"),
            w().id(3, 1).path("C:\\a.exe"),
        ];
        let gs = build_groups(&[], &ws, 1, None);
        assert_eq!(
            keys(&gs),
            vec![
                GroupKey::Path("c:\\a.exe".into()),
                GroupKey::Path("c:\\b.exe".into())
            ]
        );
    }

    #[test]
    fn filters_monitor_and_desktop() {
        let ws = [
            w().id(1, 1).path("C:\\a.exe").monitor(2),
            w().id(2, 2).path("C:\\b.exe").other_desktop(),
            w().id(3, 3).path("C:\\c.exe").tool(),
        ];
        assert!(build_groups(&[], &ws, 1, None).is_empty());
        assert_eq!(build_groups(&[], &ws, 2, None).len(), 1);
    }

    #[test]
    fn click_rules() {
        assert_eq!(
            click_action(&g(&[]).pinned(0), None),
            ClickAction::Launch(0)
        );
        assert_eq!(click_action(&g(&[7]), Some(7)), ClickAction::Minimize(7));
        assert_eq!(click_action(&g(&[7]), Some(9)), ClickAction::Activate(7));
        assert_eq!(click_action(&g(&[7, 8]), None), ClickAction::ShowThumbnails);
    }
}
