//! Bar layout geometry (pure logic, DIPs).

use crate::settings::{IconSize, Position, Shape};

/// Rectangle in device-independent pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    pub bar_h: f32,
    pub icon: f32,
    pub slot: f32,
}

pub fn metrics(size: IconSize) -> Metrics {
    match size {
        IconSize::Small => Metrics {
            bar_h: 40.0,
            icon: 20.0,
            slot: 32.0,
        },
        IconSize::Medium => Metrics {
            bar_h: 48.0,
            icon: 24.0,
            slot: 40.0,
        },
        IconSize::Large => Metrics {
            bar_h: 56.0,
            icon: 28.0,
            slot: 48.0,
        },
    }
}

#[derive(Clone, Copy, Debug)]
pub struct LayoutInput {
    pub monitor: Rect,
    pub shape: Shape,
    pub position: Position,
    pub icon_size: IconSize,
    pub app_count: usize,
    pub tray_count: usize,
    pub clock_w: f32,
    pub reserve_floating: bool,
    /// The bar carries the tray area (overflow chevron, tray icons, quick-settings button).
    /// Only the primary monitor's bar does; the others show Start, their apps and the clock.
    /// Without it `overflow_chevron` and `quick` are zero-width and `tray_count` is ignored.
    pub tray_area: bool,
    /// DIP widths of the Music and Claude-limits pills (0 = hidden). Only the bar with the tray
    /// area shows them, just left of the tray.
    pub widgets: [f32; 2],
}

#[derive(Clone, Debug)]
pub struct BarLayout {
    /// 1 for Pill/Full, 3 for Islands (start, apps, tray).
    pub surfaces: Vec<Rect>,
    pub radius: f32,
    pub start: Rect,
    pub apps: Vec<Rect>,
    pub more: Option<Rect>,
    pub overflow_chevron: Rect,
    pub tray: Vec<Rect>,
    pub quick: Rect,
    pub clock: Rect,
    /// The Music and Claude pills, `None` when hidden or on a bar without the tray area.
    pub widgets: [Option<Rect>; 2],
    pub show_desktop: Rect,
    /// DIPs to reserve at the screen edge.
    pub reserved: f32,
}

const MARGIN: f32 = 8.0;
const PAD: f32 = 6.0;
const GAP: f32 = 8.0;
const SHOW_DESKTOP_W: f32 = 6.0;
const EPS: f32 = 0.01;

/// Monitors that appeared and that went away between two lists of monitor ids, each in the
/// order of the list it comes from. A changed order alone is no change.
pub fn diff_monitors(old: &[u64], new: &[u64]) -> (Vec<u64>, Vec<u64>) {
    let added = new.iter().filter(|id| !old.contains(id)).copied().collect();
    let removed = old.iter().filter(|id| !new.contains(id)).copied().collect();
    (added, removed)
}

/// App slot sizing: (slot width, apps shown, whether the last slot is `more`).
fn fit_apps(n: usize, avail: f32, m: Metrics) -> (f32, usize, bool) {
    if n == 0 {
        return (m.slot, 0, false);
    }
    let min = m.icon + 4.0;
    let s = (avail / n as f32).clamp(min, m.slot);
    if n as f32 * s <= avail + EPS {
        return (s, n, false);
    }
    let k = ((avail + EPS) / s).floor().max(0.0) as usize;
    if k == 0 {
        (s, 0, false)
    } else {
        (s, k - 1, true)
    }
}

