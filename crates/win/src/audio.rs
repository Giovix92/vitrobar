//! Output volume, mute, the default playback device and each app's own volume (spec §5.5,
//! the quick panel's volume slider and output page with its volume mixer).
//!
//! Event-driven: `IMMNotificationClient` (devices added / removed / default changed) and
//! `IAudioEndpointVolumeCallback` (volume / mute changed, from any source),
//! `IAudioSessionNotification` (an app started playing) and `IAudioSessionEvents` (an app's
//! volume, mute or state changed) run on COM threads
//! and only queue a [`Change`] and post a payload-free wake-up (see `event_queue`); the UI
//! thread calls [`Audio::drain_changes`] when the message arrives and then reads
//! [`Audio::snapshot`]. Nothing polls.
//!
//! The COM work sits behind [`AudioBackend`] so the pure parts (clamping, ordering) and the
//! panel logic are tested with a fake that never touches the real mixer.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Arc;

use windows::core::{
    implement, interface, Interface, Ref, Result, BOOL, GUID, HRESULT, PCWSTR, PWSTR,
};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::Foundation::{HWND, LPARAM, PROPERTYKEY, WPARAM};
use windows::Win32::Media::Audio::Endpoints::{
    IAudioEndpointVolume, IAudioEndpointVolumeCallback, IAudioEndpointVolumeCallback_Impl,
};
use windows::Win32::Media::Audio::{
    eConsole, eMultimedia, eRender, AudioSessionDisconnectReason, AudioSessionState,
    AudioSessionStateExpired, EDataFlow, ERole, IAudioSessionControl, IAudioSessionControl2,
    IAudioSessionEvents, IAudioSessionEvents_Impl, IAudioSessionManager2,
    IAudioSessionNotification, IAudioSessionNotification_Impl, IMMDevice, IMMDeviceEnumerator,
    IMMNotificationClient, IMMNotificationClient_Impl, ISimpleAudioVolume, MMDeviceEnumerator,
    AUDIO_VOLUME_NOTIFICATION_DATA, DEVICE_STATE, DEVICE_STATE_ACTIVE,
};
use windows::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
};
use windows::Win32::System::Com::StructuredStorage::{PropVariantToStringAlloc, PROPVARIANT};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_APARTMENTTHREADED,
    STGM_READ,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::event_queue::WakeQueue;

/// One active playback device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputDevice {
    pub id: String,
    pub name: String,
}

/// One app's own volume on the default playback device (its sessions grouped, as the
/// Windows volume mixer shows them).
#[derive(Debug, Clone, PartialEq)]
pub struct AppVolume {
    /// Identifies the app across snapshots: its executable's path (lower case).
    pub key: String,
    /// The app's name; empty for Windows' system sounds (the UI names them).
    pub name: String,
    /// Windows' "System sounds".
    pub system: bool,
    /// 0.0..=1.0.
    pub volume: f32,
    pub muted: bool,
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
    /// Apps with audio on the default device: by name, the system sounds last.
    pub apps: Vec<AppVolume>,
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
    /// Apps with audio on the default device, in any order.
    fn apps(&self) -> Vec<AppVolume> {
        Vec::new()
    }
    fn set_app_volume(&self, _key: &str, _v: f32) {}
    fn set_app_muted(&self, _key: &str, _m: bool) {}
}

