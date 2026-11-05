//! The settings window's content: geometry (pure, shared with the app's controller for
//! hit-testing) and a retained view built on [`StartView`]'s layers.
//!
//! The window is a [`Panel`] of [`WINDOW_W_DIP`] x [`WINDOW_H_DIP`]. Its top
//! [`DRAG_DIP`] are the caption (title and the close button, the Chrome layer); below it a
//! clipped viewport shows the page (the List layer), which scrolls by moving its container.
//! The page is the control list of [`vitro_core::settings_ui::build_model`] in content DIP:
//! a label on the left of each row, the control on the right.
//!
//! Every item is keyed by its content (see [`StartView`]): a hover, a press, a new selection
//! or a scroll redraws only the visuals that changed.

use vitro_core::i18n::{tr, Key, Lang};
use vitro_core::layout::Rect;
use vitro_core::settings_ui::{Control, ControlKind, Section, SettingsModel, SECTIONS};
use vitro_core::style::{MaterialStyle, Rgba};
use windows::core::Result;

use crate::flyout::PxRect;
use crate::panel::Panel;
use crate::start_view::{Item, Layer, Part, StartView};
use crate::surfaces::{FontKind, Painter};

/// The category sidebar on the left, under the caption.
pub const NAV_W_DIP: f32 = 200.0;
/// One category's row in the sidebar.
pub const NAV_ROW_DIP: f32 = 40.0;
const NAV_PAD_DIP: f32 = 8.0;
const NAV_TEXT_DIP: f32 = 14.0;
const NAV_GLYPH_DIP: f32 = 16.0;
/// The page column right of the sidebar.
pub const PAGE_W_DIP: f32 = 720.0;
pub const WINDOW_W_DIP: f32 = NAV_W_DIP + PAGE_W_DIP;
pub const WINDOW_H_DIP: f32 = 640.0;
/// The caption band at the top: drags the window.
pub const DRAG_DIP: f32 = 40.0;
/// The close button at the band's right end.
pub const CLOSE_W_DIP: f32 = 46.0;
pub const RADIUS_DIP: f32 = 8.0;
/// Close glyph (Segoe Fluent Icons "ChromeClose").
pub const CLOSE_GLYPH: &str = "\u{E8BB}";

/// Room under the last row.
const BOTTOM_PAD_DIP: f32 = 24.0;
/// Height of a section header (above each section's first row; see `build_model`).
const HEADER_DIP: f32 = 36.0;
const PILL_H_DIP: f32 = 32.0;
const SWATCH_DIP: f32 = 28.0;
const SWATCH_GAP_DIP: f32 = 12.0;
const SWITCH_W_DIP: f32 = 40.0;
const SWITCH_H_DIP: f32 = 20.0;
/// The column a label gets when the control leaves it room (text, links).
const LABEL_MIN_DIP: f32 = 224.0;
const GAP_DIP: f32 = 16.0;

const TITLE_DIP: f32 = 12.0;
const SECTION_DIP: f32 = 18.0;
const LABEL_DIP: f32 = 14.0;
const OPTION_DIP: f32 = 13.0;
const HINT_DIP: f32 = 11.0;
const GLYPH_DIP: f32 = 10.0;

const WHITE: Rgba = Rgba(255, 255, 255, 255);
/// The close button's hover plate (as in Windows' caption buttons).
const CLOSE_RED: Rgba = Rgba(0xC4, 0x2B, 0x1C, 255);

/// A clickable part of the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Spot {
    Close,
    /// Option `choice` of control `control` (a segment of a Choice, a swatch).
    Option {
        control: usize,
        choice: usize,
    },
    /// The switch of a Toggle control.
    Toggle(usize),
    /// A link control.
    Link(usize),
    /// The sidebar's category `i` (see [`SECTIONS`]).
    Nav(usize),
}

/// The page column's width in a window `window_w_dip` wide.
pub fn page_w(window_w_dip: f32) -> f32 {
    (window_w_dip - NAV_W_DIP).max(0.0)
}

/// The sidebar row of category `i` (window DIP).
pub fn nav_rect(i: usize) -> Rect {
    Rect {
        x: NAV_PAD_DIP,
        y: DRAG_DIP + NAV_PAD_DIP + NAV_ROW_DIP * i as f32,
        w: NAV_W_DIP - 2.0 * NAV_PAD_DIP,
        h: NAV_ROW_DIP - 4.0,
    }
}

