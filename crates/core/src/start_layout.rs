//! Start panel and full-screen grid geometry (pure logic, DIPs).

use crate::layout::Rect;
use crate::settings::Position;

pub const PANEL_W_DIP: f32 = 640.0;
pub const PANEL_H_DIP: f32 = 720.0;
pub const PIN_COLS: usize = 6;
pub const PIN_ROWS: usize = 3;
pub const RECENT_ROWS: usize = 4;
pub const RESULT_ROW_H_DIP: f32 = 44.0;
pub const GRID_CELL_DIP: f32 = 96.0;

#[derive(Debug, Clone, PartialEq)]
pub struct StartLayout {
    pub panel: Rect,
    pub search: Rect,
    pub pins_title: Rect,
    pub all_apps_button: Rect,
    pub pins: Vec<Rect>,
    pub recent_title: Rect,
    pub recent: Vec<Rect>,
    pub footer: Rect,
    pub account: Rect,
    pub power: Rect,
    pub results: Vec<Rect>,
    pub list_view: Rect,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GridLayout {
    pub search: Rect,
    pub cells: Vec<Rect>,
    pub cols: usize,
    pub rows_visible: usize,
}

const PAD: f32 = 24.0;
const SEARCH_H: f32 = 40.0;
const TITLE_H: f32 = 32.0;
const PIN_CELL_H: f32 = 84.0;
const RECENT_ROW_H: f32 = 48.0;
const FOOTER_H: f32 = 64.0;
const SECTION_GAP: f32 = 16.0;
const PANEL_GAP: f32 = 12.0;
const EDGE_MARGIN: f32 = 8.0;
const MIN_PANEL_W: f32 = 400.0;
const MIN_PANEL_H: f32 = 480.0;
/// Gap between grid cells.
pub const GRID_GAP_DIP: f32 = 16.0;
const GRID_SIDE_MARGIN: f32 = 64.0;
const GRID_MAX_COLS: usize = 8;

fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect { x, y, w, h }
}

/// Lays the Start panel out inside `panel` (the placed rectangle, DIPs). The offsets assume
/// the full 640x720 size; a shorter panel gets fewer pin rows (at least 1) and then fewer
/// recent rows (none at all drops the Recent title, leaving `recent_title` empty), so that
/// everything stays between the search box and the footer.
pub fn panel_layout(panel: Rect) -> StartLayout {
    let x = panel.x + PAD;
    let w = panel.w - 2.0 * PAD;
    let search = rect(x, panel.y + PAD, w, SEARCH_H);
    let pins_title = rect(x, search.bottom() + SECTION_GAP, w, TITLE_H);
    let all_apps_button = rect(x + w - 120.0, pins_title.y, 120.0, TITLE_H);
    let pins_top = pins_title.bottom() + 8.0;
    let cell_w = w / PIN_COLS as f32;
    let footer = rect(panel.x, panel.bottom() - FOOTER_H, panel.w, FOOTER_H);
    let pin_rows = (((footer.y - pins_top) / PIN_CELL_H).floor().max(1.0) as usize).min(PIN_ROWS);
    let pins = (0..PIN_COLS * pin_rows)
        .map(|i| {
            let (r, c) = (i / PIN_COLS, i % PIN_COLS);
            rect(
                x + c as f32 * cell_w,
                pins_top + r as f32 * PIN_CELL_H,
                cell_w,
                PIN_CELL_H,
            )
        })
        .collect();
    let pins_bottom = pins_top + pin_rows as f32 * PIN_CELL_H;
    let after = footer.y - (pins_bottom + SECTION_GAP + TITLE_H + 4.0);
    let recent_rows = ((after / RECENT_ROW_H).floor().max(0.0) as usize).min(RECENT_ROWS);
    let recent_title = if recent_rows == 0 {
        rect(x, pins_bottom, 0.0, 0.0)
    } else {
        rect(x, pins_bottom + SECTION_GAP, w, TITLE_H)
    };
    let recent_top = recent_title.bottom() + 4.0;
    let recent = (0..recent_rows)
        .map(|i| rect(x, recent_top + i as f32 * RECENT_ROW_H, w, RECENT_ROW_H))
        .collect();
    let account = rect(x, footer.y + (FOOTER_H - 40.0) / 2.0, 240.0, 40.0);
    let power = rect(x + w - 40.0, footer.y + (FOOTER_H - 40.0) / 2.0, 40.0, 40.0);
    let list_top = search.bottom() + 8.0;
    let list_view = rect(x, list_top, w, (footer.y - list_top).max(0.0));
    let n = (list_view.h / RESULT_ROW_H_DIP).floor() as usize;
    let results = (0..n)
        .map(|i| {
            rect(
                x,
                list_top + i as f32 * RESULT_ROW_H_DIP,
                w,
                RESULT_ROW_H_DIP,
            )
        })
        .collect();
    StartLayout {
        panel,
        search,
        pins_title,
        all_apps_button,
        pins,
        recent_title,
        recent,
        footer,
        account,
        power,
        results,
        list_view,
    }
}

