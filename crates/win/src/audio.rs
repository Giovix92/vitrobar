//! Output volume, mute and the default playback device (spec §5.5, the quick panel's volume
//! slider and output page).
//!
//! Event-driven: `IMMNotificationClient` (devices added / removed / default changed) and
//! `IAudioEndpointVolumeCallback` (volume / mute changed, from any source) run on COM threads
//! and only queue a [`Change`] and post a payload-free wake-up (see `event_queue`); the UI
//! thread calls [`Audio::drain_changes`] when the message arrives and then reads
//! [`Audio::snapshot`]. Nothing polls.
//!
//! The COM work sits behind [`AudioBackend`] so the pure parts (clamping, ordering) and the
//! panel logic are tested with a fake that never touches the real mixer.

use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::Arc;

use windows::core::{implement, interface, Result, GUID, HRESULT, PCWSTR, PWSTR};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::{HWND, LPARAM, PROPERTYKEY, WPARAM};
use windows::Win32::Media::Audio::Endpoints::{
    IAudioEndpointVolume, IAudioEndpointVolumeCallback, IAudioEndpointVolumeCallback_Impl,
};
use windows::Win32::Media::Audio::{
    eConsole, eMultimedia, eRender, EDataFlow, ERole, IMMDevice, IMMDeviceEnumerator,
    IMMNotificationClient, IMMNotificationClient_Impl, MMDeviceEnumerator,
    AUDIO_VOLUME_NOTIFICATION_DATA, DEVICE_STATE, DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::StructuredStorage::{PropVariantToStringAlloc, PROPVARIANT};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_APARTMENTTHREADED,
    STGM_READ,
};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::event_queue::WakeQueue;

/// One active playback device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputDevice {
    pub id: String,
    pub name: String,
}

/// Everything the quick panel shows about audio.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioSnapshot {
    /// Master volume of the default playback device, 0.0..=1.0 (0 with no device).
    pub volume: f32,
    pub muted: bool,
    /// Active playback devices: the default first, then by name.
    pub devices: Vec<OutputDevice>,
    pub default_id: Option<String>,
}

/// Clamps a requested volume to 0.0..=1.0 (NaN counts as 0).
pub fn clamp_volume(v: f32) -> f32 {
    if v.is_nan() {
        0.0
    } else {
        v.clamp(0.0, 1.0)
    }
}

/// The mixer, abstracted so the logic above it is testable without touching the real one.
pub trait AudioBackend {
    fn volume(&self) -> f32;
    fn muted(&self) -> bool;
    /// Active playback devices, in any order.
    fn devices(&self) -> Vec<OutputDevice>;
    fn default_id(&self) -> Option<String>;
    fn set_volume(&self, v: f32);
    fn set_muted(&self, m: bool);
    /// Makes `id` the default for every role; `false` when it could not.
    fn set_default(&self, id: &str) -> bool;
    fn can_switch(&self) -> bool;
    /// Called with the drained changes: the real backend re-attaches its volume callback
    /// when the default device changed. Fakes ignore it.
    fn on_changes(&self, _changes: &[Change]) {}
}

/// What a COM callback reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// Volume or mute changed.
    Volume,
    /// A device was added, removed or changed state.
    Devices,
    /// The default playback device changed.
    DefaultDevice,
}

/// Audio state for the quick panel. Create it when the panel opens, drop it when it closes
/// (P4); dropping unregisters the COM callbacks.
pub struct Audio {
    backend: Box<dyn AudioBackend>,
    waker: Option<Arc<Waker>>,
}

impl Audio {
    /// Opens the real mixer and registers the notification callbacks. `notify` receives a
    /// payload-free `wake_msg` window message whenever something changed (a null `notify`
    /// posts nothing); on that message call [`Audio::drain_changes`].
    pub fn open(notify: HWND, wake_msg: u32) -> Result<Self> {
        let waker = Arc::new(Waker {
            queue: WakeQueue::new(),
            notify: notify.0 as isize,
            msg: wake_msg,
        });
        let backend = ComBackend::new(waker.clone())?;
        Ok(Self {
            backend: Box::new(backend),
            waker: Some(waker),
        })
    }

