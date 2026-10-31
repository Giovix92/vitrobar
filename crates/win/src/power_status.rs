//! Battery and power-mode state for the quick panel (spec §5.5).
//!
//! [`snapshot`] reads `GetSystemPowerStatus` and the effective power overlay; changes arrive
//! as `WM_POWERBROADCAST` / `PBT_POWERSETTINGCHANGE` on the window passed to [`register`]
//! (the app forwards the message, the panel then calls [`snapshot`] again). Nothing polls.

use windows::core::{s, w, Result, GUID};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::Foundation::{HANDLE, HWND};
use windows::Win32::System::Power::{
    GetSystemPowerStatus, RegisterPowerSettingNotification, UnregisterPowerSettingNotification, HPOWERNOTIFY,
    SYSTEM_POWER_STATUS,
};
use windows::Win32::System::SystemServices::{
    GUID_ACDC_POWER_SOURCE, GUID_BATTERY_PERCENTAGE_REMAINING, GUID_POWER_SAVING_STATUS,
};
use windows::Win32::UI::WindowsAndMessaging::DEVICE_NOTIFY_WINDOW_HANDLE;

use vitro_core::i18n::{tr, Key, Lang};

/// The Windows power mode (power slider) of the active overlay scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerMode {
    BestEfficiency,
    Balanced,
    BestPerformance,
}

/// Everything the quick panel shows about power.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PowerSnapshot {
    pub has_battery: bool,
    /// Charge 0..=100; `None` without a battery or when Windows does not know.
    pub percent: Option<u8>,
    /// The battery is charging.
    pub charging: bool,
    /// Battery saver is on.
    pub saver: bool,
    /// `None` when the overlay API is missing or the scheme is not one of the three.
    pub mode: Option<PowerMode>,
}

const OVERLAY_BEST_EFFICIENCY: GUID = GUID::from_u128(0x961cc777_2547_4f9d_8174_7d86181b8a7a);
const OVERLAY_BALANCED: GUID = GUID::from_u128(0);
const OVERLAY_BEST_PERFORMANCE: GUID = GUID::from_u128(0xded574b5_45a0_4f42_8737_46345c09c238);

/// The documented overlay GUID of `mode`.
pub fn overlay_guid(mode: PowerMode) -> GUID {
    match mode {
        PowerMode::BestEfficiency => OVERLAY_BEST_EFFICIENCY,
        PowerMode::Balanced => OVERLAY_BALANCED,
        PowerMode::BestPerformance => OVERLAY_BEST_PERFORMANCE,
    }
}

/// The mode of an overlay GUID; `None` for any other GUID.
pub fn mode_from_guid(guid: &GUID) -> Option<PowerMode> {
    [
        PowerMode::BestEfficiency,
        PowerMode::Balanced,
        PowerMode::BestPerformance,
    ]
    .into_iter()
    .find(|m| overlay_guid(*m) == *guid)
}

/// Pure interpretation of the `GetSystemPowerStatus` fields.
/// `flag` bit 128 = no system battery, bit 8 = charging, 255 = unknown; `life` 255 = unknown.
pub fn interpret_status(flag: u8, life: u8, saver: u8, mode: Option<PowerMode>) -> PowerSnapshot {
    let has_battery = flag != 255 && flag & 128 == 0;
    PowerSnapshot {
        has_battery,
        percent: (has_battery && life <= 100).then_some(life),
        charging: has_battery && flag & 8 != 0,
        saver: saver & 1 != 0,
        mode,
    }
}

/// "99%" or "99% · charging" ("in carica"); `None` without a battery or a known percentage.
/// Lives here (not in core) because it takes the snapshot type; the text comes from core's i18n.
pub fn battery_label(s: &PowerSnapshot, lang: Lang) -> Option<String> {
    if !s.has_battery {
        return None;
    }
    let p = s.percent?;
    Some(if s.charging {
        format!("{p}% · {}", tr(lang, Key::Charging))
    } else {
        format!("{p}%")
    })
}

/// Reads the current state. Never panics; a desktop yields `has_battery: false`.
pub fn snapshot() -> PowerSnapshot {
    let mut st = SYSTEM_POWER_STATUS::default();
    // SAFETY: plain query into a local struct.
    if unsafe { GetSystemPowerStatus(&mut st) }.is_err() {
        return interpret_status(255, 255, 0, current_mode());
    }
    interpret_status(
        st.BatteryFlag,
        st.BatteryLifePercent,
        st.SystemStatusFlag,
        current_mode(),
    )
}

type GetOverlayFn = unsafe extern "system" fn(*mut GUID) -> u32;
type SetOverlayFn = unsafe extern "system" fn(*const GUID) -> u32;

