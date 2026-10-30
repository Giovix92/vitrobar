//! Text, glyphs and icons drawn with Direct2D / DirectWrite into `CompositionDrawingSurface`s.
//!
//! Every surface is drawn once and then reused by the visual tree until its content changes.
//! All sizes here are in the surface's physical pixels.

use vitro_core::image::BgraImage;
use vitro_core::style::Rgba;
use windows::core::{Interface, Result, HSTRING};
use windows::Foundation::Size;
use windows::Graphics::DirectX::{DirectXAlphaMode, DirectXPixelFormat};
use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    ID2D1DeviceContext, D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_PROPERTIES1,
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_INTERPOLATION_MODE_LINEAR,
    D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextLayout, DWRITE_FACTORY_TYPE_SHARED,
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_NORMAL,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING,
    DWRITE_TEXT_METRICS, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER,
    DWRITE_WORD_WRAPPING_NO_WRAP,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::System::WinRT::Composition::ICompositionDrawingSurfaceInterop;
use windows::UI::Composition::{CompositionDrawingSurface, CompositionGraphicsDevice};
use windows_numerics::{Matrix3x2, Vector2};

use crate::device::GpuDevice;

/// Which font a piece of text uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontKind {
    /// UI text (clock).
    Text,
    /// Icon glyphs (Start, quick settings, more).
    Glyph,
}

const TEXT_FAMILIES: &[&str] = &["Segoe UI Variable Text", "Segoe UI"];
const GLYPH_FAMILIES: &[&str] = &["Segoe Fluent Icons", "Segoe MDL2 Assets"];

/// Draws surfaces for one graphics device. Cheap to create: the DirectWrite factory is shared.
pub struct Painter {
    dwrite: IDWriteFactory,
    graphics: CompositionGraphicsDevice,
    text_family: HSTRING,
    glyph_family: HSTRING,
}

fn d2d_color(c: Rgba) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: f32::from(c.0) / 255.0,
        g: f32::from(c.1) / 255.0,
        b: f32::from(c.2) / 255.0,
        a: f32::from(c.3) / 255.0,
    }
}

/// First installed family of `names` (the last one if none is found, so DirectWrite falls back).
fn pick_family(dwrite: &IDWriteFactory, names: &[&str]) -> HSTRING {
    let mut collection = None;
    // SAFETY: out-pointer to a local Option.
    if unsafe { dwrite.GetSystemFontCollection(&mut collection, false) }.is_ok() {
        if let Some(collection) = collection {
            for name in names {
                let (mut index, mut exists) = (0u32, Default::default());
                let h = HSTRING::from(*name);
                // SAFETY: out-pointers to locals; `h` outlives the call.
                let ok = unsafe { collection.FindFamilyName(&h, &mut index, &mut exists) };
                if ok.is_ok() && exists.as_bool() {
                    return h;
                }
            }
        }
    }
    HSTRING::from(*names.last().unwrap_or(&"Segoe UI"))
}

impl Painter {
    pub fn new(gpu: &GpuDevice) -> Result<Self> {
        // SAFETY: plain factory creation.
        let dwrite: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
        Ok(Self {
            text_family: pick_family(&dwrite, TEXT_FAMILIES),
            glyph_family: pick_family(&dwrite, GLYPH_FAMILIES),
            dwrite,
            graphics: gpu.graphics.clone(),
        })
    }

    /// The glyph font actually in use (for diagnostics).
    pub fn glyph_family(&self) -> String {
        self.glyph_family.to_string()
    }

    fn layout(
        &self,
        text: &str,
        kind: FontKind,
        size: f32,
        w: f32,
        h: f32,
    ) -> Result<IDWriteTextLayout> {
        let family = match kind {
            FontKind::Text => &self.text_family,
            FontKind::Glyph => &self.glyph_family,
        };
        let wide: Vec<u16> = text.encode_utf16().collect();
        // SAFETY: plain DirectWrite object creation; every argument outlives the calls.
        unsafe {
            let format = self.dwrite.CreateTextFormat(
                family,
                None,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                &HSTRING::from("en-us"),
            )?;
            format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            self.dwrite.CreateTextLayout(&wide, &format, w, h)
        }
    }

    /// Width and height of `text` (lines separated by `\n`) at font size `size`, in the
    /// same unit as `size`.
    pub fn measure(&self, text: &str, kind: FontKind, size: f32) -> Result<(f32, f32)> {
        let layout = self.layout(text, kind, size, 10_000.0, 10_000.0)?;
        let mut m = DWRITE_TEXT_METRICS::default();
        // SAFETY: out-pointer to a local.
        unsafe { layout.GetMetrics(&mut m)? };
        Ok((m.widthIncludingTrailingWhitespace, m.height))
    }

    fn new_surface(&self, w: u32, h: u32) -> Result<CompositionDrawingSurface> {
        self.graphics.CreateDrawingSurface(
            Size {
                Width: w.max(1) as f32,
                Height: h.max(1) as f32,
            },
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            DirectXAlphaMode::Premultiplied,
        )
    }