    /// An `Audio` over any backend (tests).
    pub fn with_backend(backend: Box<dyn AudioBackend>) -> Self {
        Self {
            backend,
            waker: None,
        }
    }

    /// Takes the changes queued by the callbacks and re-attaches the volume callback if the
    /// default device changed. Call when the wake-up message arrives, whoever sent it; an
    /// empty result means a forged or already-handled wake-up.
    pub fn drain_changes(&self) -> Vec<Change> {
        let changes = self
            .waker
            .as_ref()
            .map(|w| w.queue.drain())
            .unwrap_or_default();
        if !changes.is_empty() {
            self.backend.on_changes(&changes);
        }
        changes
    }

    pub fn snapshot(&self) -> AudioSnapshot {
        let default_id = self.backend.default_id();
        let mut devices = self.backend.devices();
        devices.sort_by_cached_key(|d| d.name.to_lowercase());
        if let Some(i) = default_id
            .as_deref()
            .and_then(|id| devices.iter().position(|d| d.id == id))
        {
            let d = devices.remove(i);
            devices.insert(0, d);
        }
        AudioSnapshot {
            volume: clamp_volume(self.backend.volume()),
            muted: self.backend.muted(),
            devices,
            default_id,
        }
    }

    pub fn set_volume(&self, v: f32) {
        self.backend.set_volume(clamp_volume(v));
    }

    pub fn set_muted(&self, muted: bool) {
        self.backend.set_muted(muted);
    }

    /// Whether the default device can be changed (the undocumented `IPolicyConfig` exists);
    /// when `false` the UI hides switching.
    pub fn can_switch(&self) -> bool {
        self.backend.can_switch()
    }

    /// Makes `id` the default playback device (console and multimedia roles).
    pub fn set_default(&self, id: &str) -> bool {
        self.can_switch() && self.backend.set_default(id)
    }
}

/// Shared by the COM callbacks (any thread): queue + payload-free wake-up.
struct Waker {
    queue: WakeQueue<Change>,
    notify: isize,
    msg: u32,
}

