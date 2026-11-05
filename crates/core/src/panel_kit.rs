//! The panel kit: rows (tiles, sliders, list rows, headers, buttons) stacked in a fixed-width
//! glass panel, with layout, hit testing and keyboard focus. Pure logic in DIPs; the glass
//! view that draws it lives in `vitro-render`.
//!
//! Part rectangles are in content coordinates (the top of the whole content is y = 0). The
//! panel is at most [`MAX_H_DIP`] tall; longer content scrolls, and [`hit`] takes the scroll
//! offset.

use crate::layout::Rect;
use crate::menu_model::NavKey;

pub const WIDTH_DIP: f32 = 360.0;
pub const PAD_DIP: f32 = 12.0;
pub const GAP_DIP: f32 = 8.0;
pub const TILE_H_DIP: f32 = 64.0;
pub const TILES_PER_ROW: usize = 3;
pub const SLIDER_H_DIP: f32 = 40.0;
pub const ITEM_H_DIP: f32 = 44.0;
pub const HEADER_H_DIP: f32 = 36.0;
pub const BUTTONS_H_DIP: f32 = 40.0;
pub const TEXT_H_DIP: f32 = 20.0;
pub const SEP_H_DIP: f32 = 9.0;
/// Content taller than this scrolls.
pub const MAX_H_DIP: f32 = 640.0;
/// A slider's glyph column (left of the track), and its trailing button's width.
pub const SLIDER_GLYPH_W_DIP: f32 = 32.0;
pub const SLIDER_TRAILING_W_DIP: f32 = 32.0;
/// Thumb diameter: the track's usable length stops half a thumb short of each end.
pub const THUMB_DIP: f32 = 16.0;
/// Share of a `more` tile's width taken by the chevron half.
pub const MORE_SHARE: f32 = 0.4;
/// Width of a header action button.
pub const HEADER_ACTION_W_DIP: f32 = 32.0;
/// Width of an icon-only button next to text buttons (a row of icons only shares it evenly).
pub const ICON_BUTTON_W_DIP: f32 = 48.0;

