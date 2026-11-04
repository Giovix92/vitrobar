//! Display brightness for the quick panel's sliders (spec §5.5).
//!
//! The built-in panel is driven through WMI (`root\wmi`: `WmiMonitorBrightness` to read,
//! `WmiMonitorBrightnessMethods::WmiSetBrightness` to write); external monitors through
//! DDC/CI (dxva2 `GetMonitorBrightness` / `SetMonitorBrightness`). DDC/CI calls take about
//! 50 ms each, so the service runs every read and write on one worker thread; results and the
//! hardware-key `WmiMonitorBrightnessEvent` only queue a [`Change`] and post a payload-free
//! wake-up (see `event_queue`). The UI calls [`Brightness::drain_changes`] and then reads
//! [`Brightness::displays`]. Nothing polls. Without WMI brightness (desktops) the internal
//! display is simply absent; a monitor without DDC/CI is absent too.
//!
//! The free [`displays`] / [`set_level`] block and need COM initialised on the calling thread
//! (the worker does that itself); the UI thread should use the [`Brightness`] service.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

use windows::core::{implement, w, Interface, Result, BOOL, BSTR, HRESULT, PCWSTR};
use windows::Win32::Devices::Display::{
    DestroyPhysicalMonitors, GetMonitorBrightness, GetNumberOfPhysicalMonitorsFromHMONITOR,
    GetPhysicalMonitorsFromHMONITOR, SetMonitorBrightness, PHYSICAL_MONITOR,
};
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, HDC, HMONITOR};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoSetProxyBlanket, CoUninitialize, CLSCTX_INPROC_SERVER,
    CLSCTX_LOCAL_SERVER, COINIT_MULTITHREADED, EOAC_NONE, RPC_C_AUTHN_LEVEL_CALL,
    RPC_C_IMP_LEVEL_IMPERSONATE,
};
use windows::Win32::System::Rpc::{RPC_C_AUTHN_WINNT, RPC_C_AUTHZ_NONE};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::System::Wmi::{
    IUnsecuredApartment, IWbemClassObject, IWbemLocator, IWbemObjectSink, IWbemObjectSink_Impl,
    IWbemServices, UnsecuredApartment, WbemLocator, WBEM_FLAG_FORWARD_ONLY,
    WBEM_FLAG_RETURN_IMMEDIATELY, WBEM_INFINITE,
};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::event_queue::WakeQueue;

/// `Display::monitor` of the built-in panel (WMI has one; DDC ids are `HMONITOR` values).
pub const INTERNAL_ID: u64 = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrightnessKind {
    /// Built-in panel, through WMI (its `Display::name` is empty).
    Internal,
    /// External monitor, through DDC/CI.
    Ddc,
}

/// One adjustable display.
#[derive(Debug, Clone, PartialEq)]
pub struct Display {
    /// [`INTERNAL_ID`] or an `HMONITOR` value.
    pub monitor: u64,
    pub name: String,
    /// 0.0..=1.0.
    pub level: f32,
    pub kind: BrightnessKind,
}

// ---- pure helpers ----------------------------------------------------------------------------

/// Clamps a requested level to 0.0..=1.0 (NaN counts as 0).
pub fn clamp_level(v: f32) -> f32 {
    if v.is_nan() {
        0.0
    } else {
        v.clamp(0.0, 1.0)
    }
}

/// A DDC reading `cur` in `min..=max` as 0.0..=1.0; a degenerate range (`max <= min`) is 1.0.
pub fn ddc_to_level(min: u32, cur: u32, max: u32) -> f32 {
    if max <= min {
        return 1.0;
    }
    let cur = cur.clamp(min, max);
    (cur - min) as f32 / (max - min) as f32
}

/// A level 0.0..=1.0 as a DDC value in `min..=max`; a degenerate range yields `max`.
pub fn level_to_ddc(min: u32, max: u32, level: f32) -> u32 {
    if max <= min {
        return max;
    }
    min + (clamp_level(level) as f64 * (max - min) as f64).round() as u32
}