impl Waker {
    fn wake(&self, change: Change) {
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

// ---- undocumented IPolicyConfig (Windows 7+), declared by hand ----------------------------

/// `IPolicyConfig` (the Windows 7+ layout, as used by EarTrumpet / AudioDeviceCmdlets).
/// Only `SetDefaultEndpoint` is called; the methods before it are placeholders that keep the
/// vtable order (ten methods after `IUnknown`, then `SetDefaultEndpoint`).
#[interface("f8679f50-850a-41cf-9c72-430f290290c8")]
unsafe trait IPolicyConfig: windows::core::IUnknown {
    unsafe fn get_mix_format(&self, a: PCWSTR, b: *mut *mut c_void) -> HRESULT;
    unsafe fn get_device_format(&self, a: PCWSTR, b: i32, c: *mut *mut c_void) -> HRESULT;
    unsafe fn reset_device_format(&self, a: PCWSTR) -> HRESULT;
    unsafe fn set_device_format(&self, a: PCWSTR, b: *mut c_void, c: *mut c_void) -> HRESULT;
    unsafe fn get_processing_period(&self, a: PCWSTR, b: i32, c: *mut i64, d: *mut i64) -> HRESULT;
    unsafe fn set_processing_period(&self, a: PCWSTR, b: *mut i64) -> HRESULT;
    unsafe fn get_share_mode(&self, a: PCWSTR, b: *mut c_void) -> HRESULT;
    unsafe fn set_share_mode(&self, a: PCWSTR, b: *mut c_void) -> HRESULT;
    unsafe fn get_property_value(
        &self,
        a: PCWSTR,
        b: i32,
        c: *const PROPERTYKEY,
        d: *mut PROPVARIANT,
    ) -> HRESULT;
    unsafe fn set_property_value(
        &self,
        a: PCWSTR,
        b: i32,
        c: *const PROPERTYKEY,
        d: *mut PROPVARIANT,
    ) -> HRESULT;
    unsafe fn set_default_endpoint(&self, device_id: PCWSTR, role: ERole) -> HRESULT;
}

/// CLSID_PolicyConfigClient.
const CLSID_POLICY_CONFIG: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);

// ---- COM callbacks -------------------------------------------------------------------------

#[implement(IMMNotificationClient)]
struct DeviceEvents(Arc<Waker>);

impl IMMNotificationClient_Impl for DeviceEvents_Impl {
    fn OnDeviceStateChanged(&self, _id: &PCWSTR, _state: DEVICE_STATE) -> Result<()> {
        self.0.wake(Change::Devices);
        Ok(())
    }
    fn OnDeviceAdded(&self, _id: &PCWSTR) -> Result<()> {
        self.0.wake(Change::Devices);
        Ok(())
    }
    fn OnDeviceRemoved(&self, _id: &PCWSTR) -> Result<()> {
        self.0.wake(Change::Devices);
        Ok(())
    }
    fn OnDefaultDeviceChanged(&self, flow: EDataFlow, role: ERole, _id: &PCWSTR) -> Result<()> {
        if flow == eRender && (role == eConsole || role == eMultimedia) {
            self.0.wake(Change::DefaultDevice);
        }
        Ok(())
    }
    fn OnPropertyValueChanged(&self, _id: &PCWSTR, _key: &PROPERTYKEY) -> Result<()> {
        // Fires for every property of every endpoint; a renamed device shows its new name
        // with the next real change, so this does not wake the UI.
        Ok(())
    }
}

#[implement(IAudioEndpointVolumeCallback)]
struct VolumeEvents(Arc<Waker>);

impl IAudioEndpointVolumeCallback_Impl for VolumeEvents_Impl {
    fn OnNotify(&self, _data: *mut AUDIO_VOLUME_NOTIFICATION_DATA) -> Result<()> {
        self.0.wake(Change::Volume);
        Ok(())
    }
}

// ---- the real mixer -------------------------------------------------------------------------

struct ComBackend {
    enumerator: IMMDeviceEnumerator,
    policy: Option<IPolicyConfig>,
    waker: Arc<Waker>,
    device_client: IMMNotificationClient,
    volume_client: IAudioEndpointVolumeCallback,
    /// The default endpoint's volume interface with `volume_client` registered on it.
    endpoint: RefCell<Option<IAudioEndpointVolume>>,
}

impl ComBackend {
    fn new(waker: Arc<Waker>) -> Result<Self> {
        // SAFETY: COM init on the calling (UI) thread; S_FALSE / an already-set apartment
        // are fine, the instances are created either way.
        let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        // SAFETY: plain activation of a system class.
        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }?;
        // SAFETY: as above; failure just means switching is unavailable.
        let policy: Option<IPolicyConfig> =
            unsafe { CoCreateInstance(&CLSID_POLICY_CONFIG, None, CLSCTX_ALL) }.ok();
        let device_client: IMMNotificationClient = DeviceEvents(waker.clone()).into();
        let volume_client: IAudioEndpointVolumeCallback = VolumeEvents(waker.clone()).into();
        // SAFETY: the client object outlives the registration (unregistered in Drop).
        unsafe { enumerator.RegisterEndpointNotificationCallback(&device_client) }?;
        let me = Self {
            enumerator,
            policy,
            waker,
            device_client,
            volume_client,
            endpoint: RefCell::new(None),
        };
        me.attach_volume_callback();
        Ok(me)
    }

    fn default_device(&self) -> Option<IMMDevice> {
        // SAFETY: plain query; Err = no playback device.
        unsafe { self.enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }.ok()
    }

    /// (Re-)registers the volume callback on the current default endpoint.
    fn attach_volume_callback(&self) {
        self.detach_volume_callback();
        if let Some(ep) = self.current_volume() {
            // SAFETY: the callback object lives as long as this backend.
            if unsafe { ep.RegisterControlChangeNotify(&self.volume_client) }.is_ok() {
                *self.endpoint.borrow_mut() = Some(ep);
            }
        }
    }