#[derive(Clone, Debug, PartialEq)]
pub enum Row {
    Header {
        text: String,
        trailing: Option<String>,
        /// Glyph buttons at the right end of the header (a month's chevrons), in reading
        /// order; each is a [`Part::HeaderAction`]. `trailing` text moves left of them.
        actions: Vec<(u32, char)>,
    },
    Tiles(Vec<Tile>),
    Slider {
        id: u32,
        glyph: char,
        /// 0..=1.
        value: f32,
        trailing: Option<(u32, char)>,
    },
    Item {
        id: u32,
        glyph: Option<char>,
        text: String,
        detail: Option<String>,
        checked: bool,
        busy: bool,
    },
    Buttons(Vec<Button>),
    Text(String),
    Separator,
    /// A hole of height `h` the owner draws into (a month grid, artwork, a progress bar).
    Custom {
        id: u32,
        h: f32,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tile {
    pub id: u32,
    pub glyph: char,
    pub label: String,
    pub on: bool,
    /// A chevron half that opens a sub-page, like the stock Quick Settings.
    pub more: bool,
    /// Drawn filled in this colour (white text) instead of the accent or the plain plate:
    /// a tile whose state is a choice among several (the power mode) rather than on/off.
    pub tint: Option<crate::style::Rgba>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Button {
    pub id: u32,
    pub glyph: Option<char>,
    pub text: String,
}

/// Something the pointer or the keyboard can act on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Part {
    Tile(u32),
    TileMore(u32),
    Slider(u32),
    SliderTrailing(u32),
    HeaderAction(u32),
    Item(u32),
    Button(u32),
    Custom(u32),
}

#[derive(Clone, Debug, PartialEq)]
pub struct PanelLayout {
    /// Size of the panel (height capped at [`MAX_H_DIP`]).
    pub size: (f32, f32),
    /// Interactive parts in reading order, in content coordinates.
    pub parts: Vec<(Part, Rect)>,
    /// Height of all the rows with the padding; above `size.1` when the panel scrolls.
    pub content_h: f32,
    /// Full-width band of each row, in content coordinates.
    pub rows: Vec<Rect>,
}

fn row_h(r: &Row) -> f32 {
    match r {
        Row::Header { .. } => HEADER_H_DIP,
        Row::Tiles(t) => {
            let n = t.len().div_ceil(TILES_PER_ROW);
            n as f32 * TILE_H_DIP + n.saturating_sub(1) as f32 * GAP_DIP
        }
        Row::Slider { .. } => SLIDER_H_DIP,
        Row::Item { .. } => ITEM_H_DIP,
        Row::Buttons(_) => BUTTONS_H_DIP,
        Row::Text(_) => TEXT_H_DIP,
        Row::Separator => SEP_H_DIP,
        Row::Custom { h, .. } => {
            if h.is_finite() {
                h.max(0.0)
            } else {
                0.0
            }
        }
    }
}

pub fn layout(rows: &[Row]) -> PanelLayout {
    let inner = WIDTH_DIP - 2.0 * PAD_DIP;
    let mut y = PAD_DIP;
    let mut parts = Vec::new();
    let mut bands = Vec::with_capacity(rows.len());
    for (k, r) in rows.iter().enumerate() {
        if k > 0 {
            y += GAP_DIP;
        }
        let h = row_h(r);
        bands.push(Rect {
            x: 0.0,
            y,
            w: WIDTH_DIP,
            h,
        });
        match r {
            Row::Header { actions, .. } => {
                let n = actions.len() as f32;
                for (i, (id, _)) in actions.iter().enumerate() {
                    parts.push((
                        Part::HeaderAction(*id),
                        Rect {
                            x: WIDTH_DIP - PAD_DIP - (n - i as f32) * HEADER_ACTION_W_DIP,
                            y,
                            w: HEADER_ACTION_W_DIP,
                            h,
                        },
                    ));
                }
            }
            Row::Tiles(tiles) => {
                let w = (inner - (TILES_PER_ROW - 1) as f32 * GAP_DIP) / TILES_PER_ROW as f32;
                for (i, t) in tiles.iter().enumerate() {
                    let x = PAD_DIP + (i % TILES_PER_ROW) as f32 * (w + GAP_DIP);
                    let ty = y + (i / TILES_PER_ROW) as f32 * (TILE_H_DIP + GAP_DIP);
                    let rect = |x: f32, w: f32| Rect {
                        x,
                        y: ty,
                        w,
                        h: TILE_H_DIP,
                    };
                    if t.more {
                        let more_w = (w * MORE_SHARE).round();
                        parts.push((Part::Tile(t.id), rect(x, w - more_w)));
                        parts.push((Part::TileMore(t.id), rect(x + w - more_w, more_w)));
                    } else {
                        parts.push((Part::Tile(t.id), rect(x, w)));
                    }
                }
            }
            Row::Slider { id, trailing, .. } => {
                let x = PAD_DIP + SLIDER_GLYPH_W_DIP;
                let right = if trailing.is_some() {
                    WIDTH_DIP - PAD_DIP - SLIDER_TRAILING_W_DIP
                } else {
                    WIDTH_DIP - PAD_DIP
                };
                parts.push((
                    Part::Slider(*id),
                    Rect {
                        x,
                        y,
                        w: right - x,
                        h,
                    },
                ));
                if let Some((tid, _)) = trailing {
                    parts.push((
                        Part::SliderTrailing(*tid),
                        Rect {
                            x: right,
                            y,
                            w: SLIDER_TRAILING_W_DIP,
                            h,
                        },
                    ));
                }
            }
            Row::Item { id, .. } => parts.push((
                Part::Item(*id),
                Rect {
                    x: PAD_DIP,
                    y,
                    w: inner,
                    h,
                },
            )),
            Row::Buttons(btns) if !btns.is_empty() => {
                let icon = |b: &Button| b.text.is_empty() && b.glyph.is_some();
                let n = btns.len() as f32;
                let n_icon = btns.iter().filter(|b| icon(b)).count() as f32;
                let free = inner - (n - 1.0) * GAP_DIP;
                // Icons next to text keep a fixed width; text buttons share the rest.
                let (icon_w, text_w) = if n_icon == 0.0 || n_icon == n {
                    (free / n, free / n)
                } else {
                    let iw = ICON_BUTTON_W_DIP;
                    (iw, ((free - n_icon * iw) / (n - n_icon)).max(0.0))
                };
                let mut x = PAD_DIP;
                for b in btns {
                    let w = if icon(b) { icon_w } else { text_w };
                    parts.push((Part::Button(b.id), Rect { x, y, w, h }));
                    x += w + GAP_DIP;
                }
            }
            Row::Custom { id, .. } => parts.push((
                Part::Custom(*id),
                Rect {
                    x: PAD_DIP,
                    y,
                    w: inner,
                    h,
                },
            )),
            _ => {}
        }
        y += h;
    }
    let content_h = y + PAD_DIP;
    PanelLayout {
        size: (WIDTH_DIP, content_h.min(MAX_H_DIP)),
        parts,
        content_h,
        rows: bands,
    }
}

/// The part under `pt_dip` (panel client DIPs) when the content is scrolled by `scroll` DIPs.
pub fn hit(l: &PanelLayout, pt_dip: (f32, f32), scroll: f32) -> Option<Part> {
    let (x, y) = (pt_dip.0, pt_dip.1 + scroll);
    if pt_dip.1 < 0.0 || pt_dip.1 >= l.size.1 {
        return None;
    }
    l.parts
        .iter()
        .find(|(_, r)| x >= r.x && x < r.right() && y >= r.y && y < r.bottom())
        .map(|(p, _)| *p)
}

/// Slider `id`'s value for a pointer at `x_dip` (panel client DIPs), clamped to 0..=1: the
/// track runs between the centres of the thumb at its two ends.
pub fn slider_value(l: &PanelLayout, id: u32, x_dip: f32) -> Option<f32> {
    let (_, r) = l.parts.iter().find(|(p, _)| *p == Part::Slider(id))?;
    let (a, b) = (r.x + THUMB_DIP / 2.0, r.right() - THUMB_DIP / 2.0);
    if b <= a {
        return Some(0.0);
    }
    Some(((x_dip - a) / (b - a)).clamp(0.0, 1.0))
}

/// The part focused after `key`, in reading order, wrapping around (the caller maps Tab to
/// `Down` and Shift+Tab to `Up`). Custom holes are skipped.
pub fn focus_step(l: &PanelLayout, cur: Option<Part>, key: NavKey) -> Option<Part> {
    let order: Vec<Part> = l
        .parts
        .iter()
        .map(|(p, _)| *p)
        .filter(|p| !matches!(p, Part::Custom(_)))
        .collect();
    let (first, last) = (*order.first()?, *order.last()?);
    let at = cur.and_then(|c| order.iter().position(|p| *p == c));
    Some(match (key, at) {
        (NavKey::Home, _) | (NavKey::Down, None) => first,
        (NavKey::End, _) | (NavKey::Up, None) => last,
        (NavKey::Down, Some(i)) => order[(i + 1) % order.len()],
        (NavKey::Up, Some(i)) => order[(i + order.len() - 1) % order.len()],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_buttons_keep_their_width_next_to_text() {
        let b = |id: u32, text: &str| Button {
            id,
            glyph: Some('x'),
            text: text.into(),
        };
        let l = layout(&[Row::Buttons(vec![b(1, "80%"), b(2, ""), b(3, "")])]);
        let w = |id| {
            l.parts
                .iter()
                .find(|(p, _)| *p == Part::Button(id))
                .unwrap()
                .1
        };
        assert_eq!(w(2).w, ICON_BUTTON_W_DIP);
        assert_eq!(w(3).w, ICON_BUTTON_W_DIP);
        let inner = WIDTH_DIP - 2.0 * PAD_DIP;
        assert_eq!(w(1).w, inner - 2.0 * GAP_DIP - 2.0 * ICON_BUTTON_W_DIP);
        assert_eq!(w(3).right(), PAD_DIP + inner);
        // Icons alone (media transport) share the row evenly.
        let l = layout(&[Row::Buttons(vec![b(1, ""), b(2, ""), b(3, "")])]);
        let w1 = l.parts[0].1.w;
        assert!(l.parts.iter().all(|(_, r)| r.w == w1));
    }

    fn tile(i: u32) -> Tile {
        Tile {
            tint: None,
            id: i,
            glyph: 'a',
            label: format!("t{i}"),
            on: false,
            more: false,
        }
    }

    fn item(i: u32) -> Row {
        Row::Item {
            id: i,
            glyph: None,
            text: format!("i{i}"),
            detail: None,
            checked: false,
            busy: false,
        }
    }

    fn rect_of(l: &PanelLayout, p: Part) -> Rect {
        l.parts.iter().find(|(q, _)| *q == p).unwrap().1
    }

    #[test]
    fn tiles_wrap_three_per_row() {
        let r = vec![Row::Tiles((0..4).map(tile).collect())];
        let l = layout(&r);
        assert_eq!(l.size.1, 12.0 + 64.0 + 8.0 + 64.0 + 12.0);
        assert_eq!(rect_of(&l, Part::Tile(3)).y, 12.0 + 64.0 + 8.0);
        assert_eq!(rect_of(&l, Part::Tile(0)).w, (360.0 - 24.0 - 16.0) / 3.0);
    }

    #[test]
    fn more_half_is_its_own_part() {
        let mut t = tile(0);
        t.more = true;
        let l = layout(&[Row::Tiles(vec![t])]);
        let (a, b) = (rect_of(&l, Part::Tile(0)), rect_of(&l, Part::TileMore(0)));
        let whole = (360.0 - 24.0 - 16.0) / 3.0;
        assert_eq!(a.x, 12.0);
        assert_eq!(a.right(), b.x);
        assert_eq!(b.right(), 12.0 + whole);
        assert!((b.w / whole - 0.4).abs() < 0.01);
        let y = 12.0 + 10.0;
        assert_eq!(hit(&l, (12.0 + 5.0, y), 0.0), Some(Part::Tile(0)));
        assert_eq!(
            hit(&l, (12.0 + whole - 5.0, y), 0.0),
            Some(Part::TileMore(0))
        );
        // The padding belongs to nothing.
        assert_eq!(hit(&l, (3.0, y), 0.0), None);
    }

    #[test]
    fn slider_maps_x_to_value_and_clamps() {
        let r = vec![Row::Slider {
            id: 7,
            glyph: 'v',
            value: 0.5,
            trailing: Some((8, 'm')),
        }];
        let l = layout(&r);
        let s = rect_of(&l, Part::Slider(7));
        let t = rect_of(&l, Part::SliderTrailing(8));
        assert_eq!(
            s.right(),
            t.x,
            "the trailing button sits right of the track"
        );
        let (a, b) = (s.x + 8.0, s.right() - 8.0);
        assert_eq!(slider_value(&l, 7, a), Some(0.0));
        assert_eq!(slider_value(&l, 7, b), Some(1.0));
        assert_eq!(slider_value(&l, 7, (a + b) / 2.0), Some(0.5));
        assert_eq!(slider_value(&l, 7, -50.0), Some(0.0));
        assert_eq!(slider_value(&l, 7, 5000.0), Some(1.0));
        assert_eq!(slider_value(&l, 99, 100.0), None);
    }

    #[test]
    fn tall_content_scrolls_and_hit_follows_scroll() {
        let rows: Vec<Row> = (0..30).map(item).collect();
        let l = layout(&rows);
        assert_eq!(l.size.1, 640.0);
        assert!(l.content_h > 640.0);
        // Item k starts at 12 + k * 52. At scroll 0, y = 20 is item 0; scrolled by 104
        // (two rows), the same pointer is on item 2.
        assert_eq!(hit(&l, (100.0, 20.0), 0.0), Some(Part::Item(0)));
        assert_eq!(hit(&l, (100.0, 20.0), 104.0), Some(Part::Item(2)));
        // Below the visible panel nothing is hit.
        assert_eq!(hit(&l, (100.0, 700.0), 0.0), None);
    }

    #[test]
    fn focus_walks_in_reading_order_and_wraps() {
        let rows = vec![
            Row::Tiles(vec![tile(1), tile(2)]),
            Row::Custom { id: 50, h: 30.0 },
            item(3),
            Row::Buttons(vec![Button {
                id: 4,
                glyph: None,
                text: "ok".into(),
            }]),
        ];
        let l = layout(&rows);
        let mut cur = None;
        let mut seen = Vec::new();
        for _ in 0..5 {
            cur = focus_step(&l, cur, NavKey::Down);
            seen.push(cur.unwrap());
        }
        assert_eq!(
            seen,
            vec![
                Part::Tile(1),
                Part::Tile(2),
                Part::Item(3),
                Part::Button(4),
                Part::Tile(1)
            ],
            "the Custom hole is skipped and the walk wraps"
        );
        assert_eq!(
            focus_step(&l, Some(Part::Tile(1)), NavKey::Up),
            Some(Part::Button(4))
        );
        assert_eq!(focus_step(&l, None, NavKey::Up), Some(Part::Button(4)));
        assert_eq!(
            focus_step(&l, Some(Part::Item(3)), NavKey::Home),
            Some(Part::Tile(1))
        );
        assert_eq!(
            focus_step(&l, Some(Part::Item(3)), NavKey::End),
            Some(Part::Button(4))
        );
        assert_eq!(focus_step(&layout(&[]), None, NavKey::Down), None);
    }

    #[test]
    fn header_actions_sit_at_the_right_end_in_reading_order() {
        let l = layout(&[Row::Header {
            text: "h".into(),
            trailing: None,
            actions: vec![(1, 'a'), (2, 'b')],
        }]);
        let (a, b) = (
            rect_of(&l, Part::HeaderAction(1)),
            rect_of(&l, Part::HeaderAction(2)),
        );
        assert_eq!(b.right(), 360.0 - 12.0);
        assert_eq!(a.right(), b.x);
        assert_eq!((a.w, a.y, a.h), (32.0, 12.0, 36.0));
        assert_eq!(hit(&l, (b.x + 1.0, 20.0), 0.0), Some(Part::HeaderAction(2)));
        assert_eq!(hit(&l, (20.0, 20.0), 0.0), None);
        // A header without actions adds no part.
        let l = layout(&[Row::Header {
            text: "h".into(),
            trailing: None,
            actions: Vec::new(),
        }]);
        assert!(l.parts.is_empty());
    }

    #[test]
    fn empty_panel_has_padding_only() {
        assert_eq!(layout(&[]).size, (360.0, 24.0));
    }

    #[test]
    fn rows_stack_with_gaps_and_custom_keeps_its_height() {
        let l = layout(&[
            Row::Header {
                text: "h".into(),
                trailing: None,
                actions: Vec::new(),
            },
            Row::Custom { id: 1, h: 100.0 },
            Row::Separator,
        ]);
        assert_eq!(l.rows[1].y, 12.0 + 36.0 + 8.0);
        assert_eq!(rect_of(&l, Part::Custom(1)).h, 100.0);
        assert_eq!(l.size.1, 12.0 + 36.0 + 8.0 + 100.0 + 8.0 + 9.0 + 12.0);
    }
}