/// The sidebar category under `pt` (window DIP).
pub fn nav_hit(pt: (f32, f32)) -> Option<usize> {
    (0..SECTIONS.len()).find(|&i| {
        let r = nav_rect(i);
        pt.0 >= r.x && pt.0 < r.right() && pt.1 >= r.y && pt.1 < r.bottom()
    })
}

/// Width of one segment of a Choice of `n` options.
fn segment_w(n: usize) -> f32 {
    match n {
        0..=2 => 168.0,
        3 => 128.0,
        _ => 124.0,
    }
}

/// The control's own part of its row (right-aligned), and the label column left of it.
fn columns(c: &Control) -> (Rect, Rect) {
    let r = c.rect;
    let control_w = match &c.kind {
        ControlKind::Choice { options, .. } => segment_w(options.len()) * options.len() as f32,
        ControlKind::Toggle(_) => SWITCH_W_DIP,
        ControlKind::Swatches { colors, .. } => {
            let n = colors.len() as f32;
            n * SWATCH_DIP + (n - 1.0).max(0.0) * SWATCH_GAP_DIP
        }
        // A note row (its text is its label) has no control part: the note gets the row.
        ControlKind::Text(k) if *k == c.label => 0.0,
        ControlKind::Link(_) | ControlKind::Text(_) => r.w - LABEL_MIN_DIP - GAP_DIP,
    }
    .min(r.w - GAP_DIP)
    .max(0.0);
    let control = Rect {
        x: r.right() - control_w,
        y: r.y,
        w: control_w,
        h: r.h,
    };
    let label = Rect {
        x: r.x,
        y: r.y,
        w: (r.w - control_w - GAP_DIP).max(0.0),
        h: r.h,
    };
    (label, control)
}

fn centred_v(area: Rect, x: f32, w: f32, h: f32) -> Rect {
    Rect {
        x,
        y: area.y + (area.h - h) / 2.0,
        w,
        h,
    }
}

/// The clickable parts of control `index` (content DIP).
pub fn parts(c: &Control, index: usize) -> Vec<(Spot, Rect)> {
    let (_, area) = columns(c);
    match &c.kind {
        ControlKind::Choice { options, .. } => {
            let w = segment_w(options.len());
            (0..options.len())
                .map(|i| {
                    let spot = Spot::Option {
                        control: index,
                        choice: i,
                    };
                    (spot, centred_v(area, area.x + w * i as f32, w, PILL_H_DIP))
                })
                .collect()
        }
        ControlKind::Toggle(_) => vec![(
            Spot::Toggle(index),
            centred_v(area, area.x, SWITCH_W_DIP, SWITCH_H_DIP + 8.0),
        )],
        ControlKind::Swatches { colors, .. } => (0..colors.len())
            .map(|i| {
                let x = area.x + (SWATCH_DIP + SWATCH_GAP_DIP) * i as f32;
                let spot = Spot::Option {
                    control: index,
                    choice: i,
                };
                (spot, centred_v(area, x, SWATCH_DIP, SWATCH_DIP))
            })
            .collect(),
        ControlKind::Link(_) => vec![(Spot::Link(index), centred_v(area, area.x, area.w, 28.0))],
        ControlKind::Text(_) => Vec::new(),
    }
}

/// Where the focus ring of control `c` goes (content DIP): around its control part.
pub fn focus_rect(c: &Control) -> Option<Rect> {
    let (_, area) = columns(c);
    let inflate = |r: Rect, d: f32| Rect {
        x: r.x - d,
        y: r.y - d,
        w: r.w + 2.0 * d,
        h: r.h + 2.0 * d,
    };
    match &c.kind {
        ControlKind::Choice { .. } => {
            Some(inflate(centred_v(area, area.x, area.w, PILL_H_DIP), 3.0))
        }
        ControlKind::Toggle(_) => Some(inflate(
            centred_v(area, area.x, SWITCH_W_DIP, SWITCH_H_DIP),
            4.0,
        )),
        ControlKind::Swatches { .. } => {
            Some(inflate(centred_v(area, area.x, area.w, SWATCH_DIP), 5.0))
        }
        ControlKind::Link(_) => Some(inflate(centred_v(area, area.x, area.w, 28.0), 0.0)),
        ControlKind::Text(_) => None,
    }
}