    fn detach_volume_callback(&self) {
        if let Some(ep) = self.endpoint.borrow_mut().take() {
            // SAFETY: undoes the registration made in `attach_volume_callback`.
            let _ = unsafe { ep.UnregisterControlChangeNotify(&self.volume_client) };
        }
    }

    /// The volume interface of the current default endpoint.
    fn current_volume(&self) -> Option<IAudioEndpointVolume> {
        let dev = self.default_device()?;
        // SAFETY: activation of a standard interface on a live endpoint.
        unsafe { dev.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None) }.ok()
    }
}

fn device_id(dev: &IMMDevice) -> Option<String> {
    // SAFETY: GetId returns a CoTaskMem string we free after copying.
    unsafe {
        let p: PWSTR = dev.GetId().ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const c_void));
        s
    }
}

fn device_name(dev: &IMMDevice) -> Option<String> {
    // SAFETY: read-only property query; the string is freed after copying.
    unsafe {
        let store = dev.OpenPropertyStore(STGM_READ).ok()?;
        let var = store.GetValue(&PKEY_Device_FriendlyName).ok()?;
        let p = PropVariantToStringAlloc(&var).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const c_void));
        s
    }
}

impl AudioBackend for ComBackend {
    fn volume(&self) -> f32 {
        self.current_volume()
            // SAFETY: plain getter.
            .and_then(|ep| unsafe { ep.GetMasterVolumeLevelScalar() }.ok())
            .unwrap_or(0.0)
    }

    fn muted(&self) -> bool {
        self.current_volume()
            // SAFETY: plain getter.
            .and_then(|ep| unsafe { ep.GetMute() }.ok())
            .map(|b| b.as_bool())
            .unwrap_or(false)
    }

    fn devices(&self) -> Vec<OutputDevice> {
        // SAFETY: enumeration of active render endpoints.
        let Ok(list) = (unsafe {
            self.enumerator
                .EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)
        }) else {
            return Vec::new();
        };
        // SAFETY: plain getter on the collection.
        let n = unsafe { list.GetCount() }.unwrap_or(0);
        (0..n)
            .filter_map(|i| {
                // SAFETY: i < count.
                let dev = unsafe { list.Item(i) }.ok()?;
                let id = device_id(&dev)?;
                let name = device_name(&dev).unwrap_or_else(|| id.clone());
                Some(OutputDevice { id, name })
            })
            .collect()
    }

    fn default_id(&self) -> Option<String> {
        self.default_device().as_ref().and_then(device_id)
    }

    fn set_volume(&self, v: f32) {
        if let Some(ep) = self.current_volume() {
            // SAFETY: a null event context; our own change just re-renders the panel.
            let _ = unsafe { ep.SetMasterVolumeLevelScalar(v, std::ptr::null()) };
        }
    }

    fn set_muted(&self, m: bool) {
        if let Some(ep) = self.current_volume() {
            // SAFETY: as in `set_volume`.
            let _ = unsafe { ep.SetMute(m, std::ptr::null()) };
        }
    }

    fn set_default(&self, id: &str) -> bool {
        let Some(policy) = &self.policy else {
            return false;
        };
        let wide: Vec<u16> = id.encode_utf16().chain(Some(0)).collect();
        [eConsole, eMultimedia].into_iter().all(|role| {
            // SAFETY: `wide` is NUL-terminated and outlives the call.
            unsafe { policy.set_default_endpoint(PCWSTR(wide.as_ptr()), role) }.is_ok()
        })
    }

    fn can_switch(&self) -> bool {
        self.policy.is_some()
    }

    fn on_changes(&self, changes: &[Change]) {
        if changes.contains(&Change::DefaultDevice) {
            self.attach_volume_callback();
        }
    }
}

