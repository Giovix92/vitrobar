//! Battery and power-mode state for the quick panel (spec §5.5).
//!
//! [`snapshot`] reads `GetSystemPowerStatus` and the effective power overlay; changes arrive
//! as `WM_POWERBROADCAST` / `PBT_POWERSETTINGCHANGE` on the window passed to [`register`]
//! (the app forwards the message, the panel then calls [`snapshot`] again). Nothing polls.

use std::ffi::c_void;
use std::sync::atomic::{AtomicI8, AtomicIsize, Ordering};
use std::sync::OnceLock;

use windows::core::{s, w, IInspectable, Interface, Result, GUID, HRESULT, HSTRING};
use windows::Foundation::{IPropertyValue, PropertyValue};
use windows::Win32::Foundation::{E_POINTER, HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Power::{
    GetSystemPowerStatus, RegisterPowerSettingNotification, UnregisterPowerSettingNotification,
    HPOWERNOTIFY, SYSTEM_POWER_STATUS,
};
use windows::Win32::System::Power::{
    PowerRegisterForEffectivePowerModeNotifications, EFFECTIVE_POWER_MODE, EFFECTIVE_POWER_MODE_V2,
};
use windows::Win32::System::SystemServices::{
    GUID_ACDC_POWER_SOURCE, GUID_BATTERY_PERCENTAGE_REMAINING, GUID_POWER_SAVING_STATUS,
};
use windows::Win32::UI::WindowsAndMessaging::{
    PostMessageW, DEVICE_NOTIFY_WINDOW_HANDLE, PBT_APMPOWERSTATUSCHANGE, WM_POWERBROADCAST,
};

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
    /// Energy (battery) saver is on.
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
    // The switch when it can be read; Windows' own flag otherwise.
    let saver = match switch_saver_state() {
        Some(on) => u8::from(on),
        None => st.SystemStatusFlag,
    };
    interpret_status(
        st.ACLineStatus,
        st.BatteryFlag,
        st.BatteryLifePercent,
        saver,
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

/// The "Turn on now" switch of Windows' energy saver page, served by the same Settings
/// handler that Settings and the stock Quick Settings use. It works on battery and on mains
/// power (where Windows, like its own Quick Settings, records it as "always use energy
/// saver"). Its value is also the reliable way to read the state: on mains power energy
/// saver does not show as `EffectivePowerModeBatterySaver`, and newer builds no longer
/// report it in `GetSystemPowerStatus`.
const SAVER_SETTING: &str = "SystemSettings_BatterySaver_LandingPage_OverrideControl";

/// `HRESULT GetSetting(HSTRING id, ISettingItem** item, void* reserved)`.
type GetSettingFn =
    unsafe extern "system" fn(*mut c_void, *mut *mut c_void, *mut c_void) -> HRESULT;
/// `ISettingItem::GetValue(HSTRING name, IInspectable** value)` and
/// `SetValue(HSTRING name, IInspectable* value)`: slots 13 and 14 of the vtable (after
/// IInspectable's six methods and the seven property getters).
type GetValueFn = unsafe extern "system" fn(*mut c_void, *mut c_void, *mut *mut c_void) -> HRESULT;
type SetValueFn = unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void) -> HRESULT;
const GET_VALUE_SLOT: usize = 13;
const SET_VALUE_SLOT: usize = 14;

fn get_setting_api() -> Option<GetSettingFn> {
    static API: OnceLock<Option<usize>> = OnceLock::new();
    let f = (*API.get_or_init(|| {
        // SAFETY: loading a system DLL (kept loaded for the process: the switch completes
        // on its threads after SetValue returns) and resolving its exported entry point.
        unsafe {
            let lib = LoadLibraryW(w!("SettingsHandlers_OneCore_BatterySaver.dll")).ok()?;
            GetProcAddress(lib, s!("GetSetting")).map(|f| f as usize)
        }
    }))?;
    // SAFETY: the export has the GetSettingFn signature.
    Some(unsafe { std::mem::transmute::<usize, GetSettingFn>(f) })
}

/// Energy saver as the switch last read: 1 on, 0 off, -1 unknown.
static SAVER_ON: AtomicI8 = AtomicI8::new(-1);
/// The window told when energy saver changes (0: none). It stays set: the app has one
/// notify window, and a wake-up nobody needs costs one snapshot.
static WATCHER: AtomicIsize = AtomicIsize::new(0);

/// Runs `f` on its own COM (MTA) thread, as the Settings handler expects, and waits up to
/// `wait` for its result; the thread then stays alive for `linger` (the handler finishes a
/// switch asynchronously and must not have its item torn down under it).
fn on_com_thread<T: Send + 'static>(
    wait: std::time::Duration,
    linger: std::time::Duration,
    f: impl FnOnce() -> Option<(T, Option<IInspectable>)> + Send + 'static,
) -> Option<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("vitrobar-saver".into())
        .spawn(move || {
            // SAFETY: COM on this thread only; balanced below.
            let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
            let (out, keep) = match f() {
                Some((v, keep)) => (Some(v), keep),
                None => (None, None),
            };
            let _ = tx.send(out);
            if keep.is_some() {
                std::thread::sleep(linger);
            }
            drop(keep);
            if com {
                // SAFETY: pairs the CoInitializeEx above.
                unsafe { CoUninitialize() };
            }
        })
        .ok()?;
    rx.recv_timeout(wait).ok().flatten()
}

