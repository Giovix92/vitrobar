//! The bar's right-click menus: native popup menus (they follow the system theme, keyboard
//! and accessibility for free), shown modally with `TrackPopupMenuEx` and returning the
//! chosen command.
//!
//! The owner is the hidden notify window. As documented for `TrackPopupMenu`, it is made the
//! foreground window first (so a click elsewhere dismisses the menu) and is posted a
//! `WM_NULL` afterwards (so a second right-click works at once). While the menu is open its
//! modal loop dispatches everything else; the notify window records it in its inbox.

use vitro_core::i18n::{tr, Key, Lang};
use vitro_core::settings::{HoverEffect, IconSize, Material, Position, Settings, Shape};
use windows::core::{Result, HSTRING, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetMenuItemCount, InsertMenuItemW, PostMessageW,
    SetForegroundWindow, TrackPopupMenuEx, HMENU, MENUITEMINFOW, MENU_ITEM_STATE, MFS_CHECKED,
    MFT_RADIOCHECK, MFT_STRING, MF_POPUP, MF_SEPARATOR, MIIM_FTYPE, MIIM_ID, MIIM_STATE,
    MIIM_STRING, TPM_BOTTOMALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, TPM_TOPALIGN, WM_NULL,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuChoice {
    Pin,
    Unpin,
    NewWindow,
    CloseAll,
    SetShape(Shape),
    SetMaterial(Material),
    SetHover(HoverEffect),
    SetIconSize(IconSize),
    SetPosition(Position),
    ToggleStartWithWindows,
    Exit,
}

const SHAPES: &[(Shape, Key)] = &[
    (Shape::Pill, Key::ShapePill),
    (Shape::Islands, Key::ShapeIslands),
    (Shape::Full, Key::ShapeFull),
];
/// Stage-1 materials (Liquid comes later).
const MATERIALS: &[(Material, Key)] = &[
    (Material::Frosted, Key::MaterialFrosted),
    (Material::Smoked, Key::MaterialSmoked),
    (Material::SolidBlack, Key::MaterialSolidBlack),
    (Material::SolidWhite, Key::MaterialSolidWhite),
];
const HOVERS: &[(HoverEffect, Key)] = &[
    (HoverEffect::Plate, Key::HoverPlate),
    (HoverEffect::Lift, Key::HoverLift),
    (HoverEffect::Magnify, Key::HoverMagnify),
    (HoverEffect::Sweep, Key::HoverSweep),
];
const SIZES: &[(IconSize, Key)] = &[
    (IconSize::Small, Key::Small),
    (IconSize::Medium, Key::Medium),
    (IconSize::Large, Key::Large),
];
const POSITIONS: &[(Position, Key)] = &[(Position::Bottom, Key::Bottom), (Position::Top, Key::Top)];

// Command ids: 0 is "nothing chosen"; submenu entries are base + index.
const ID_PIN: u32 = 1;
const ID_UNPIN: u32 = 2;
const ID_NEW_WINDOW: u32 = 3;
const ID_CLOSE_ALL: u32 = 4;
const ID_START_WITH_WINDOWS: u32 = 5;
const ID_EXIT: u32 = 6;
const ID_SHAPE: u32 = 0x100;
const ID_MATERIAL: u32 = 0x200;
const ID_HOVER: u32 = 0x300;
const ID_SIZE: u32 = 0x400;
const ID_POSITION: u32 = 0x500;

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
        _ => match id & !0xFF {
            ID_SHAPE => MenuChoice::SetShape(pick(SHAPES, ID_SHAPE, id)?),
            ID_MATERIAL => MenuChoice::SetMaterial(pick(MATERIALS, ID_MATERIAL, id)?),
            ID_HOVER => MenuChoice::SetHover(pick(HOVERS, ID_HOVER, id)?),
            ID_SIZE => MenuChoice::SetIconSize(pick(SIZES, ID_SIZE, id)?),
            ID_POSITION => MenuChoice::SetPosition(pick(POSITIONS, ID_POSITION, id)?),
            _ => return None,
        },
    })
}

/// A popup menu, destroyed (with its attached submenus) on drop.
struct Menu(HMENU);

impl Menu {
    fn new() -> Result<Self> {
        // SAFETY: plain creation; ownership goes to the returned value.
        Ok(Self(unsafe { CreatePopupMenu()? }))
    }