impl Drop for ComBackend {
    fn drop(&mut self) {
        self.detach_volume_callback();
        // SAFETY: undoes the registration made in `new`.
        let _ = unsafe {
            self.enumerator
                .UnregisterEndpointNotificationCallback(&self.device_client)
        };
        self.waker.queue.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct Fake {
        volume: RefCell<f32>,
        muted: RefCell<bool>,
        devices: Vec<OutputDevice>,
        default: RefCell<Option<String>>,
        switch: bool,
    }

    fn dev(id: &str, name: &str) -> OutputDevice {
        OutputDevice {
            id: id.into(),
            name: name.into(),
        }
    }

    impl AudioBackend for Fake {
        fn volume(&self) -> f32 {
            *self.volume.borrow()
        }
        fn muted(&self) -> bool {
            *self.muted.borrow()
        }
        fn devices(&self) -> Vec<OutputDevice> {
            self.devices.clone()
        }
        fn default_id(&self) -> Option<String> {
            self.default.borrow().clone()
        }
        fn set_volume(&self, v: f32) {
            *self.volume.borrow_mut() = v;
        }
        fn set_muted(&self, m: bool) {
            *self.muted.borrow_mut() = m;
        }
        fn set_default(&self, id: &str) -> bool {
            *self.default.borrow_mut() = Some(id.into());
            true
        }
        fn can_switch(&self) -> bool {
            self.switch
        }
    }

    fn fake() -> Fake {
        Fake {
            devices: vec![
                dev("c", "Speakers"),
                dev("a", "headphones"),
                dev("b", "HDMI"),
            ],
            default: RefCell::new(Some("c".into())),
            switch: true,
            ..Default::default()
        }
    }

    #[test]
    fn clamp_handles_range_and_nan() {
        assert_eq!(clamp_volume(-0.5), 0.0);
        assert_eq!(clamp_volume(1.7), 1.0);
        assert_eq!(clamp_volume(0.25), 0.25);
        assert_eq!(clamp_volume(f32::NAN), 0.0);
        assert_eq!(clamp_volume(f32::INFINITY), 1.0);
    }

    #[test]
    fn set_volume_is_clamped_before_reaching_the_backend() {
        let audio = Audio::with_backend(Box::new(fake()));
        audio.set_volume(3.0);
        assert_eq!(audio.snapshot().volume, 1.0);
        audio.set_volume(-1.0);
        assert_eq!(audio.snapshot().volume, 0.0);
    }

    #[test]
    fn mute_round_trips() {
        let audio = Audio::with_backend(Box::new(fake()));
        audio.set_muted(true);
        assert!(audio.snapshot().muted);
        audio.set_muted(false);
        assert!(!audio.snapshot().muted);
    }

    #[test]
    fn devices_sorted_by_name_with_default_first() {
        let snap = Audio::with_backend(Box::new(fake())).snapshot();
        let names: Vec<_> = snap.devices.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["Speakers", "HDMI", "headphones"]);
        assert_eq!(snap.default_id.as_deref(), Some("c"));
    }

    #[test]
    fn no_default_device_sorts_plainly() {
        let mut f = fake();
        f.default = RefCell::new(None);
        let snap = Audio::with_backend(Box::new(f)).snapshot();
        let names: Vec<_> = snap.devices.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["HDMI", "headphones", "Speakers"]);
    }

    #[test]
    fn set_default_switches_only_when_allowed() {
        let audio = Audio::with_backend(Box::new(fake()));
        assert!(audio.can_switch());
        assert!(audio.set_default("a"));
        assert_eq!(audio.snapshot().default_id.as_deref(), Some("a"));
        assert_eq!(audio.snapshot().devices[0].id, "a", "the new default leads");

        let mut f = fake();
        f.switch = false;
        let audio = Audio::with_backend(Box::new(f));
        assert!(!audio.can_switch());
        assert!(!audio.set_default("a"));
        assert_eq!(audio.snapshot().default_id.as_deref(), Some("c"));
        assert_eq!(audio.snapshot().devices.len(), 3, "still listed");
    }

    /// Read-only: touches no volume, mute or device setting.
    #[test]
    #[ignore = "reads the real audio endpoint; run manually"]
    fn real_snapshot_is_in_range() {
        // A null notify window: no wake-ups are posted.
        let audio = Audio::open(HWND::default(), 0).expect("audio");
        let v = audio.snapshot().volume;
        assert!((0.0..=1.0).contains(&v), "{v}");
    }
}
