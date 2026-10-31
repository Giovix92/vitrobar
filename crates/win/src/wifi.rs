//! Wi-Fi for the quick panel (spec §5.5): radio, network list, connect (WPA2/WPA3-personal and
//! open networks), disconnect.
//!
//! The WLAN API is event driven: [`Wifi::open`] registers one `WlanRegisterNotification`
//! (ACM + MSM) whose callback runs on a system thread and only queues a [`Change`] and posts a
//! payload-free wake-up (see `event_queue`); nothing polls. The UI calls
//! [`Wifi::drain_changes`] and then [`Wifi::snapshot`]. `WlanScan` and `WlanConnect` return
//! at once and report through that callback (scan complete, connection complete / failed), so
//! the UI thread never waits on them. The radio (Windows.Devices.Radios) is awaited on a
//! worker thread; its state changes arrive through `Radio.StateChanged`.
//!
//! Failures meant for the user are [`WifiError`] values; the UI chooses the text. Captive
//! portals are left to Windows. Connecting to an open or personal network creates a *manual*
//! (not auto-connect) user profile, so the machine never joins it behind the user's back.

use std::collections::HashSet;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::core::{Result, GUID, PCWSTR};
use windows::Devices::Radios::{Radio, RadioAccessStatus, RadioKind, RadioState};
use windows::Foundation::TypedEventHandler;
use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, ERROR_NOT_FOUND, HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::NetworkManagement::WiFi::{
    dot11_BSS_type_infrastructure, wlan_connection_mode_profile, wlan_interface_state_connected,
    wlan_intf_opcode_current_connection, WlanCloseHandle, WlanConnect, WlanDeleteProfile, WlanDisconnect,
    WlanEnumInterfaces, WlanFreeMemory, WlanGetAvailableNetworkList, WlanOpenHandle, WlanQueryInterface,
    WlanRegisterNotification, WlanScan, WlanSetProfile, DOT11_SSID, L2_NOTIFICATION_DATA,
    WLAN_AVAILABLE_NETWORK, WLAN_AVAILABLE_NETWORK_HAS_PROFILE, WLAN_AVAILABLE_NETWORK_LIST, WLAN_CONNECTION_ATTRIBUTES,
    WLAN_CONNECTION_PARAMETERS, WLAN_INTERFACE_INFO_LIST, WLAN_NOTIFICATION_SOURCES,
    WLAN_NOTIFICATION_SOURCE_ACM, WLAN_NOTIFICATION_SOURCE_MSM,
};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::brightness::ComGuard;
use crate::event_queue::WakeQueue;

/// How a network is joined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetKind {
    Open,
    /// WPA2/WPA3-personal: a passphrase is enough.
    Psk,
    /// Anything needing more than a passphrase (802.1X, WEP, OWE): handed to Windows Settings.
    Enterprise,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Network {
    pub ssid: String,
    /// 0..=100.
    pub signal: u8,
    pub secured: bool,
    pub kind: NetKind,
    /// Windows already has a profile for it (connecting needs no password).
    pub known: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WifiSnapshot {
    /// `None` when there is no Wi-Fi radio object to ask.
    pub radio_on: Option<bool>,
    pub connected: Option<String>,
    pub networks: Vec<Network>,
    pub scanning: bool,
}

/// A user-facing failure; the UI picks the (localized) text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WifiError {
    /// Needs Windows' own UI (enterprise network): open `ms-settings:network-wifi`.
    HandOff,
    /// Windows refused the radio change (user or system policy).
    Denied,
    /// The passphrase is not 8..=63 printable characters.
    BadPassword,
    Failed,
}

/// Authentication of a profile built by [`profile_xml`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Auth {
    Open,
    Wpa2Psk,
    Wpa3Sae,
}

// ---- pure helpers ----------------------------------------------------------------------------

/// Escapes the five XML specials.
pub fn escape_xml(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&apos;"),
            c => o.push(c),
        }
    }
    o
}

/// The SSID's UTF-8 bytes as upper-case hex, as Windows writes `<hex>`.
pub fn hex_ssid(s: &str) -> String {
    s.bytes().map(|b| format!("{b:02X}")).collect()
}