    /// Appends a command; `radio`: shown with a radio bullet when `checked`.
    fn item(&self, id: u32, text: &str, checked: bool, radio: bool) -> Result<()> {
        let mut wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
        let info = MENUITEMINFOW {
            cbSize: size_of::<MENUITEMINFOW>() as u32,
            fMask: MIIM_FTYPE | MIIM_ID | MIIM_STATE | MIIM_STRING,
            fType: if radio {
                MFT_STRING | MFT_RADIOCHECK
            } else {
                MFT_STRING
            },
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

    /// Attaches `sub` under `text`; from then on this menu owns it.
    fn submenu(&self, text: &str, sub: Menu) -> Result<()> {
        let text = HSTRING::from(text);
        // SAFETY: plain call on our own menus; the text is copied.
        unsafe { AppendMenuW(self.0, MF_POPUP, sub.0 .0 as usize, &text)? };
        std::mem::forget(sub);
        Ok(())
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

fn radio_submenu<T: Copy + PartialEq>(
    menu: &Menu,
    title: Key,
    list: &[(T, Key)],
    base: u32,
    current: T,
    lang: Lang,
) -> Result<()> {
    let sub = Menu::new()?;
    for (i, (v, key)) in list.iter().enumerate() {
        sub.item(base + i as u32, tr(lang, *key), *v == current, true)?;
    }
    menu.submenu(tr(lang, title), sub)
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
        m.item(ID_NEW_WINDOW, tr(lang, Key::NewWindow), false, false)?;
        if pinned {
            m.item(ID_UNPIN, tr(lang, Key::Unpin), false, false)?;
        } else {
            m.item(ID_PIN, tr(lang, Key::Pin), false, false)?;
        }
        if has_windows {
            m.separator()?;
            m.item(ID_CLOSE_ALL, tr(lang, Key::CloseAllWindows), false, false)?;
        }
        Ok(m)
    };
    let menu = report("app", build())?;
    choice_for(menu.track(owner, pt_px))
}

/// The bar's own menu; the current settings carry radio checks.
pub fn bar_menu(owner: HWND, pt_px: (i32, i32), s: &Settings, lang: Lang) -> Option<MenuChoice> {
    let build = || -> Result<Menu> {
        let m = Menu::new()?;
        radio_submenu(&m, Key::Shape, SHAPES, ID_SHAPE, s.shape, lang)?;
        radio_submenu(&m, Key::Material, MATERIALS, ID_MATERIAL, s.material, lang)?;
        radio_submenu(&m, Key::Hover, HOVERS, ID_HOVER, s.hover, lang)?;
        radio_submenu(&m, Key::IconSize, SIZES, ID_SIZE, s.icon_size, lang)?;
        radio_submenu(&m, Key::Position, POSITIONS, ID_POSITION, s.position, lang)?;
        m.separator()?;
        m.item(
            ID_START_WITH_WINDOWS,
            tr(lang, Key::StartWithWindows),
            s.start_with_windows,
            false,
        )?;
        m.separator()?;
        m.item(ID_EXIT, tr(lang, Key::ExitVitrobar), false, false)?;
        Ok(m)
    };
    let menu = report("bar", build())?;
    choice_for(menu.track(owner, pt_px))
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
        for (i, (v, _)) in SHAPES.iter().enumerate() {
            assert_eq!(
                choice_for(ID_SHAPE + i as u32),
                Some(MenuChoice::SetShape(*v))
            );
        }
        for (i, (v, _)) in MATERIALS.iter().enumerate() {
            assert_eq!(
                choice_for(ID_MATERIAL + i as u32),
                Some(MenuChoice::SetMaterial(*v))
            );
        }
        for (i, (v, _)) in HOVERS.iter().enumerate() {
            assert_eq!(
                choice_for(ID_HOVER + i as u32),
                Some(MenuChoice::SetHover(*v))
            );
        }
        for (i, (v, _)) in SIZES.iter().enumerate() {
            assert_eq!(
                choice_for(ID_SIZE + i as u32),
                Some(MenuChoice::SetIconSize(*v))
            );
        }
        for (i, (v, _)) in POSITIONS.iter().enumerate() {
            assert_eq!(
                choice_for(ID_POSITION + i as u32),
                Some(MenuChoice::SetPosition(*v))
            );
        }
        assert_eq!(choice_for(ID_SHAPE + 3), None);
        assert_eq!(choice_for(ID_MATERIAL + 4), None);
        assert_eq!(choice_for(0x900), None);
    }

    #[test]
    fn material_menu_has_no_liquid() {
        assert!(MATERIALS.iter().all(|(m, _)| *m != Material::Liquid));
        assert_eq!(MATERIALS.len(), 4);
    }

    #[test]
    fn menus_build_in_both_languages() {
        // Building (not showing) exercises every item and submenu.
        for lang in [Lang::En, Lang::It] {
            let m = Menu::new().unwrap();
            let s = Settings::default();
            radio_submenu(&m, Key::Shape, SHAPES, ID_SHAPE, s.shape, lang).unwrap();
            radio_submenu(&m, Key::Material, MATERIALS, ID_MATERIAL, s.material, lang).unwrap();
            m.separator().unwrap();
            m.item(ID_EXIT, tr(lang, Key::ExitVitrobar), false, false)
                .unwrap();
            // SAFETY: plain query on our own menu.
            assert_eq!(unsafe { GetMenuItemCount(Some(m.0)) }, 4);
        }
    }
}
