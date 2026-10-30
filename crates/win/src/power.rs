//! The Start panel's power menu. Every action goes through [`perform`]; with
//! [`set_dry_run`] on it only records what it would have done (tests and live runs on a real
//! desktop must never lock, sleep, restart, shut down, sign out or switch user).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use windows::core::{Error, Result, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_NOT_ALL_ASSIGNED, HANDLE, LUID};
use windows::Win32::Security::{
    AdjustTokenPrivileges, LookupPrivilegeValueW, LUID_AND_ATTRIBUTES, SE_PRIVILEGE_ENABLED,
    SE_SHUTDOWN_NAME, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows::Win32::System::Power::SetSuspendState;
use windows::Win32::System::RemoteDesktop::{
    WTSDisconnectSession, WTS_CURRENT_SERVER_HANDLE, WTS_CURRENT_SESSION,
};
use windows::Win32::System::Shutdown::{
    ExitWindowsEx, LockWorkStation, EWX_LOGOFF, EWX_POWEROFF, EWX_REBOOT, SHTDN_REASON_FLAG_PLANNED,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerAction {
    Lock,
    Sleep,
    Restart,
    ShutDown,
    SignOut,
    SwitchUser,
}

static DRY_RUN: AtomicBool = AtomicBool::new(false);
static LAST_DRY_RUN: Mutex<Option<PowerAction>> = Mutex::new(None);

/// While on, [`perform`] records the action (see [`last_dry_run`]) and does nothing.
pub fn set_dry_run(on: bool) {
    DRY_RUN.store(on, Ordering::SeqCst);
}

/// The last action [`perform`] recorded in dry-run mode.
pub fn last_dry_run() -> Option<PowerAction> {
    *LAST_DRY_RUN.lock().unwrap_or_else(|e| e.into_inner())
}

/// Carries out `a` (or, in dry-run mode, only records it). Lock, sign-out, restart and
/// shut-down return as soon as Windows accepted the request.
pub fn perform(a: PowerAction) -> Result<()> {
    if DRY_RUN.load(Ordering::SeqCst) {
        *LAST_DRY_RUN.lock().unwrap_or_else(|e| e.into_inner()) = Some(a);
        return Ok(());
    }
    // SAFETY: plain system calls without pointers into our memory.
    unsafe {
        match a {
            PowerAction::Lock => LockWorkStation(),
            PowerAction::Sleep => {
                if SetSuspendState(false, false, false) {
                    Ok(())
                } else {
                    Err(Error::from_thread())
                }
            }
            PowerAction::Restart => {
                enable_shutdown_privilege()?;
                ExitWindowsEx(EWX_REBOOT, SHTDN_REASON_FLAG_PLANNED)
            }
            PowerAction::ShutDown => {
                enable_shutdown_privilege()?;
                ExitWindowsEx(EWX_POWEROFF, SHTDN_REASON_FLAG_PLANNED)
            }
            PowerAction::SignOut => ExitWindowsEx(EWX_LOGOFF, SHTDN_REASON_FLAG_PLANNED),
            PowerAction::SwitchUser => {
                WTSDisconnectSession(Some(WTS_CURRENT_SERVER_HANDLE), WTS_CURRENT_SESSION, false)
            }
        }
    }
}

/// Enables `SeShutdownPrivilege` in this process's token (a standard user holds it,
/// disabled). Only called right before a restart or shut-down.
fn enable_shutdown_privilege() -> Result<()> {
    let mut token = HANDLE::default();
    // SAFETY: `token` is an out-parameter, closed below; the pseudo-handle of the current
    // process needs no closing.
    unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )?;
    }
    let result = (|| {
        let mut luid = LUID::default();
        // SAFETY: `luid` is an out-parameter; the privilege name is a static string.
        unsafe { LookupPrivilegeValueW(PCWSTR::null(), SE_SHUTDOWN_NAME, &mut luid)? };
        let tp = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        // SAFETY: `tp` outlives the call; no previous state is requested.
        unsafe {
            AdjustTokenPrivileges(token, false, Some(&tp), 0, None, None)?;
            // Success can still mean "not assigned": the token lacks the privilege.
            if GetLastError() == ERROR_NOT_ALL_ASSIGNED {
                return Err(Error::from(ERROR_NOT_ALL_ASSIGNED));
            }
        }
        Ok(())
    })();
    // SAFETY: closes the token opened above.
    unsafe {
        let _ = CloseHandle(token);
    }
    result
}