/// Apps by name (case-insensitive), the system sounds last.
pub fn sort_apps(apps: &mut [AppVolume]) {
    apps.sort_by_cached_key(|a| (a.system, a.name.to_lowercase()));
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
    /// An app's volume or mute changed.
    AppVolume,
    /// An app started or stopped having audio.
    Apps,
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
        let mut apps = self.backend.apps();
        for a in &mut apps {
            a.volume = clamp_volume(a.volume);
        }
        sort_apps(&mut apps);
        AudioSnapshot {
            volume: clamp_volume(self.backend.volume()),
            muted: self.backend.muted(),
            devices,
            default_id,
            apps,
        }
    }

    /// Sets the volume of every session of the app `key` (see [`AppVolume::key`]).
    pub fn set_app_volume(&self, key: &str, v: f32) {
        self.backend.set_app_volume(key, clamp_volume(v));
    }

    pub fn set_app_muted(&self, key: &str, muted: bool) {
        self.backend.set_app_muted(key, muted);
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

#[implement(IAudioSessionNotification)]
struct SessionCreated(Arc<Waker>);

impl IAudioSessionNotification_Impl for SessionCreated_Impl {
    fn OnSessionCreated(&self, _session: Ref<IAudioSessionControl>) -> Result<()> {
        self.0.wake(Change::Apps);
        Ok(())
    }
}

/// One object registered on every session.
#[implement(IAudioSessionEvents)]
struct SessionEvents(Arc<Waker>);

impl IAudioSessionEvents_Impl for SessionEvents_Impl {
    fn OnDisplayNameChanged(&self, _name: &PCWSTR, _ctx: *const GUID) -> Result<()> {
        self.0.wake(Change::Apps);
        Ok(())
    }
    fn OnIconPathChanged(&self, _path: &PCWSTR, _ctx: *const GUID) -> Result<()> {
        Ok(())
    }
    fn OnSimpleVolumeChanged(&self, _v: f32, _mute: BOOL, _ctx: *const GUID) -> Result<()> {
        self.0.wake(Change::AppVolume);
        Ok(())
    }
    fn OnChannelVolumeChanged(
        &self,
        _count: u32,
        _volumes: *const f32,
        _changed: u32,
        _ctx: *const GUID,
    ) -> Result<()> {
        Ok(())
    }
    fn OnGroupingParamChanged(&self, _param: *const GUID, _ctx: *const GUID) -> Result<()> {
        Ok(())
    }
    fn OnStateChanged(&self, _state: AudioSessionState) -> Result<()> {
        self.0.wake(Change::Apps);
        Ok(())
    }
    fn OnSessionDisconnected(&self, _why: AudioSessionDisconnectReason) -> Result<()> {
        self.0.wake(Change::Apps);
        Ok(())
    }
}

/// The default device's sessions, with our callbacks registered on the manager and on each.
struct Sessions {
    manager: IAudioSessionManager2,
    controls: Vec<IAudioSessionControl>,
}

/// The executable path of process `pid`.
fn exe_path(pid: u32) -> Option<String> {
    // SAFETY: query-only open; the handle is closed below.
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buf = vec![0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: `buf` holds `len` u16; `len` comes back as the written length.
    let ok = unsafe {
        QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len)
    }
    .is_ok();
    // SAFETY: the handle came from OpenProcess.
    let _ = unsafe { CloseHandle(h) };
    ok.then(|| String::from_utf16_lossy(&buf[..len as usize]))
}

/// The "File description" of an executable (what Task Manager and the mixer show).
fn file_description(path: &str) -> Option<String> {
    let wide: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
    // SAFETY: version-info calls on a NUL-terminated path into a buffer of the reported
    // size; the pointers VerQueryValueW returns point into that buffer.
    unsafe {
        let n = GetFileVersionInfoSizeW(PCWSTR(wide.as_ptr()), None);
        if n == 0 {
            return None;
        }
        let mut data = vec![0u8; n as usize];
        GetFileVersionInfoW(PCWSTR(wide.as_ptr()), None, n, data.as_mut_ptr().cast()).ok()?;
        let mut p: *mut c_void = std::ptr::null_mut();
        let mut len = 0u32;
        let langs = if VerQueryValueW(
            data.as_ptr().cast(),
            windows::core::w!("\\VarFileInfo\\Translation"),
            &mut p,
            &mut len,
        )
        .as_bool()
            && len >= 4
        {
            let pairs = std::slice::from_raw_parts(p as *const u16, (len / 2) as usize);
            pairs
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| format!("{:04x}{:04x}", c[0], c[1]))
                .collect()
        } else {
            vec!["040904b0".to_string()]
        };
        for l in langs {
            let key: Vec<u16> = format!("\\StringFileInfo\\{l}\\FileDescription")
                .encode_utf16()
                .chain(Some(0))
                .collect();
            if VerQueryValueW(data.as_ptr().cast(), PCWSTR(key.as_ptr()), &mut p, &mut len)
                .as_bool()
                && len > 1
            {
                let s = std::slice::from_raw_parts(p as *const u16, len as usize);
                let s = String::from_utf16_lossy(s);
                let s = s.trim_end_matches('\0').trim();
                if !s.is_empty() {
                    return Some(s.to_string());
                }
            }
        }
        None
    }
}