/// A queued request for the worker.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Job {
    Refresh,
    Set(u64, f32),
}

/// Collapses a burst of jobs (a dragged slider): one `Refresh` at most (first position) and
/// only the latest `Set` per monitor (at the position of its first occurrence).
fn coalesce(jobs: Vec<Job>) -> Vec<Job> {
    let mut out: Vec<Job> = Vec::new();
    for j in jobs {
        match j {
            Job::Refresh => {
                if !out.contains(&Job::Refresh) {
                    out.push(j);
                }
            }
            Job::Set(m, _) => {
                if let Some(slot) = out
                    .iter_mut()
                    .find(|e| matches!(e, Job::Set(em, _) if *em == m))
                {
                    *slot = j;
                } else {
                    out.push(j);
                }
            }
        }
    }
    out
}

// ---- service ----------------------------------------------------------------------------------

/// What the worker / WMI event reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// The display list was (re)read.
    Displays,
    /// The built-in panel's level changed (hardware keys, another app).
    Level,
}

struct Shared {
    queue: WakeQueue<Change>,
    displays: Mutex<Vec<Display>>,
    /// Levels set by the user whose hardware write has not completed yet; a refresh must
    /// not overwrite them with the stale value it read.
    pending: Mutex<HashMap<u64, f32>>,
    notify: isize,
    msg: u32,
    closed: AtomicBool,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Display>> {
        self.displays.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn pending(&self) -> std::sync::MutexGuard<'_, HashMap<u64, f32>> {
        self.pending.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn wake(&self, change: Change) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        if self.queue.push(change) && self.notify != 0 {
            // SAFETY: a payload-free post to a window of this process.
            if unsafe {
                PostMessageW(
                    Some(HWND(self.notify as *mut _)),
                    self.msg,
                    WPARAM(0),
                    LPARAM(0),
                )
            }
            .is_err()
            {
                self.queue.wake_failed();
            }
        }
    }
}

/// Brightness state for the quick panel. Create it when the panel opens, drop it when it
/// closes (P4); the worker thread then ends and the WMI event subscription is cancelled.
pub struct Brightness {
    shared: Arc<Shared>,
    jobs: Sender<Job>,
}

impl Brightness {
    /// Starts the worker (which reads the displays and subscribes to brightness events).
    /// `notify` receives a payload-free `wake_msg` whenever something changed (a null
    /// `notify` posts nothing); on that message call [`Brightness::drain_changes`].
    pub fn open(notify: HWND, wake_msg: u32) -> Result<Self> {
        let shared = Arc::new(Shared {
            queue: WakeQueue::new(),
            displays: Mutex::new(Vec::new()),
            pending: Mutex::new(HashMap::new()),
            notify: notify.0 as isize,
            msg: wake_msg,
            closed: AtomicBool::new(false),
        });
        let (tx, rx) = channel();
        let _ = tx.send(Job::Refresh);
        let sh = shared.clone();
        std::thread::Builder::new()
            .name("vitro-brightness".into())
            .spawn(move || worker(sh, rx))
            .map_err(|_| windows::core::Error::from(windows::Win32::Foundation::E_FAIL))?;
        Ok(Self { shared, jobs: tx })
    }

    /// Takes the queued changes. Call when the wake-up message arrives, whoever sent it; an
    /// empty result means a forged or already-handled wake-up.
    pub fn drain_changes(&self) -> Vec<Change> {
        self.shared.queue.drain()
    }

    /// The displays as last read (empty until the worker's first result arrives).
    pub fn displays(&self) -> Vec<Display> {
        self.shared.lock().clone()
    }

    /// Asks the worker to re-read the displays (a monitor was plugged in or removed).
    pub fn refresh(&self) {
        let _ = self.jobs.send(Job::Refresh);
    }

