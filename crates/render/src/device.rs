//! The D3D11 + D2D device that draws Vitrobar's content (icons, text, glyphs), wrapped as a
//! composition graphics device.
//!
//! By default the device is WARP, the software rasterizer: content is drawn on the CPU and
//! only when it changes, while DWM still composites the glass (blur, animations,
//! thumbnails) on the GPU. A hardware device costs ~24.5 MB of private bytes in the driver
//! alone (measured on an AMD iGPU, stage 1 Task 16) against ~0.4 MB for WARP, with the same
//! pixels. A hardware device on the minimum-power adapter (never the discrete GPU) is for
//! features that need GPU shaders (liquid glass, stage 4) and is created only while one is on.

use windows::core::{Interface, Result};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HMODULE};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Device, ID2D1Factory1, D2D1_FACTORY_TYPE_SINGLE_THREADED,
};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11Device4, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_CREATE_DEVICE_FLAG, D3D11_CREATE_DEVICE_PREVENT_INTERNAL_THREADING_OPTIMIZATIONS,
    D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter, IDXGIAdapter1, IDXGIDevice, IDXGIFactory6,
    DXGI_GPU_PREFERENCE_MINIMUM_POWER,
};
use windows::Win32::System::Threading::CreateEventW;
use windows::Win32::System::WinRT::Composition::{
    ICompositionGraphicsDeviceInterop, ICompositorInterop,
};
use windows::UI::Composition::{CompositionGraphicsDevice, Compositor};

/// Which D3D device draws the content.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum DeviceKind {
    /// WARP, the software rasterizer (the default: no GPU memory or driver in the process).
    #[default]
    Warp,
    /// A hardware device on DXGI's minimum-power adapter (the integrated GPU on hybrids,
    /// never the discrete one). Only while a feature that needs GPU shaders is enabled.
    MinPowerHardware,
}

/// The device objects used for all drawing.
pub struct GpuDevice {
    pub d3d: ID3D11Device,
    pub d2d: ID2D1Device,
    pub graphics: CompositionGraphicsDevice,
    kind: DeviceKind,
    adapter: String,
    /// Auto-reset event signalled by D3D when the device is removed (TDR, driver update,
    /// adapter change). Wait on it in the message loop, then call [`GpuDevice::recreate`].
    removed_event: HANDLE,
    removed_cookie: Option<u32>,
}

struct Devices {
    d3d: ID3D11Device,
    d2d: ID2D1Device,
    adapter: String,
}

fn utf16_until_nul(w: &[u16]) -> String {
    let len = w.iter().position(|&c| c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..len])
}

fn create_devices(kind: DeviceKind) -> Result<Devices> {
    // SAFETY: plain object creation; every out-pointer is a local.
    unsafe {
        let (adapter, driver, flags): (Option<IDXGIAdapter>, _, D3D11_CREATE_DEVICE_FLAG) =
            match kind {
                DeviceKind::Warp => (None, D3D_DRIVER_TYPE_WARP, D3D11_CREATE_DEVICE_BGRA_SUPPORT),
                DeviceKind::MinPowerHardware => {
                    let factory: IDXGIFactory6 = CreateDXGIFactory1()?;
                    let adapter: IDXGIAdapter1 =
                        factory.EnumAdapterByGpuPreference(0, DXGI_GPU_PREFERENCE_MINIMUM_POWER)?;
                    (
                        Some(adapter.cast()?),
                        D3D_DRIVER_TYPE_UNKNOWN,
                        // No driver worker threads: the content is a few small surfaces drawn
                        // only on change (measured ~1-2 MB less on AMD; no effect on WARP).
                        D3D11_CREATE_DEVICE_BGRA_SUPPORT
                            | D3D11_CREATE_DEVICE_PREVENT_INTERNAL_THREADING_OPTIMIZATIONS,
                    )
                }
            };

        let mut d3d = None;
        D3D11CreateDevice(
            adapter.as_ref(),
            driver,
            HMODULE::default(),
            flags,
            None,
            D3D11_SDK_VERSION,
            Some(&mut d3d),
            None,
            None,
        )?;
        let d3d = d3d.ok_or_else(windows::core::Error::empty)?;
        let dxgi: IDXGIDevice = d3d.cast()?;
        // The adapter the device actually runs on ("Microsoft Basic Render Driver" for WARP).
        let description = utf16_until_nul(&dxgi.GetAdapter()?.GetDesc()?.Description);

        let d2d_factory: ID2D1Factory1 =
            D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
        let d2d = d2d_factory.CreateDevice(&dxgi)?;
        Ok(Devices {
            d3d,
            d2d,
            adapter: description,
        })
    }
}