/// A WLANProfile v1 document for `ssid`: manual connection, user supplies `password` (ignored
/// for [`Auth::Open`]). SSID and key are escaped.
pub fn profile_xml(ssid: &str, password: &str, auth: Auth) -> String {
    let (authentication, encryption) = match auth {
        Auth::Open => ("open", "none"),
        Auth::Wpa2Psk => ("WPA2PSK", "AES"),
        Auth::Wpa3Sae => ("WPA3SAE", "AES"),
    };
    let key = if auth == Auth::Open {
        String::new()
    } else {
        format!(
            "<sharedKey><keyType>passPhrase</keyType><protected>false</protected>\
             <keyMaterial>{}</keyMaterial></sharedKey>",
            escape_xml(password)
        )
    };
    format!(
        "<?xml version=\"1.0\"?>\
         <WLANProfile xmlns=\"http://www.microsoft.com/networking/WLAN/profile/v1\">\
         <name>{name}</name>\
         <SSIDConfig><SSID><hex>{hex}</hex><name>{name}</name></SSID></SSIDConfig>\
         <connectionType>ESS</connectionType><connectionMode>manual</connectionMode>\
         <MSM><security><authEncryption><authentication>{authentication}</authentication>\
         <encryption>{encryption}</encryption><useOneX>false</useOneX></authEncryption>{key}\
         </security></MSM></WLANProfile>",
        name = escape_xml(ssid),
        hex = hex_ssid(ssid),
    )
}

/// 1..=4 bars for a 0..=100 signal quality (0..=25 is one bar, 76.. is four).
pub fn signal_bars(signal: u8) -> u8 {
    match signal {
        0..=25 => 1,
        26..=50 => 2,
        51..=75 => 3,
        _ => 4,
    }
}

/// Drops hidden (empty SSID) networks, merges duplicates by SSID (strongest signal wins,
/// `known` if any was), and sorts the connected network first, then by signal, then name.
pub fn arrange(nets: Vec<Network>, connected: Option<&str>) -> Vec<Network> {
    let mut out: Vec<Network> = Vec::new();
    for n in nets.into_iter().filter(|n| !n.ssid.is_empty()) {
        match out.iter_mut().find(|o| o.ssid == n.ssid) {
            Some(o) => {
                let known = o.known || n.known;
                if n.signal > o.signal {
                    *o = n;
                }
                o.known = known;
            }
            None => out.push(n),
        }
    }
    out.sort_by(|a, b| {
        let ca = Some(a.ssid.as_str()) == connected;
        let cb = Some(b.ssid.as_str()) == connected;
        cb.cmp(&ca)
            .then(b.signal.cmp(&a.signal))
            .then(a.ssid.cmp(&b.ssid))
    });
    out
}

/// Maps a `DOT11_AUTH_ALGORITHM` value to how the network is joined and whether it is
/// WPA3-SAE. Only open and personal networks can be joined here; everything else (802.1X,
/// WEP, OWE, legacy WPA-PSK/TKIP, unknown) is [`NetKind::Enterprise`], i.e. handed to Windows.
pub fn classify_auth(auth: i32, secured: bool) -> (NetKind, bool) {
    match auth {
        1 if !secured => (NetKind::Open, false),
        7 => (NetKind::Psk, false),
        9 => (NetKind::Psk, true),
        _ => (NetKind::Enterprise, false),
    }
}

/// A WPA passphrase: 8..=63 ASCII bytes, no control characters (which XML cannot carry).
pub fn password_ok(p: &str) -> bool {
    p.is_ascii() && (8..=63).contains(&p.len()) && !p.chars().any(|c| c.is_control())
}

// ---- service ----------------------------------------------------------------------------------

/// What the callbacks / worker report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// Scan results, the connection or the interface state changed: re-read [`Wifi::snapshot`].
    Networks,
    /// The radio state changed.
    Radio,
    /// A connection attempt to this SSID failed (wrong passphrase, out of range...).
    ConnectFailed(String),
    /// A radio change failed, or a connect that used a profile Vitrobar did not create failed
    /// ([`WifiError::HandOff`]: open Windows' Wi-Fi settings).
    Error(WifiError),
}

