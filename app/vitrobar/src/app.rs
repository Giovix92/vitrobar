//! App state and the event-driven message loop (nothing redraws unless something happened).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use vitro_core::layout::{compute_layout, LayoutInput, Rect};
use vitro_core::settings::{self, Settings, ThemeChoice};
use vitro_core::style::{material_style, MaterialStyle, Theme};
use vitro_render::compositor::{init_compositor, Comp};
use vitro_render::device::create_gpu_device;
use vitro_render::material::backdrop_visual;
use vitro_render::Vector2;
use vitro_win::monitors::list_monitors;
use vitro_win::system_theme::read_system_theme;
use windows::core::{Error, Result, BOOL};
use windows::Win32::Foundation::{E_FAIL, LPARAM, WAIT_OBJECT_0, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::System::Console::SetConsoleCtrlHandler;
use windows::Win32::System::Threading::{GetCurrentThreadId, INFINITE};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MsgWaitForMultipleObjectsEx, PeekMessageW, PostThreadMessageW,
    TranslateMessage, MSG, MWMO_INPUTAVAILABLE, PM_REMOVE, QS_ALLINPUT, WM_QUIT,
};

use crate::bar_window::{create_bar_window, BarWindow, WM_APP_THEME};

/// Clock width placeholder until the clock is drawn (DIP).
const CLOCK_W: f32 = 64.0;

static UI_THREAD: AtomicU32 = AtomicU32::new(0);

unsafe extern "system" fn on_console_ctrl(_ctrl_type: u32) -> BOOL {
    // Ctrl+C, Ctrl+Break, console close: ask the UI thread to quit cleanly.
    let tid = UI_THREAD.load(Ordering::SeqCst);
    unsafe {
        PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0))
            .is_ok()
            .into()
    }
}

fn settings_path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("Vitrobar").join("settings.toml"))
}

fn style_for(settings: &Settings) -> MaterialStyle {
    let sys = read_system_theme();
    let theme = match settings.theme {
        ThemeChoice::System => sys.theme,
        ThemeChoice::Light => Theme::Light,
        ThemeChoice::Dark => Theme::Dark,
    };
    material_style(settings.material, theme, sys.transparency)
}

fn to_px(r: Rect, scale: f32) -> (i32, i32, i32, i32) {
    let p = |v: f32| (v * scale).round() as i32;
    (p(r.x), p(r.y), p(r.right()), p(r.bottom()))
}

/// One bar surface on screen with the inputs needed to rebuild its backdrop.
struct Bar {
    window: BarWindow,
    size_px: Vector2,
    radius_px: f32,
    style: MaterialStyle,
}

impl Bar {
    fn draw(&self, comp: &Comp) -> Result<()> {
        let visual = backdrop_visual(comp, &self.style, self.size_px, self.radius_px)?;
        self.window.set_content(&visual)
    }
}

struct ComGuard;
impl Drop for ComGuard {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

pub fn run() -> Result<()> {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()? };
    let _com = ComGuard;
    UI_THREAD.store(unsafe { GetCurrentThreadId() }, Ordering::SeqCst);
    unsafe { SetConsoleCtrlHandler(Some(on_console_ctrl), true)? };

    let comp = init_compositor()?;
    let mut gpu = create_gpu_device(&comp)?;
    // Read only: defaults when missing; the settings file is not written yet.
    let settings = settings_path()
        .map(|p| settings::load(&p))
        .unwrap_or_default();

    let monitor = list_monitors()
        .into_iter()
        .find(|m| m.primary)
        .ok_or_else(|| Error::new(E_FAIL, "no primary monitor"))?;
    let scale = monitor.dpi as f32 / 96.0;
    let (ml, mt, mr, mb) = monitor.rect_px;
    let layout = compute_layout(&LayoutInput {
        monitor: Rect {
            x: ml as f32 / scale,
            y: mt as f32 / scale,
            w: (mr - ml) as f32 / scale,
            h: (mb - mt) as f32 / scale,
        },
        shape: settings.shape,
        position: settings.position,
        icon_size: settings.icon_size,
        app_count: 0,
        tray_count: 0,
        clock_w: CLOCK_W,
        reserve_floating: settings.reserve_space_floating,
    });
    let rect_px = to_px(layout.surfaces[0], scale);
    let (l, t, r, b) = rect_px;
    let mut bar = Bar {
        window: create_bar_window(&comp, rect_px)?,
        size_px: Vector2::new((r - l) as f32, (b - t) as f32),
        radius_px: layout.radius * scale,
        style: style_for(&settings),
    };
    bar.draw(&comp)?;
    bar.window.show();

    let mut msg = MSG::default();
    'event: loop {
        let removed = gpu.removed_event();
        let wait = unsafe {
            MsgWaitForMultipleObjectsEx(
                Some(&[removed]),
                INFINITE,
                QS_ALLINPUT,
                MWMO_INPUTAVAILABLE,
            )
        };
        let mut redraw = false;
        if wait == WAIT_OBJECT_0 {
            // GPU device removed (TDR, driver update): recreate, then redraw everything.
            match gpu.recreate() {
                Ok(()) => redraw = true,
                Err(e) => eprintln!("vitrobar: GPU device recreation failed: {e}"),
            }
        }
        let mut theme_dirty = false;
        while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
            if msg.message == WM_QUIT {
                break 'event;
            }
            if msg.message == WM_APP_THEME {
                theme_dirty = true;
                continue;
            }
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        if theme_dirty {
            let style = style_for(&settings);
            if style != bar.style {
                bar.style = style;
                redraw = true;
            }
        }
        if redraw {
            bar.draw(&comp)?;
        }
    }
    drop(bar);
    drop(gpu);
    drop(comp);
    Ok(())
}
