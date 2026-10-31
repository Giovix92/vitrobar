//! Bluetooth for the quick panel (spec §5.5): radio, paired devices, connect / disconnect of
//! audio devices.
//!
//! Event driven, nothing polls: [`Bluetooth::open`] starts a `DeviceWatcher` on the paired
//! Bluetooth devices (asking for `System.Devices.Aep.IsConnected` and the battery level) and
//! registers `Radio.StateChanged`. Their handlers only update the shared device table, queue a
//! [`Change`] and post a payload-free wake-up (see `event_queue`); the UI calls
//! [`Bluetooth::drain_changes`] and then [`Bluetooth::snapshot`].
//!
//! Everything that can block runs on a worker thread with balanced COM: the radio switch
//! (`SetStateAsync`), resolving a newly seen device (`BluetoothDevice.FromIdAsync`: class of
//! device and radio address) and `BluetoothSetServiceState`.
//!
//! There is no general Windows API to connect a Bluetooth device (ruling P3). For *audio*
//! devices (major class 0x04) connect / disconnect enables / disables the A2DP sink and
//! Hands-Free services through `BluetoothSetServiceState`; any other device is
//! [`BtError::HandOff`] (open `ms-settings:bluetooth`). The device address comes from
//! `BluetoothDevice.BluetoothAddress` (not from parsing the device id, whose layout is not
//! documented).
//!
//! Failures meant for the user are [`BtError`] values; the UI chooses the text.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

use windows::core::{Interface, Result, GUID, HSTRING};
use windows::Devices::Bluetooth::BluetoothDevice;
use windows::Devices::Enumeration::{
    DeviceInformation, DeviceInformationKind, DeviceInformationUpdate, DeviceWatcher,
};
use windows::Devices::Radios::{Radio, RadioAccessStatus, RadioKind, RadioState};
use windows_collections::{IIterable, IMapView};
use windows::core::IInspectable;
use windows::Foundation::{IReference, TypedEventHandler};
use windows::Win32::Devices::Bluetooth::{
    BluetoothFindFirstRadio, BluetoothFindRadioClose, BluetoothSetServiceState, BLUETOOTH_DEVICE_INFO,
    BLUETOOTH_FIND_RADIO_PARAMS,
};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::brightness::ComGuard;
use crate::event_queue::WakeQueue;

/// A2DP sink (audio playback) service class.
pub const A2DP_SINK: GUID = GUID::from_u128(0x0000110B_0000_1000_8000_00805F9B34FB);
/// Hands-Free (headset microphone / calls) service class.
pub const HANDS_FREE: GUID = GUID::from_u128(0x0000111E_0000_1000_8000_00805F9B34FB);

const AEP_CONNECTED: &str = "System.Devices.Aep.IsConnected";
const AEP_BATTERY_LE: &str = "System.Devices.Aep.Bluetooth.Le.BatteryLevel";
/// `{104EA319-6EE2-4701-BD47-8DDBF425BBE5} 2`: the battery percentage Windows keeps for
/// classic devices that report it.
const AEP_BATTERY_CLASSIC: &str = "{104EA319-6EE2-4701-BD47-8DDBF425BBE5} 2";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BtDevice {
    pub id: String,
    pub name: String,
    pub connected: bool,
    /// Audio / video major class: connect and disconnect are offered.
    pub audio: bool,
    /// 0..=100 when the device reports it.
    pub battery: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BtSnapshot {
    /// `None` when there is no Bluetooth radio object to ask.
    pub radio_on: Option<bool>,
    pub devices: Vec<BtDevice>,
}

/// A user-facing failure; the UI picks the (localized) text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BtError {
    /// Needs Windows' own UI (not an audio device): open `ms-settings:bluetooth`.
    HandOff,
    /// Windows refused the radio change (user or system policy).
    Denied,
    Failed,
}

/// What the handlers / workers report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// A device appeared, changed or went away: re-read [`Bluetooth::snapshot`].
    Devices,
    /// The radio state changed.
    Radio,
    /// A radio change or a service-state change failed.
    Error(BtError),
}

// ---- pure helpers ----------------------------------------------------------------------------

/// Whether a 24-bit Bluetooth class of device has major class 0x04 (audio / video:
/// headsets, speakers, headphones...). Bits 8..=12 hold the major class.
pub fn is_audio_class(class_of_device: u32) -> bool {
    (class_of_device >> 8) & 0x1F == 0x04
}