/// A scan that never reports back stops counting as scanning after this long (see
/// [`Wifi::scan_deadline`]).
const SCAN_TIMEOUT: Duration = Duration::from_secs(12);

struct Shared {
    queue: WakeQueue<Change>,
    notify: isize,
    msg: u32,
    closed: AtomicBool,
    scanning: Mutex<Option<Instant>>,
    radio_on: Mutex<Option<bool>>,
    /// SSIDs whose scan result says WPA3-SAE.
    sae: Mutex<HashSet<String>>,
    /// Profile created by the last connect to a new network (removed again if it fails).
    created: Mutex<Option<String>>,
    /// SSID being connected through a profile that already existed (not ours: never touched).
    foreign: Mutex<Option<String>>,
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

pub struct Wifi {
    shared: Arc<Shared>,
    handle: HANDLE,
    guid: GUID,
    /// One strong count of `shared`, lent to the notification callback as its context.
    ctx: *const Shared,
    jobs: Sender<RadioJob>,
}

// ACM notification codes (WLAN_NOTIFICATION_ACM).
const ACM_SCAN_COMPLETE: u32 = 7;
const ACM_SCAN_FAIL: u32 = 8;
const ACM_CONNECTION_COMPLETE: u32 = 10;
const ACM_CONNECTION_ATTEMPT_FAIL: u32 = 11;
// MSM notification code (WLAN_NOTIFICATION_MSM) for signal-quality changes.
const MSM_SIGNAL_QUALITY_CHANGE: u32 = 8;

/// `WLAN_CONNECTION_NOTIFICATION_DATA` (wlanapi.h), the `pData` of the connection codes.
#[repr(C)]
struct ConnectionNotification {
    mode: i32,
    profile_name: [u16; 256],
    ssid: DOT11_SSID,
    bss_type: i32,
    security: i32,
    reason: u32,
}

unsafe extern "system" fn on_notification(data: *mut L2_NOTIFICATION_DATA, ctx: *mut c_void) {
    if data.is_null() || ctx.is_null() {
        return;
    }
    // SAFETY: `ctx` is the `Shared` kept alive by `Wifi` until the callback is unregistered;
    // `data` is valid for the duration of the call.
    let (shared, d) = unsafe { (&*(ctx as *const Shared), &*data) };
    if d.NotificationSource == WLAN_NOTIFICATION_SOURCE_MSM {
        // Signal-quality changes arrive about every second while connected and change nothing
        // the panel shows between scans.
        if d.NotificationCode as u32 == MSM_SIGNAL_QUALITY_CHANGE {
            return;
        }
    } else if d.NotificationSource == WLAN_NOTIFICATION_SOURCE_ACM {
        match d.NotificationCode as u32 {
            ACM_SCAN_COMPLETE | ACM_SCAN_FAIL => {
                *lock(&shared.scanning) = None;
            }
            ACM_CONNECTION_COMPLETE | ACM_CONNECTION_ATTEMPT_FAIL => {
                let failed_code = d.NotificationCode as u32 == ACM_CONNECTION_ATTEMPT_FAIL;
                if !d.pData.is_null()
                    && d.dwDataSize as usize >= std::mem::size_of::<ConnectionNotification>()
                {
                    // SAFETY: size checked above; the structure is plain data.
                    let c = unsafe { std::ptr::read_unaligned(d.pData as *const ConnectionNotification) };
                    let ssid = ssid_lossy(&c.ssid);
                    if c.reason != 0 || failed_code {
                        shared.wake(Change::ConnectFailed(ssid));
                    } else {
                        // Connected: the profile we created is the user's now, keep it.
                        let mut created = lock(&shared.created);
                        if created.as_deref() == Some(ssid.as_str()) {
                            created.take();
                        }
                        let mut foreign = lock(&shared.foreign);
                        if foreign.as_deref() == Some(ssid.as_str()) {
                            foreign.take();
                        }
                    }
                } else if failed_code {
                    // No usable payload: the empty SSID means "the attempt we started".
                    shared.wake(Change::ConnectFailed(String::new()));
                }
            }
            _ => {}
        }
    }
    shared.wake(Change::Networks);
}

impl Wifi {
    /// Opens the first Wi-Fi interface, registers for notifications and starts the radio
    /// worker. Fails (`ERROR_NOT_FOUND`) without a Wi-Fi adapter: the caller hides the tile.
    /// `notify` receives a payload-free `wake_msg` on every change; a null `notify` posts
    /// nothing. On that message call [`Wifi::drain_changes`].
    pub fn open(notify: HWND, wake_msg: u32) -> Result<Self> {
        let mut negotiated = 0u32;
        let mut handle = HANDLE::default();
        // SAFETY: plain out-parameters.
        let rc = unsafe { WlanOpenHandle(2, None, &mut negotiated, &mut handle) };
        if rc != 0 {
            return Err(windows::core::Error::from_hresult(windows::core::HRESULT::from_win32(rc)));
        }
        let close = |h: HANDLE| {
            // SAFETY: closes the handle opened above.
            let _ = unsafe { WlanCloseHandle(h, None) };
        };
        let Some(guid) = first_interface(handle) else {
            close(handle);
            return Err(ERROR_NOT_FOUND.into());
        };
        let shared = Arc::new(Shared {
            queue: WakeQueue::new(),
            notify: notify.0 as isize,
            msg: wake_msg,
            closed: AtomicBool::new(false),
            scanning: Mutex::new(None),
            radio_on: Mutex::new(None),
            sae: Mutex::new(HashSet::new()),
            created: Mutex::new(None),
            foreign: Mutex::new(None),
        });
        let ctx = Arc::into_raw(shared.clone());
        // SAFETY: `ctx` stays valid until `Drop` unregisters the callback and then releases it.
        let rc = unsafe {
            WlanRegisterNotification(
                handle,
                WLAN_NOTIFICATION_SOURCES(WLAN_NOTIFICATION_SOURCE_ACM.0 | WLAN_NOTIFICATION_SOURCE_MSM.0),
                false,
                Some(on_notification),
                Some(ctx as *const c_void),
                None,
                None,
            )
        };
        if rc != 0 {
            // SAFETY: the callback was not registered; take the context back.
            drop(unsafe { Arc::from_raw(ctx) });
            close(handle);
            return Err(windows::core::Error::from_hresult(windows::core::HRESULT::from_win32(rc)));
        }
        let (tx, rx) = channel();
        let sh = shared.clone();
        // If the thread cannot start only the radio switch is unavailable (`jobs` is then
        // disconnected and `set_radio` is a no-op); the list and connect still work.
        let _ = std::thread::Builder::new()
            .name("vitro-wifi-radio".into())
            .spawn(move || radio_worker(sh, rx));
        Ok(Self { shared, handle, guid, ctx, jobs: tx })
    }

