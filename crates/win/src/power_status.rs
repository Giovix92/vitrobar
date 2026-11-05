//! Battery and power-mode state for the quick panel (spec §5.5).
//!
//! [`snapshot`] reads `GetSystemPowerStatus` and the effective power overlay; changes arrive
//! as `WM_POWERBROADCAST` / `PBT_POWERSETTINGCHANGE` on the window passed to [`register`]
//! (the app forwards the message, the panel then calls [`snapshot`] again). Nothing polls.

use std::sync::OnceLock;

use windows::core::PCWSTR;
use windows::core::{s, w, Result, GUID};
use windows::Win32::Foundation::{LocalFree, ERROR_SUCCESS, HANDLE, HLOCAL, HWND};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Power::{
    GetSystemPowerStatus, RegisterPowerSettingNotification, UnregisterPowerSettingNotification,
    HPOWERNOTIFY, SYSTEM_POWER_STATUS,
};
use windows::Win32::System::Power::{
    PowerGetActiveScheme, PowerReadDCValueIndex, PowerSetActiveScheme, PowerWriteDCValueIndex,
};
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_DWORD,
    RRF_RT_REG_DWORD,
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
    /// Running on mains power (also true on a desktop).
    pub on_ac: bool,
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
/// `ac` is `ACLineStatus`: 0 = on battery, 1 = on mains, 255 = unknown (taken as mains).
pub fn interpret_status(
    ac: u8,
    flag: u8,
    life: u8,
    saver: u8,
    mode: Option<PowerMode>,
) -> PowerSnapshot {
    let has_battery = flag != 255 && flag & 128 == 0;
    PowerSnapshot {
        has_battery,
        percent: (has_battery && life <= 100).then_some(life),
        charging: has_battery && flag & 8 != 0,
        saver: saver & 1 != 0,
        on_ac: !has_battery || ac != 0,
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
        return interpret_status(255, 255, 255, 0, current_mode());
    }
    interpret_status(
        st.ACLineStatus,
        st.BatteryFlag,
        st.BatteryLifePercent,
        st.SystemStatusFlag,
        current_mode(),
    )
}

type GetOverlayFn = unsafe extern "system" fn(*mut GUID) -> u32;
/// `DWORD PowerSetActiveOverlayScheme(GUID)`: the GUID is passed by value (matters on ARM64).
type SetOverlayFn = unsafe extern "system" fn(GUID) -> u32;

/// The overlay-scheme exports of `powrprof.dll` (Windows 10 1709+), looked up at run time so
/// a system without them just reports no power mode.
fn overlay_api() -> Option<(GetOverlayFn, SetOverlayFn)> {
    static API: OnceLock<Option<(GetOverlayFn, SetOverlayFn)>> = OnceLock::new();
    *API.get_or_init(load_overlay_api)
}

fn load_overlay_api() -> Option<(GetOverlayFn, SetOverlayFn)> {
    // SAFETY: loading a system DLL by name and resolving two documented exports whose
    // signatures are `DWORD f(GUID*)` / `DWORD f(GUID)`.
    unsafe {
        let lib = LoadLibraryW(w!("powrprof.dll")).ok()?;
        let get = GetProcAddress(lib, s!("PowerGetEffectiveOverlayScheme"))?;
        let set = GetProcAddress(lib, s!("PowerSetActiveOverlayScheme"))?;
        Some((
            std::mem::transmute::<unsafe extern "system" fn() -> isize, GetOverlayFn>(get),
            std::mem::transmute::<unsafe extern "system" fn() -> isize, SetOverlayFn>(set),
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
    (unsafe { set(g) }) == 0
}

/// `SUB_ENERGYSAVER` / `ESBATTTHRESHOLD`: the charge level (percent) at or below which
/// Windows turns battery saver on by itself while on battery. 100 means "always", which is
/// what Windows' own Settings writes for "Always use energy saver".
const SUB_ENERGYSAVER: GUID = GUID::from_u128(0xde830923_a562_41af_a086_e3a2c6bad2da);
const ESBATTTHRESHOLD: GUID = GUID::from_u128(0xe69653ca_cf7f_4f05_aa73_cb833fa90ad4);
/// Windows' default threshold, for a toggle-off with nothing remembered.
pub const DEFAULT_SAVER_THRESHOLD: u32 = 20;
const SAVED_KEY: PCWSTR = w!("Software\\Vitrobar");
const SAVED_VALUE: PCWSTR = w!("SaverThreshold");

/// The threshold to put back when the saver is switched off: the user's own one, unless it
/// is 100 already (then saver was "always on" before Vitrobar touched it, and the default is
/// the sensible way off).
pub fn threshold_after_off(saved: Option<u32>) -> u32 {
    saved
        .filter(|t| *t < 100)
        .unwrap_or(DEFAULT_SAVER_THRESHOLD)
}

fn read_saved_threshold() -> Option<u32> {
    let mut v = 0u32;
    let mut n = size_of::<u32>() as u32;
    // SAFETY: a DWORD read into a local of the stated size.
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            SAVED_KEY,
            SAVED_VALUE,
            RRF_RT_REG_DWORD,
            None,
            Some((&mut v as *mut u32).cast()),
            Some(&mut n),
        )
    };
    (r == ERROR_SUCCESS).then_some(v)
}

fn write_saved_threshold(t: Option<u32>) {
    // SAFETY: plain registry calls with static names and a local DWORD.
    unsafe {
        match t {
            Some(t) => {
                let _ = RegSetKeyValueW(
                    HKEY_CURRENT_USER,
                    SAVED_KEY,
                    SAVED_VALUE,
                    REG_DWORD.0,
                    Some((&t as *const u32).cast()),
                    size_of::<u32>() as u32,
                );
            }
            None => {
                let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, SAVED_KEY, SAVED_VALUE);
            }
        }
    }
}

