//! Launching pins, switching between windows and opening the stock shell panels.
//!
//! Nothing here waits on another app's thread: window state changes are asynchronous
//! (`ShowWindowAsync`, `PostMessageW`), so a hung app cannot freeze the bar. Launching
//! (`launch_pin`, the `open_stock_*` URIs) can block for seconds: callers on the UI thread run
//! it on a worker. Failures never show shell error dialogs (`SEE_MASK_FLAG_NO_UI`).

use vitro_core::settings::Pin;
use windows::core::{w, Error, Result, HSTRING, PCWSTR};
use windows::Win32::Foundation::{E_INVALIDARG, HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_LOCAL_SERVER};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
    VIRTUAL_KEY, VK_CONTROL, VK_ESCAPE,
};
use windows::Win32::UI::Shell::{
    ApplicationActivationManager, IApplicationActivationManager, ShellExecuteExW, AO_NONE,
    SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    IsIconic, PostMessageW, SetForegroundWindow, ShowWindowAsync, SW_MINIMIZE, SW_RESTORE,
    SW_SHOWNORMAL, WM_CLOSE,
};

/// Starts a pinned app: packaged apps by AUMID, everything else through the shell.
/// Blocking; needs COM initialised on the calling thread.
pub fn launch_pin(p: &Pin) -> Result<()> {
    if let Some(aumid) = p.aumid.as_deref().filter(|a| !a.is_empty()) {
        // SAFETY: plain COM activation on the calling (STA) thread.
        unsafe {
            let mgr: IApplicationActivationManager =
                CoCreateInstance(&ApplicationActivationManager, None, CLSCTX_LOCAL_SERVER)?;
            mgr.ActivateApplication(&HSTRING::from(aumid), PCWSTR::null(), AO_NONE)?;
        }
        return Ok(());
    }
    let path = p
        .path
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::new(E_INVALIDARG, "pin has neither AUMID nor path"))?;
    shell_open(&HSTRING::from(path))
}

fn shell_open(file: &HSTRING) -> Result<()> {
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        // NOASYNC: the call may run on a worker thread that does not pump messages.
        fMask: SEE_MASK_FLAG_NO_UI | SEE_MASK_NOASYNC,
        lpVerb: w!("open"),
        lpFile: PCWSTR(file.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: `info` and the strings it points to outlive the call.
    unsafe { ShellExecuteExW(&mut info) }
}

/// Restores the window if minimized, then brings it to the foreground. The click on the
/// bar gave this process the right to set the foreground window. Returns whether
/// `SetForegroundWindow` reported success.
pub fn activate_window(h: HWND) -> bool {
    // SAFETY: plain calls on a window handle; they fail harmlessly if it went stale.
    unsafe {
        if IsIconic(h).as_bool() {
            let _ = ShowWindowAsync(h, SW_RESTORE);
        }
        SetForegroundWindow(h).as_bool()
    }
}

pub fn minimize_window(h: HWND) {
    // SAFETY: asynchronous; never waits on the window's thread.
    unsafe {
        let _ = ShowWindowAsync(h, SW_MINIMIZE);
    }
}

/// Asks the window to close (`WM_CLOSE`, posted).
pub fn close_window(h: HWND) {
    // SAFETY: posting never blocks on the receiver.
    unsafe {
        let _ = PostMessageW(Some(h), WM_CLOSE, WPARAM(0), LPARAM(0));
    }
}

fn key(vk: VIRTUAL_KEY, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                dwFlags: if up {
                    KEYEVENTF_KEYUP
                } else {
                    KEYBD_EVENT_FLAGS(0)
                },
                ..Default::default()
            },
        },
    }
}

/// Opens the stock Start menu (Ctrl+Esc).
pub fn open_stock_start() {
    let inputs = [
        key(VK_CONTROL, false),
        key(VK_ESCAPE, false),
        key(VK_ESCAPE, true),
        key(VK_CONTROL, true),
    ];
    // SAFETY: the slice and the element size describe the array above.
    unsafe {
        SendInput(&inputs, size_of::<INPUT>() as i32);
    }
}

/// Quick Settings (Wi-Fi, volume, battery). Blocking.
pub fn open_stock_quick_settings() -> Result<()> {
    shell_open(&HSTRING::from("ms-actioncenter:controlcenter/"))
}

/// Notification centre and calendar. Blocking.
pub fn open_stock_notifications() -> Result<()> {
    shell_open(&HSTRING::from("ms-actioncenter:"))
}