/// The close button (window DIP).
pub fn close_rect(window_w_dip: f32) -> Rect {
    Rect {
        x: window_w_dip - CLOSE_W_DIP,
        y: 0.0,
        w: CLOSE_W_DIP,
        h: DRAG_DIP,
    }
}

/// Height of the scrolled page (content DIP).
pub fn content_height(m: &SettingsModel) -> f32 {
    m.controls.last().map_or(0.0, |c| c.rect.bottom()) + BOTTOM_PAD_DIP
}

/// The page's viewport height in a window `window_h_dip` tall.
pub fn viewport_h(window_h_dip: f32) -> f32 {
    (window_h_dip - DRAG_DIP).max(0.0)
}

/// Highest scroll offset (DIP) for a viewport `view_h` tall.
pub fn max_scroll(m: &SettingsModel, view_h: f32) -> f32 {
    (content_height(m) - view_h).max(0.0)
}

/// The scroll offset that shows control `i` whole (with its section header when it opens a
/// section), starting from `scroll`.
pub fn reveal(m: &SettingsModel, i: usize, scroll: f32, view_h: f32) -> f32 {
    let Some(c) = m.controls.get(i) else {
        return scroll;
    };
    let opens_section = i == 0 || m.controls[i - 1].section != c.section;
    let top = if i == 0 {
        // The page's top padding too.
        0.0
    } else if opens_section {
        c.rect.y - HEADER_DIP
    } else {
        c.rect.y
    };
    let s = if top < scroll {
        top
    } else if c.rect.bottom() > scroll + view_h {
        c.rect.bottom() - view_h + 12.0
    } else {
        scroll
    };
    s.clamp(0.0, max_scroll(m, view_h))
}

/// The part of the page under `pt` (content DIP).
pub fn hit(m: &SettingsModel, pt: (f32, f32)) -> Option<Spot> {
    let inside = |r: &Rect| pt.0 >= r.x && pt.0 < r.right() && pt.1 >= r.y && pt.1 < r.bottom();
    m.controls
        .iter()
        .enumerate()
        .flat_map(|(i, c)| parts(c, i))
        .find(|(_, r)| inside(r))
        .map(|(s, _)| s)
}

/// Colours and strings the window is drawn with.
pub struct SettingsLook<'a> {
    pub style: &'a MaterialStyle,
    /// The accent in use (the chosen one, else the system's).
    pub accent: Rgba,
    /// The system accent (the "System" swatch).
    pub system_accent: Rgba,
    pub lang: Lang,
    pub title: &'a str,
    /// The category shown (highlighted in the sidebar).
    pub page: Section,
    /// The text of the About section's version row.
    pub version: &'a str,
    /// A note under the label of a control: (control index, text).
    pub hint: Option<(usize, &'a str)>,
}

/// What the pointer and the keyboard are doing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Interaction {
    pub hover: Option<Spot>,
    pub pressed: Option<Spot>,
    /// The control with the focus ring (`None`: no ring; it shows after keyboard use).
    pub ring: Option<usize>,
}

fn alpha(c: Rgba, a: u8) -> Rgba {
    Rgba(c.0, c.1, c.2, a)
}

fn px_rect(r: Rect, scale: f32) -> PxRect {
    let p = |v: f32| (v * scale).round() as i32;
    (p(r.x), p(r.y), p(r.right()), p(r.bottom()))
}

