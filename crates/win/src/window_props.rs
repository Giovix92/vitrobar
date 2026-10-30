//! Reads the taskbar-relevant properties of one top-level window.
//!
//! Nothing here sends a message to the window's thread (titles come from
//! `InternalGetWindowText`, which reads the window structure directly), so a hung app cannot
//! block the bar.

use std::ffi::c_void;

use vitro_core::windows_model::WindowInfo;
use windows::core::{BOOL, BSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::Graphics::Gdi::{MonitorFromWindow, MONITOR_DEFAULTTONEAREST};
use windows::Win32::Storage::EnhancedStorage::PKEY_AppUserModel_ID;
use windows::Win32::Storage::Packaging::Appx::GetApplicationUserModelId;
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, SHGetPropertyStoreForWindow};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, GetClassNameW, GetWindow, GetWindowLongPtrW, GetWindowThreadProcessId,
    InternalGetWindowText, IsIconic, IsWindow, IsWindowVisible, GWL_EXSTYLE, GW_OWNER,
    WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
};

use crate::desktops::Desktops;
use crate::monitors::monitor_id;

/// `DWM_CLOAKED_APP`: the app cloaked its own window (as opposed to shell or inherited cloaking).
const DWM_CLOAKED_APP: u32 = 1;
/// `DWM_CLOAKED_SHELL`: cloaked by the shell. That is how windows on other virtual desktops
/// are hidden, but also suspended UWP frames, "Windows Input Experience" and similar
/// always-present system windows that the stock taskbar never lists.
const DWM_CLOAKED_SHELL: u32 = 2;

struct Process(HANDLE);

impl Process {
    fn open(pid: u32) -> Option<Self> {
        // SAFETY: plain query-only open; the handle is closed in Drop.
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
            .ok()
            .map(Self)
    }

    fn exe_path(&self) -> Option<String> {
        let mut buf = vec![0u16; 4096];
        let mut len = buf.len() as u32;
        // SAFETY: `buf` is valid for `len` u16 and `len` is updated to the written length.
        unsafe {
            QueryFullProcessImageNameW(
                self.0,
                PROCESS_NAME_WIN32,
                PWSTR(buf.as_mut_ptr()),
                &mut len,
            )
        }
        .ok()?;
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }

    fn aumid(&self) -> Option<String> {
        let mut len = 0u32;
        // SAFETY: the first call with a null buffer only reports the required length.
        let _ = unsafe { GetApplicationUserModelId(self.0, &mut len, None) };
        if len < 2 {
            return None;
        }
        let mut buf = vec![0u16; len as usize];
        // SAFETY: `buf` holds exactly `len` u16.
        let rc =
            unsafe { GetApplicationUserModelId(self.0, &mut len, Some(PWSTR(buf.as_mut_ptr()))) };
        if rc.0 != 0 {
            return None;
        }
        let n = (len as usize).saturating_sub(1).min(buf.len());
        Some(String::from_utf16_lossy(&buf[..n]))
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        // SAFETY: the handle came from OpenProcess and is closed exactly once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn pid_of(hwnd: HWND) -> u32 {
    let mut pid = 0u32;
    // SAFETY: out-pointer to a local; the call does not message the window.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid
}

/// Full path of the executable of the process that owns `hwnd`.
pub(crate) fn exe_of_window(hwnd: HWND) -> Option<String> {
    Process::open(pid_of(hwnd))?.exe_path()
}

pub(crate) fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    // SAFETY: the slice is the output buffer.
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn title(hwnd: HWND) -> String {
    let mut buf = [0u16; 512];
    // SAFETY: InternalGetWindowText reads the window structure; it never sends WM_GETTEXT.
    let n = unsafe { InternalGetWindowText(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.clamp(0, 512) as usize])
}

/// Explicit AUMID set on the window (or inherited from its shortcut), if any.
fn window_aumid(hwnd: HWND) -> Option<String> {
    // SAFETY: documented shell API; it reads the window's property store.
    unsafe {
        let store: IPropertyStore = SHGetPropertyStoreForWindow(hwnd).ok()?;
        let pv = store.GetValue(&PKEY_AppUserModel_ID).ok()?;
        let s = BSTR::try_from(&pv).ok()?.to_string();
        (!s.is_empty()).then_some(s)
    }
}

unsafe extern "system" fn find_core_window(child: HWND, data: LPARAM) -> BOOL {
    // SAFETY: `data` is the `&mut (u32, Option<u32>)` passed by `hosted_core_pid`.
    let (frame_pid, found) = unsafe { &mut *(data.0 as *mut (u32, Option<u32>)) };
    if class_name(child) == "Windows.UI.Core.CoreWindow" {
        let pid = pid_of(child);
        if pid != *frame_pid {
            *found = Some(pid);
            return false.into();
        }
    }
    true.into()
}

/// For an `ApplicationFrameHost` frame: the process that owns its hosted `CoreWindow`.
fn hosted_core_pid(frame: HWND, frame_pid: u32) -> Option<u32> {
    let mut state: (u32, Option<u32>) = (frame_pid, None);
    // SAFETY: the callback only touches `state`, which outlives the synchronous enumeration.
    unsafe {
        let _ = EnumChildWindows(
            Some(frame),
            Some(find_core_window),
            LPARAM(&mut state as *mut (u32, Option<u32>) as isize),
        );
    }
    state.1
}

fn is_frame_host(path: &str) -> bool {
    path.rsplit(['\\', '/'])
        .next()
        .is_some_and(|f| f.eq_ignore_ascii_case("ApplicationFrameHost.exe"))
}

/// Snapshot of `hwnd`, or `None` if it is not a window any more. `seq` is the caller's
/// first-seen counter. Never blocks on the window's thread.
pub fn read_window_info(hwnd: HWND, seq: u64, desktops: &Desktops) -> Option<WindowInfo> {
    // SAFETY: every call below takes the handle by value and fails harmlessly if it went stale.
    unsafe {
        if !IsWindow(Some(hwnd)).as_bool() {
            return None;
        }
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        let has_owner = GetWindow(hwnd, GW_OWNER).is_ok_and(|o| !o.0.is_null());
        let mut cloak = 0u32;
        let _ = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloak as *mut u32 as *mut c_void,
            size_of::<u32>() as u32,
        );
        let hmon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);

        let pid = pid_of(hwnd);
        let mut proc = Process::open(pid);
        let mut exe_path = proc.as_ref().and_then(Process::exe_path);
        let mut aumid = window_aumid(hwnd);
        if exe_path.as_deref().is_some_and(is_frame_host) {
            if let Some(core) = hosted_core_pid(hwnd, pid).and_then(Process::open) {
                exe_path = core.exe_path().or(exe_path);
                proc = Some(core);
            }
        }
        if aumid.is_none() {
            aumid = proc.as_ref().and_then(Process::aumid);
        }

        let on_current_desktop = desktops.is_on_current(hwnd);
        // A shell-cloaked window on the current desktop is a hidden system window; on another
        // desktop the cloak is the desktop switch itself, reported by `on_current_desktop`.
        let cloaked_by_app =
            cloak & DWM_CLOAKED_APP != 0 || (cloak & DWM_CLOAKED_SHELL != 0 && on_current_desktop);

        Some(WindowInfo {
            hwnd: hwnd.0 as usize as u64,
            seq,
            aumid,
            exe_path,
            title: title(hwnd),
            visible: IsWindowVisible(hwnd).as_bool(),
            tool_window: ex & WS_EX_TOOLWINDOW.0 != 0,
            app_window: ex & WS_EX_APPWINDOW.0 != 0,
            has_owner,
            cloaked_by_app,
            on_current_desktop,
            monitor: monitor_id(hmon).unwrap_or(0),
            minimized: IsIconic(hwnd).as_bool(),
            flashing: false,
        })
    }
}