/// The switch's setting item.
fn saver_item(get: GetSettingFn) -> Result<IInspectable> {
    let id = HSTRING::from(SAVER_SETTING);
    let mut raw: *mut c_void = std::ptr::null_mut();
    // SAFETY: GetSetting fills `raw` with an owned ISettingItem (an IInspectable), taken
    // over by `from_raw`; the HSTRING is passed as its raw handle and outlives the call.
    unsafe {
        get(
            std::mem::transmute_copy::<HSTRING, *mut c_void>(&id),
            &mut raw,
            std::ptr::null_mut(),
        )
        .ok()?;
        if raw.is_null() {
            return Err(E_POINTER.into());
        }
        Ok(IInspectable::from_raw(raw))
    }
}

/// The method in `slot` of `item`'s vtable.
///
/// # Safety
/// `F` must be the signature of that slot.
unsafe fn vtable_fn<F: Copy>(item: &IInspectable, slot: usize) -> F {
    // SAFETY: a COM object starts with its vtable pointer; the caller vouches for `F`.
    unsafe {
        let vtable = *(item.as_raw() as *const *const usize);
        std::mem::transmute_copy::<usize, F>(&*vtable.add(slot))
    }
}

fn read_switch(get: GetSettingFn) -> Result<bool> {
    let item = saver_item(get)?;
    let name = HSTRING::from("Value");
    let mut out: *mut c_void = std::ptr::null_mut();
    // SAFETY: GetValue through the vtable with a borrowed name; `out` is an owned
    // IInspectable (a boxed boolean) taken over by `from_raw`.
    unsafe {
        let get_value: GetValueFn = vtable_fn(&item, GET_VALUE_SLOT);
        get_value(
            item.as_raw(),
            std::mem::transmute_copy::<HSTRING, *mut c_void>(&name),
            &mut out,
        )
        .ok()?;
        if out.is_null() {
            return Err(E_POINTER.into());
        }
        IInspectable::from_raw(out)
            .cast::<IPropertyValue>()?
            .GetBoolean()
    }
}

fn write_switch(get: GetSettingFn, on: bool) -> Result<IInspectable> {
    let item = saver_item(get)?;
    let name = HSTRING::from("Value");
    let value = PropertyValue::CreateBoolean(on)?;
    // SAFETY: SetValue through the vtable with borrowed arguments.
    unsafe {
        let set: SetValueFn = vtable_fn(&item, SET_VALUE_SLOT);
        set(
            item.as_raw(),
            std::mem::transmute_copy::<HSTRING, *mut c_void>(&name),
            value.as_raw(),
        )
        .ok()?;
    }
    Ok(item)
}