/// Builds the window's items: (caption, page). The caption is in window px; the page is in
/// content px (the viewport scrolls it).
pub fn items(
    m: &SettingsModel,
    look: &SettingsLook,
    ui: &Interaction,
    window_w_dip: f32,
    scale: f32,
) -> (Vec<Item>, Vec<Item>) {
    let px = |r: Rect| px_rect(r, scale);
    let text = look.style.text;
    let secondary = alpha(text, 0xA0);
    let font = |dip: f32| dip * scale;
    let mut chrome = Vec::new();

    // Caption: the title and the close button.
    let close = close_rect(window_w_dip);
    chrome.push(Item {
        rect: px(Rect {
            x: 16.0,
            y: 0.0,
            w: (close.x - 16.0).max(0.0),
            h: DRAG_DIP,
        }),
        part: Part::Text {
            text: look.title.to_string(),
            font: FontKind::Text,
            size_px: font(TITLE_DIP),
            color: text,
            centred: false,
        },
    });
    // The sidebar: a row per category; the current one has a plate and an accent mark.
    for (i, sec) in SECTIONS.iter().enumerate() {
        let r = nav_rect(i);
        let current = *sec == look.page;
        let hot = ui.hover == Some(Spot::Nav(i));
        let down = hot && ui.pressed == Some(Spot::Nav(i));
        let plate = match (current, hot) {
            (true, _) => Some(alpha(text, if down { 0x22 } else { 0x1A })),
            (false, true) => Some(alpha(text, if down { 0x18 } else { 0x0F })),
            _ => None,
        };
        if let Some(color) = plate {
            chrome.push(Item {
                rect: px(r),
                part: Part::Plate {
                    color,
                    radius_px: 6.0 * scale,
                },
            });
        }
        if current {
            chrome.push(Item {
                rect: px(Rect {
                    x: r.x,
                    y: r.y + r.h / 2.0 - 8.0,
                    w: 3.0,
                    h: 16.0,
                }),
                part: Part::Plate {
                    color: look.accent,
                    radius_px: 1.5 * scale,
                },
            });
        }
        chrome.push(Item {
            rect: px(Rect {
                x: r.x + 12.0,
                w: 20.0,
                ..r
            }),
            part: Part::Text {
                text: sec.glyph().to_string(),
                font: FontKind::Glyph,
                size_px: font(NAV_GLYPH_DIP),
                color: text,
                centred: true,
            },
        });
        chrome.push(Item {
            rect: px(Rect {
                x: r.x + 44.0,
                w: (r.w - 52.0).max(0.0),
                ..r
            }),
            part: Part::Text {
                text: tr(look.lang, sec.title()).to_string(),
                font: FontKind::Text,
                size_px: font(NAV_TEXT_DIP),
                color: if current { text } else { alpha(text, 0xD0) },
                centred: false,
            },
        });
    }

    let close_hot = ui.hover == Some(Spot::Close);
    if close_hot {
        let pressed = ui.pressed == Some(Spot::Close);
        chrome.push(Item {
            rect: px(close),
            part: Part::Plate {
                color: alpha(CLOSE_RED, if pressed { 0xCC } else { 0xFF }),
                radius_px: RADIUS_DIP * scale,
            },
        });
    }
    chrome.push(Item {
        rect: px(close),
        part: Part::Text {
            text: CLOSE_GLYPH.to_string(),
            font: FontKind::Glyph,
            size_px: font(GLYPH_DIP),
            color: if close_hot { WHITE } else { text },
            centred: true,
        },
    });

    let mut page = Vec::new();
    for (i, c) in m.controls.iter().enumerate() {
        if i == 0 || m.controls[i - 1].section != c.section {
            page.push(Item {
                rect: px(Rect {
                    x: c.rect.x,
                    y: c.rect.y - HEADER_DIP,
                    w: c.rect.w,
                    h: HEADER_DIP,
                }),
                part: Part::Text {
                    text: tr(look.lang, c.section.title()).to_string(),
                    font: FontKind::Text,
                    size_px: font(SECTION_DIP),
                    color: text,
                    centred: false,
                },
            });
        }
        let (label, area) = columns(c);
        let hint = look.hint.filter(|(k, _)| *k == i).map(|(_, h)| h);
        let label_box = match hint {
            Some(_) => Rect {
                h: 28.0,
                y: label.y + 2.0,
                ..label
            },
            None => label,
        };
        let note = is_note(c);
        page.push(Item {
            rect: px(label_box),
            part: Part::Text {
                text: tr(look.lang, c.label).to_string(),
                font: FontKind::Text,
                size_px: font(if note { HINT_DIP } else { LABEL_DIP }),
                color: if note { secondary } else { text },
                centred: false,
            },
        });
        if let Some(h) = hint {
            page.push(Item {
                rect: px(Rect {
                    y: label.y + 26.0,
                    h: 18.0,
                    ..label
                }),
                part: Part::Text {
                    text: h.to_string(),
                    font: FontKind::Text,
                    size_px: font(HINT_DIP),
                    color: secondary,
                    centred: false,
                },
            });
        }
        control_items(&mut page, c, i, area, look, ui, scale);
        if ui.ring == Some(i) {
            if let Some(r) = focus_rect(c) {
                page.push(Item {
                    rect: px(r),
                    part: Part::Outline {
                        color: text,
                        radius_px: 8.0 * scale,
                        thickness_px: (2.0 * scale).round().max(1.0),
                    },
                });
            }
        }
    }
    (chrome, page)
}