/// The overlay-scheme exports of `powrprof.dll` (Windows 10 1709+), looked up at run time so
/// a system without them just reports no power mode.
fn overlay_api() -> Option<(GetOverlayFn, SetOverlayFn)> {
    // SAFETY: loading a system DLL by name and resolving two documented exports whose
    // signatures are `DWORD f(GUID*)` / `DWORD f(const GUID*)`.
    unsafe {
        let lib = LoadLibraryW(w!("powrprof.dll")).ok()?;
        let get = GetProcAddress(lib, s!("PowerGetEffectiveOverlayScheme"))?;
        let set = GetProcAddress(lib, s!("PowerSetActiveOverlayScheme"))?;
        Some((
            std::mem::transmute::<_, GetOverlayFn>(get),
            std::mem::transmute::<_, SetOverlayFn>(set),
        ))
    }
}

fn current_mode() -> Option<PowerMode> {
    let (get, _) = overlay_api()?;
    let mut g = GUID::zeroed();
    // SAFETY: out-pointer to a local GUID; a non-zero return means the API failed.
    if unsafe { get(&mut g) } != 0 {
        return None;
    }
    mode_from_guid(&g)
}

/// Switches the Windows power mode; `false` when Windows refused or the API is missing.
pub fn set_mode(mode: PowerMode) -> bool {
    let Some((_, set)) = overlay_api() else {
        return false;
    };
    let g = overlay_guid(mode);
    // SAFETY: the GUID is a documented overlay scheme.
    (unsafe { set(&g) }) == 0
}

/// The registration of the three power settings on a window; unregisters on drop.
pub struct PowerRegistration(Vec<HPOWERNOTIFY>);

/// Subscribes `notify` to `WM_POWERBROADCAST` for battery percentage, AC/DC source and the
/// saver status. The app forwards the message; keep the value alive while the panel is open.
pub fn register(notify: HWND) -> Result<PowerRegistration> {
    let mut reg = PowerRegistration(Vec::new());
    for g in [
        GUID_BATTERY_PERCENTAGE_REMAINING,
        GUID_ACDC_POWER_SOURCE,
        GUID_POWER_SAVING_STATUS,
    ] {
        // SAFETY: a window of this process; unregistered in Drop (also on the error path).
        let h = unsafe {
            RegisterPowerSettingNotification(HANDLE(notify.0), &g, DEVICE_NOTIFY_WINDOW_HANDLE)
        }?;
        reg.0.push(h);
    }
    Ok(reg)
}

impl Drop for PowerRegistration {
    fn drop(&mut self) {
        for h in self.0.drain(..) {
            // SAFETY: undoes the registration made in `register`.
            let _ = unsafe { UnregisterPowerSettingNotification(h) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_guids_map_both_ways() {
        for m in [
            PowerMode::BestEfficiency,
            PowerMode::Balanced,
            PowerMode::BestPerformance,
        ] {
            assert_eq!(mode_from_guid(&overlay_guid(m)), Some(m));
        }
        assert_eq!(
            overlay_guid(PowerMode::BestPerformance),
            GUID::from_u128(0xded574b5_45a0_4f42_8737_46345c09c238)
        );
        assert_eq!(overlay_guid(PowerMode::Balanced), GUID::zeroed());
    }

    #[test]
    fn unknown_guid_is_none() {
        assert_eq!(mode_from_guid(&GUID::from_u128(0x1234)), None);
        // "Better performance" is not one of the three modes we expose.
        assert_eq!(
            mode_from_guid(&GUID::from_u128(0x3af9b8d9_7c97_431d_ad78_34a8bfea439f)),
            None
        );
    }

    fn snap(has: bool, pct: Option<u8>, charging: bool) -> PowerSnapshot {
        PowerSnapshot { has_battery: has, percent: pct, charging, saver: false, mode: None }
    }

    #[test]
    fn label_plain_and_charging_in_both_languages() {
        assert_eq!(battery_label(&snap(true, Some(99), false), Lang::En).as_deref(), Some("99%"));
        assert_eq!(
            battery_label(&snap(true, Some(99), true), Lang::En).as_deref(),
            Some("99% · charging")
        );
        assert_eq!(
            battery_label(&snap(true, Some(7), true), Lang::It).as_deref(),
            Some("7% · in carica")
        );
    }

    #[test]
    fn label_hidden_without_battery_or_percent() {
        assert_eq!(battery_label(&snap(false, None, false), Lang::En), None);
        assert_eq!(battery_label(&snap(true, None, false), Lang::En), None);
    }

    #[test]
    fn status_interpretation() {
        // Desktop: no system battery.
        let s = interpret_status(128, 255, 0, None);
        assert!(!s.has_battery && s.percent.is_none() && !s.charging);
        // Laptop charging at 80 %, saver on.
        let s = interpret_status(8, 80, 1, Some(PowerMode::Balanced));
        assert!(s.has_battery && s.charging && s.saver);
        assert_eq!(s.percent, Some(80));
        // Unknown percentage.
        assert_eq!(interpret_status(1, 255, 0, None).percent, None);
        // Unknown flags.
        assert!(!interpret_status(255, 255, 0, None).has_battery);
    }

    /// Read-only: changes nothing.
    #[test]
    #[ignore = "reads the real power state; run manually"]
    fn real_snapshot_percent_in_range() {
        let s = snapshot();
        assert!(s.percent.map_or(true, |p| p <= 100), "{s:?}");
    }
}