/// Registers `event` to be signalled when `d3d` is removed. `None` if the runtime
/// lacks ID3D11Device4 (then loss is only seen through [`GpuDevice::is_lost`] or a failed draw).
fn register_removed(d3d: &ID3D11Device, event: HANDLE) -> Option<u32> {
    let d4: ID3D11Device4 = d3d.cast().ok()?;
    // SAFETY: `event` is a valid event handle owned by the `GpuDevice`, which unregisters
    // the cookie (`unregister_removed`) before it closes the handle.
    unsafe { d4.RegisterDeviceRemovedEvent(event).ok() }
}

/// The default drawing device ([`DeviceKind::Warp`]).
pub fn create_gpu_device(compositor: &Compositor) -> Result<GpuDevice> {
    create_gpu_device_of(compositor, DeviceKind::default())
}

pub fn create_gpu_device_of(compositor: &Compositor, kind: DeviceKind) -> Result<GpuDevice> {
    let devices = create_devices(kind)?;
    let interop: ICompositorInterop = compositor.cast()?;
    // SAFETY: `devices.d2d` is a live D2D device; the interop call only adds a reference.
    let graphics = unsafe { interop.CreateGraphicsDevice(&devices.d2d)? };
    // SAFETY: an unnamed auto-reset event with default security; closed in `Drop`.
    let removed_event = unsafe { CreateEventW(None, false, false, None)? };
    let removed_cookie = register_removed(&devices.d3d, removed_event);
    Ok(GpuDevice {
        d3d: devices.d3d,
        d2d: devices.d2d,
        graphics,
        kind,
        adapter: devices.adapter,
        removed_event,
        removed_cookie,
    })
}

pub fn adapter_description(d: &GpuDevice) -> String {
    d.adapter.clone()
}

impl GpuDevice {
    /// Handle to wait on (e.g. with `MsgWaitForMultipleObjectsEx`); signalled on device removal.
    pub fn removed_event(&self) -> HANDLE {
        self.removed_event
    }

    /// True if the D3D device has been removed and must be recreated.
    pub fn is_lost(&self) -> bool {
        // SAFETY: a read-only query on a live device.
        unsafe { self.d3d.GetDeviceRemovedReason().is_err() }
    }

    /// The kind of device this is (kept across [`GpuDevice::recreate`]).
    pub fn kind(&self) -> DeviceKind {
        self.kind
    }

    /// Recreates D3D/D2D of the same [`DeviceKind`] (a hardware device on the current
    /// minimum-power adapter) and swaps them into the existing composition graphics device,
    /// so surfaces created from it stay valid. Their content is lost: the caller must redraw
    /// everything afterwards.
    pub fn recreate(&mut self) -> Result<()> {
        let devices = create_devices(self.kind)?;
        let interop: ICompositionGraphicsDeviceInterop = self.graphics.cast()?;
        // SAFETY: `devices.d2d` is a live D2D device; the composition device keeps its own
        // reference, and surfaces made from it stay valid (their content must be redrawn).
        unsafe { interop.SetRenderingDevice(&devices.d2d)? };
        self.unregister_removed();
        self.removed_cookie = register_removed(&devices.d3d, self.removed_event);
        self.d3d = devices.d3d;
        self.d2d = devices.d2d;
        self.adapter = devices.adapter;
        Ok(())
    }

    fn unregister_removed(&mut self) {
        if let Some(cookie) = self.removed_cookie.take() {
            if let Ok(d4) = self.d3d.cast::<ID3D11Device4>() {
                // SAFETY: `cookie` came from `RegisterDeviceRemovedEvent` on this device and is
                // unregistered once (`take` above).
                unsafe { d4.UnregisterDeviceRemoved(cookie) };
            }
        }
    }
}

impl Drop for GpuDevice {
    fn drop(&mut self) {
        self.unregister_removed();
        // SAFETY: the event handle is owned by this device, no longer registered with D3D,
        // and closed exactly once here.
        unsafe {
            let _ = CloseHandle(self.removed_event);
        }
    }
}