/// A line of explanation under the row above it (a `Text` control whose text is its label).
fn is_note(c: &Control) -> bool {
    matches!(c.kind, ControlKind::Text(k) if k == c.label)
}

fn control_items(
    page: &mut Vec<Item>,
    c: &Control,
    i: usize,
    area: Rect,
    look: &SettingsLook,
    ui: &Interaction,
    scale: f32,
) {
    let px = |r: Rect| px_rect(r, scale);
    let text = look.style.text;
    let hot = |s: Spot| ui.hover == Some(s);
    let down = |s: Spot| ui.pressed == Some(s) && ui.hover == Some(s);
    match &c.kind {
        ControlKind::Choice { options, selected } => {
            let track = centred_v(area, area.x, area.w, PILL_H_DIP);
            page.push(Item {
                rect: px(track),
                part: Part::Plate {
                    color: alpha(text, 0x14),
                    radius_px: PILL_H_DIP / 2.0 * scale,
                },
            });
            for ((spot, r), key) in parts(c, i).into_iter().zip(options) {
                let Spot::Option { choice, .. } = spot else {
                    continue;
                };
                let inner = Rect {
                    x: r.x + 2.0,
                    y: r.y + 2.0,
                    w: r.w - 4.0,
                    h: r.h - 4.0,
                };
                let chosen = choice == *selected;
                if chosen {
                    page.push(Item {
                        rect: px(inner),
                        part: Part::Plate {
                            color: look.accent,
                            radius_px: inner.h / 2.0 * scale,
                        },
                    });
                } else if hot(spot) {
                    page.push(Item {
                        rect: px(inner),
                        part: Part::Plate {
                            color: alpha(text, if down(spot) { 0x0C } else { 0x1C }),
                            radius_px: inner.h / 2.0 * scale,
                        },
                    });
                }
                page.push(Item {
                    rect: px(Rect {
                        x: inner.x + 2.0,
                        w: inner.w - 4.0,
                        ..inner
                    }),
                    part: Part::Text {
                        text: tr(look.lang, *key).to_string(),
                        font: FontKind::Text,
                        size_px: OPTION_DIP * scale,
                        color: if chosen { WHITE } else { text },
                        centred: true,
                    },
                });
            }
        }
        ControlKind::Toggle(on) => {
            let spot = Spot::Toggle(i);
            let track = centred_v(area, area.x, SWITCH_W_DIP, SWITCH_H_DIP);
            let knob_d = SWITCH_H_DIP - 8.0 + if hot(spot) { 2.0 } else { 0.0 };
            let knob_x = if *on {
                track.right() - 4.0 - knob_d + if hot(spot) { 1.0 } else { 0.0 }
            } else {
                track.x + 4.0 - if hot(spot) { 1.0 } else { 0.0 }
            };
            if *on {
                page.push(Item {
                    rect: px(track),
                    part: Part::Plate {
                        color: look.accent,
                        radius_px: SWITCH_H_DIP / 2.0 * scale,
                    },
                });
            } else {
                page.push(Item {
                    rect: px(track),
                    part: Part::Outline {
                        color: alpha(text, 0xC0),
                        radius_px: SWITCH_H_DIP / 2.0 * scale,
                        thickness_px: scale.round().max(1.0),
                    },
                });
            }
            page.push(Item {
                rect: px(centred_v(track, knob_x, knob_d, knob_d)),
                part: Part::Plate {
                    color: if *on { WHITE } else { alpha(text, 0xC0) },
                    radius_px: knob_d / 2.0 * scale,
                },
            });
        }
        ControlKind::Swatches { colors, selected } => {
            for ((spot, r), color) in parts(c, i).into_iter().zip(colors) {
                let Spot::Option { choice, .. } = spot else {
                    continue;
                };
                let fill = color.map_or(look.system_accent, |c| {
                    Rgba((c >> 16) as u8, (c >> 8) as u8, c as u8, 255)
                });
                if choice == *selected || hot(spot) {
                    let ring = Rect {
                        x: r.x - 4.0,
                        y: r.y - 4.0,
                        w: r.w + 8.0,
                        h: r.h + 8.0,
                    };
                    page.push(Item {
                        rect: px(ring),
                        part: Part::Outline {
                            color: alpha(text, if choice == *selected { 0xFF } else { 0x60 }),
                            radius_px: ring.w / 2.0 * scale,
                            thickness_px: (2.0 * scale).round().max(1.0),
                        },
                    });
                }
                page.push(Item {
                    rect: px(r),
                    part: Part::Plate {
                        color: fill,
                        radius_px: r.w / 2.0 * scale,
                    },
                });
                if color.is_none() {
                    // "System": follows the Windows accent.
                    page.push(Item {
                        rect: px(r),
                        part: Part::Text {
                            text: "A".into(),
                            font: FontKind::Text,
                            size_px: 12.0 * scale,
                            color: WHITE,
                            centred: true,
                        },
                    });
                }
            }
        }
        ControlKind::Link(url) => {
            let spot = Spot::Link(i);
            let r = centred_v(area, area.x, area.w, 28.0);
            if hot(spot) {
                page.push(Item {
                    rect: px(r),
                    part: Part::Plate {
                        color: alpha(text, if down(spot) { 0x0C } else { 0x14 }),
                        radius_px: 6.0 * scale,
                    },
                });
            }
            page.push(Item {
                rect: px(Rect {
                    x: r.x + 8.0,
                    w: r.w - 16.0,
                    ..r
                }),
                part: Part::Text {
                    text: url.to_string(),
                    font: FontKind::Text,
                    size_px: OPTION_DIP * scale,
                    color: look.accent,
                    centred: false,
                },
            });
        }
        // A note is drawn by its label.
        ControlKind::Text(_) if is_note(c) => {}
        ControlKind::Text(key) => {
            let value = match key {
                Key::Version => look.version.to_string(),
                k => tr(look.lang, *k).to_string(),
            };
            page.push(Item {
                rect: px(Rect {
                    x: area.x + 8.0,
                    w: area.w - 8.0,
                    ..area
                }),
                part: Part::Text {
                    text: value,
                    font: FontKind::Text,
                    size_px: OPTION_DIP * scale,
                    color: alpha(text, 0xC0),
                    centred: false,
                },
            });
        }
    }
}