    /// Takes the queued changes (an empty result means a forged or already-handled wake-up).
    /// A failed attempt removes the profile *this service* created for it (never any other);
    /// a failed attempt through a pre-existing profile adds `Error(HandOff)`.
    pub fn drain_changes(&self) -> Vec<Change> {
        let mut changes = self.shared.queue.drain();
        let mut extra = Vec::new();
        for c in &changes {
            if let Change::ConnectFailed(ssid) = c {
                let mut created = lock(&self.shared.created);
                if created.as_deref().is_some_and(|n| ssid.is_empty() || n == ssid) {
                    if let Some(name) = created.take() {
                        self.delete_profile(&name);
                    }
                }
                drop(created);
                let mut foreign = lock(&self.shared.foreign);
                if foreign.as_deref().is_some_and(|n| ssid.is_empty() || n == ssid) {
                    foreign.take();
                    extra.push(Change::Error(WifiError::HandOff));
                }
            }
        }
        changes.extend(extra);
        changes
    }

    /// Deletes a profile this service created.
    fn delete_profile(&self, name: &str) {
        let w = wide(name);
        // SAFETY: NUL-terminated name; the profile was created by this service.
        let _ = unsafe { WlanDeleteProfile(self.handle, &self.guid, PCWSTR(w.as_ptr()), None) };
    }

