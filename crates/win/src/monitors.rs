//! Display monitors in physical pixels (the process is Per-Monitor-V2 DPI aware).

use windows::core::BOOL;
use windows::Win32::Foundation::{LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::MONITORINFOF_PRIMARY;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonitorInfo {
    /// Stable for a session: hash of the GDI device name (e.g. `\\.\DISPLAY1`).
    pub id: u64,
    /// Full monitor rectangle `(left, top, right, bottom)` in physical pixels.
    pub rect_px: (i32, i32, i32, i32),
    /// Effective DPI (96 = 100 %).
    pub dpi: u32,
    pub primary: bool,
}

fn fnv1a(units: &[u16]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &u in units {
        for b in u.to_le_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

fn info(hmon: HMONITOR) -> Option<MonitorInfo> {
    let mut mi = MONITORINFOEXW {
        monitorInfo: MONITORINFO {
            cbSize: size_of::<MONITORINFOEXW>() as u32,
            ..Default::default()
        },
        ..Default::default()
    };
    if !unsafe { GetMonitorInfoW(hmon, &mut mi.monitorInfo) }.as_bool() {
        return None;
    }
    let (mut dx, mut dy) = (96, 96);
    if unsafe { GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) }.is_err() {
        dx = 96;
    }
    let name_len = mi.szDevice.iter().position(|&c| c == 0).unwrap_or(32);
    let r = mi.monitorInfo.rcMonitor;
    Some(MonitorInfo {
        id: fnv1a(&mi.szDevice[..name_len]),
        rect_px: (r.left, r.top, r.right, r.bottom),
        dpi: dx,
        primary: mi.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
    })
}

unsafe extern "system" fn collect(hmon: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
    // SAFETY: `data` is the `&mut Vec<HMONITOR>` passed by `list_monitors`, alive for the call.
    let list = unsafe { &mut *(data.0 as *mut Vec<HMONITOR>) };
    list.push(hmon);
    true.into()
}

/// All attached monitors, primary first.
pub fn list_monitors() -> Vec<MonitorInfo> {
    let mut handles: Vec<HMONITOR> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(collect),
            LPARAM(&mut handles as *mut Vec<HMONITOR> as isize),
        );
    }
    let mut out: Vec<MonitorInfo> = handles.into_iter().filter_map(info).collect();
    out.sort_by_key(|m| !m.primary);
    out
}
