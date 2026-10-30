//! Icon loading (shell items, window icons, tray handles) and an LRU cache.
//!
//! All sizes are physical pixels (`icon DIP * dpi / 96`). Results are exactly
//! `size_px` x `size_px`, premultiplied BGRA, top-down rows.
//!
//! COM must already be initialised on the calling thread (the app thread is
//! STA); nothing here calls `CoInitializeEx`.

use std::ffi::c_void;

use vitro_core::image::BgraImage;
use windows::core::{Interface, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDIBits, GetObjectW,
    SelectObject, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HGDIOBJ,
};
use windows::Win32::System::Com::IBindCtx;
use windows::Win32::UI::Shell::{
    IShellItem, IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_ICONONLY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CopyIcon, DestroyIcon, DrawIconEx, GetClassLongPtrW, SendMessageTimeoutW, DI_NORMAL,
    GCLP_HICON, HICON, ICON_BIG, ICON_SMALL2, SMTO_ABORTIFHUNG, SMTO_BLOCK, WM_GETICON,
};

/// Where an icon comes from.
#[derive(Clone, Copy, Debug)]
pub enum IconSource<'a> {
    Aumid(&'a str),
    Path(&'a str),
    Window(HWND),
    TrayHandle(u32),
}

const CACHE_CAPACITY: usize = 256;

/// Loads an icon at `size_px` x `size_px`; `None` if it cannot be obtained.
pub fn load_icon(src: IconSource, size_px: u32) -> Option<BgraImage> {
    if size_px == 0 || size_px > 1024 {
        return None;
    }
    match src {
        IconSource::Aumid(a) => load_shell_item(&format!("shell:AppsFolder\\{a}"), size_px),
        IconSource::Path(p) => load_shell_item(p, size_px),
        IconSource::Window(h) => {
            let icon = window_icon(h)?;
            // SAFETY: the HICON is owned by the window/class; we only draw it.
            unsafe { render_icon(icon, size_px) }
        }
        IconSource::TrayHandle(v) => {
            let handle = HICON(v as i32 as isize as *mut c_void); // 32-bit handle, sign-extended
                                                                  // SAFETY: CopyIcon returns Err on an invalid handle; the copy is ours
                                                                  // and is destroyed right after drawing.
            unsafe {
                let copy = CopyIcon(handle).ok()?;
                let img = render_icon(copy, size_px);
                let _ = DestroyIcon(copy);
                img
            }
        }
    }
}

fn window_icon(h: HWND) -> Option<HICON> {
    for which in [ICON_BIG, ICON_SMALL2] {
        let mut result: usize = 0;
        // SAFETY: ABORTIFHUNG bounds the call by the timeout; `result` outlives it.
        let ok = unsafe {
            SendMessageTimeoutW(
                h,
                WM_GETICON,
                WPARAM(which as usize),
                LPARAM(0),
                SMTO_ABORTIFHUNG | SMTO_BLOCK,
                100,
                Some(&mut result),
            )
        };
        if ok.0 == 0 {
            // Timed out or hung: at most one 100 ms wait per call. Skip the
            // remaining message and use the class icon (sends no message).
            break;
        }
        if result != 0 {
            return Some(HICON(result as *mut c_void));
        }
    }
    // SAFETY: plain class-long read.
    let v = unsafe { GetClassLongPtrW(h, GCLP_HICON) };
    (v != 0).then_some(HICON(v as *mut c_void))
}

fn load_shell_item(parsing_name: &str, size_px: u32) -> Option<BgraImage> {
    let wide: Vec<u16> = parsing_name.encode_utf16().chain(Some(0)).collect();
    // SAFETY: `wide` is NUL-terminated and outlives the call.
    let item: IShellItem =
        unsafe { SHCreateItemFromParsingName(PCWSTR(wide.as_ptr()), None::<&IBindCtx>) }.ok()?;
    let factory: IShellItemImageFactory = item.cast().ok()?;
    let size = SIZE {
        cx: size_px as i32,
        cy: size_px as i32,
    };
    // SAFETY: plain COM call; the returned HBITMAP is ours to delete.
    // Observed once: the very first GetImage after a fresh build failed and a
    // rerun succeeded. The cause is unknown; we retry once, only if the first
    // call failed.
    let hbmp = unsafe { factory.GetImage(size, SIIGBF_ICONONLY) }
        .or_else(|_| unsafe { factory.GetImage(size, SIIGBF_ICONONLY) })
        .ok()?;
    // SAFETY: hbmp is a valid bitmap we own; it is deleted inside.
    unsafe { hbitmap_to_bgra(hbmp, size_px) }
}

/// Converts and always deletes `hbmp`.
///
/// # Safety
/// `hbmp` must be a valid, unselected bitmap owned by the caller.
unsafe fn hbitmap_to_bgra(hbmp: HBITMAP, size_px: u32) -> Option<BgraImage> {
    unsafe {
        let mut bm = BITMAP::default();
        let got = GetObjectW(
            HGDIOBJ(hbmp.0),
            std::mem::size_of::<BITMAP>() as i32,
            Some(&mut bm as *mut _ as *mut c_void),
        );
        if got == 0 || bm.bmWidth <= 0 || bm.bmHeight <= 0 {
            let _ = DeleteObject(HGDIOBJ(hbmp.0));
            return None;
        }
        let (w, h) = (bm.bmWidth as usize, bm.bmHeight as usize);
        let mut buf = vec![0u8; w * h * 4];
        let mut bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w as i32,
                biHeight: -(h as i32), // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let dc = CreateCompatibleDC(None);
        let lines = GetDIBits(
            dc,
            hbmp,
            0,
            h as u32,
            Some(buf.as_mut_ptr() as *mut c_void),
            &mut bi,
            DIB_RGB_COLORS,
        );
        let _ = DeleteDC(dc);
        let _ = DeleteObject(HGDIOBJ(hbmp.0));
        if lines == 0 {
            return None;
        }
        normalize_alpha(&mut buf);
        Some(scale_to(&buf, w, h, size_px as usize))
    }
}

/// No alpha at all -> opaque; straight alpha (a channel exceeds alpha) ->
/// premultiply; otherwise assumed already premultiplied.
fn normalize_alpha(buf: &mut [u8]) {
    let (px, _) = buf.as_chunks_mut::<4>();
    if px.iter().all(|p| p[3] == 0) {
        for p in px.iter_mut() {
            p[3] = 255;
        }
        return;
    }
    let straight = px.iter().any(|p| p[0] > p[3] || p[1] > p[3] || p[2] > p[3]);
    if straight {
        for p in px.iter_mut() {
            let a = p[3] as u32;
            for c in &mut p[..3] {
                *c = ((*c as u32 * a + 127) / 255) as u8;
            }
        }
    }
}

/// Bilinear scale of premultiplied BGRA to a square `n` x `n` (copy if equal).
fn scale_to(src: &[u8], w: usize, h: usize, n: usize) -> BgraImage {
    if w == n && h == n {
        return BgraImage {
            width: n as u32,
            height: n as u32,
            pixels: src.to_vec(),
        };
    }
    let mut out = vec![0u8; n * n * 4];
    for y in 0..n {
        let fy = ((y as f32 + 0.5) * h as f32 / n as f32 - 0.5).clamp(0.0, (h - 1) as f32);
        let y0 = fy as usize;
        let y1 = (y0 + 1).min(h - 1);
        let ty = fy - y0 as f32;
        for x in 0..n {
            let fx = ((x as f32 + 0.5) * w as f32 / n as f32 - 0.5).clamp(0.0, (w - 1) as f32);
            let x0 = fx as usize;
            let x1 = (x0 + 1).min(w - 1);
            let tx = fx - x0 as f32;
            for c in 0..4 {
                let p = |xx: usize, yy: usize| src[(yy * w + xx) * 4 + c] as f32;
                let top = p(x0, y0) * (1.0 - tx) + p(x1, y0) * tx;
                let bot = p(x0, y1) * (1.0 - tx) + p(x1, y1) * tx;
                out[(y * n + x) * 4 + c] = (top * (1.0 - ty) + bot * ty + 0.5) as u8;
            }
        }
    }
    BgraImage {
        width: n as u32,
        height: n as u32,
        pixels: out,
    }
}

/// Draws `icon` at `size_px` over black and over white and recovers alpha from
/// the difference; this works for mask icons and 32-bit alpha icons alike.
///
/// # Safety
/// `icon` must be a valid HICON for the duration of the call.
unsafe fn render_icon(icon: HICON, size_px: u32) -> Option<BgraImage> {
    unsafe {
        let n = size_px as usize;
        let on_black = draw_on(icon, size_px, 0x00)?;
        let on_white = draw_on(icon, size_px, 0xFF)?;
        let mut out = vec![0u8; n * n * 4];
        let (out_px, _) = out.as_chunks_mut::<4>();
        let (black_px, _) = on_black.as_chunks::<4>();
        let (white_px, _) = on_white.as_chunks::<4>();
        for ((o, b), w) in out_px.iter_mut().zip(black_px).zip(white_px) {
            // white - black = 255 - alpha in every colour channel.
            let spread = (0..3).map(|c| w[c].saturating_sub(b[c])).min().unwrap_or(0);
            let a = 255 - spread;
            o[0] = b[0].min(a);
            o[1] = b[1].min(a);
            o[2] = b[2].min(a);
            o[3] = a;
        }
        Some(BgraImage {
            width: size_px,
            height: size_px,
            pixels: out,
        })
    }
}

/// Draws the icon onto a `size_px` DIB pre-filled with `fill`, returns its bytes.
///
/// # Safety
/// `icon` must be a valid HICON.
unsafe fn draw_on(icon: HICON, size_px: u32, fill: u8) -> Option<Vec<u8>> {
    unsafe {
        let n = size_px as i32;
        let bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: n,
                biHeight: -n,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let dc = CreateCompatibleDC(None);
        let mut bits: *mut c_void = std::ptr::null_mut();
        let Ok(dib) = CreateDIBSection(Some(dc), &bi, DIB_RGB_COLORS, &mut bits, None, 0) else {
            let _ = DeleteDC(dc);
            return None;
        };
        if bits.is_null() {
            let _ = DeleteObject(HGDIOBJ(dib.0));
            let _ = DeleteDC(dc);
            return None;
        }
        let len = size_px as usize * size_px as usize * 4;
        // SAFETY: the DIB section owns `len` bytes at `bits` while `dib` lives.
        std::ptr::write_bytes(bits as *mut u8, fill, len);
        let old = SelectObject(dc, HGDIOBJ(dib.0));
        let drawn = DrawIconEx(dc, 0, 0, icon, n, n, 0, None, DI_NORMAL).is_ok();
        let out = drawn.then(|| std::slice::from_raw_parts(bits as *const u8, len).to_vec());
        SelectObject(dc, old);
        let _ = DeleteObject(HGDIOBJ(dib.0));
        let _ = DeleteDC(dc);
        out
    }
}

struct Entry {
    key: String,
    size: u32,
    image: BgraImage,
}

/// LRU cache of loaded icons (most recently used at the back).
#[derive(Default)]
pub struct IconCache {
    entries: Vec<Entry>,
}

fn key_of(src: &IconSource) -> String {
    match src {
        IconSource::Aumid(a) => format!("aumid:{a}"),
        IconSource::Path(p) => format!("path:{}", p.to_lowercase()),
        IconSource::Window(h) => format!("hwnd:{}", h.0 as usize),
        IconSource::TrayHandle(v) => format!("tray:{v}"),
    }
}

impl IconCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns the cached icon or loads it; failures are not cached.
    pub fn get(&mut self, src: IconSource, size_px: u32) -> Option<&BgraImage> {
        let key = key_of(&src);
        if !self.touch(&key, size_px) {
            let image = load_icon(src, size_px)?;
            self.push(key, size_px, image);
        }
        self.entries.last().map(|e| &e.image)
    }

    /// Moves an existing entry to the most-recent position.
    fn touch(&mut self, key: &str, size: u32) -> bool {
        match self
            .entries
            .iter()
            .position(|e| e.size == size && e.key == key)
        {
            Some(i) => {
                let e = self.entries.remove(i);
                self.entries.push(e);
                true
            }
            None => false,
        }
    }

    /// Drops every cached size for this window handle.
    pub fn invalidate_window(&mut self, h: HWND) {
        let key = key_of(&IconSource::Window(h));
        self.entries.retain(|e| e.key != key);
    }

    fn push(&mut self, key: String, size: u32, image: BgraImage) {
        if self.entries.len() >= CACHE_CAPACITY {
            self.entries.remove(0);
        }
        self.entries.push(Entry { key, size, image });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn has(c: &IconCache, k: &str) -> bool {
        c.entries.iter().any(|e| e.key == k)
    }

    fn fill(c: &mut IconCache, n: usize) {
        for i in 0..n {
            c.push(format!("path:{i}"), 16, BgraImage::new(1, 1));
        }
    }

    #[test]
    fn evicts_oldest_at_capacity() {
        let mut c = IconCache::new();
        fill(&mut c, 257);
        assert_eq!(c.len(), 256);
        assert!(!has(&c, "path:0"));
        assert!(has(&c, "path:1"));
        assert!(has(&c, "path:256"));
    }

    #[test]
    fn touched_entry_survives_eviction() {
        let mut c = IconCache::new();
        fill(&mut c, 256);
        assert!(c.touch("path:0", 16));
        c.push("path:new".into(), 16, BgraImage::new(1, 1));
        assert!(has(&c, "path:0"), "touched key survives");
        assert!(!has(&c, "path:1"), "next-oldest evicted");
        assert!(has(&c, "path:new"));
        assert_eq!(c.len(), 256);
    }

    #[test]
    fn invalidate_window_removes_all_sizes_only_for_that_window() {
        let mut c = IconCache::new();
        let h = HWND(0x1234 as *mut c_void);
        let other = HWND(0x99 as *mut c_void);
        c.push(key_of(&IconSource::Window(h)), 16, BgraImage::new(1, 1));
        c.push(key_of(&IconSource::Window(h)), 32, BgraImage::new(1, 1));
        c.push(key_of(&IconSource::Window(other)), 16, BgraImage::new(1, 1));
        c.invalidate_window(h);
        assert_eq!(c.len(), 1);
        assert!(has(&c, &key_of(&IconSource::Window(other))));
    }

    #[test]
    fn zero_alpha_becomes_opaque() {
        let mut b = [10, 20, 30, 0, 1, 2, 3, 0];
        normalize_alpha(&mut b);
        assert_eq!(b, [10, 20, 30, 255, 1, 2, 3, 255]);
    }

    #[test]
    fn straight_alpha_is_premultiplied() {
        let mut b = [200, 200, 200, 128, 0, 0, 0, 0];
        normalize_alpha(&mut b);
        assert_eq!(b[3], 128);
        for c in &b[..3] {
            assert!((*c as i32 - 100).abs() <= 1, "got {c}");
        }
    }

    #[test]
    fn premultiplied_is_unchanged() {
        let mut b = [100, 100, 100, 128, 0, 0, 0, 0];
        let before = b;
        normalize_alpha(&mut b);
        assert_eq!(b, before);
    }
}