    /// Sets a display's level on the worker; the stored level follows immediately so the
    /// slider does not jump back while the (slow) hardware write is pending.
    pub fn set_level(&self, monitor: u64, level: f32) {
        let level = clamp_level(level);
        self.shared.pending().insert(monitor, level);
        if let Some(d) = self.shared.lock().iter_mut().find(|d| d.monitor == monitor) {
            d.level = level;
        }
        let _ = self.jobs.send(Job::Set(monitor, level));
    }
}

impl Drop for Brightness {
    fn drop(&mut self) {
        // The worker ends when the sender is dropped (right after this); it is not joined so
        // closing the panel never waits on a slow DDC call.
        self.shared.closed.store(true, Ordering::Release);
        self.shared.queue.clear();
    }
}

/// Initialises COM for the thread and uninitialises it (only if it succeeded) on drop.
pub(crate) struct ComGuard(bool);

impl ComGuard {
    pub(crate) fn init() -> Self {
        // SAFETY: worker-thread MTA init, balanced by `CoUninitialize` in Drop on success.
        Self(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok())
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: balances the successful `CoInitializeEx` in `init`.
            unsafe { CoUninitialize() };
        }
    }
}

fn worker(shared: Arc<Shared>, rx: Receiver<Job>) {
    let _com = ComGuard::init();
    // Keep the WMI connection and the sink alive for the event subscription.
    let subscription = Wmi::connect().and_then(|w| {
        let sink: IWbemObjectSink = BrightnessEvents(shared.clone()).into();
        // Deliver through an unsecured-apartment stub: a plain sink often gets no callbacks
        // (or E_ACCESSDENIED) at the default process COM security, and we must not call
        // CoInitializeSecurity (process-wide).
        // SAFETY: plain activation; the stub forwards to `sink`, which it keeps alive.
        let apt: IUnsecuredApartment =
            unsafe { CoCreateInstance(&UnsecuredApartment, None, CLSCTX_LOCAL_SERVER) }?;
        let stub: IWbemObjectSink = unsafe { apt.CreateObjectStub(&sink) }?.cast()?;
        // SAFETY: the stub outlives the subscription (cancelled below before it is dropped).
        unsafe {
            w.svc.ExecNotificationQueryAsync(
                &BSTR::from("WQL"),
                &BSTR::from("SELECT * FROM WmiMonitorBrightnessEvent"),
                Default::default(),
                None,
                &stub,
            )
        }?;
        Ok((w, stub))
    });

    while let Ok(first) = rx.recv() {
        let mut batch = vec![first];
        while let Ok(j) = rx.try_recv() {
            batch.push(j);
        }
        for job in coalesce(batch) {
            match job {
                Job::Refresh => {
                    let mut fresh = displays();
                    {
                        // Keep levels the user set after this read was queued.
                        let pending = shared.pending();
                        for d in &mut fresh {
                            if let Some(&l) = pending.get(&d.monitor) {
                                d.level = l;
                            }
                        }
                    }
                    *shared.lock() = fresh;
                    shared.wake(Change::Displays);
                }
                Job::Set(monitor, level) => {
                    set_level(monitor, level);
                    // Done, unless a newer level was set meanwhile (its job is still queued).
                    let mut p = shared.pending();
                    if p.get(&monitor) == Some(&level) {
                        p.remove(&monitor);
                    }
                }
            }
        }
    }

    if let Ok((w, sink)) = &subscription {
        // SAFETY: cancels the subscription made above.
        let _ = unsafe { w.svc.CancelAsyncCall(sink) };
    }
}

// ---- WMI ----------------------------------------------------------------------------------------

struct Wmi {
    svc: IWbemServices,
}

