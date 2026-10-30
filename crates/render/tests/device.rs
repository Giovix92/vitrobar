//! The bar must render on the minimum-power GPU (integrated on hybrid laptops).

use vitro_render::compositor::init_compositor;
use vitro_render::device::{adapter_description, create_gpu_device};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory6, DXGI_GPU_PREFERENCE_MINIMUM_POWER,
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

#[test]
fn device_uses_min_power_adapter() {
    let comp = init_compositor().unwrap();
    let d = create_gpu_device(&comp).unwrap();
    let expected = min_power_adapter_description();
    println!("min-power adapter: {expected}");
    assert_eq!(adapter_description(&d), expected);
}
