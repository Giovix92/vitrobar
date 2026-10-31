//! vitro-render: GPU device, Windows.UI.Composition compositor and bar materials.

pub mod bar_visual;
pub mod compositor;
pub mod device;
pub mod flyout;
pub mod material;
pub mod menu_view;
pub mod panel;
pub mod panel_view;
pub mod settings_view;
pub mod start_view;
pub mod surfaces;
pub mod tray_flyout;

pub use windows_numerics::{Vector2, Vector3};