/// The visuals of an open settings window. Dropping it removes them from the panel.
pub struct SettingsView {
    view: StartView,
}

impl SettingsView {
    /// Adds the (empty) view on top of `panel`'s glass.
    pub fn new(panel: &Panel) -> Result<Self> {
        Ok(Self {
            view: StartView::new(panel)?,
        })
    }

    /// Draws the window `size_px` big at `scale` (only what changed is drawn again), the page
    /// scrolled by `scroll_dip`.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        painter: &Painter,
        m: &SettingsModel,
        look: &SettingsLook,
        ui: &Interaction,
        scroll_dip: f32,
        size_px: (i32, i32),
        scale: f32,
    ) -> Result<()> {
        let (chrome, page) = items(m, look, ui, size_px.0 as f32 / scale, scale);
        self.view.set_layer(Layer::Chrome, painter, chrome)?;
        let top = (DRAG_DIP * scale).round() as i32;
        let left = (NAV_W_DIP * scale).round() as i32;
        self.view
            .set_viewport(Some((left, top, size_px.0, size_px.1)))?;
        self.view.set_scroll(scroll_dip * scale)?;
        self.view.set_layer(Layer::List, painter, page)
    }

    /// Visuals currently shown (tests and diagnostics).
    pub fn visual_count(&self) -> usize {
        self.view.visual_count(Layer::Chrome) + self.view.visual_count(Layer::List)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vitro_core::settings::Settings;
    use vitro_core::settings_ui::{build_model, ControlId};
    use vitro_core::style::{material_style, Theme};

    fn model() -> SettingsModel {
        build_model(&Settings::default(), WINDOW_W_DIP)
    }

    fn index(m: &SettingsModel, id: ControlId) -> usize {
        m.controls.iter().position(|c| c.id == id).unwrap()
    }

    fn centre(r: Rect) -> (f32, f32) {
        (r.x + r.w / 2.0, r.y + r.h / 2.0)
    }

    #[test]
    fn every_part_hits_itself_and_parts_stay_in_their_row() {
        let m = model();
        for (i, c) in m.controls.iter().enumerate() {
            for (spot, r) in parts(c, i) {
                assert_eq!(hit(&m, centre(r)), Some(spot), "{:?}", c.id);
                assert!(r.x >= c.rect.x && r.right() <= c.rect.right() + 0.01);
                assert!(r.y >= c.rect.y && r.bottom() <= c.rect.bottom());
            }
        }
        let shape = index(&m, ControlId::Shape);
        assert_eq!(parts(&m.controls[shape], shape).len(), 3);
        let accent = index(&m, ControlId::Accent);
        assert_eq!(parts(&m.controls[accent], accent).len(), 9);
        // Labels and empty space hit nothing.
        assert_eq!(hit(&m, (30.0, m.controls[0].rect.y + 20.0)), None);
        assert_eq!(hit(&m, (30.0, 5.0)), None);
    }

    #[test]
    fn labels_keep_room_next_to_controls() {
        for c in &model().controls {
            let (label, area) = columns(c);
            assert!(label.w >= 150.0, "{:?}: {}", c.id, label.w);
            assert!(label.right() <= area.x);
        }
    }

    #[test]
    fn scrolling_reveals_the_focused_row() {
        let m = model();
        let view = viewport_h(WINDOW_H_DIP);
        assert!(
            max_scroll(&m, view) > 0.0,
            "the page is taller than the window"
        );
        let last = m.controls.len() - 1;
        let s = reveal(&m, last, 0.0, view);
        assert!(m.controls[last].rect.bottom() <= s + view);
        assert_eq!(s, max_scroll(&m, view).min(s));
        // Back to the first row: its section header shows too.
        assert_eq!(reveal(&m, 0, s, view), 0.0);
        // Already visible: no move.
        assert_eq!(reveal(&m, 1, 0.0, view), 0.0);
    }

    #[test]
    fn items_follow_interaction() {
        let m = model();
        let style = material_style(vitro_core::settings::Material::Frosted, Theme::Dark, true);
        let look = SettingsLook {
            style: &style,
            accent: Rgba(0, 0x78, 0xD4, 255),
            system_accent: Rgba(0, 0x78, 0xD4, 255),
            lang: Lang::En,
            title: "Vitrobar settings",
            version: "0.1.0",
            hint: None,
            page: vitro_core::settings_ui::Section::Appearance,
        };
        let calm = items(&m, &look, &Interaction::default(), WINDOW_W_DIP, 1.0);
        let rings = |v: &[Item]| {
            v.iter()
                .filter(|it| matches!(it.part, Part::Outline { thickness_px, .. } if thickness_px >= 2.0))
                .count()
        };
        // Only the selected swatch has a ring when nothing is focused.
        assert_eq!(rings(&calm.1), 1);
        let ringed = items(
            &m,
            &look,
            &Interaction {
                ring: Some(0),
                ..Default::default()
            },
            WINDOW_W_DIP,
            1.0,
        );
        assert_eq!(rings(&ringed.1), 2);
        let hot = items(
            &m,
            &look,
            &Interaction {
                hover: Some(Spot::Close),
                ..Default::default()
            },
            WINDOW_W_DIP,
            1.0,
        );
        assert_eq!(hot.0.len(), calm.0.len() + 1, "the red plate");
        let titles: Vec<_> = calm
            .1
            .iter()
            .filter_map(|it| match &it.part {
                Part::Text { text, size_px, .. } if *size_px == SECTION_DIP => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            titles,
            [
                "Appearance",
                "Behaviour",
                "Quick panel",
                "Widgets",
                "Language",
                "About"
            ]
        );
        // The sidebar names every category too (in the caption layer).
        let nav = calm
            .0
            .iter()
            .filter(|it| matches!(&it.part, Part::Text { size_px, .. } if *size_px == NAV_TEXT_DIP))
            .count();
        assert_eq!(nav, SECTIONS.len());
    }
}
