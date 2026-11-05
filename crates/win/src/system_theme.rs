//! Windows colour mode, accent, transparency and animation settings (read-only, except the
//! dark-mode switch of the quick panel).

use vitro_core::style::Theme;
use windows::core::{w, BOOL, PCWSTR};
use windows::Win32::Globalization::{GetUserDefaultUILanguage, LCIDToLocaleName};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows::Win32::UI::WindowsAndMessaging::{
    SystemParametersInfoW, SPI_GETCLIENTAREAANIMATION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
};
use windows::UI::ViewManagement::{UIColorType, UISettings};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemTheme {
    /// System (taskbar/Start) colour mode, not the app mode.
    pub theme: Theme,
    /// Accent colour `0xRRGGBB`.
    pub accent: u32,
    /// Settings › Personalization › Colors › Transparency effects.
    pub transparency: bool,
    /// Settings › Accessibility › Visual effects › Animation effects.
    pub animations: bool,
}

const PERSONALIZE: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
const DEFAULT_ACCENT: u32 = 0x0078D4;

fn personalize_dword(name: PCWSTR) -> Option<u32> {
    let mut value: u32 = 0;
    let mut size = size_of::<u32>() as u32;
    let err = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PERSONALIZE,
            name,
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut _),
            Some(&mut size),
        )
    };
    err.is_ok().then_some(value)
}

fn accent() -> Option<u32> {
    let c = UISettings::new()
        .ok()?
        .GetColorValue(UIColorType::Accent)
        .ok()?;
    Some((u32::from(c.R) << 16) | (u32::from(c.G) << 8) | u32::from(c.B))
}

fn animations() -> bool {
    let mut on = BOOL(1);
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            Some(&mut on as *mut BOOL as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    ok.is_err() || on.as_bool()
}

/// Missing values fall back to the Windows 11 defaults (light, transparency on, animations on).
pub fn read_system_theme() -> SystemTheme {
    let light = personalize_dword(w!("SystemUsesLightTheme")).unwrap_or(1) != 0;
    SystemTheme {
        theme: if light { Theme::Light } else { Theme::Dark },
        accent: accent().unwrap_or(DEFAULT_ACCENT),
        transparency: personalize_dword(w!("EnableTransparency")).unwrap_or(1) != 0,
        animations: animations(),
    }
}

/// Windows' dark mode (apps and system both dark); `None` when the value cannot be read.
pub fn dark_mode() -> Option<bool> {
    let apps = personalize_dword(w!("AppsUseLightTheme"))?;
    let system = personalize_dword(w!("SystemUsesLightTheme")).unwrap_or(apps);
    Some(apps == 0 && system == 0)
}

/// Switches apps and system to dark (or light), as Settings › Personalization › Colors does:
/// both values, then the `ImmersiveColorSet` broadcast every window listens for. The
/// broadcast runs on its own short-lived thread (a hung window would block it for its
/// timeout). `false` when the values could not be written.
pub fn set_dark_mode(on: bool) -> bool {
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::System::Registry::{RegSetKeyValueW, REG_DWORD};
    use windows::Win32::UI::WindowsAndMessaging::{
        SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE,
    };
    let v: u32 = u32::from(!on);
    let mut ok = true;
    for name in [w!("AppsUseLightTheme"), w!("SystemUsesLightTheme")] {
        // SAFETY: a DWORD write under the user's own Personalize key.
        let r = unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                PERSONALIZE,
                name,
                REG_DWORD.0,
                Some((&v as *const u32).cast()),
                size_of::<u32>() as u32,
            )
        };
        ok &= r.is_ok();
    }
    let _ = std::thread::Builder::new()
        .name("vitro-theme-broadcast".into())
        .spawn(|| {
            let topic: Vec<u16> = "ImmersiveColorSet\0".encode_utf16().collect();
            // SAFETY: `topic` is NUL-terminated and outlives the call; the broadcast waits at
            // most 1 s per window and skips hung ones.
            unsafe {
                SendMessageTimeoutW(
                    HWND_BROADCAST,
                    WM_SETTINGCHANGE,
                    WPARAM(0),
                    LPARAM(topic.as_ptr() as isize),
                    SMTO_ABORTIFHUNG,
                    1000,
                    None,
                );
            }
        });
    ok
}

/// The Windows display language as a locale tag ("it-IT"), `None` if it cannot be read.
pub fn user_ui_language_tag() -> Option<String> {
    let mut buf = [0u16; 85]; // LOCALE_NAME_MAX_LENGTH
                              // SAFETY: plain query; the buffer and its length are passed together.
    let n = unsafe { LCIDToLocaleName(u32::from(GetUserDefaultUILanguage()), Some(&mut buf), 0) };
    let n = usize::try_from(n).ok()?.checked_sub(1)?; // without the terminating NUL
    (n > 0).then(|| String::from_utf16_lossy(&buf[..n.min(buf.len())]))
}