/// Connected devices first, then by name (case-insensitive), then by id for a stable order.
pub fn arrange(mut devices: Vec<BtDevice>) -> Vec<BtDevice> {
    devices.sort_by(|a, b| {
        b.connected
            .cmp(&a.connected)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.id.cmp(&b.id))
    });
    devices
}

// ---- service ----------------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
struct Entry {
    name: String,
    connected: bool,
    battery: Option<u8>,
    /// `None` until the device worker resolved it (an unresolved device is not offered
    /// connect / disconnect).
    audio: Option<bool>,
    address: Option<u64>,
}

struct Shared {
    queue: WakeQueue<Change>,
    notify: isize,
    msg: u32,
    closed: AtomicBool,
    radio_on: Mutex<Option<bool>>,
    devices: Mutex<HashMap<String, Entry>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Shared {
    fn wake(&self, change: Change) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        if self.queue.push(change) && self.notify != 0 {
            // SAFETY: a payload-free post to a window of this process.
            if unsafe { PostMessageW(Some(HWND(self.notify as *mut _)), self.msg, WPARAM(0), LPARAM(0)) }
                .is_err()
            {
                self.queue.wake_failed();
            }
        }
    }
}

enum RadioJob {
    Set(bool),
}

enum DeviceJob {
    /// Read the class of device and address of a newly seen device.
    Resolve(String),
    /// Enable (connect) or disable (disconnect) the audio services of the device at `address`.
    Services { address: u64, on: bool },
}

pub struct Bluetooth {
    shared: Arc<Shared>,
    watcher: Option<DeviceWatcher>,
    tokens: Vec<WatcherToken>,
    radio_jobs: Sender<RadioJob>,
    device_jobs: Sender<DeviceJob>,
}

enum WatcherToken {
    Added(i64),
    Updated(i64),
    Removed(i64),
    Completed(i64),
}

impl Bluetooth {
    /// Starts the device watcher and the two workers. A machine without Bluetooth is not an
    /// error: the snapshot is simply `radio_on == None` with no devices, and the caller hides
    /// the tile. `notify` receives a payload-free `wake_msg` on every change; a null `notify`
    /// posts nothing. On that message call [`Bluetooth::drain_changes`].
    pub fn open(notify: HWND, wake_msg: u32) -> Result<Self> {
        let shared = Arc::new(Shared {
            queue: WakeQueue::new(),
            notify: notify.0 as isize,
            msg: wake_msg,
            closed: AtomicBool::new(false),
            radio_on: Mutex::new(None),
            devices: Mutex::new(HashMap::new()),
        });
        let (radio_tx, radio_rx) = channel();
        let (dev_tx, dev_rx) = channel();
        // If a thread cannot start, only its feature (radio switch / connect) is unavailable.
        let sh = shared.clone();
        let _ = std::thread::Builder::new()
            .name("vitro-bt-radio".into())
            .spawn(move || radio_worker(sh, radio_rx));
        let sh = shared.clone();
        let _ = std::thread::Builder::new()
            .name("vitro-bt-devices".into())
            .spawn(move || device_worker(sh, dev_rx));

        let mut this = Self { shared: shared.clone(), watcher: None, tokens: Vec::new(), radio_jobs: radio_tx, device_jobs: dev_tx };
        this.start_watcher()?;
        Ok(this)
    }