pub fn compute_layout(i: &LayoutInput) -> BarLayout {
    let m = metrics(i.icon_size);
    let mon = i.monitor;
    let full = i.shape == Shape::Full;
    let floating_margin = if full { 0.0 } else { MARGIN };

    let sy = match i.position {
        Position::Bottom => mon.bottom() - floating_margin - m.bar_h,
        Position::Top => mon.y + floating_margin,
    };
    let item_y = sy + (m.bar_h - m.slot) / 2.0;
    let item = |x: f32, w: f32| Rect {
        x,
        y: item_y,
        w,
        h: m.slot,
    };

    // chevron + tray icons + quick button (one slot each) + clock; the clock alone without
    // the tray area.
    let tray_count = if i.tray_area { i.tray_count } else { 0 };
    let tray_slot = if i.tray_area { m.slot } else { 0.0 };
    let widget_w = |k: usize| {
        if i.tray_area && i.widgets[k] > 0.0 {
            i.widgets[k]
        } else {
            0.0
        }
    };
    // each shown widget takes its width plus a GAP towards what follows it
    let widgets_w: f32 = (0..2)
        .filter(|&k| widget_w(k) > 0.0)
        .map(|k| widget_w(k) + GAP)
        .sum();
    let right_w = widgets_w + m.slot * tray_count as f32 + 2.0 * tray_slot + i.clock_w;
    // right group with its inner padding on both sides, then the show-desktop strip
    let right_surface_w = PAD + right_w + PAD + SHOW_DESKTOP_W;

    let (surfaces, start_x, apps_x, right_x, s, shown, has_more);
    match i.shape {
        Shape::Pill => {
            let fixed = PAD + m.slot + GAP + GAP + right_surface_w - PAD;
            let avail = (mon.w - 2.0 * MARGIN - fixed).max(0.0);
            (s, shown, has_more) = fit_apps(i.app_count, avail, m);
            let apps_w = (shown + has_more as usize) as f32 * s;
            let w = fixed + apps_w;
            let x = mon.x + (mon.w - w) / 2.0;
            surfaces = vec![Rect {
                x,
                y: sy,
                w,
                h: m.bar_h,
            }];
            start_x = x + PAD;
            apps_x = start_x + m.slot + GAP;
            right_x = apps_x + apps_w + GAP;
        }
        Shape::Full => {
            let lo = mon.x + PAD + m.slot + GAP;
            let rx = mon.right() - SHOW_DESKTOP_W - PAD - right_w;
            let hi = rx - GAP;
            (s, shown, has_more) = fit_apps(i.app_count, (hi - lo).max(0.0), m);
            let apps_w = (shown + has_more as usize) as f32 * s;
            let cx = mon.x + (mon.w - apps_w) / 2.0;
            apps_x = cx.min(hi - apps_w).max(lo);
            start_x = mon.x + PAD;
            right_x = rx;
            surfaces = vec![Rect {
                x: mon.x,
                y: sy,
                w: mon.w,
                h: m.bar_h,
            }];
        }
        Shape::Islands => {
            let left = Rect {
                x: mon.x + MARGIN,
                y: sy,
                w: PAD + m.slot + PAD,
                h: m.bar_h,
            };
            let right = Rect {
                x: mon.right() - MARGIN - right_surface_w,
                y: sy,
                w: right_surface_w,
                h: m.bar_h,
            };
            let lo = left.right() + MARGIN;
            let hi = right.x - MARGIN;
            (s, shown, has_more) = fit_apps(i.app_count, (hi - lo - 2.0 * PAD).max(0.0), m);
            let apps_w = (shown + has_more as usize) as f32 * s;
            let mid_w = 2.0 * PAD + apps_w;
            let cx = mon.x + (mon.w - mid_w) / 2.0;
            let mid_x = cx.min(hi - mid_w).max(lo);
            surfaces = vec![
                left,
                Rect {
                    x: mid_x,
                    y: sy,
                    w: mid_w,
                    h: m.bar_h,
                },
                right,
            ];
            start_x = left.x + PAD;
            apps_x = mid_x + PAD;
            right_x = right.x + PAD;
        }
    }

    let apps: Vec<Rect> = (0..shown).map(|k| item(apps_x + k as f32 * s, s)).collect();
    let more = has_more.then(|| item(apps_x + shown as f32 * s, s));

    let mut wx = right_x;
    let widgets = [0, 1].map(|k| {
        let w = widget_w(k);
        (w > 0.0).then(|| {
            let r = item(wx, w);
            wx += w + GAP;
            r
        })
    });
    let tray_x = right_x + widgets_w;
    let overflow_chevron = item(tray_x, tray_slot);
    let tray: Vec<Rect> = (0..tray_count)
        .map(|k| item(tray_x + tray_slot + k as f32 * m.slot, m.slot))
        .collect();
    let quick_x = tray_x + tray_slot + tray_count as f32 * m.slot;
    let quick = item(quick_x, tray_slot);
    let clock = item(quick_x + tray_slot, i.clock_w);

    let last = surfaces.last().copied().expect("at least one surface");
    let show_desktop = Rect {
        x: last.right() - SHOW_DESKTOP_W,
        y: sy,
        w: SHOW_DESKTOP_W,
        h: m.bar_h,
    };

    let reserved = if full {
        m.bar_h
    } else if i.reserve_floating {
        m.bar_h + 2.0 * MARGIN
    } else {
        0.0
    };

    BarLayout {
        surfaces,
        radius: if full { 0.0 } else { m.bar_h / 2.0 },
        start: item(start_x, m.slot),
        apps,
        more,
        overflow_chevron,
        tray,
        quick,
        clock,
        widgets,
        show_desktop,
        reserved,
    }
}