    /// The current state, read from the WLAN service's cache (no scan, no radio traffic).
    pub fn snapshot(&self) -> WifiSnapshot {
        let connected = self.connected_ssid();
        let nets = self.available().unwrap_or_default();
        let scanning = {
            let mut s = lock(&self.shared.scanning);
            if s.is_some_and(|t| t.elapsed() > SCAN_TIMEOUT) {
                *s = None;
            }
            s.is_some()
        };
        WifiSnapshot {
            radio_on: *lock(&self.shared.radio_on),
            networks: arrange(nets, connected.as_deref()),
            connected,
            scanning,
        }
    }

    /// Asks Windows to scan; completion arrives as a [`Change::Networks`] wake-up. The flag is
    /// set before the call (the completion can arrive before it returns) and cleared if the
    /// call fails.
    pub fn scan(&self) {
        *lock(&self.shared.scanning) = Some(Instant::now());
        // SAFETY: plain call with the open handle; returns at once.
        let rc = unsafe { WlanScan(self.handle, &self.guid, None, None, None) };
        if rc != 0 {
            *lock(&self.shared.scanning) = None;
        }
    }

    /// When a pending scan stops counting as scanning, if one is pending. The panel arms one
    /// one-shot timer for this instant and re-reads [`Wifi::snapshot`] then, so a scan that
    /// never reports back does not leave the spinner on.
    pub fn scan_deadline(&self) -> Option<Instant> {
        (*lock(&self.shared.scanning)).map(|t| t + SCAN_TIMEOUT)
    }

    /// Switches the Wi-Fi radio on a worker; the outcome is a [`Change::Radio`] or
    /// [`Change::Error`] (`Denied` when Windows refuses).
    pub fn set_radio(&self, on: bool) {
        let _ = self.jobs.send(RadioJob::Set(on));
    }

    /// Connects to `ssid`: a known network with its stored profile, an open or personal one
    /// through a new manual user profile (`password` required for personal), an enterprise
    /// one is [`WifiError::HandOff`]. Returns once the attempt is *started*; success or
    /// failure arrives as wake-ups ([`Change::Networks`] / [`Change::ConnectFailed`]).
    pub fn connect(&self, ssid: &str, password: Option<&str>) -> std::result::Result<(), WifiError> {
        let list = self.raw_available().ok_or(WifiError::Failed)?;
        let entry = list
            .into_iter()
            .filter(|e| e.ssid == ssid)
            .max_by_key(|e| (e.known, e.signal))
            .ok_or(WifiError::Failed)?;
        if entry.known {
            return self.start_connect(&entry.profile);
        }
        let (auth, pw) = match entry.kind {
            NetKind::Enterprise => return Err(WifiError::HandOff),
            NetKind::Open => (Auth::Open, ""),
            NetKind::Psk => {
                let p = password.unwrap_or("");
                if !password_ok(p) {
                    return Err(WifiError::BadPassword);
                }
                (if entry.sae { Auth::Wpa3Sae } else { Auth::Wpa2Psk }, p)
            }
        };
        // An earlier attempt of ours that never finished is replaced; a profile of another
        // network that we created is dropped (unless it is the connected one).
        {
            let mut created = lock(&self.shared.created);
            if let Some(old) = created.take() {
                if old == ssid || self.connected_ssid().as_deref() != Some(old.as_str()) {
                    self.delete_profile(&old);
                }
            }
        }
        let mut xml = wide(&profile_xml(ssid, pw, auth));
        let mut reason = 0u32;
        // SAFETY: NUL-terminated UTF-16 profile, user scope (flag 2), never overwriting.
        let rc = unsafe {
            WlanSetProfile(self.handle, &self.guid, 2, PCWSTR(xml.as_ptr()), PCWSTR::null(), false, None, &mut reason)
        };
        wipe_u16(&mut xml);
        if rc == ERROR_ALREADY_EXISTS.0 {
            // Somebody else's profile of that name: use it as it is, and hand off if it fails.
            *lock(&self.shared.foreign) = Some(ssid.to_string());
            return self.start_connect(ssid).map_err(|_| {
                lock(&self.shared.foreign).take();
                WifiError::HandOff
            });
        }
        if rc != 0 {
            return Err(WifiError::Failed);
        }
        *lock(&self.shared.created) = Some(ssid.to_string());
        self.start_connect(ssid)
    }

