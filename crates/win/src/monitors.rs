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
    // SAFETY: MONITORINFOEXW starts with MONITORINFO and cbSize says it is the EX variant;
    // the pointer is derived from the whole struct, so it may cover all of it.
    let p = &mut mi as *mut MONITORINFOEXW as *mut MONITORINFO;
    if !unsafe { GetMonitorInfoW(hmon, p) }.as_bool() {
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

/// The [`MonitorInfo::id`] of a monitor handle (what `WindowInfo::monitor` holds).
pub fn monitor_id(hmon: HMONITOR) -> Option<u64> {
    info(hmon).map(|m| m.id)
}

/// Device name of the monitor [`parse_monitor_spec`] makes up (never a real GDI name).
const FAKE_DEVICE: &str = r"\\.\VITROBAR-FAKE";

/// A made-up monitor from `left,top,right,bottom,dpi` (physical px; spaces allowed), for
/// exercising multi-monitor paths on a machine with fewer monitors. Not primary; its id
/// never collides with a real monitor's. `None` if the text is not such a spec or the
/// rectangle is empty.
pub fn parse_monitor_spec(spec: &str) -> Option<MonitorInfo> {
    let v: Vec<i32> = spec
        .trim()
        .split(',')
        .map(|p| p.trim().parse().ok())
        .collect::<Option<_>>()?;
    let [l, t, r, b, dpi] = v[..] else {
        return None;
    };
    if r <= l || b <= t || !(48..=960).contains(&dpi) {
        return None;
    }
    let name: Vec<u16> = FAKE_DEVICE.encode_utf16().collect();
    Some(MonitorInfo {
        id: fnv1a(&name),
        rect_px: (l, t, r, b),
        dpi: dpi as u32,
        primary: false,
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monitor_spec_parses_and_rejects() {
        let m = parse_monitor_spec(
            " 1920, 0, 3840 ,1080,144
",
        )
        .unwrap();
        assert_eq!(m.rect_px, (1920, 0, 3840, 1080));
        assert_eq!(m.dpi, 144);
        assert!(!m.primary);
        assert_eq!(
            parse_monitor_spec("0,0,10,10,96").unwrap().id,
            m.id,
            "one fake id"
        );
        for bad in [
            "",
            "1,2,3,4",
            "0,0,10,10,96,1",
            "0,0,0,10,96",
            "0,0,10,10,0",
            "a,0,10,10,96",
        ] {
            assert_eq!(parse_monitor_spec(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn fake_id_differs_from_real_monitors() {
        let fake = parse_monitor_spec("0,0,10,10,96").unwrap().id;
        let real = list_monitors();
        assert!(!real.is_empty());
        assert!(real.iter().all(|m| m.id != fake));
        assert!(real[0].primary, "primary first");
        for m in &real {
            assert!(m.dpi >= 96 && m.rect_px.2 > m.rect_px.0);
        }
    }
}