/// Full-screen Start: a centred search box and a grid of `app_count` cells, row-major.
/// Cells below the visible area continue downward; scrolling is the renderer's offset.
pub fn grid_layout(work_area: Rect, app_count: usize) -> GridLayout {
    let step = GRID_CELL_DIP + GRID_GAP_DIP;
    let avail_w = work_area.w - 2.0 * GRID_SIDE_MARGIN;
    let cols = (((avail_w + GRID_GAP_DIP) / step).floor().max(1.0) as usize).min(GRID_MAX_COLS);
    let grid_w = cols as f32 * step - GRID_GAP_DIP;
    let x0 = work_area.x + (work_area.w - grid_w) / 2.0;
    let search = rect(
        work_area.x + (work_area.w - 480.0) / 2.0,
        work_area.y + 32.0,
        480.0,
        SEARCH_H,
    );
    let top = search.bottom() + 32.0;
    let cells = (0..app_count)
        .map(|i| {
            let (r, c) = (i / cols, i % cols);
            rect(
                x0 + c as f32 * step,
                top + r as f32 * step,
                GRID_CELL_DIP,
                GRID_CELL_DIP,
            )
        })
        .collect();
    let avail_h = work_area.bottom() - top;
    let rows_visible = if avail_h < GRID_CELL_DIP {
        0
    } else {
        ((avail_h + GRID_GAP_DIP) / step).floor() as usize
    };
    GridLayout {
        search,
        cells,
        cols,
        rows_visible,
    }
}

/// Places the panel with its left edge on `anchor`'s (the Start button: the panel grows out
/// of the button that opened it, wherever the bar puts it), 12 DIP away from it (above for a
/// bottom bar, below for a top bar), clamped inside `work_area` with an 8 DIP margin. Too
/// little room shrinks it, but never below 400x480; if even that does not fit, the panel takes
/// the work area's size.
pub fn place_panel(anchor: Rect, position: Position, work_area: Rect) -> Rect {
    let room = match position {
        Position::Bottom => anchor.y - PANEL_GAP - (work_area.y + EDGE_MARGIN),
        Position::Top => work_area.bottom() - EDGE_MARGIN - (anchor.bottom() + PANEL_GAP),
    };
    let w = PANEL_W_DIP
        .min(work_area.w - 2.0 * EDGE_MARGIN)
        .max(MIN_PANEL_W)
        .min(work_area.w);
    let h = PANEL_H_DIP
        .min(room)
        .min(work_area.h - 2.0 * EDGE_MARGIN)
        .max(MIN_PANEL_H)
        .min(work_area.h);
    let x = clamp_into(anchor.x, work_area.x, work_area.w, w);
    let y_pref = match position {
        Position::Bottom => anchor.y - PANEL_GAP - h,
        Position::Top => anchor.bottom() + PANEL_GAP,
    };
    let y = clamp_into(y_pref, work_area.y, work_area.h, h);
    rect(x, y, w, h)
}