/// Turns energy saver on or off. The change itself lands a moment later and is reported
/// like any other (see [`register`]). `false` when Windows refused or the switch is
/// missing (the caller then opens Settings).
pub fn set_saver(on: bool) -> bool {
    let Some(get) = get_setting_api() else {
        return false;
    };
    let ok = on_com_thread(
        std::time::Duration::from_secs(2),
        std::time::Duration::from_secs(3),
        move || write_switch(get, on).ok().map(|item| ((), Some(item))),
    )
    .is_some();
    if ok {
        SAVER_ON.store(i8::from(on), Ordering::Release);
    }
    ok
}

/// Reads the switch again (on its own thread) and, if it changed, tells the watcher.
fn reread_saver(post: bool) {
    let Some(get) = get_setting_api() else {
        return;
    };
    let _ = std::thread::Builder::new()
        .name("vitrobar-saver-read".into())
        .spawn(move || {
            let Some(on) = on_com_thread(
                std::time::Duration::from_secs(2),
                std::time::Duration::ZERO,
                move || read_switch(get).ok().map(|v| (v, None)),
            ) else {
                return;
            };
            let before = SAVER_ON.swap(i8::from(on), Ordering::AcqRel);
            let w = WATCHER.load(Ordering::Acquire);
            if post && before != i8::from(on) && w != 0 {
                // SAFETY: posting to a window of this process; a stale handle just fails.
                unsafe {
                    let _ = PostMessageW(
                        Some(HWND(w as *mut c_void)),
                        WM_POWERBROADCAST,
                        WPARAM(PBT_APMPOWERSTATUSCHANGE as usize),
                        LPARAM(0),
                    );
                }
            }
        });
}

/// Energy saver (and everything else that changes the effective power mode) moved: the
/// switch is read again. Runs on a pool thread.
unsafe extern "system" fn on_effective_mode(_mode: EFFECTIVE_POWER_MODE, _: *const c_void) {
    reread_saver(true);
}

/// Starts following energy saver (once per process; never unregistered): a first read of
/// the switch (waited for briefly, so the first snapshot is right), then a new read on each
/// change of the effective power mode.
fn watch() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        if let Some(get) = get_setting_api() {
            if let Some(on) = on_com_thread(
                std::time::Duration::from_millis(500),
                std::time::Duration::ZERO,
                move || read_switch(get).ok().map(|v| (v, None)),
            ) {
                SAVER_ON.store(i8::from(on), Ordering::Release);
            }
        }
        let mut h: *mut c_void = std::ptr::null_mut();
        // SAFETY: a static callback with no context; the registration lives as long as
        // the process.
        let _ = unsafe {
            PowerRegisterForEffectivePowerModeNotifications(
                EFFECTIVE_POWER_MODE_V2,
                Some(on_effective_mode),
                None,
                &mut h,
            )
        };
    });
}

/// Energy saver as the switch reads; `None` when it could not be read.
fn switch_saver_state() -> Option<bool> {
    watch();
    match SAVER_ON.load(Ordering::Acquire) {
        -1 => None,
        v => Some(v == 1),
    }
}

/// The registration of the three power settings on a window; unregisters on drop.
pub struct PowerRegistration(Vec<HPOWERNOTIFY>);

/// Subscribes `notify` to `WM_POWERBROADCAST` for battery percentage, AC/DC source and the
/// saver status. The app forwards the message; keep the value alive while the panel is open.
pub fn register(notify: HWND) -> Result<PowerRegistration> {
    watch();
    WATCHER.store(notify.0 as isize, Ordering::Release);
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

    /// Read-only: reads Windows' energy saver switch, changes nothing.
    #[test]
    #[ignore = "reads the real energy saver switch; run manually"]
    fn real_saver_switch_reads() {
        let on = switch_saver_state().expect("the switch reads");
        eprintln!("energy saver on: {on}");
        assert_eq!(snapshot().saver, on);
    }

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

    /// Read-only: changes nothing.
    #[test]
    #[ignore = "reads the real power state; run manually"]
    fn real_snapshot_percent_in_range() {
        let s = snapshot();
        assert!(s.percent.is_none_or(|p| p <= 100), "{s:?}");
    }
}