impl Wmi {
    fn connect() -> Result<Self> {
        // SAFETY: plain activation of a system class on a COM-initialised thread.
        let locator: IWbemLocator =
            unsafe { CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER) }?;
        // SAFETY: connects to the local `root\wmi` namespace as the current user.
        let svc = unsafe {
            locator.ConnectServer(
                &BSTR::from("ROOT\\WMI"),
                &BSTR::new(),
                &BSTR::new(),
                &BSTR::new(),
                0,
                &BSTR::new(),
                None,
            )
        }?;
        // SAFETY: standard proxy security for a local WMI service.
        unsafe {
            CoSetProxyBlanket(
                &svc,
                RPC_C_AUTHN_WINNT,
                RPC_C_AUTHZ_NONE,
                PCWSTR::null(),
                RPC_C_AUTHN_LEVEL_CALL,
                RPC_C_IMP_LEVEL_IMPERSONATE,
                None,
                EOAC_NONE,
            )
        }?;
        Ok(Self { svc })
    }

    /// The first row of `class` (an active panel), if any.
    fn first(&self, class: &str, only_active: bool) -> Option<IWbemClassObject> {
        let wql = if only_active {
            format!("SELECT * FROM {class} WHERE Active = TRUE")
        } else {
            format!("SELECT * FROM {class}")
        };
        // SAFETY: a read-only query; the enumerator is released on drop.
        let en = unsafe {
            self.svc.ExecQuery(
                &BSTR::from("WQL"),
                &BSTR::from(wql),
                WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY,
                None,
            )
        }
        .ok()?;
        let mut row: [Option<IWbemClassObject>; 1] = [None];
        let mut got = 0u32;
        // SAFETY: one-element buffer and its count.
        let hr = unsafe { en.Next(WBEM_INFINITE, &mut row, &mut got) };
        if hr.is_err() || got == 0 {
            return None;
        }
        row[0].take()
    }

    /// The built-in panel's level, if WMI brightness exists.
    fn internal_level(&self) -> Option<f32> {
        let row = self.first("WmiMonitorBrightness", true)?;
        get_i32(&row, w!("CurrentBrightness")).map(|b| clamp_level(b as f32 / 100.0))
    }

    fn set_internal(&self, level: f32) -> Option<()> {
        let target = self.first("WmiMonitorBrightnessMethods", true)?;
        let path = {
            let mut v = VARIANT::default();
            // SAFETY: reads the `__PATH` system property into a local VARIANT.
            unsafe { target.Get(w!("__PATH"), 0, &mut v, None, None) }.ok()?;
            BSTR::try_from(&v).ok()?
        };
        let mut class = None;
        // SAFETY: fetches the class definition to build the method's input parameters.
        unsafe {
            self.svc.GetObject(
                &BSTR::from("WmiMonitorBrightnessMethods"),
                Default::default(),
                None,
                Some(&mut class),
                None,
            )
        }
        .ok()?;
        let class: IWbemClassObject = class?;
        let mut sig = None;
        // SAFETY: method signature lookup on the class object.
        unsafe { class.GetMethod(w!("WmiSetBrightness"), 0, &mut sig, &mut None) }.ok()?;
        // SAFETY: instance of the input-parameter class.
        let params = unsafe { sig?.SpawnInstance(0) }.ok()?;
        let percent = (clamp_level(level) * 100.0).round() as i32;
        // SAFETY: sets the two documented input parameters; WMI coerces the VT_I4s.
        unsafe {
            params.Put(w!("Timeout"), 0, &VARIANT::from(0i32), 0).ok()?;
            params
                .Put(w!("Brightness"), 0, &VARIANT::from(percent), 0)
                .ok()?;
            self.svc
                .ExecMethod(
                    &path,
                    &BSTR::from("WmiSetBrightness"),
                    Default::default(),
                    None,
                    &params,
                    None,
                    None,
                )
                .ok()
        }
    }
}

fn get_i32(obj: &IWbemClassObject, name: PCWSTR) -> Option<i32> {
    let mut v = VARIANT::default();
    // SAFETY: reads one property into a local VARIANT.
    unsafe { obj.Get(name, 0, &mut v, None, None) }.ok()?;
    i32::try_from(&v).ok()
}

/// `WmiMonitorBrightnessEvent` sink: keeps the stored internal level current and wakes the UI.
#[implement(IWbemObjectSink)]
struct BrightnessEvents(Arc<Shared>);