    fn start_watcher(&mut self) -> Result<()> {
        let props = IIterable::<HSTRING>::from(vec![
            HSTRING::from(AEP_CONNECTED),
            HSTRING::from(AEP_BATTERY_LE),
            HSTRING::from(AEP_BATTERY_CLASSIC),
        ]);
        // Paired devices only; the selector already carries the Bluetooth protocol id.
        let paired = BluetoothDevice::GetDeviceSelectorFromPairingState(true)?;
        let watcher = DeviceInformation::CreateWatcherWithKindAqsFilterAndAdditionalProperties(
            &paired,
            &props,
            DeviceInformationKind::AssociationEndpoint,
        )?;

        let sh = self.shared.clone();
        let jobs = self.device_jobs.clone();
        let added = watcher.Added(&TypedEventHandler::new(
            move |_: windows::core::Ref<'_, DeviceWatcher>, info: windows::core::Ref<'_, DeviceInformation>| {
                if let Some(info) = info.as_ref() {
                    let id = info.Id().map(|s| s.to_string()).unwrap_or_default();
                    if !id.is_empty() {
                        let mut e = Entry { name: info.Name().map(|s| s.to_string()).unwrap_or_default(), ..Entry::default() };
                        if let Ok(p) = info.Properties() {
                            apply_properties(&mut e, &p);
                        }
                        lock(&sh.devices).insert(id.clone(), e);
                        let _ = jobs.send(DeviceJob::Resolve(id));
                        sh.wake(Change::Devices);
                    }
                }
                Ok(())
            },
        ))?;
        self.tokens.push(WatcherToken::Added(added));

        let sh = self.shared.clone();
        let updated = watcher.Updated(&TypedEventHandler::new(
            move |_: windows::core::Ref<'_, DeviceWatcher>, upd: windows::core::Ref<'_, DeviceInformationUpdate>| {
                if let Some(upd) = upd.as_ref() {
                    let id = upd.Id().map(|s| s.to_string()).unwrap_or_default();
                    if let (Some(e), Ok(p)) = (lock(&sh.devices).get_mut(&id), upd.Properties()) {
                        apply_properties(e, &p);
                    }
                    sh.wake(Change::Devices);
                }
                Ok(())
            },
        ))?;
        self.tokens.push(WatcherToken::Updated(updated));

        let sh = self.shared.clone();
        let removed = watcher.Removed(&TypedEventHandler::new(
            move |_: windows::core::Ref<'_, DeviceWatcher>, upd: windows::core::Ref<'_, DeviceInformationUpdate>| {
                if let Some(upd) = upd.as_ref() {
                    let id = upd.Id().map(|s| s.to_string()).unwrap_or_default();
                    lock(&sh.devices).remove(&id);
                    sh.wake(Change::Devices);
                }
                Ok(())
            },
        ))?;
        self.tokens.push(WatcherToken::Removed(removed));

        let sh = self.shared.clone();
        let completed = watcher.EnumerationCompleted(&TypedEventHandler::new(
            move |_: windows::core::Ref<'_, DeviceWatcher>, _: windows::core::Ref<'_, IInspectable>| {
                sh.wake(Change::Devices);
                Ok(())
            },
        ))?;
        self.tokens.push(WatcherToken::Completed(completed));

        watcher.Start()?;
        self.watcher = Some(watcher);
        Ok(())
    }

    /// Takes the queued changes (an empty result means a forged or already-handled wake-up).
    pub fn drain_changes(&self) -> Vec<Change> {
        self.shared.queue.drain()
    }

    /// The current state, from the table the watcher keeps (no radio traffic).
    pub fn snapshot(&self) -> BtSnapshot {
        let devices = lock(&self.shared.devices)
            .iter()
            .map(|(id, e)| BtDevice {
                id: id.clone(),
                name: if e.name.is_empty() { id.clone() } else { e.name.clone() },
                connected: e.connected,
                audio: e.audio == Some(true),
                battery: e.battery,
            })
            .collect();
        BtSnapshot { radio_on: *lock(&self.shared.radio_on), devices: arrange(devices) }
    }

    /// Switches the Bluetooth radio on a worker; the outcome is a [`Change::Radio`] or
    /// [`Change::Error`] (`Denied` when Windows refuses).
    pub fn set_radio(&self, on: bool) {
        let _ = self.radio_jobs.send(RadioJob::Set(on));
    }

    /// Connects or disconnects an *audio* device by enabling / disabling its A2DP and
    /// Hands-Free services. Returns at once; the result arrives as [`Change::Devices`]
    /// (the watcher sees the connection change) or [`Change::Error`]. Other devices, unknown
    /// ids and devices not yet resolved are [`BtError::HandOff`].
    pub fn set_connected(&self, id: &str, on: bool) -> std::result::Result<(), BtError> {
        let address = {
            let devices = lock(&self.shared.devices);
            match devices.get(id) {
                Some(Entry { audio: Some(true), address: Some(a), .. }) => *a,
                _ => return Err(BtError::HandOff),
            }
        };
        self.device_jobs.send(DeviceJob::Services { address, on }).map_err(|_| BtError::Failed)
    }
}