    /// `text` centred in a `w` x `h` px surface, `size` px font, straight-alpha `color`.
    pub fn text(
        &self,
        text: &str,
        kind: FontKind,
        size: f32,
        color: Rgba,
        w: u32,
        h: u32,
    ) -> Result<CompositionDrawingSurface> {
        let surface = self.new_surface(w, h)?;
        let layout = self.layout(text, kind, size, w as f32, h as f32)?;
        draw(&surface, |dc| unsafe {
            // SAFETY: `dc` is the context returned by BeginDraw, valid until EndDraw.
            let brush = dc.CreateSolidColorBrush(&d2d_color(color), None)?;
            dc.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            dc.DrawTextLayout(
                Vector2::new(0.0, 0.0),
                &layout,
                &brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
            Ok(())
        })?;
        Ok(surface)
    }

    /// One line of text per `(text, x, w)` box, left-aligned and vertically centred in a
    /// `w` x `h` px surface, cut with an ellipsis when it does not fit its box (the titles
    /// under the window thumbnails). `size` is the font size in px.
    pub fn text_row(
        &self,
        boxes: &[(&str, f32, f32)],
        size: f32,
        color: Rgba,
        w: u32,
        h: u32,
    ) -> Result<CompositionDrawingSurface> {
        let surface = self.new_surface(w, h)?;
        let mut layouts = Vec::with_capacity(boxes.len());
        for &(text, x, bw) in boxes {
            let layout = self.layout(text, FontKind::Text, size, bw.max(1.0), h as f32)?;
            // SAFETY: plain DirectWrite calls on objects we own; the trimming options are a
            // local that outlives the call (DirectWrite copies them).
            unsafe {
                layout.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING)?;
                let trimming = DWRITE_TRIMMING {
                    granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                    delimiter: 0,
                    delimiterCount: 0,
                };
                let sign = self.dwrite.CreateEllipsisTrimmingSign(&layout)?;
                layout.SetTrimming(&trimming, &sign)?;
            }
            layouts.push((x, layout));
        }
        draw(&surface, |dc| unsafe {
            // SAFETY: `dc` is the context returned by BeginDraw, valid until EndDraw.
            let brush = dc.CreateSolidColorBrush(&d2d_color(color), None)?;
            dc.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            for (x, layout) in &layouts {
                dc.DrawTextLayout(
                    Vector2::new(*x, 0.0),
                    layout,
                    &brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                );
            }
            Ok(())
        })?;
        Ok(surface)
    }

    /// The image at its own pixel size.
    pub fn image(&self, img: &BgraImage) -> Result<CompositionDrawingSurface> {
        if !img.is_valid() || img.width == 0 || img.height == 0 {
            return Err(windows::core::Error::from(
                windows::Win32::Foundation::E_INVALIDARG,
            ));
        }
        let surface = self.new_surface(img.width, img.height)?;
        draw(&surface, |dc| unsafe {
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
                ..Default::default()
            };
            // SAFETY: `pixels` holds width*height*4 bytes (checked by is_valid) with the given
            // pitch; D2D copies them during the call.
            let bitmap = dc.CreateBitmap(
                D2D_SIZE_U {
                    width: img.width,
                    height: img.height,
                },
                Some(img.pixels.as_ptr().cast()),
                img.width * 4,
                &props,
            )?;
            let dest = D2D_RECT_F {
                left: 0.0,
                top: 0.0,
                right: img.width as f32,
                bottom: img.height as f32,
            };
            dc.DrawBitmap(
                &bitmap,
                Some(&dest),
                1.0,
                D2D1_INTERPOLATION_MODE_LINEAR,
                None,
                None,
            );
            Ok(())
        })?;
        Ok(surface)
    }
}

/// Runs `f` between BeginDraw and EndDraw on the whole surface, cleared to transparent,
/// with the context translated to the surface's update offset and at 96 DPI (1 unit = 1 px).
fn draw(
    surface: &CompositionDrawingSurface,
    f: impl FnOnce(&ID2D1DeviceContext) -> Result<()>,
) -> Result<()> {
    let interop: ICompositionDrawingSurfaceInterop = surface.cast()?;
    let mut offset = POINT::default();
    // SAFETY: BeginDraw returns a device context valid until the matching EndDraw below,
    // which runs whether or not `f` succeeds.
    unsafe {
        let dc: ID2D1DeviceContext = interop.BeginDraw(None, &mut offset)?;
        dc.SetDpi(96.0, 96.0);
        dc.SetTransform(&Matrix3x2::translation(offset.x as f32, offset.y as f32));
        dc.Clear(Some(&D2D1_COLOR_F::default()));
        let drawn = f(&dc);
        let ended = interop.EndDraw();
        drawn.and(ended)
    }
}