impl IWbemObjectSink_Impl for BrightnessEvents_Impl {
    fn Indicate(&self, count: i32, objects: *const Option<IWbemClassObject>) -> Result<()> {
        if count > 0 && !objects.is_null() {
            // SAFETY: WMI passes `count` valid (possibly null) interface slots.
            let slice = unsafe { std::slice::from_raw_parts(objects, count as usize) };
            for obj in slice.iter().flatten() {
                if let Some(b) = get_i32(obj, w!("Brightness")) {
                    let level = clamp_level(b as f32 / 100.0);
                    if let Some(d) = self
                        .0
                        .lock()
                        .iter_mut()
                        .find(|d| d.kind == BrightnessKind::Internal)
                    {
                        d.level = level;
                    }
                }
            }
        }
        self.0.wake(Change::Level);
        Ok(())
    }

    fn SetStatus(
        &self,
        _flags: i32,
        _result: HRESULT,
        _param: &BSTR,
        _obj: windows::core::Ref<'_, IWbemClassObject>,
    ) -> Result<()> {
        Ok(())
    }
}

// ---- DDC/CI --------------------------------------------------------------------------------------

unsafe extern "system" fn collect_monitor(
    h: HMONITOR,
    _dc: HDC,
    _rect: *mut RECT,
    data: LPARAM,
) -> BOOL {
    // SAFETY: `data` is the `&mut Vec<isize>` passed by `ddc_monitors`.
    let v = unsafe { &mut *(data.0 as *mut Vec<isize>) };
    v.push(h.0 as isize);
    BOOL(1)
}

fn ddc_monitors() -> Vec<isize> {
    let mut v: Vec<isize> = Vec::new();
    // SAFETY: the callback only pushes into `v`, which outlives the call.
    let _ = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(collect_monitor),
            LPARAM(&mut v as *mut _ as isize),
        )
    };
    v
}

/// Runs `f` with the first physical monitor of `hmon`, then releases the handles.
fn with_physical<T>(hmon: isize, f: impl FnOnce(&PHYSICAL_MONITOR) -> Option<T>) -> Option<T> {
    let h = HMONITOR(hmon as *mut _);
    let mut n = 0u32;
    // SAFETY: plain query with an out-count.
    unsafe { GetNumberOfPhysicalMonitorsFromHMONITOR(h, &mut n) }.ok()?;
    if n == 0 {
        return None;
    }
    let mut mons = vec![PHYSICAL_MONITOR::default(); n as usize];
    // SAFETY: the buffer holds exactly `n` entries; destroyed below.
    unsafe { GetPhysicalMonitorsFromHMONITOR(h, &mut mons) }.ok()?;
    let r = f(&mons[0]);
    // SAFETY: releases the handles obtained above.
    let _ = unsafe { DestroyPhysicalMonitors(&mons) };
    r
}

fn ddc_display(hmon: isize) -> Option<Display> {
    with_physical(hmon, |pm| {
        let (mut min, mut cur, mut max) = (0u32, 0u32, 0u32);
        // SAFETY: handle valid until `with_physical` returns; a monitor without DDC/CI fails.
        if unsafe { GetMonitorBrightness(pm.hPhysicalMonitor, &mut min, &mut cur, &mut max) } == 0 {
            return None;
        }
        // Copy out of the packed struct before borrowing.
        let desc = pm.szPhysicalMonitorDescription;
        let end = desc.iter().position(|&c| c == 0).unwrap_or(desc.len());
        Some(Display {
            monitor: hmon as usize as u64,
            name: String::from_utf16_lossy(&desc[..end]),
            level: ddc_to_level(min, cur, max),
            kind: BrightnessKind::Ddc,
        })
    })
}

