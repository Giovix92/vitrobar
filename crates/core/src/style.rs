//! Visual tokens for the bar material and hover effects (pure data).

use crate::settings::{HoverEffect, Material};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Theme {
    Light,
    Dark,
}

/// Straight (non-premultiplied) colour, alpha 0..=255.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgba(pub u8, pub u8, pub u8, pub u8);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaterialStyle {
    pub backdrop_blur: bool,
    pub tint: Rgba,
    pub border: Rgba,
    pub highlight: Rgba,
    pub text: Rgba,
}

const TEXT_DARK_ON_LIGHT: Rgba = Rgba(17, 17, 17, 255);
const TEXT_WHITE: Rgba = Rgba(255, 255, 255, 255);

fn solid_black() -> MaterialStyle {
    MaterialStyle {
        backdrop_blur: false,
        tint: Rgba(28, 28, 30, 255),
        border: Rgba(255, 255, 255, 20),
        highlight: Rgba(255, 255, 255, 0),
        text: TEXT_WHITE,
    }
}

fn solid_white() -> MaterialStyle {
    MaterialStyle {
        backdrop_blur: false,
        tint: Rgba(245, 245, 247, 255),
        border: Rgba(0, 0, 0, 20),
        highlight: Rgba(255, 255, 255, 0),
        text: TEXT_DARK_ON_LIGHT,
    }
}

pub fn material_style(m: Material, t: Theme, transparency_enabled: bool) -> MaterialStyle {
    match m {
        Material::SolidBlack => solid_black(),
        Material::SolidWhite => solid_white(),
        _ if !transparency_enabled => match t {
            Theme::Dark => solid_black(),
            Theme::Light => solid_white(),
        },
        Material::Smoked => MaterialStyle {
            backdrop_blur: true,
            tint: Rgba(30, 20, 50, 140),
            border: match t {
                Theme::Light => Rgba(255, 255, 255, 89),
                Theme::Dark => Rgba(255, 255, 255, 38),
            },
            highlight: match t {
                Theme::Light => Rgba(255, 255, 255, 128),
                Theme::Dark => Rgba(255, 255, 255, 51),
            },
            text: TEXT_WHITE,
        },
        // Liquid is Frosted in stage 1.
        Material::Frosted | Material::Liquid => match t {
            Theme::Light => MaterialStyle {
                backdrop_blur: true,
                tint: Rgba(255, 255, 255, 51),
                border: Rgba(255, 255, 255, 89),
                highlight: Rgba(255, 255, 255, 128),
                text: TEXT_DARK_ON_LIGHT,
            },
            Theme::Dark => MaterialStyle {
                backdrop_blur: true,
                tint: Rgba(32, 32, 32, 102),
                border: Rgba(255, 255, 255, 38),
                highlight: Rgba(255, 255, 255, 51),
                text: TEXT_WHITE,
            },
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HoverStyle {
    pub duration_ms: u32,
    pub lift: f32,
    pub scale: f32,
    pub neighbour_scale: f32,
    pub plate: bool,
    pub sweep: bool,
}

pub const ALL_HOVERS: &[HoverEffect] = &[
    HoverEffect::Plate,
    HoverEffect::Lift,
    HoverEffect::Magnify,
    HoverEffect::Sweep,
];

pub fn hover_style(h: HoverEffect) -> HoverStyle {
    let base = HoverStyle {
        duration_ms: 150,
        lift: 0.0,
        scale: 1.0,
        neighbour_scale: 1.0,
        plate: false,
        sweep: false,
    };
    match h {
        HoverEffect::Plate => HoverStyle {
            plate: true,
            ..base
        },
        HoverEffect::Lift => HoverStyle {
            duration_ms: 220,
            lift: 5.0,
            ..base
        },
        HoverEffect::Magnify => HoverStyle {
            duration_ms: 180,
            scale: 1.55,
            neighbour_scale: 1.22,
            ..base
        },
        // 450 ms is the light streak (ruling R1); lift/scale motion stays <= 250 ms in the renderer.
        HoverEffect::Sweep => HoverStyle {
            duration_ms: 450,
            lift: 2.0,
            scale: 1.06,
            sweep: true,
            ..base
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{HoverEffect, Material};

    #[test]
    fn transparency_off_forces_solid() {
        assert!(!material_style(Material::Frosted, Theme::Dark, false).backdrop_blur);
        assert_eq!(
            material_style(Material::Smoked, Theme::Dark, false),
            material_style(Material::SolidBlack, Theme::Dark, true)
        );
        assert_eq!(
            material_style(Material::Frosted, Theme::Light, false),
            material_style(Material::SolidWhite, Theme::Light, true)
        );
    }
    #[test]
    fn liquid_falls_back_to_frosted_in_stage1() {
        assert_eq!(
            material_style(Material::Liquid, Theme::Light, true),
            material_style(Material::Frosted, Theme::Light, true)
        );
    }
    #[test]
    fn pinned_material_values() {
        let f = material_style(Material::Frosted, Theme::Light, true);
        assert_eq!(f.tint, Rgba(255, 255, 255, 51));
        assert_eq!(f.border, Rgba(255, 255, 255, 89));
        assert_eq!(f.highlight, Rgba(255, 255, 255, 128));
        assert_eq!(f.text, Rgba(17, 17, 17, 255));
        let d = material_style(Material::Frosted, Theme::Dark, true);
        assert_eq!(d.tint, Rgba(32, 32, 32, 102));
        assert_eq!(d.border, Rgba(255, 255, 255, 38));
        assert_eq!(d.highlight, Rgba(255, 255, 255, 51));
        let s = material_style(Material::Smoked, Theme::Light, true);
        assert_eq!(s.tint, Rgba(30, 20, 50, 140));
        assert!(s.backdrop_blur);
        assert_eq!(s.text, Rgba(255, 255, 255, 255));
        let b = material_style(Material::SolidBlack, Theme::Light, true);
        assert_eq!(b.tint, Rgba(28, 28, 30, 255));
        assert_eq!(b.border, Rgba(255, 255, 255, 20));
        assert!(!b.backdrop_blur);
        let w = material_style(Material::SolidWhite, Theme::Dark, true);
        assert_eq!(w.tint, Rgba(245, 245, 247, 255));
        assert_eq!(w.border, Rgba(0, 0, 0, 20));
        assert_eq!(w.text, Rgba(17, 17, 17, 255));
        assert_eq!(w.highlight.3, 0);
    }
    #[test]
    fn hover_durations_in_range() {
        for &h in ALL_HOVERS {
            let d = hover_style(h).duration_ms;
            assert!((150..=450).contains(&d));
        }
    }
    #[test]
    fn pinned_hover_values() {
        let p = hover_style(HoverEffect::Plate);
        assert_eq!(
            (p.duration_ms, p.lift, p.scale, p.neighbour_scale),
            (150, 0.0, 1.0, 1.0)
        );
        assert!(p.plate && !p.sweep);
        let l = hover_style(HoverEffect::Lift);
        assert_eq!((l.duration_ms, l.lift), (220, 5.0));
        let m = hover_style(HoverEffect::Magnify);
        assert_eq!(
            (m.duration_ms, m.scale, m.neighbour_scale),
            (180, 1.55, 1.22)
        );
        let s = hover_style(HoverEffect::Sweep);
        assert_eq!((s.duration_ms, s.lift, s.scale), (450, 2.0, 1.06));
        assert!(s.sweep && !s.plate);
    }
}
