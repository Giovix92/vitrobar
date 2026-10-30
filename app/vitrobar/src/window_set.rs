//! The app's view of the top-level windows: the last read [`WindowInfo`] of every window
//! that is (or was) a taskbar window, kept current from the tracker's events.

use std::collections::{HashMap, HashSet};

use vitro_core::windows_model::{is_taskbar_window, Hwnd, WindowInfo};
use vitro_win::desktops::Desktops;
use vitro_win::window_props::read_window_info;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{GetAncestor, IsWindowVisible, GA_ROOT};

fn to_hwnd(h: Hwnd) -> HWND {
    HWND(h as usize as *mut _)
}

/// Visible top-level window. WinEvent hooks also report child windows (WinUI input sinks,
/// the stock taskbar's own button list...), which must never become bar items.
fn visible_top_level(h: HWND) -> bool {
    // SAFETY: plain queries on a handle that may be stale (then they return false / null).
    unsafe { IsWindowVisible(h).as_bool() && GetAncestor(h, GA_ROOT) == h }
}

#[derive(Default)]
pub struct WindowSet {
    infos: HashMap<Hwnd, WindowInfo>,
    /// Windows flashing for attention (from `WindowEvent::Flash`, cleared on activation).
    flashing: HashSet<Hwnd>,
    next_seq: u64,
}

impl WindowSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Initial fill. `EnumWindows` lists top of the Z order first; the bottom-most window is
    /// given the lowest sequence number as the best guess for "opened first".
    pub fn load(&mut self, hwnds: &[HWND], desktops: &Desktops) {
        for &h in hwnds.iter().rev() {
            // Invisible windows are skipped here and picked up by the shell's WINDOWCREATED
            // event when they appear.
            if visible_top_level(h) {
                self.refresh(h.0 as usize as Hwnd, desktops);
            }
        }
    }

    /// Re-reads one window. Windows not known yet are only kept if they are taskbar windows.
    /// Returns true if anything stored changed.
    pub fn refresh(&mut self, h: Hwnd, desktops: &Desktops) -> bool {
        let seq = match self.infos.get(&h) {
            Some(i) => i.seq,
            None => {
                // Cheap pre-filter: most LOCATIONCHANGE noise is from windows we never show.
                if !visible_top_level(to_hwnd(h)) {
                    return false;
                }
                self.next_seq + 1
            }
        };
        match read_window_info(to_hwnd(h), seq, desktops) {
            None => self.remove(h),
            Some(mut info) => {
                info.flashing = self.flashing.contains(&h);
                let known = self.infos.contains_key(&h);
                if !known && !is_taskbar_window(&info) {
                    return false;
                }
                if !known {
                    self.next_seq = seq;
                }
                let changed = self.infos.get(&h) != Some(&info);
                self.infos.insert(h, info);
                changed
            }
        }
    }

    pub fn remove(&mut self, h: Hwnd) -> bool {
        self.flashing.remove(&h);
        self.infos.remove(&h).is_some()
    }

    pub fn set_flashing(&mut self, h: Hwnd, on: bool) -> bool {
        let changed = if on {
            self.flashing.insert(h)
        } else {
            self.flashing.remove(&h)
        };
        if let Some(i) = self.infos.get_mut(&h) {
            i.flashing = on;
        }
        changed
    }

    pub fn contains(&self, h: Hwnd) -> bool {
        self.infos.contains_key(&h)
    }

    /// After a virtual desktop switch: re-checks desktop membership of every window.
    pub fn refresh_desktops(&mut self, desktops: &Desktops) -> bool {
        let mut changed = false;
        for (h, info) in self.infos.iter_mut() {
            let on = desktops.is_on_current(to_hwnd(*h));
            changed |= info.on_current_desktop != on;
            info.on_current_desktop = on;
        }
        changed
    }

    pub fn infos(&self) -> Vec<WindowInfo> {
        self.infos.values().cloned().collect()
    }

    pub fn title(&self, h: Hwnd) -> Option<&str> {
        self.infos.get(&h).map(|i| i.title.as_str())
    }
}