fn set_ddc(hmon: isize, level: f32) {
    with_physical(hmon, |pm| {
        let (mut min, mut cur, mut max) = (0u32, 0u32, 0u32);
        // SAFETY: as in `ddc_display`.
        if unsafe { GetMonitorBrightness(pm.hPhysicalMonitor, &mut min, &mut cur, &mut max) } == 0 {
            return None;
        }
        let v = level_to_ddc(min, max, level);
        // SAFETY: writes a value inside the monitor's reported range.
        let _ = unsafe { SetMonitorBrightness(pm.hPhysicalMonitor, v) };
        Some(())
    });
}

// ---- blocking entry points ---------------------------------------------------------------------

/// Reads every adjustable display (built-in through WMI, externals through DDC/CI). Slow:
/// call from a worker thread with COM initialised. Never panics; unavailable sources are
/// simply absent.
pub fn displays() -> Vec<Display> {
    let mut out = Vec::new();
    if let Some(level) = Wmi::connect().ok().and_then(|w| w.internal_level()) {
        out.push(Display {
            monitor: INTERNAL_ID,
            // The UI labels the built-in panel by kind (`Key::BuiltInDisplay`).
            name: String::new(),
            level,
            kind: BrightnessKind::Internal,
        });
    }
    out.extend(ddc_monitors().into_iter().filter_map(ddc_display));
    out
}

/// Sets one display's brightness (0.0..=1.0). Slow: call from a worker thread with COM
/// initialised.
pub fn set_level(monitor: u64, level: f32) {
    let level = clamp_level(level);
    if monitor == INTERNAL_ID {
        if let Ok(w) = Wmi::connect() {
            let _ = w.set_internal(level);
        }
    } else {
        set_ddc(monitor as usize as isize, level);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ddc_range_normalises_both_ways() {
        assert_eq!(ddc_to_level(0, 0, 100), 0.0);
        assert_eq!(ddc_to_level(0, 100, 100), 1.0);
        assert_eq!(ddc_to_level(10, 55, 100), 0.5);
        assert_eq!(level_to_ddc(10, 100, 0.0), 10);
        assert_eq!(level_to_ddc(10, 100, 1.0), 100);
        assert_eq!(level_to_ddc(10, 100, 0.5), 55);
        // Round trip across a non-trivial range.
        for cur in [20, 33, 77, 120] {
            let back = level_to_ddc(20, 120, ddc_to_level(20, cur, 120));
            assert_eq!(back, cur);
        }
    }

    #[test]
    fn ddc_range_edge_cases() {
        assert_eq!(ddc_to_level(5, 5, 5), 1.0, "max == min");
        assert_eq!(ddc_to_level(9, 3, 5), 1.0, "max < min");
        assert_eq!(level_to_ddc(5, 5, 0.3), 5);
        assert_eq!(ddc_to_level(10, 2, 20), 0.0, "reading below min clamps");
        assert_eq!(ddc_to_level(10, 99, 20), 1.0, "reading above max clamps");
        assert_eq!(level_to_ddc(0, 100, 7.0), 100);
        assert_eq!(level_to_ddc(0, 100, -1.0), 0);
        assert_eq!(level_to_ddc(0, 100, f32::NAN), 0);
        assert_eq!(clamp_level(f32::NAN), 0.0);
    }

    #[test]
    fn a_drag_collapses_to_the_latest_set_per_monitor() {
        let jobs = vec![
            Job::Set(1, 0.1),
            Job::Set(2, 0.5),
            Job::Refresh,
            Job::Set(1, 0.2),
            Job::Refresh,
            Job::Set(1, 0.3),
        ];
        assert_eq!(
            coalesce(jobs),
            vec![Job::Set(1, 0.3), Job::Set(2, 0.5), Job::Refresh]
        );
        assert!(coalesce(Vec::new()).is_empty());
    }

    /// Read-only: reads levels, sets nothing.
    #[test]
    #[ignore = "reads the real displays over WMI and DDC/CI; run manually"]
    fn real_displays_return_without_panic() {
        let _com = ComGuard::init();
        for d in displays() {
            assert!((0.0..=1.0).contains(&d.level), "{d:?}");
        }
    }
}
