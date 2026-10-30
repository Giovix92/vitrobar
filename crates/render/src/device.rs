//! D3D11 + D2D device on the minimum-power GPU, wrapped as a composition graphics device.

use windows::core::{Interface, Result};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HMODULE};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Device, ID2D1Factory1, D2D1_FACTORY_TYPE_SINGLE_THREADED,
};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11Device4, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_CREATE_DEVICE_PREVENT_INTERNAL_THREADING_OPTIMIZATIONS, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIDevice, IDXGIFactory6,
    DXGI_GPU_PREFERENCE_MINIMUM_POWER,
};
use windows::Win32::System::Threading::CreateEventW;
use windows::Win32::System::WinRT::Composition::{
    ICompositionGraphicsDeviceInterop, ICompositorInterop,
};
use windows::UI::Composition::{CompositionGraphicsDevice, Compositor};

/// GPU objects used for all drawing. The adapter is always DXGI's minimum-power
/// preference (integrated GPU on hybrids; WARP / Basic Render on GPU-less machines).
pub struct GpuDevice {
    pub d3d: ID3D11Device,
    pub d2d: ID2D1Device,
    pub graphics: CompositionGraphicsDevice,
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

fn create_devices() -> Result<Devices> {
    unsafe {
        let factory: IDXGIFactory6 = CreateDXGIFactory1()?;
        let adapter: IDXGIAdapter1 =
            factory.EnumAdapterByGpuPreference(0, DXGI_GPU_PREFERENCE_MINIMUM_POWER)?;
        let description = utf16_until_nul(&adapter.GetDesc1()?.Description);

        let mut d3d = None;
        D3D11CreateDevice(
            &adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            // No driver worker threads: Vitrobar draws a few small surfaces only on change,
            // and the threads' buffers cost ~1 MB of private bytes (measured with the
            // memprobe example on an AMD iGPU, Task 16).
            D3D11_CREATE_DEVICE_BGRA_SUPPORT
                | D3D11_CREATE_DEVICE_PREVENT_INTERNAL_THREADING_OPTIMIZATIONS,
            None,
            D3D11_SDK_VERSION,
            Some(&mut d3d),
            None,
            None,
        )?;
        let d3d = d3d.ok_or_else(windows::core::Error::empty)?;

        let d2d_factory: ID2D1Factory1 =
            D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
        let d2d = d2d_factory.CreateDevice(&d3d.cast::<IDXGIDevice>()?)?;
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
    unsafe { d4.RegisterDeviceRemovedEvent(event).ok() }
}

pub fn create_gpu_device(compositor: &Compositor) -> Result<GpuDevice> {
    let devices = create_devices()?;
    let interop: ICompositorInterop = compositor.cast()?;
    let graphics = unsafe { interop.CreateGraphicsDevice(&devices.d2d)? };
    let removed_event = unsafe { CreateEventW(None, false, false, None)? };
    let removed_cookie = register_removed(&devices.d3d, removed_event);
    Ok(GpuDevice {
        d3d: devices.d3d,
        d2d: devices.d2d,
        graphics,
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
        unsafe { self.d3d.GetDeviceRemovedReason().is_err() }
    }

    /// Recreates D3D/D2D on the current minimum-power adapter and swaps them into the
    /// existing composition graphics device, so surfaces created from it stay valid.
    /// Their content is lost: the caller must redraw everything afterwards.
    pub fn recreate(&mut self) -> Result<()> {
        let devices = create_devices()?;
        let interop: ICompositionGraphicsDeviceInterop = self.graphics.cast()?;
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
                unsafe { d4.UnregisterDeviceRemoved(cookie) };
            }
        }
    }
}

impl Drop for GpuDevice {
    fn drop(&mut self) {
        self.unregister_removed();
        unsafe {
            let _ = CloseHandle(self.removed_event);
        }
    }
}