impl Drop for Bluetooth {
    fn drop(&mut self) {
        self.shared.closed.store(true, Ordering::Release);
        if let Some(w) = self.watcher.take() {
            // Stop returns at once; the Stopped event is not awaited (a handler that is
            // still running only touches `Shared`, which it keeps alive, and `closed` mutes
            // its wake-ups).
            let _ = w.Stop();
            for t in self.tokens.drain(..) {
                let _ = match t {
                    WatcherToken::Added(t) => w.RemoveAdded(t),
                    WatcherToken::Updated(t) => w.RemoveUpdated(t),
                    WatcherToken::Removed(t) => w.RemoveRemoved(t),
                    WatcherToken::Completed(t) => w.RemoveEnumerationCompleted(t),
                };
            }
        }
        // The workers end when their job senders drop with `self`.
        self.shared.queue.clear();
    }
}

// ---- property reading ---------------------------------------------------------------------------

fn lookup(p: &IMapView<HSTRING, IInspectable>, key: &str) -> Option<IInspectable> {
    p.Lookup(&HSTRING::from(key)).ok()
}

fn as_bool(v: &IInspectable) -> Option<bool> {
    v.cast::<IReference<bool>>().ok()?.Value().ok()
}

fn as_percent(v: &IInspectable) -> Option<u8> {
    let n = v
        .cast::<IReference<u8>>()
        .ok()
        .and_then(|r| r.Value().ok())
        .map(i64::from)
        .or_else(|| v.cast::<IReference<i32>>().ok().and_then(|r| r.Value().ok()).map(i64::from))
        .or_else(|| v.cast::<IReference<u32>>().ok().and_then(|r| r.Value().ok()).map(i64::from))?;
    (0..=100).contains(&n).then_some(n as u8)
}

/// Applies whichever of the requested properties `p` carries (an update carries only the
/// changed ones).
fn apply_properties(e: &mut Entry, p: &IMapView<HSTRING, IInspectable>) {
    if let Some(c) = lookup(p, AEP_CONNECTED).and_then(|v| as_bool(&v)) {
        e.connected = c;
    }
    if let Some(b) = lookup(p, AEP_BATTERY_LE)
        .and_then(|v| as_percent(&v))
        .or_else(|| lookup(p, AEP_BATTERY_CLASSIC).and_then(|v| as_percent(&v)))
    {
        e.battery = Some(b);
    }
}

// ---- workers -------------------------------------------------------------------------------------

fn bluetooth_radio() -> Option<Radio> {
    let radios = Radio::GetRadiosAsync().ok()?.join().ok()?;
    radios.into_iter().find(|r| r.Kind().ok() == Some(RadioKind::Bluetooth))
}

fn radio_worker(shared: Arc<Shared>, rx: Receiver<RadioJob>) {
    let _com = ComGuard::init();
    let radio = bluetooth_radio();
    let mut token = None;
    if let Some(r) = &radio {
        *lock(&shared.radio_on) = r.State().ok().map(|s| s == RadioState::On);
        let sh = shared.clone();
        token = r
            .StateChanged(&TypedEventHandler::new(move |sender: windows::core::Ref<'_, Radio>, _| {
                if let Some(r) = sender.as_ref() {
                    *lock(&sh.radio_on) = r.State().ok().map(|s| s == RadioState::On);
                }
                sh.wake(Change::Radio);
                Ok(())
            }))
            .ok();
        shared.wake(Change::Radio);
    }
    while let Ok(RadioJob::Set(on)) = rx.recv() {
        let outcome = match &radio {
            None => Err(BtError::Failed),
            Some(r) => {
                let state = if on { RadioState::On } else { RadioState::Off };
                match r.SetStateAsync(state).and_then(|a| a.join()) {
                    Ok(RadioAccessStatus::Allowed) => Ok(()),
                    Ok(RadioAccessStatus::DeniedByUser) | Ok(RadioAccessStatus::DeniedBySystem) => {
                        Err(BtError::Denied)
                    }
                    _ => Err(BtError::Failed),
                }
            }
        };
        shared.wake(match outcome {
            Ok(()) => Change::Radio,
            Err(e) => Change::Error(e),
        });
    }
    if let (Some(r), Some(t)) = (&radio, token) {
        let _ = r.RemoveStateChanged(t);
    }
}