/// The bar elements a panel can open from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AnchorTarget {
    Quick,
    Clock,
    /// 0 = Music, 1 = Claude limits.
    Widget(usize),
}

/// The DIP rect a panel anchors to: the element itself, on every shape. A hidden widget (or an
/// index past the last) falls back to the quick-settings button so a panel still has a spot.
pub fn anchor_for(layout: &BarLayout, target: AnchorTarget) -> Rect {
    match target {
        AnchorTarget::Quick => layout.quick,
        AnchorTarget::Clock => layout.clock,
        AnchorTarget::Widget(k) => layout
            .widgets
            .get(k)
            .copied()
            .flatten()
            .unwrap_or(layout.quick),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_SHAPES: [Shape; 3] = [Shape::Pill, Shape::Islands, Shape::Full];
    const MON: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 1920.0,
        h: 1080.0,
    };

    fn input(monitor: Rect, shape: Shape, pos: Position, n: usize, reserve: bool) -> LayoutInput {
        LayoutInput {
            monitor,
            shape,
            position: pos,
            icon_size: IconSize::Medium,
            app_count: n,
            tray_count: 3,
            clock_w: 64.0,
            reserve_floating: reserve,
            tray_area: true,
            widgets: [0.0; 2],
        }
    }
    fn lay(shape: Shape, n: usize) -> BarLayout {
        compute_layout(&input(MON, shape, Position::Bottom, n, true))
    }
    fn lay_pos(shape: Shape, pos: Position) -> BarLayout {
        compute_layout(&input(MON, shape, pos, 5, true))
    }
    fn lay_noreserve(shape: Shape) -> BarLayout {
        compute_layout(&input(MON, shape, Position::Bottom, 5, false))
    }
    fn lay_on(mon: Rect, shape: Shape, n: usize) -> BarLayout {
        compute_layout(&input(mon, shape, Position::Bottom, n, true))
    }
    fn all_items(l: &BarLayout) -> Vec<Rect> {
        let mut v = vec![l.start];
        v.extend(l.apps.iter().copied());
        v.extend(l.more);
        v.push(l.overflow_chevron);
        v.extend(l.tray.iter().copied());
        v.push(l.quick);
        v.push(l.clock);
        v.extend(l.widgets.iter().flatten().copied());
        v.push(l.show_desktop);
        v
    }
    fn contains(outer: &Rect, r: &Rect) -> bool {
        r.x >= outer.x - EPS
            && r.y >= outer.y - EPS
            && r.right() <= outer.right() + EPS
            && r.bottom() <= outer.bottom() + EPS
    }
    fn assert_no_overlap(rs: &[Rect]) {
        for (a, ra) in rs.iter().enumerate() {
            for rb in &rs[a + 1..] {
                let ox = ra.right().min(rb.right()) - ra.x.max(rb.x);
                let oy = ra.bottom().min(rb.bottom()) - ra.y.max(rb.y);
                assert!(!(ox > EPS && oy > EPS), "overlap: {ra:?} vs {rb:?}");
            }
        }
    }

    #[test]
    fn metrics_values() {
        assert_eq!(metrics(IconSize::Small).slot, 32.0);
        assert_eq!(metrics(IconSize::Medium).bar_h, 48.0);
        assert_eq!(metrics(IconSize::Large).icon, 28.0);
    }
    #[test]
    fn pill_is_centred_and_floats() {
        let l = lay(Shape::Pill, 5);
        let s = l.surfaces[0];
        assert_eq!(l.surfaces.len(), 1);
        assert!((s.x + s.w / 2.0 - 960.0).abs() < 0.5);
        assert_eq!(s.y + s.h, 1080.0 - 8.0);
        assert_eq!(s.h, 48.0);
        assert_eq!(l.radius, 24.0);
    }
    #[test]
    fn full_spans_monitor() {
        let s = lay(Shape::Full, 5).surfaces[0];
        assert_eq!((s.x, s.w, s.y + s.h), (0.0, 1920.0, 1080.0));
    }
    #[test]
    fn islands_three_surfaces_no_overlap() {
        let l = lay(Shape::Islands, 5);
        assert_eq!(l.surfaces.len(), 3);
        assert_eq!(l.surfaces[0].x, 8.0);
        assert_eq!(l.surfaces[2].x + l.surfaces[2].w, 1912.0);
        assert_no_overlap(&l.surfaces);
    }
    #[test]
    fn top_position_mirrors() {
        let s = lay_pos(Shape::Pill, Position::Top).surfaces[0];
        assert_eq!(s.y, 8.0);
    }
    #[test]
    fn reserved_values() {
        assert_eq!(lay(Shape::Full, 1).reserved, 48.0);
        assert_eq!(lay(Shape::Pill, 1).reserved, 64.0);
        assert_eq!(lay_noreserve(Shape::Pill).reserved, 0.0);
    }
    #[test]
    fn every_item_inside_a_surface() {
        for shape in ALL_SHAPES {
            for n in [0, 1, 12, 200] {
                let l = lay(shape, n);
                for r in all_items(&l) {
                    assert!(
                        l.surfaces.iter().any(|s| contains(s, &r)),
                        "{shape:?} n={n}: {r:?} outside"
                    );
                }
            }
        }
    }
    #[test]
    fn many_apps_use_more_button() {
        let l = lay(Shape::Pill, 200);
        assert!(l.more.is_some());
        assert!(l.apps.len() < 200);
        assert_no_overlap(&all_items(&l));
    }
    #[test]
    fn portrait_narrow_monitor() {
        let mon = Rect {
            x: 0.,
            y: 0.,
            w: 600.,
            h: 1024.,
        };
        let l = lay_on(mon, Shape::Islands, 30);
        assert_no_overlap(&l.surfaces);
        assert_no_overlap(&all_items(&l));
    }
    #[test]
    fn offset_monitor() {
        let mon = Rect {
            x: -1920.,
            y: 200.,
            w: 1920.,
            h: 1080.,
        };
        let l = lay_on(mon, Shape::Pill, 3);
        assert!(l.surfaces[0].x > -1920.0 && l.surfaces[0].x + l.surfaces[0].w < 0.0);
    }
    #[test]
    fn no_overlap_all_shapes() {
        for shape in ALL_SHAPES {
            for n in [0, 1, 5, 30, 200] {
                let l = lay(shape, n);
                assert_no_overlap(&all_items(&l));
                assert_no_overlap(&l.surfaces);
            }
        }
    }
    #[test]
    fn show_desktop_at_right_edge() {
        for shape in ALL_SHAPES {
            let l = lay(shape, 5);
            let last = l.surfaces.last().unwrap();
            assert_eq!(l.show_desktop.w, 6.0);
            assert_eq!(l.show_desktop.h, 48.0);
            assert!((l.show_desktop.right() - last.right()).abs() < EPS);
        }
    }
    #[test]
    fn secondary_bar_has_no_tray_area() {
        for shape in ALL_SHAPES {
            let with = lay(shape, 5);
            let without = compute_layout(&LayoutInput {
                tray_area: false,
                ..input(MON, shape, Position::Bottom, 5, true)
            });
            assert!(without.tray.is_empty(), "{shape:?}");
            assert_eq!(without.quick.w, 0.0);
            assert_eq!(without.overflow_chevron.w, 0.0);
            assert_eq!(without.clock.w, 64.0);
            // The clock still ends where the show-desktop strip begins.
            assert!(
                (without.clock.right() + PAD + SHOW_DESKTOP_W - without.show_desktop.right()).abs()
                    < EPS
            );
            let last = |l: &BarLayout| l.surfaces.last().unwrap().w;
            assert!(
                last(&without) < last(&with) || shape == Shape::Full,
                "{shape:?}"
            );
            assert_eq!(without.apps.len(), 5);
            assert_eq!(without.reserved, with.reserved);
            for r in all_items(&without) {
                assert!(
                    without.surfaces.iter().any(|s| contains(s, &r)),
                    "{shape:?}: {r:?}"
                );
            }
            assert_no_overlap(
                &all_items(&without)
                    .into_iter()
                    .filter(|r| r.w > 0.0)
                    .collect::<Vec<_>>(),
            );
        }
    }
    #[test]
    fn diff_monitors_added_and_removed() {
        assert_eq!(diff_monitors(&[1, 2], &[2, 3]), (vec![3], vec![1]));
        assert_eq!(diff_monitors(&[1, 2], &[1, 2]), (vec![], vec![]));
        assert_eq!(
            diff_monitors(&[2, 1], &[1, 2]),
            (vec![], vec![]),
            "order is no change"
        );
        assert_eq!(
            diff_monitors(&[], &[5, 4]),
            (vec![5, 4], vec![]),
            "new order kept"
        );
        assert_eq!(
            diff_monitors(&[7, 8], &[]),
            (vec![], vec![7, 8]),
            "old order kept"
        );
    }
    #[test]
    fn full_apps_centred_when_room() {
        let l = lay(Shape::Full, 5);
        let first = l.apps.first().unwrap();
        let last = l.apps.last().unwrap();
        assert!(((first.x + last.right()) / 2.0 - 960.0).abs() < 0.5);
    }
    #[test]
    fn widgets_sit_left_of_the_tray_in_the_pill() {
        let mut i = input(MON, Shape::Pill, Position::Bottom, 5, true);
        i.widgets = [120.0, 90.0];
        let l = compute_layout(&i);
        let (m, c) = (l.widgets[0].unwrap(), l.widgets[1].unwrap());
        assert!(m.right() <= c.x && c.right() <= l.overflow_chevron.x);
        assert!(l.surfaces[0].x <= m.x && c.right() <= l.surfaces[0].right());
        assert_eq!((m.w, c.w), (120.0, 90.0));
    }
    #[test]
    fn widgets_fit_and_dont_overlap_in_every_shape() {
        for shape in ALL_SHAPES {
            for n in [0, 5, 200] {
                let mut i = input(MON, shape, Position::Bottom, n, true);
                i.widgets = [120.0, 90.0];
                let l = compute_layout(&i);
                for r in all_items(&l) {
                    assert!(
                        l.surfaces.iter().any(|s| contains(s, &r)),
                        "{shape:?} {r:?}"
                    );
                }
                assert_no_overlap(&all_items(&l));
            }
        }
        // Islands: the widgets open the right island.
        let mut i = input(MON, Shape::Islands, Position::Bottom, 5, true);
        i.widgets = [120.0, 0.0];
        let l = compute_layout(&i);
        assert_eq!(l.widgets[0].unwrap().x, l.surfaces[2].x + PAD);
        assert!(l.widgets[1].is_none());
    }
    #[test]
    fn widgets_hidden_on_secondary_bars_and_when_zero() {
        let mut i = input(MON, Shape::Pill, Position::Bottom, 5, true);
        i.widgets = [120.0, 90.0];
        i.tray_area = false;
        assert_eq!(compute_layout(&i).widgets, [None, None]);
        i.tray_area = true;
        i.widgets = [0.0, 90.0];
        let l = compute_layout(&i);
        assert!(l.widgets[0].is_none() && l.widgets[1].is_some());
        i.widgets = [0.0, 0.0];
        assert_eq!(compute_layout(&i).widgets, [None, None]);
    }
    #[test]
    fn pill_grows_by_the_widgets() {
        let base = input(MON, Shape::Pill, Position::Bottom, 5, true);
        let mut with = base;
        with.widgets = [120.0, 90.0];
        let w0 = compute_layout(&base).surfaces[0].w;
        let w1 = compute_layout(&with).surfaces[0].w;
        assert!((w1 - (w0 + 120.0 + 90.0 + 2.0 * GAP)).abs() < EPS);
    }
    #[test]
    fn anchors_follow_the_element() {
        let mut i = input(MON, Shape::Islands, Position::Top, 5, true);
        i.widgets = [120.0, 90.0];
        let l = compute_layout(&i);
        assert_eq!(anchor_for(&l, AnchorTarget::Quick), l.quick);
        assert_eq!(anchor_for(&l, AnchorTarget::Clock), l.clock);
        assert_eq!(
            anchor_for(&l, AnchorTarget::Widget(1)),
            l.widgets[1].unwrap()
        );
        // hidden widget: falls back to the quick button
        let l = lay(Shape::Pill, 5);
        assert_eq!(anchor_for(&l, AnchorTarget::Widget(0)), l.quick);
    }
}