    pub fn disconnect(&self) {
        // SAFETY: plain call with the open handle.
        let _ = unsafe { WlanDisconnect(self.handle, &self.guid, None) };
    }

    fn start_connect(&self, profile: &str) -> std::result::Result<(), WifiError> {
        let name = wide(profile);
        let params = WLAN_CONNECTION_PARAMETERS {
            wlanConnectionMode: wlan_connection_mode_profile,
            strProfile: PCWSTR(name.as_ptr()),
            pDot11Ssid: std::ptr::null_mut(),
            pDesiredBssidList: std::ptr::null_mut(),
            dot11BssType: dot11_BSS_type_infrastructure,
            dwFlags: 0,
        };
        // SAFETY: `params` and the profile name outlive the call, which returns at once.
        let rc = unsafe { WlanConnect(self.handle, &self.guid, &params, None) };
        if rc == 0 {
            Ok(())
        } else {
            Err(WifiError::Failed)
        }
    }

    fn connected_ssid(&self) -> Option<String> {
        let mut size = 0u32;
        let mut data: *mut c_void = std::ptr::null_mut();
        // SAFETY: the returned block is freed below.
        let rc = unsafe {
            WlanQueryInterface(
                self.handle,
                &self.guid,
                wlan_intf_opcode_current_connection,
                None,
                &mut size,
                &mut data,
                None,
            )
        };
        if rc != 0 || data.is_null() {
            return None;
        }
        // SAFETY: a successful query returns a WLAN_CONNECTION_ATTRIBUTES.
        let attrs = unsafe { &*(data as *const WLAN_CONNECTION_ATTRIBUTES) };
        let out = (attrs.isState == wlan_interface_state_connected)
            .then(|| ssid_string(&attrs.wlanAssociationAttributes.dot11Ssid))
            .flatten();
        // SAFETY: frees the block returned by the query.
        unsafe { WlanFreeMemory(data) };
        out.filter(|s| !s.is_empty())
    }

    fn available(&self) -> Option<Vec<Network>> {
        let raw = self.raw_available()?;
        let mut sae = lock(&self.shared.sae);
        sae.clear();
        sae.extend(raw.iter().filter(|e| e.sae).map(|e| e.ssid.clone()));
        Some(
            raw.into_iter()
                .map(|e| Network {
                    ssid: e.ssid,
                    signal: e.signal,
                    secured: e.secured,
                    kind: e.kind,
                    known: e.known,
                })
                .collect(),
        )
    }

