//! Launching pins, switching between windows and opening the stock shell panels.
//!
//! Nothing here waits on another app's thread: window state changes are asynchronous
//! (`ShowWindowAsync`, `PostMessageW`), so a hung app cannot freeze the bar. Launching
//! (`launch_pin`, the `open_stock_*` URIs) can block for seconds: callers on the UI thread run
//! it on a worker. Failures never show shell error dialogs (`SEE_MASK_FLAG_NO_UI`).

use vitro_core::settings::Pin;
use windows::core::{w, Error, Result, HSTRING, PCWSTR};
use windows::Win32::Foundation::{ERROR_CANCELLED, E_INVALIDARG, HWND, LPARAM, WPARAM};
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
    IsIconic, PostMessageW, SetForegroundWindow, ShowWindowAsync, SC_CLOSE, SW_MINIMIZE,
    SW_RESTORE, SW_SHOWNORMAL, WM_CLOSE, WM_SYSCOMMAND,
};

/// One way of starting a pin, in the order [`launch_plan`] tries them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchStep {
    /// Open the shortcut the pin was imported from.
    Link(String),
    /// Activate the packaged app.
    Aumid(String),
    /// Open the executable through the shell.
    Exe(String),
}

/// The ways to start `p`, best first: the shortcut it was imported from while that still
/// exists (`link_exists`; it keeps arguments such as a web app's id), then its AUMID, then
/// its executable. Empty fields are skipped.
pub fn launch_plan(p: &Pin, link_exists: impl Fn(&str) -> bool) -> Vec<LaunchStep> {
    let some = |v: &Option<String>| v.as_deref().filter(|s| !s.is_empty()).map(str::to_owned);
    let mut plan = Vec::new();
    if let Some(l) = some(&p.link).filter(|l| link_exists(l)) {
        plan.push(LaunchStep::Link(l));
    }
    if let Some(a) = some(&p.aumid) {
        plan.push(LaunchStep::Aumid(a));
    }
    if let Some(e) = some(&p.path) {
        plan.push(LaunchStep::Exe(e));
    }
    plan
}

/// Starts a pinned app, trying each step of [`launch_plan`] until one works: a shortcut that
/// will not open, or an AUMID that does not activate (a desktop app's explicit AUMID), falls
/// back to the next. The user declining (`ERROR_CANCELLED`, e.g. a UAC prompt) ends it there.
/// Returns the last failure. Blocking; needs COM initialised on the calling
/// thread.
pub fn launch_pin(p: &Pin) -> Result<()> {
    let mut last = Error::new(E_INVALIDARG, "pin has neither shortcut, AUMID nor path");
    for step in launch_plan(p, |l| std::path::Path::new(l).is_file()) {
        let r = match &step {
            LaunchStep::Link(l) | LaunchStep::Exe(l) => shell_open(&HSTRING::from(l.as_str())),
            LaunchStep::Aumid(a) => activate_aumid(a),
        };
        match r {
            Ok(()) => return Ok(()),
            Err(e) if e.code() == ERROR_CANCELLED.to_hresult() => return Err(e),
            Err(e) => last = e,
        }
    }
    Err(last)
}

fn activate_aumid(aumid: &str) -> Result<()> {
    // SAFETY: plain COM activation on the calling (STA) thread.
    unsafe {
        let mgr: IApplicationActivationManager =
            CoCreateInstance(&ApplicationActivationManager, None, CLSCTX_LOCAL_SERVER)?;
        mgr.ActivateApplication(&HSTRING::from(aumid), PCWSTR::null(), AO_NONE)?;
    }
    Ok(())
}

/// Opens `file` (a path, shortcut or URI such as `ms-settings:`) with its default handler,
/// without shell error dialogs. Blocking; needs COM initialised on the calling thread.
pub fn shell_open(file: &HSTRING) -> Result<()> {
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

/// Opens an Explorer window on the folder holding `path`, with the item selected ("Open file
/// location"). Blocking; needs COM initialised on the calling thread.
pub fn reveal_in_folder(path: &str) -> Result<()> {
    if path.is_empty() || path.contains('"') {
        return Err(Error::from(E_INVALIDARG));
    }
    let params = HSTRING::from(format!("/select,\"{path}\""));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_FLAG_NO_UI | SEE_MASK_NOASYNC,
        lpVerb: w!("open"),
        lpFile: w!("explorer.exe"),
        lpParameters: PCWSTR(params.as_ptr()),
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

/// Asks the window to close the way the stock taskbar does (`SC_CLOSE`, posted): the window
/// may ask to save or refuse. Never forces it and never ends the task.
pub fn request_close(h: HWND) {
    // SAFETY: posting never blocks on the receiver; it fails harmlessly on a stale handle.
    unsafe {
        let _ = PostMessageW(Some(h), WM_SYSCOMMAND, WPARAM(SC_CLOSE as usize), LPARAM(0));
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

/// Closes the stock flyout in front (Esc). The caller has checked that the foreground window
/// is the flyout, otherwise the key would go to the user's app.
pub fn dismiss_foreground_flyout() {
    let inputs = [key(VK_ESCAPE, false), key(VK_ESCAPE, true)];
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

#[cfg(test)]
mod tests {
    use super::*;

    fn pin(link: Option<&str>, aumid: Option<&str>, path: Option<&str>) -> Pin {
        Pin {
            link: link.map(Into::into),
            aumid: aumid.map(Into::into),
            path: path.map(Into::into),
            name: "x".into(),
        }
    }

    #[test]
    fn link_then_aumid_then_exe() {
        let p = pin(Some("a.lnk"), Some("App!Id"), Some("a.exe"));
        assert_eq!(
            launch_plan(&p, |_| true),
            vec![
                LaunchStep::Link("a.lnk".into()),
                LaunchStep::Aumid("App!Id".into()),
                LaunchStep::Exe("a.exe".into()),
            ]
        );
        // A shortcut that is gone is not tried.
        assert_eq!(
            launch_plan(&p, |_| false),
            vec![
                LaunchStep::Aumid("App!Id".into()),
                LaunchStep::Exe("a.exe".into()),
            ]
        );
    }

    #[test]
    fn empty_fields_are_skipped() {
        let p = pin(Some(""), Some(""), Some("a.exe"));
        assert_eq!(
            launch_plan(&p, |_| true),
            vec![LaunchStep::Exe("a.exe".into())]
        );
        assert!(launch_plan(&pin(None, None, None), |_| true).is_empty());
        let only_link = pin(Some("a.lnk"), None, None);
        assert_eq!(
            launch_plan(&only_link, |_| true),
            vec![LaunchStep::Link("a.lnk".into())]
        );
    }

    #[test]
    fn cancelled_is_the_hresult_shell_execute_reports() {
        // ShellExecuteEx fails with HRESULT_FROM_WIN32(ERROR_CANCELLED) when UAC is declined.
        assert_eq!(ERROR_CANCELLED.to_hresult().0 as u32, 0x8007_04C7);
    }

    #[test]
    fn reveal_refuses_empty_and_quoted_paths() {
        assert!(reveal_in_folder("").is_err());
        assert!(reveal_in_folder("a\"b").is_err());
    }

    #[test]
    fn nothing_to_launch_is_an_error() {
        assert!(launch_pin(&pin(None, None, None)).is_err());
    }
}