fn device_worker(shared: Arc<Shared>, rx: Receiver<DeviceJob>) {
    let _com = ComGuard::init();
    while let Ok(job) = rx.recv() {
        match job {
            DeviceJob::Resolve(id) => {
                let resolved = BluetoothDevice::FromIdAsync(&HSTRING::from(id.as_str()))
                    .and_then(|a| a.join())
                    .ok()
                    .and_then(|d| {
                        let cod = d.ClassOfDevice().ok()?.RawValue().ok()?;
                        Some((is_audio_class(cod), d.BluetoothAddress().ok()?))
                    });
                if let Some((audio, address)) = resolved {
                    if let Some(e) = lock(&shared.devices).get_mut(&id) {
                        e.audio = Some(audio);
                        e.address = Some(address);
                    }
                    shared.wake(Change::Devices);
                }
            }
            DeviceJob::Services { address, on } => {
                let ok = set_audio_services(address, on);
                shared.wake(if ok { Change::Devices } else { Change::Error(BtError::Failed) });
            }
        }
    }
}

/// Enables / disables the A2DP sink and Hands-Free services of the device at `address`; true
/// when at least one call succeeded (a device usually has only one of the two).
fn set_audio_services(address: u64, on: bool) -> bool {
    let params = BLUETOOTH_FIND_RADIO_PARAMS {
        dwSize: std::mem::size_of::<BLUETOOTH_FIND_RADIO_PARAMS>() as u32,
    };
    let mut radio = HANDLE::default();
    // SAFETY: plain out-parameters; the find handle and the radio handle are closed below.
    let Ok(find) = (unsafe { BluetoothFindFirstRadio(&params, &mut radio) }) else {
        return false;
    };
    let mut info = BLUETOOTH_DEVICE_INFO {
        dwSize: std::mem::size_of::<BLUETOOTH_DEVICE_INFO>() as u32,
        ..Default::default()
    };
    info.Address.Anonymous.ullLong = address;
    let flag = u32::from(on);
    let mut ok = false;
    for guid in [A2DP_SINK, HANDS_FREE] {
        // SAFETY: `info` and `guid` outlive the call; `radio` is the open radio handle.
        ok |= unsafe { BluetoothSetServiceState(Some(radio), &info, &guid, flag) } == 0;
    }
    // SAFETY: closes the handles opened above.
    unsafe {
        let _ = CloseHandle(radio);
        let _ = BluetoothFindRadioClose(find);
    }
    ok
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(id: &str, name: &str, connected: bool) -> BtDevice {
        BtDevice { id: id.into(), name: name.into(), connected, audio: false, battery: None }
    }

    #[test]
    fn audio_class_is_major_class_four() {
        // Headset 0x240404, loudspeaker 0x240414, headphones 0x240418.
        assert!(is_audio_class(0x240404));
        assert!(is_audio_class(0x240414));
        assert!(is_audio_class(0x240418));
        // Phone 0x5A020C (major 2), keyboard 0x002540 (major 5), computer 0x00010C (major 1).
        assert!(!is_audio_class(0x5A020C));
        assert!(!is_audio_class(0x002540));
        assert!(!is_audio_class(0x00010C));
        assert!(!is_audio_class(0));
    }

    #[test]
    fn service_guids_are_the_bluetooth_uuids() {
        assert_eq!(format!("{A2DP_SINK:?}").to_uppercase(), "0000110B-0000-1000-8000-00805F9B34FB");
        assert_eq!(format!("{HANDS_FREE:?}").to_uppercase(), "0000111E-0000-1000-8000-00805F9B34FB");
    }

    #[test]
    fn arrange_connected_first_then_name_case_insensitive() {
        let out = arrange(vec![
            dev("1", "zeta", false),
            dev("2", "Alpha", false),
            dev("3", "beta", true),
            dev("4", "Aardvark", true),
            dev("5", "alpha", false),
        ]);
        let ids: Vec<&str> = out.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, ["4", "3", "2", "5", "1"]);
        assert!(arrange(Vec::new()).is_empty());
    }

    /// Read-only: starts the watcher and reads what Windows already knows; no radio change,
    /// no connect or disconnect.
    #[test]
    #[ignore = "reads the real Bluetooth state; run manually"]
    fn real_snapshot_returns_without_panic() {
        if let Ok(b) = Bluetooth::open(HWND::default(), 0) {
            std::thread::sleep(std::time::Duration::from_secs(3));
            let _ = b.drain_changes();
            let s = b.snapshot();
            assert!(s.devices.iter().all(|d| d.battery.is_none_or(|p| p <= 100)), "{s:?}");
            // An unknown id is handed off, never acted upon.
            assert_eq!(b.set_connected("no-such-device", true), Err(BtError::HandOff));
        }
    }
}
