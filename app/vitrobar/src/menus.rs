//! The bar's right-click menus: native popup menus (they follow the system theme, keyboard
//! and accessibility for free), shown modally with `TrackPopupMenuEx` and returning the
//! chosen command.
//!
//! The owner is the hidden notify window. As documented for `TrackPopupMenu`, it is made the
//! foreground window first (so a click elsewhere dismisses the menu) and is posted a
//! `WM_NULL` afterwards (so a second right-click works at once). While the menu is open its
//! modal loop dispatches everything else; the notify window records it in its inbox.

use vitro_core::i18n::{tr, Key, Lang};
use vitro_core::settings::Settings;
use vitro_win::power::PowerAction;
use windows::core::{Result, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetMenuItemCount, InsertMenuItemW, PostMessageW,
    SetForegroundWindow, TrackPopupMenuEx, HMENU, MENUITEMINFOW, MENU_ITEM_STATE, MFS_CHECKED,
    MFT_STRING, MF_SEPARATOR, MIIM_FTYPE, MIIM_ID, MIIM_STATE, MIIM_STRING, TPM_BOTTOMALIGN,
    TPM_RETURNCMD, TPM_RIGHTBUTTON, TPM_TOPALIGN, WM_NULL,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuChoice {
    Pin,
    Unpin,
    NewWindow,
    CloseAll,
    /// Opens the settings window (every look-and-behaviour setting lives there).
    OpenSettings,
    ToggleStartWithWindows,
    Exit,
    PinToStart,
    UnpinFromStart,
    OpenFileLocation,
}

/// The Start panel's power menu, in order.
const POWER: &[(PowerAction, Key)] = &[
    (PowerAction::Lock, Key::Lock),
    (PowerAction::Sleep, Key::Sleep),
    (PowerAction::Restart, Key::Restart),
    (PowerAction::ShutDown, Key::ShutDown),
    (PowerAction::SignOut, Key::SignOut),
    (PowerAction::SwitchUser, Key::SwitchUser),
];

// Command ids: 0 is "nothing chosen"; submenu entries are base + index.
const ID_PIN: u32 = 1;
const ID_UNPIN: u32 = 2;
const ID_NEW_WINDOW: u32 = 3;
const ID_CLOSE_ALL: u32 = 4;
const ID_START_WITH_WINDOWS: u32 = 5;
const ID_EXIT: u32 = 6;
const ID_PIN_TO_START: u32 = 7;
const ID_UNPIN_FROM_START: u32 = 8;
const ID_OPEN_LOCATION: u32 = 9;
const ID_SETTINGS: u32 = 10;
const ID_POWER: u32 = 0x600;

fn pick<T: Copy>(list: &[(T, Key)], base: u32, id: u32) -> Option<T> {
    let i = id.checked_sub(base)? as usize;
    list.get(i).map(|(v, _)| *v)
}

/// The choice behind a command id returned by the menu.
fn choice_for(id: u32) -> Option<MenuChoice> {
    Some(match id {
        ID_PIN => MenuChoice::Pin,
        ID_UNPIN => MenuChoice::Unpin,
        ID_NEW_WINDOW => MenuChoice::NewWindow,
        ID_CLOSE_ALL => MenuChoice::CloseAll,
        ID_START_WITH_WINDOWS => MenuChoice::ToggleStartWithWindows,
        ID_EXIT => MenuChoice::Exit,
        ID_PIN_TO_START => MenuChoice::PinToStart,
        ID_UNPIN_FROM_START => MenuChoice::UnpinFromStart,
        ID_OPEN_LOCATION => MenuChoice::OpenFileLocation,
        ID_SETTINGS => MenuChoice::OpenSettings,
        _ => return None,
    })
}

/// A popup menu, destroyed (with its attached submenus) on drop.
struct Menu(HMENU);

impl Menu {
    fn new() -> Result<Self> {
        // SAFETY: plain creation; ownership goes to the returned value.
        Ok(Self(unsafe { CreatePopupMenu()? }))
    }

    /// Appends a command, with a check mark when `checked`.
    fn item(&self, id: u32, text: &str, checked: bool) -> Result<()> {
        let mut wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
        let info = MENUITEMINFOW {
            cbSize: size_of::<MENUITEMINFOW>() as u32,
            fMask: MIIM_FTYPE | MIIM_ID | MIIM_STATE | MIIM_STRING,
            fType: MFT_STRING,
            fState: if checked {
                MFS_CHECKED
            } else {
                MENU_ITEM_STATE(0)
            },
            wID: id,
            dwTypeData: PWSTR(wide.as_mut_ptr()),
            cch: (wide.len() - 1) as u32,
            ..Default::default()
        };
        // SAFETY: `info` and the string it points to outlive the call; the menu copies the
        // text. Inserting at the item count appends.
        unsafe {
            let at = GetMenuItemCount(Some(self.0)).max(0) as u32;
            InsertMenuItemW(self.0, at, true, &info)
        }
    }

    fn separator(&self) -> Result<()> {
        // SAFETY: plain call on our own menu.
        unsafe { AppendMenuW(self.0, MF_SEPARATOR, 0, None) }
    }

    /// Shows the menu at `pt_px` (screen px) and waits for a choice.
    fn track(&self, owner: HWND, pt_px: (i32, i32)) -> u32 {
        // Opens away from the screen edge the bar sits on.
        let mut mi = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let pt = POINT {
            x: pt_px.0,
            y: pt_px.1,
        };
        // SAFETY: plain queries; `mi` is a sized local.
        let lower_half = unsafe {
            let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
            GetMonitorInfoW(mon, &mut mi).as_bool()
                && pt.y > (mi.rcMonitor.top + mi.rcMonitor.bottom) / 2
        };
        let align = if lower_half {
            TPM_BOTTOMALIGN
        } else {
            TPM_TOPALIGN
        };
        // SAFETY: the documented TrackPopupMenu pattern on our own owner window: foreground
        // first, WM_NULL after. With TPM_RETURNCMD the result is the command id (0 = none).
        unsafe {
            let _ = SetForegroundWindow(owner);
            let id = TrackPopupMenuEx(
                self.0,
                (TPM_RETURNCMD | TPM_RIGHTBUTTON | align).0,
                pt.x,
                pt.y,
                owner,
                None,
            );
            let _ = PostMessageW(Some(owner), WM_NULL, WPARAM(0), LPARAM(0));
            id.0 as u32
        }
    }
}

impl Drop for Menu {
    fn drop(&mut self) {
        // SAFETY: destroys the menu this value owns, with its attached submenus.
        unsafe {
            let _ = DestroyMenu(self.0);
        }
    }
}

fn report<T>(what: &str, r: Result<T>) -> Option<T> {
    r.inspect_err(|e| eprintln!("vitrobar: the {what} menu failed: {e}"))
        .ok()
}

/// The menu of an app on the bar.
pub fn app_menu(
    owner: HWND,
    pt_px: (i32, i32),
    pinned: bool,
    has_windows: bool,
    lang: Lang,
) -> Option<MenuChoice> {
    let build = || -> Result<Menu> {
        let m = Menu::new()?;
        m.item(ID_NEW_WINDOW, tr(lang, Key::NewWindow), false)?;
        if pinned {
            m.item(ID_UNPIN, tr(lang, Key::Unpin), false)?;
        } else {
            m.item(ID_PIN, tr(lang, Key::Pin), false)?;
        }
        if has_windows {
            m.separator()?;
            m.item(ID_CLOSE_ALL, tr(lang, Key::CloseAllWindows), false)?;
        }
        Ok(m)
    };
    let menu = report("app", build())?;
    choice_for(menu.track(owner, pt_px))
}

/// The menu of an app in the Start panel (a pin or an All-apps entry). `owner` is the panel,
/// so the menu does not take its activation away.
pub fn start_item_menu(
    owner: HWND,
    pt_px: (i32, i32),
    in_start: bool,
    in_bar: bool,
    has_location: bool,
    lang: Lang,
) -> Option<MenuChoice> {
    let build = || -> Result<Menu> {
        let m = Menu::new()?;
        if in_start {
            m.item(ID_UNPIN_FROM_START, tr(lang, Key::UnpinFromStart), false)?;
        } else {
            m.item(ID_PIN_TO_START, tr(lang, Key::PinToStart), false)?;
        }
        if in_bar {
            m.item(ID_UNPIN, tr(lang, Key::Unpin), false)?;
        } else {
            m.item(ID_PIN, tr(lang, Key::Pin), false)?;
        }
        if has_location {
            m.separator()?;
            m.item(ID_OPEN_LOCATION, tr(lang, Key::OpenFileLocation), false)?;
        }
        Ok(m)
    };
    let menu = report("Start item", build())?;
    choice_for(menu.track(owner, pt_px))
}

/// The power action behind a command id of the power menu.
fn power_for(id: u32) -> Option<PowerAction> {
    pick(POWER, ID_POWER, id).filter(|_| id & !0xFF == ID_POWER)
}

/// The Start panel's power menu (Lock, Sleep, Restart, Shut down, Sign out, Switch user).
pub fn power_menu(owner: HWND, pt_px: (i32, i32), lang: Lang) -> Option<PowerAction> {
    let build = || -> Result<Menu> {
        let m = Menu::new()?;
        for (i, (_, key)) in POWER.iter().enumerate() {
            if i == 2 {
                m.separator()?;
            }
            m.item(ID_POWER + i as u32, tr(lang, *key), false)?;
        }
        Ok(m)
    };
    let menu = report("power", build())?;
    power_for(menu.track(owner, pt_px))
}

/// The bar's own menu: the settings window, "Start with Windows" (checked when on) and Exit.
pub fn bar_menu(owner: HWND, pt_px: (i32, i32), s: &Settings, lang: Lang) -> Option<MenuChoice> {
    let menu = report("bar", build_bar_menu(s, lang))?;
    choice_for(menu.track(owner, pt_px))
}

fn build_bar_menu(s: &Settings, lang: Lang) -> Result<Menu> {
    let m = Menu::new()?;
    m.item(ID_SETTINGS, tr(lang, Key::SettingsEllipsis), false)?;
    m.separator()?;
    m.item(
        ID_START_WITH_WINDOWS,
        tr(lang, Key::StartWithWindows),
        s.start_with_windows,
    )?;
    m.item(ID_EXIT, tr(lang, Key::ExitVitrobar), false)?;
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_id_maps_back() {
        assert_eq!(choice_for(0), None, "dismissed");
        assert_eq!(choice_for(ID_PIN), Some(MenuChoice::Pin));
        assert_eq!(choice_for(ID_UNPIN), Some(MenuChoice::Unpin));
        assert_eq!(choice_for(ID_NEW_WINDOW), Some(MenuChoice::NewWindow));
        assert_eq!(choice_for(ID_CLOSE_ALL), Some(MenuChoice::CloseAll));
        assert_eq!(
            choice_for(ID_START_WITH_WINDOWS),
            Some(MenuChoice::ToggleStartWithWindows)
        );
        assert_eq!(choice_for(ID_EXIT), Some(MenuChoice::Exit));
        assert_eq!(choice_for(ID_PIN_TO_START), Some(MenuChoice::PinToStart));
        assert_eq!(
            choice_for(ID_UNPIN_FROM_START),
            Some(MenuChoice::UnpinFromStart)
        );
        assert_eq!(
            choice_for(ID_OPEN_LOCATION),
            Some(MenuChoice::OpenFileLocation)
        );
        for (i, (a, _)) in POWER.iter().enumerate() {
            assert_eq!(power_for(ID_POWER + i as u32), Some(*a));
            assert_eq!(choice_for(ID_POWER + i as u32), None);
        }
        assert_eq!(power_for(ID_POWER + 6), None);
        assert_eq!(power_for(0), None);
        assert_eq!(power_for(ID_EXIT), None);
        assert_eq!(choice_for(ID_SETTINGS), Some(MenuChoice::OpenSettings));
        assert_eq!(choice_for(0x100), None);
        assert_eq!(choice_for(0x900), None);
    }

    #[test]
    fn menus_build_in_both_languages() {
        // Building (not showing) exercises every item.
        for lang in [Lang::En, Lang::It] {
            let s = Settings::default();
            let m = build_bar_menu(&s, lang).unwrap();
            // SAFETY: plain query on our own menu.
            assert_eq!(unsafe { GetMenuItemCount(Some(m.0)) }, 4);
        }
    }
}
