//! Windows colour mode, accent, transparency and animation settings (read-only).

use vitro_core::style::Theme;
use windows::core::{w, BOOL, PCWSTR};
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