fn with_active_scheme<T>(f: impl FnOnce(&GUID) -> T) -> Option<T> {
    let mut p: *mut GUID = std::ptr::null_mut();
    // SAFETY: PowerGetActiveScheme allocates the GUID with LocalAlloc; freed below.
    unsafe {
        if PowerGetActiveScheme(None, &mut p) != ERROR_SUCCESS || p.is_null() {
            return None;
        }
        let out = f(&*p);
        let _ = LocalFree(Some(HLOCAL(p.cast())));
        Some(out)
    }
}

/// The on-battery saver threshold of the active power scheme.
fn dc_threshold() -> Option<u32> {
    with_active_scheme(|scheme| {
        let mut v = 0u32;
        // SAFETY: reads one DWORD of the active scheme into a local.
        let r = unsafe {
            PowerReadDCValueIndex(
                None,
                Some(scheme),
                Some(&SUB_ENERGYSAVER),
                Some(&ESBATTTHRESHOLD),
                &mut v,
            )
        };
        (r == ERROR_SUCCESS.0).then_some(v)
    })
    .flatten()
}

fn write_dc_threshold(t: u32) -> bool {
    with_active_scheme(|scheme| {
        // SAFETY: writes one documented, user-writable value of the active scheme, then
        // re-applies the scheme so the change takes effect at once (as Settings does).
        unsafe {
            PowerWriteDCValueIndex(
                None,
                scheme,
                Some(&SUB_ENERGYSAVER),
                Some(&ESBATTTHRESHOLD),
                t,
            ) == ERROR_SUCCESS.0
                && PowerSetActiveScheme(None, Some(scheme)) == ERROR_SUCCESS
        }
    })
    .unwrap_or(false)
}

/// Turns battery saver on or off while on battery, the way Windows' "Always use energy
/// saver" does it: by raising its threshold to 100 % (the user's own threshold is kept in
/// `HKCU\Software\Vitrobar\SaverThreshold` and put back on the way off). Windows has no
/// public switch for saver on mains power; there the caller opens Settings instead.
/// `false` when Windows refused.
pub fn set_saver(on: bool) -> bool {
    if on {
        let Some(cur) = dc_threshold() else {
            return false;
        };
        if cur < 100 {
            write_saved_threshold(Some(cur));
        }
        write_dc_threshold(100)
    } else {
        let back = threshold_after_off(read_saved_threshold());
        let ok = write_dc_threshold(back);
        if ok {
            write_saved_threshold(None);
        }
        ok
    }
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
        PowerSnapshot {
            has_battery: has,
            percent: pct,
            charging,
            saver: false,
            on_ac: true,
            mode: None,
        }
    }

    #[test]
    fn label_plain_and_charging_in_both_languages() {
        assert_eq!(
            battery_label(&snap(true, Some(99), false), Lang::En).as_deref(),
            Some("99%")
        );
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
        let s = interpret_status(1, 128, 255, 0, None);
        assert!(!s.has_battery && s.percent.is_none() && !s.charging && s.on_ac);
        // Laptop charging at 80 %, saver on.
        let s = interpret_status(1, 8, 80, 1, Some(PowerMode::Balanced));
        assert!(s.has_battery && s.charging && s.saver && s.on_ac);
        assert_eq!(s.percent, Some(80));
        // On battery.
        assert!(!interpret_status(0, 1, 50, 0, None).on_ac);
        // Unknown percentage.
        assert_eq!(interpret_status(1, 1, 255, 0, None).percent, None);
        // Unknown flags.
        assert!(!interpret_status(255, 255, 255, 0, None).has_battery);
        // A desktop is always "on mains", whatever the AC field says.
        assert!(interpret_status(0, 128, 255, 0, None).on_ac);
    }

    #[test]
    fn saver_off_puts_the_users_threshold_back() {
        assert_eq!(threshold_after_off(Some(30)), 30);
        assert_eq!(threshold_after_off(Some(0)), 0);
        assert_eq!(threshold_after_off(Some(100)), DEFAULT_SAVER_THRESHOLD);
        assert_eq!(threshold_after_off(None), DEFAULT_SAVER_THRESHOLD);
    }

    /// Read-only: changes nothing.
    #[test]
    #[ignore = "reads the real power state; run manually"]
    fn real_snapshot_percent_in_range() {
        let s = snapshot();
        assert!(s.percent.is_none_or(|p| p <= 100), "{s:?}");
    }
}
