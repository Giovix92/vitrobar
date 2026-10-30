//! Private bytes (`PrivateUsage`, what the perf check budgets) after each step of the GPU
//! set-up Vitrobar does: compositor, DXGI, D3D11 device, D2D, composition graphics device,
//! surfaces, drawing. Diagnostic tool for the 40 MB budget (stage 1, Task 16).
//!
//! Diagnostic only: it repeats the set-up of `vitro_render::device` by hand instead of
//! calling it (to time each step), so it may drift from what the app does. Vitrobar's
//! default device is WARP (`--warp` here); without `--warp` this measures the hardware
//! device on the minimum-power adapter (`DeviceKind::MinPowerHardware`).
//! `cargo run --release -p vitro-render --example memprobe -- [options]`
//!   --fl <9_1|10_0|10_1|11_0|11_1>   one feature level only (default: the runtime's list)
//!   --single                         D3D11_CREATE_DEVICE_SINGLETHREADED
//!   --nothreads                      D3D11_CREATE_DEVICE_PREVENT_INTERNAL_THREADING_OPTIMIZATIONS
//!   --video                          D3D11_CREATE_DEVICE_VIDEO_SUPPORT
//!   --warp                           WARP (software) instead of the minimum-power adapter
//!   --d2d-mt                         multithreaded D2D factory
//!   --regions                        committed memory by kind before/after the D3D device
//!   --one                            draw 41 times into one surface instead of 41 surfaces
//!   --clear                          ID2D1Device::ClearResources after drawing
//!   --trim                           IDXGIDevice3::Trim after drawing

use std::time::Duration;

use windows::core::Interface;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Factory1, D2D1_FACTORY_TYPE_MULTI_THREADED,
    D2D1_FACTORY_TYPE_SINGLE_THREADED,
};
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::System::WinRT::Composition::ICompositorInterop;

#[repr(C)]
#[derive(Default)]
struct Pmc {
    cb: u32,
    page_faults: u32,
    peak_ws: usize,
    ws: usize,
    quota: [usize; 4],
    pagefile: usize,
    peak_pagefile: usize,
    private: usize,
}

#[link(name = "kernel32")]
extern "system" {
    fn K32GetProcessMemoryInfo(h: isize, p: *mut Pmc, cb: u32) -> i32;
}