/// "spotify" from "C:\\…\\Spotify.exe" when the file has no description.
fn exe_stem(path: &str) -> String {
    let file = path.rsplit(['\\', '/']).next().unwrap_or(path);
    file.strip_suffix(".exe")
        .or_else(|| file.strip_suffix(".EXE"))
        .unwrap_or(file)
        .to_string()
}

/// The name a session's own display name gives, when it is a plain name (an
/// `@dll,-id` resource reference is not).
fn plain_display_name(c: &IAudioSessionControl) -> Option<String> {
    // SAFETY: GetDisplayName returns a CoTaskMem string we free after copying.
    let name = unsafe {
        let p = c.GetDisplayName().ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const c_void));
        s
    }?;
    let name = name.trim().to_string();
    (!name.is_empty() && !name.starts_with('@')).then_some(name)
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
    session_client: IAudioSessionNotification,
    session_events: IAudioSessionEvents,
    sessions: RefCell<Option<Sessions>>,
    /// App names by key: reading version info once per app is enough.
    names: RefCell<HashMap<String, String>>,
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
        let session_client: IAudioSessionNotification = SessionCreated(waker.clone()).into();
        let session_events: IAudioSessionEvents = SessionEvents(waker.clone()).into();
        // SAFETY: the client object outlives the registration (unregistered in Drop).
        unsafe { enumerator.RegisterEndpointNotificationCallback(&device_client) }?;
        let me = Self {
            enumerator,
            policy,
            waker,
            device_client,
            volume_client,
            endpoint: RefCell::new(None),
            session_client,
            session_events,
            sessions: RefCell::new(None),
            names: RefCell::new(HashMap::new()),
        };
        me.attach_volume_callback();
        me.attach_sessions();
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

    /// (Re-)reads the default device's sessions and registers the session callbacks on its
    /// manager and on each session (a new or expired session re-runs this).
    fn attach_sessions(&self) {
        self.detach_sessions();
        let Some(dev) = self.default_device() else {
            return;
        };
        // SAFETY: activation of a standard interface on a live endpoint; the callback
        // objects live as long as this backend and are unregistered in `detach_sessions`.
        unsafe {
            let Ok(manager) = dev.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else {
                return;
            };
            let _ = manager.RegisterSessionNotification(&self.session_client);
            // Windows only starts the "session created" notifications once the sessions
            // have been enumerated.
            let mut controls = Vec::new();
            if let Ok(list) = manager.GetSessionEnumerator() {
                for i in 0..list.GetCount().unwrap_or(0) {
                    if let Ok(c) = list.GetSession(i) {
                        if c.RegisterAudioSessionNotification(&self.session_events)
                            .is_ok()
                        {
                            controls.push(c);
                        }
                    }
                }
            }
            *self.sessions.borrow_mut() = Some(Sessions { manager, controls });
        }
    }

    fn detach_sessions(&self) {
        if let Some(s) = self.sessions.borrow_mut().take() {
            // SAFETY: undoes the registrations made in `attach_sessions`.
            unsafe {
                for c in &s.controls {
                    let _ = c.UnregisterAudioSessionNotification(&self.session_events);
                }
                let _ = s
                    .manager
                    .UnregisterSessionNotification(&self.session_client);
            }
        }
    }

    /// Every live session of the default device with the key of its app.
    fn app_sessions(&self) -> Vec<(String, bool, IAudioSessionControl)> {
        let sessions = self.sessions.borrow();
        let Some(s) = sessions.as_ref() else {
            return Vec::new();
        };
        s.controls
            .iter()
            .filter_map(|c| {
                // SAFETY: plain getters on a live session.
                unsafe {
                    if c.GetState().ok()? == AudioSessionStateExpired {
                        return None;
                    }
                    let c2: IAudioSessionControl2 = c.cast().ok()?;
                    if c2.IsSystemSoundsSession() == windows::Win32::Foundation::S_OK {
                        return Some(("#system".to_string(), true, c.clone()));
                    }
                    let pid = c2.GetProcessId().ok()?;
                    let key = exe_path(pid)
                        .map(|p| p.to_lowercase())
                        .unwrap_or_else(|| format!("#pid{pid}"));
                    Some((key, false, c.clone()))
                }
            })
            .collect()
    }

    /// The name of the app `key` (cached).
    fn app_name(&self, key: &str, c: &IAudioSessionControl) -> String {
        if let Some(n) = self.names.borrow().get(key) {
            return n.clone();
        }
        let name = plain_display_name(c)
            .or_else(|| file_description(key))
            .unwrap_or_else(|| exe_stem(key));
        self.names
            .borrow_mut()
            .insert(key.to_string(), name.clone());
        name
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
        if changes.contains(&Change::DefaultDevice) || changes.contains(&Change::Apps) {
            self.attach_sessions();
        }
    }

    fn apps(&self) -> Vec<AppVolume> {
        let mut out: Vec<AppVolume> = Vec::new();
        for (key, system, c) in self.app_sessions() {
            if out.iter().any(|a| a.key == key) {
                continue;
            }
            let Ok(v) = c.cast::<ISimpleAudioVolume>() else {
                continue;
            };
            // SAFETY: plain getters.
            let (volume, muted) = unsafe {
                (
                    v.GetMasterVolume().unwrap_or(1.0),
                    v.GetMute().map(|b| b.as_bool()).unwrap_or(false),
                )
            };
            let name = if system {
                String::new()
            } else {
                self.app_name(&key, &c)
            };
            out.push(AppVolume {
                key,
                name,
                system,
                volume,
                muted,
            });
        }
        out
    }

    fn set_app_volume(&self, key: &str, v: f32) {
        for (k, _, c) in self.app_sessions() {
            if k == key {
                if let Ok(sv) = c.cast::<ISimpleAudioVolume>() {
                    // SAFETY: a null event context, as for the master volume.
                    let _ = unsafe { sv.SetMasterVolume(v, std::ptr::null()) };
                }
            }
        }
    }

    fn set_app_muted(&self, key: &str, m: bool) {
        for (k, _, c) in self.app_sessions() {
            if k == key {
                if let Ok(sv) = c.cast::<ISimpleAudioVolume>() {
                    // SAFETY: as in `set_app_volume`.
                    let _ = unsafe { sv.SetMute(m, std::ptr::null()) };
                }
            }
        }
    }
}

impl Drop for ComBackend {
    fn drop(&mut self) {
        self.detach_volume_callback();
        self.detach_sessions();
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
    fn apps_by_name_with_system_sounds_last() {
        let app = |name: &str, system: bool| AppVolume {
            key: name.to_lowercase(),
            name: name.into(),
            system,
            volume: 1.0,
            muted: false,
        };
        let mut v = vec![app("", true), app("spotify", false), app("Discord", false)];
        sort_apps(&mut v);
        let names: Vec<_> = v.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["Discord", "spotify", ""]);
    }

    #[test]
    fn exe_stem_drops_folder_and_extension() {
        assert_eq!(
            exe_stem("c:\\program files\\spotify\\spotify.exe"),
            "spotify"
        );
        assert_eq!(exe_stem("C:/x/App.EXE"), "App");
        assert_eq!(exe_stem("plain"), "plain");
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
        let snap = audio.snapshot();
        let v = snap.volume;
        assert!((0.0..=1.0).contains(&v), "{v}");
        for a in &snap.apps {
            eprintln!(
                "app {:?} {:?} {:.2} muted={}",
                a.key, a.name, a.volume, a.muted
            );
        }
    }
}