/// Clamps `pos` so `[pos, pos+size]` lies inside `[origin+8, origin+len-8]`; if it cannot,
/// the span starts at `origin`.
fn clamp_into(pos: f32, origin: f32, len: f32, size: f32) -> f32 {
    let lo = origin + EDGE_MARGIN;
    let hi = origin + len - EDGE_MARGIN - size;
    if hi < lo {
        origin
    } else {
        pos.clamp(lo, hi)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inside(outer: &Rect, r: &Rect) -> bool {
        r.x >= outer.x - 0.01
            && r.y >= outer.y - 0.01
            && r.right() <= outer.right() + 0.01
            && r.bottom() <= outer.bottom() + 0.01
    }
    fn overlap(a: &Rect, b: &Rect) -> bool {
        a.x < b.right() - 0.01
            && b.x < a.right() - 0.01
            && a.y < b.bottom() - 0.01
            && b.y < a.bottom() - 0.01
    }
    fn assert_no_overlap(v: &[Rect]) {
        for i in 0..v.len() {
            for j in i + 1..v.len() {
                assert!(
                    !overlap(&v[i], &v[j]),
                    "{i} overlaps {j}: {:?} {:?}",
                    v[i],
                    v[j]
                );
            }
        }
    }
    fn wa() -> Rect {
        rect(0.0, 0.0, 1920.0, 1032.0)
    }
    fn start_rect_at_left() -> Rect {
        rect(8.0, 552.0, 40.0, 40.0)
    }

    #[test]
    fn panel_layout_fits_and_has_18_pins() {
        let p = rect(100.0, 50.0, PANEL_W_DIP, PANEL_H_DIP);
        let l = panel_layout(p);
        assert_eq!(l.pins.len(), PIN_COLS * PIN_ROWS);
        assert_eq!(l.recent.len(), RECENT_ROWS);
        let mut all = vec![
            l.search,
            l.pins_title,
            l.all_apps_button,
            l.recent_title,
            l.footer,
            l.account,
            l.power,
            l.list_view,
        ];
        all.extend(&l.pins);
        all.extend(&l.recent);
        all.extend(&l.results);
        for r in &all {
            assert!(inside(&p, r), "{r:?}");
        }
        assert_no_overlap(&l.pins);
        assert_no_overlap(&l.recent);
        assert_no_overlap(&l.results);
        assert_no_overlap(&[l.account, l.power]);
        assert_no_overlap(&[l.search, l.pins_title, l.recent_title, l.footer]);
        assert!(!overlap(&l.pins[17], &l.recent_title));
        assert!(!overlap(&l.recent[3], &l.footer));
        assert!(inside(&l.pins_title, &l.all_apps_button));
        assert_eq!(l.all_apps_button.right(), l.pins_title.right());
        assert_eq!((l.all_apps_button.w, l.all_apps_button.h), (120.0, 32.0));
        assert_eq!((l.power.w, l.power.h), (40.0, 40.0));
        assert_eq!(l.account.w, 240.0);
        assert_eq!(l.footer.h, 64.0);
        assert_eq!(l.results[0].h, RESULT_ROW_H_DIP);
        assert!(l.results.iter().all(|r| r.bottom() <= l.footer.y));
        assert!(l.results.len() >= 8);
    }

    #[test]
    fn panel_layout_adapts_to_short_panels() {
        for (w, h, pins, recent) in [
            (640.0, 720.0, 18, 4),
            (640.0, 532.0, 18, 0),
            (400.0, 480.0, 18, 0),
        ] {
            let p = rect(10.0, 20.0, w, h);
            let l = panel_layout(p);
            assert_eq!(l.pins.len(), pins, "{w}x{h}");
            assert_eq!(l.recent.len(), recent, "{w}x{h}");
            let mut all = vec![
                l.search,
                l.pins_title,
                l.all_apps_button,
                l.recent_title,
                l.footer,
                l.account,
                l.power,
                l.list_view,
            ];
            all.extend(&l.pins);
            all.extend(&l.recent);
            all.extend(&l.results);
            for r in &all {
                assert!(inside(&p, r), "{w}x{h}: {r:?}");
            }
            let mut stack = vec![l.search, l.pins_title];
            stack.extend(&l.pins);
            if recent > 0 {
                stack.push(l.recent_title);
                stack.extend(&l.recent);
            }
            stack.push(l.footer);
            for r in &stack[..stack.len() - 1] {
                assert!(r.bottom() <= l.footer.y + 0.01, "{w}x{h}: {r:?}");
            }
            assert_no_overlap(&l.pins);
            assert_no_overlap(&l.recent);
            assert!(!overlap(&l.pins_title, &l.pins[0]));
            for r in l.pins.iter().chain(&l.recent) {
                assert!(!overlap(r, &l.footer));
            }
        }
    }

    #[test]
    fn panel_layout_drops_pin_rows_when_very_short() {
        let l = panel_layout(rect(0.0, 0.0, 400.0, 400.0));
        assert!(l.pins.len() < 18 && l.pins.len() >= PIN_COLS);
        assert!(l.recent.is_empty());
        assert!(l.pins.iter().all(|r| r.bottom() <= l.footer.y));
    }

    #[test]
    fn place_panel_clamps_and_shrinks() {
        let small = rect(0.0, 0.0, 1000.0, 600.0);
        let a = rect(8.0, 552.0, 40.0, 40.0);
        let r = place_panel(a, Position::Bottom, small);
        assert!(r.x >= 0.0 && r.bottom() <= 600.0 && r.h >= 480.0);
        assert!(r.right() <= small.right());
        assert!(r.bottom() <= a.y);
        let r = place_panel(start_rect_at_left(), Position::Bottom, wa());
        assert_eq!(r.x, 8.0);
        assert_eq!(r.bottom(), 552.0 - 12.0);
        assert_eq!(r.h, 552.0 - 12.0 - 8.0);
        let low = rect(8.0, 980.0, 40.0, 40.0);
        let r = place_panel(low, Position::Bottom, wa());
        assert_eq!((r.w, r.h), (PANEL_W_DIP, PANEL_H_DIP));
    }

    #[test]
    fn place_panel_lines_up_with_the_start_button() {
        let a = rect(300.0, 980.0, 40.0, 40.0);
        let r = place_panel(a, Position::Bottom, wa());
        assert_eq!(r.x, 300.0);
        // Near the right edge it is pushed back inside.
        let far = rect(wa().right() - 50.0, 980.0, 40.0, 40.0);
        let r = place_panel(far, Position::Bottom, wa());
        assert_eq!(r.right(), wa().right() - 8.0);
    }

    #[test]
    fn place_panel_top_opens_below() {
        let a = rect(900.0, 8.0, 40.0, 40.0);
        let r = place_panel(a, Position::Top, wa());
        assert!(r.y >= a.bottom());
        assert_eq!(r.y, a.bottom() + 12.0);
    }

    #[test]
    fn place_panel_tiny_work_area_uses_it() {
        let tiny = rect(0.0, 0.0, 300.0, 400.0);
        let r = place_panel(rect(0.0, 360.0, 40.0, 40.0), Position::Bottom, tiny);
        assert_eq!((r.w, r.h), (300.0, 400.0));
        assert!(inside(&tiny, &r));
    }

    #[test]
    fn grid_layout_no_overlap() {
        let g = grid_layout(wa(), 120);
        assert_eq!(g.cells.len(), 120);
        assert_eq!(g.cols, 8);
        assert_no_overlap(&g.cells);
        assert_eq!((g.search.w, g.search.h), (480.0, 40.0));
        assert_eq!(g.search.y, 32.0);
        assert_eq!(g.search.x + g.search.w / 2.0, 960.0);
        assert_eq!(g.cells[0].y, g.search.bottom() + 32.0);
        assert!(g.cells[0].x >= 64.0);
        let last_visible = g.rows_visible * g.cols - 1;
        assert!(g.cells[last_visible].bottom() <= 1032.0);
        assert!(g.cells[(g.rows_visible) * g.cols].bottom() > 1032.0);
    }

    #[test]
    fn grid_layout_narrow_and_empty() {
        let g = grid_layout(rect(0.0, 0.0, 600.0, 800.0), 5);
        assert_eq!(g.cols, 4);
        assert!(g.cells.iter().all(|c| c.x >= 64.0 && c.right() <= 536.0));
        assert!(grid_layout(wa(), 0).cells.is_empty());
    }
}