fn report(step: &str, last: &mut f64) {
    std::thread::sleep(Duration::from_millis(200));
    let mut m = Pmc {
        cb: size_of::<Pmc>() as u32,
        ..Default::default()
    };
    // SAFETY: out-pointer to a local of the declared size; -1 is the current process.
    unsafe { K32GetProcessMemoryInfo(-1, &mut m, m.cb) };
    let mb = m.private as f64 / 1048576.0;
    println!(
        "{step:<28} private {mb:7.2} MB  (+{:6.2})  ws {:7.2} MB",
        mb - *last,
        m.ws as f64 / 1048576.0
    );
    *last = mb;
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct Mbi {
    base: usize,
    alloc_base: usize,
    alloc_protect: u32,
    partition: u16,
    region: usize,
    state: u32,
    protect: u32,
    kind: u32,
}

#[link(name = "kernel32")]
extern "system" {
    fn VirtualQuery(addr: usize, mbi: *mut Mbi, len: usize) -> usize;
    fn K32GetMappedFileNameW(h: isize, addr: usize, name: *mut u16, len: u32) -> u32;
}

/// Committed bytes by kind: private (heaps, driver allocations), and writable image
/// pages per module (counted in private bytes once written).
fn regions(label: &str) {
    const MEM_COMMIT: u32 = 0x1000;
    const MEM_PRIVATE: u32 = 0x20000;
    const MEM_MAPPED: u32 = 0x40000;
    const MEM_IMAGE: u32 = 0x1000000;
    let mut addr = 0usize;
    let (mut private, mut mapped_w) = (0usize, 0usize);
    let mut image: std::collections::BTreeMap<String, usize> = Default::default();
    let mut big: Vec<(usize, usize, u32)> = Vec::new();
    loop {
        let mut m = Mbi::default();
        // SAFETY: out-pointer of the declared size.
        if unsafe { VirtualQuery(addr, &mut m, size_of::<Mbi>()) } == 0 {
            break;
        }
        if m.state == MEM_COMMIT {
            let writable = m.protect & (0x04 | 0x08 | 0x40 | 0x80) != 0;
            match m.kind {
                MEM_PRIVATE => {
                    private += m.region;
                    big.push((m.region, m.base, m.protect));
                }
                MEM_MAPPED if writable => mapped_w += m.region,
                MEM_IMAGE if writable => {
                    let mut name = [0u16; 260];
                    // SAFETY: buffer of the given length.
                    let n = unsafe { K32GetMappedFileNameW(-1, m.base, name.as_mut_ptr(), 260) };
                    let full = String::from_utf16_lossy(&name[..n as usize]);
                    let short = full.rsplit('\\').next().unwrap_or("?").to_string();
                    *image.entry(short).or_default() += m.region;
                }
                _ => {}
            }
        }
        addr = m.base + m.region;
    }
    println!(
        "  [{label}] committed MEM_PRIVATE {:.2} MB, writable mapped {:.2} MB",
        private as f64 / 1048576.0,
        mapped_w as f64 / 1048576.0
    );
    big.sort_by_key(|r| std::cmp::Reverse(r.0));
    for (size, base, prot) in big.iter().take(8) {
        println!(
            "    private region {:#x} {:.2} MB prot {prot:#x}",
            base,
            *size as f64 / 1048576.0
        );
    }
    let mut im: Vec<_> = image.into_iter().collect();
    im.sort_by_key(|r| std::cmp::Reverse(r.1));
    for (name, size) in im.iter().take(8) {
        println!(
            "    writable image {name:<28} {:.2} MB",
            *size as f64 / 1048576.0
        );
    }
}

fn main() -> windows::core::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |f: &str| args.iter().any(|a| a == f);
    let fl = args
        .iter()
        .position(|a| a == "--fl")
        .and_then(|i| args.get(i + 1))
        .map(|s| match s.as_str() {
            "9_1" => D3D_FEATURE_LEVEL_9_1,
            "10_0" => D3D_FEATURE_LEVEL_10_0,
            "10_1" => D3D_FEATURE_LEVEL_10_1,
            "11_0" => D3D_FEATURE_LEVEL_11_0,
            _ => D3D_FEATURE_LEVEL_11_1,
        });
    let mut last = 0.0;
    report("start", &mut last);
    let comp = vitro_render::compositor::init_compositor()?;
    report("compositor", &mut last);
    // SAFETY: plain object creation; every out-pointer is a local.
    unsafe {
        let factory: IDXGIFactory6 = CreateDXGIFactory1()?;
        let adapter: IDXGIAdapter1 =
            factory.EnumAdapterByGpuPreference(0, DXGI_GPU_PREFERENCE_MINIMUM_POWER)?;
        report("dxgi factory + adapter", &mut last);
        if has("--regions") {
            regions("before d3d");
        }
        let mut flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT;
        if has("--single") {
            flags |= D3D11_CREATE_DEVICE_SINGLETHREADED;
        }
        if has("--nothreads") {
            flags |= D3D11_CREATE_DEVICE_PREVENT_INTERNAL_THREADING_OPTIMIZATIONS;
        }
        if has("--video") {
            flags |= D3D11_CREATE_DEVICE_VIDEO_SUPPORT;
        }
        let levels = fl.map(|l| [l]);
        let mut d3d = None;
        let mut got = D3D_FEATURE_LEVEL::default();
        if has("--warp") {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_WARP,
                HMODULE::default(),
                flags,
                levels.as_ref().map(|l| &l[..]),
                D3D11_SDK_VERSION,
                Some(&mut d3d),
                Some(&mut got),
                None,
            )?;
        } else {
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                flags,
                levels.as_ref().map(|l| &l[..]),
                D3D11_SDK_VERSION,
                Some(&mut d3d),
                Some(&mut got),
                None,
            )?;
        }
        let d3d: ID3D11Device = d3d.unwrap();
        report(&format!("d3d11 device (fl {:#x})", got.0), &mut last);
        if has("--regions") {
            regions("after d3d");
        }
        let kind = if has("--d2d-mt") {
            D2D1_FACTORY_TYPE_MULTI_THREADED
        } else {
            D2D1_FACTORY_TYPE_SINGLE_THREADED
        };
        let d2f: ID2D1Factory1 = D2D1CreateFactory(kind, None)?;
        report("d2d factory", &mut last);
        let d2d = d2f.CreateDevice(&d3d.cast::<IDXGIDevice>()?)?;
        report("d2d device", &mut last);
        let interop: ICompositorInterop = comp.compositor.cast()?;
        let graphics = interop.CreateGraphicsDevice(&d2d)?;
        report("composition graphics device", &mut last);
        let surface = graphics.CreateDrawingSurface(
            windows::Foundation::Size {
                Width: 32.0,
                Height: 32.0,
            },
            windows::Graphics::DirectX::DirectXPixelFormat::B8G8R8A8UIntNormalized,
            windows::Graphics::DirectX::DirectXAlphaMode::Premultiplied,
        )?;
        report("one 32x32 surface", &mut last);
        let mut many = Vec::new();
        for _ in 0..40 {
            many.push(graphics.CreateDrawingSurface(
                windows::Foundation::Size {
                    Width: 32.0,
                    Height: 32.0,
                },
                windows::Graphics::DirectX::DirectXPixelFormat::B8G8R8A8UIntNormalized,
                windows::Graphics::DirectX::DirectXAlphaMode::Premultiplied,
            )?);
        }
        report("40 more (undrawn)", &mut last);
        let pixels = vec![0x80u8; 48 * 48 * 4];
        let targets: Vec<_> = if has("--one") {
            vec![&surface; 41]
        } else {
            many.iter().chain(std::iter::once(&surface)).collect()
        };
        for s in targets {
            use windows::Win32::Graphics::Direct2D::Common::*;
            use windows::Win32::Graphics::Direct2D::*;
            let interop: windows::Win32::System::WinRT::Composition::ICompositionDrawingSurfaceInterop = s.cast()?;
            let mut off = windows::Win32::Foundation::POINT::default();
            let dc: ID2D1DeviceContext = interop.BeginDraw(None, &mut off)?;
            dc.Clear(Some(&D2D1_COLOR_F::default()));
            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                ..Default::default()
            };
            let bmp = dc.CreateBitmap(
                D2D_SIZE_U {
                    width: 32,
                    height: 32,
                },
                Some(pixels.as_ptr().cast()),
                32 * 4,
                &props,
            )?;
            dc.DrawBitmap(&bmp, None, 1.0, D2D1_INTERPOLATION_MODE_LINEAR, None, None);
            interop.EndDraw()?;
        }
        report("41 drawn", &mut last);
        if has("--clear") {
            d2d.ClearResources(0);
            report("d2d ClearResources", &mut last);
        }
        if has("--trim") {
            d3d.cast::<IDXGIDevice3>()?.Trim();
            report("trim", &mut last);
        }
        drop((many, surface, graphics, d2d, d2f, d3d));
        report("all released", &mut last);
    }
    Ok(())
}