    fn raw_available(&self) -> Option<Vec<RawEntry>> {
        let mut list: *mut WLAN_AVAILABLE_NETWORK_LIST = std::ptr::null_mut();
        // SAFETY: the returned list is freed below.
        let rc = unsafe { WlanGetAvailableNetworkList(self.handle, &self.guid, 0, None, &mut list) };
        if rc != 0 || list.is_null() {
            return None;
        }
        // SAFETY: `dwNumberOfItems` entries follow the header in one allocation.
        let entries = unsafe {
            let l = &*list;
            std::slice::from_raw_parts(
                std::ptr::addr_of!((*list).Network).cast::<WLAN_AVAILABLE_NETWORK>(),
                l.dwNumberOfItems as usize,
            )
        };
        let out = entries
            .iter()
            .filter_map(|n| {
                let ssid = ssid_string(&n.dot11Ssid)?;
                let secured = n.bSecurityEnabled.as_bool();
                let (kind, sae) = classify_auth(n.dot11DefaultAuthAlgorithm.0, secured);
                let end = n.strProfileName.iter().position(|&c| c == 0).unwrap_or(256);
                Some(RawEntry {
                    ssid,
                    profile: String::from_utf16_lossy(&n.strProfileName[..end]),
                    signal: n.wlanSignalQuality.min(100) as u8,
                    secured,
                    kind,
                    sae,
                    known: n.dwFlags & WLAN_AVAILABLE_NETWORK_HAS_PROFILE != 0,
                })
            })
            .collect();
        // SAFETY: frees the list returned above.
        unsafe { WlanFreeMemory(list as *const c_void) };
        Some(out)
    }
}

struct RawEntry {
    ssid: String,
    profile: String,
    signal: u8,
    secured: bool,
    kind: NetKind,
    sae: bool,
    known: bool,
}

impl Drop for Wifi {
    fn drop(&mut self) {
        self.shared.closed.store(true, Ordering::Release);
        // A connect that never completed leaves no profile of ours behind.
        if let Some(name) = lock(&self.shared.created).take() {
            self.delete_profile(&name);
        }
        // SAFETY: unregistering waits for a running callback to finish, so the context can
        // be released afterwards; the radio worker ends when `jobs` is dropped.
        unsafe {
            let _ = WlanRegisterNotification(
                self.handle,
                WLAN_NOTIFICATION_SOURCES(0),
                false,
                None,
                None,
                None,
                None,
            );
            let _ = WlanCloseHandle(self.handle, None);
            drop(Arc::from_raw(self.ctx));
        }
        self.shared.queue.clear();
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The SSID if it is valid UTF-8 (the only kind we can match, hex-encode and show faithfully).
fn ssid_string(s: &DOT11_SSID) -> Option<String> {
    let len = (s.uSSIDLength as usize).min(32);
    std::str::from_utf8(&s.ucSSID[..len]).ok().map(str::to_owned)
}

fn ssid_lossy(s: &DOT11_SSID) -> String {
    let len = (s.uSSIDLength as usize).min(32);
    String::from_utf8_lossy(&s.ucSSID[..len]).into_owned()
}

/// Zeroes a buffer that held a passphrase (volatile so it is not optimised away).
fn wipe_u16(b: &mut [u16]) {
    for c in b.iter_mut() {
        // SAFETY: a valid, exclusive element.
        unsafe { std::ptr::write_volatile(c, 0) };
    }
}

fn first_interface(handle: HANDLE) -> Option<GUID> {
    let mut list: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
    // SAFETY: the returned list is freed below.
    if unsafe { WlanEnumInterfaces(handle, None, &mut list) } != 0 || list.is_null() {
        return None;
    }
    // SAFETY: `dwNumberOfItems` entries follow the header.
    let guid = unsafe {
        let l = &*list;
        (l.dwNumberOfItems > 0).then(|| l.InterfaceInfo[0].InterfaceGuid)
    };
    // SAFETY: frees the list returned above.
    unsafe { WlanFreeMemory(list as *const c_void) };
    guid
}

// ---- radio worker --------------------------------------------------------------------------------

fn wifi_radio() -> Option<Radio> {
    let radios = Radio::GetRadiosAsync().ok()?.join().ok()?;
    radios.into_iter().find(|r| r.Kind().ok() == Some(RadioKind::WiFi))
}

fn radio_worker(shared: Arc<Shared>, rx: Receiver<RadioJob>) {
    let _com = ComGuard::init();
    let radio = wifi_radio();
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
            None => Err(WifiError::Failed),
            Some(r) => {
                let state = if on { RadioState::On } else { RadioState::Off };
                match r.SetStateAsync(state).and_then(|a| a.join()) {
                    Ok(RadioAccessStatus::Allowed) => Ok(()),
                    Ok(RadioAccessStatus::DeniedByUser) | Ok(RadioAccessStatus::DeniedBySystem) => {
                        Err(WifiError::Denied)
                    }
                    _ => Err(WifiError::Failed),
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

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::HWND;

    fn net(ssid: &str, signal: u8, known: bool) -> Network {
        Network { ssid: ssid.into(), signal, secured: true, kind: NetKind::Psk, known }
    }

    #[test]
    fn xml_escapes_the_five_specials() {
        assert_eq!(escape_xml("a&b<c>d\"e'f"), "a&amp;b&lt;c&gt;d&quot;e&apos;f");
        assert_eq!(escape_xml("plain"), "plain");
    }

    #[test]
    fn ssid_hex_is_utf8_uppercase() {
        assert_eq!(hex_ssid("Café"), "436166C3A9");
        assert_eq!(hex_ssid(""), "");
    }

    #[test]
    fn profile_escapes_ssid_and_key_and_carries_hex() {
        let x = profile_xml("Café <&>", "p\"a'ss&<>word", Auth::Wpa2Psk);
        assert!(x.contains("<hex>436166C3A920 3C263E</hex>".replace(' ', "").as_str()), "{x}");
        assert!(x.contains("<name>Café &lt;&amp;&gt;</name>"), "{x}");
        assert!(x.contains("<keyMaterial>p&quot;a&apos;ss&amp;&lt;&gt;word</keyMaterial>"), "{x}");
        assert!(x.contains("<authentication>WPA2PSK</authentication>"));
        assert!(x.contains("<encryption>AES</encryption>"));
        assert!(x.contains("<keyType>passPhrase</keyType>"));
        assert!(x.contains("<protected>false</protected>"));
        assert!(x.contains("<connectionType>ESS</connectionType>"));
        assert!(x.contains("http://www.microsoft.com/networking/WLAN/profile/v1"));
    }

    #[test]
    fn profile_auth_variants() {
        assert!(profile_xml("n", "12345678", Auth::Wpa3Sae).contains("<authentication>WPA3SAE</authentication>"));
        let open = profile_xml("n", "ignored", Auth::Open);
        assert!(open.contains("<authentication>open</authentication>"));
        assert!(open.contains("<encryption>none</encryption>"));
        assert!(!open.contains("keyMaterial") && !open.contains("ignored"));
    }

    #[test]
    fn bars_boundaries() {
        let got: Vec<u8> = [0, 1, 25, 26, 50, 51, 75, 76, 100, 200].iter().map(|&s| signal_bars(s)).collect();
        assert_eq!(got, vec![1, 1, 1, 2, 2, 3, 3, 4, 4, 4]);
    }

    #[test]
    fn arrange_dedups_drops_hidden_and_sorts() {
        let nets = vec![
            net("Cafe", 40, false),
            net("", 99, false),
            net("Home", 60, false),
            net("Cafe", 80, true),
            net("Zed", 80, false),
            net("Mine", 10, true),
        ];
        let out = arrange(nets, Some("Mine"));
        let names: Vec<&str> = out.iter().map(|n| n.ssid.as_str()).collect();
        assert_eq!(names, ["Mine", "Cafe", "Zed", "Home"]);
        let cafe = out.iter().find(|n| n.ssid == "Cafe").unwrap();
        assert_eq!(cafe.signal, 80, "strongest kept");
        assert!(cafe.known, "known survives the merge");
        assert!(arrange(Vec::new(), None).is_empty());
    }

    #[test]
    fn auth_classification() {
        assert_eq!(classify_auth(1, false), (NetKind::Open, false));
        assert_eq!(classify_auth(1, true), (NetKind::Enterprise, false), "WEP-like");
        assert_eq!(classify_auth(7, true), (NetKind::Psk, false));
        assert_eq!(classify_auth(4, true), (NetKind::Enterprise, false), "legacy WPA/TKIP handed off");
        assert_eq!(classify_auth(9, true), (NetKind::Psk, true), "SAE");
        assert_eq!(classify_auth(6, true), (NetKind::Enterprise, false));
        assert_eq!(classify_auth(10, true), (NetKind::Enterprise, false), "OWE handed off");
    }

    /// Read-only: lists what Windows already knows; no scan, no connect, no radio change.
    #[test]
    #[ignore = "reads the real WLAN state; run manually"]
    fn real_snapshot_returns_without_panic() {
        // No Wi-Fi adapter is fine: the tile hides itself.
        if let Ok(w) = Wifi::open(HWND::default(), 0) {
            let s = w.snapshot();
            assert!(s.networks.iter().all(|n| n.signal <= 100 && !n.ssid.is_empty()), "{s:?}");
        }
    }

    #[test]
    fn passphrase_length_and_control_chars() {
        assert!(!password_ok("1234567"));
        assert!(password_ok("12345678"));
        assert!(password_ok(&"a".repeat(63)));
        assert!(!password_ok(&"a".repeat(64)));
        assert!(!password_ok("pass\nword1"));
        assert!(!password_ok("pässwörd12"), "non-ASCII");
    }
}
