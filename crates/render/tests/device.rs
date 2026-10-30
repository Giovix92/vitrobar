//! Content is drawn with WARP by default; a hardware device, when asked for, is always on
//! the minimum-power GPU (integrated on hybrid laptops), never the discrete one.

use vitro_core::style::Rgba;
use vitro_render::compositor::init_compositor;
use vitro_render::device::{
    adapter_description, create_gpu_device, create_gpu_device_of, DeviceKind,
};
use vitro_render::surfaces::{FontKind, Painter};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter, IDXGIAdapter1, IDXGIFactory4, IDXGIFactory6,
    DXGI_GPU_PREFERENCE_MINIMUM_POWER,
};

/// Enumerates DXGI on its own, independently of `vitro_render::device`.
fn min_power_adapter_description() -> String {
    unsafe {
        let factory: IDXGIFactory6 = CreateDXGIFactory1().unwrap();
        let adapter: IDXGIAdapter1 = factory
            .EnumAdapterByGpuPreference(0, DXGI_GPU_PREFERENCE_MINIMUM_POWER)
            .unwrap();
        let desc = adapter.GetDesc1().unwrap();
        let len = desc
            .Description
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(desc.Description.len());
        String::from_utf16_lossy(&desc.Description[..len])
    }
}

/// WARP's adapter, enumerated on its own.
fn warp_adapter_description() -> String {
    unsafe {
        let factory: IDXGIFactory4 = CreateDXGIFactory1().unwrap();
        let adapter: IDXGIAdapter = factory.EnumWarpAdapter().unwrap();
        let desc = adapter.GetDesc().unwrap();
        let len = desc
            .Description
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(desc.Description.len());
        String::from_utf16_lossy(&desc.Description[..len])
    }
}

#[test]
fn default_device_is_warp() {
    let comp = init_compositor().unwrap();
    let d = create_gpu_device(&comp).unwrap();
    assert_eq!(d.kind(), DeviceKind::Warp);
    let expected = warp_adapter_description();
    println!("WARP adapter: {expected}");
    assert_eq!(adapter_description(&d), expected);
}

#[test]
fn hardware_device_uses_min_power_adapter() {
    let comp = init_compositor().unwrap();
    let d = create_gpu_device_of(&comp, DeviceKind::MinPowerHardware).unwrap();
    assert_eq!(d.kind(), DeviceKind::MinPowerHardware);
    let expected = min_power_adapter_description();
    println!("min-power adapter: {expected}");
    assert_eq!(adapter_description(&d), expected);
}

/// The device-lost path: a recreated device keeps its kind and the surfaces still draw.
#[test]
fn recreate_keeps_the_kind_and_draws() {
    let comp = init_compositor().unwrap();
    for kind in [DeviceKind::Warp, DeviceKind::MinPowerHardware] {
        let mut d = create_gpu_device_of(&comp, kind).unwrap();
        let before = adapter_description(&d);
        d.recreate().unwrap();
        assert_eq!(d.kind(), kind);
        assert_eq!(adapter_description(&d), before);
        assert!(!d.is_lost());
        let p = Painter::new(&d).unwrap();
        p.text(
            "12:34",
            FontKind::Text,
            14.0,
            Rgba(255, 255, 255, 255),
            60,
            20,
        )
        .unwrap();
    }
}
