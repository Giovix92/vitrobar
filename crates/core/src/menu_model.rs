//! The glass right-click menus' model: entries, layout, hit test, keyboard navigation and
//! placement. Pure; `vitro-render` draws it and the app runs it.

use crate::layout::Rect;

/// Around the rows, and the side inset of a row's highlight.
pub const PAD_DIP: f32 = 4.0;
pub const ROW_H_DIP: f32 = 32.0;
pub const SEP_H_DIP: f32 = 9.0;
/// Where a row's text starts (room for the check mark before it).
pub const TEXT_LEFT_DIP: f32 = 36.0;
pub const TEXT_RIGHT_DIP: f32 = 16.0;
pub const MIN_W_DIP: f32 = 180.0;
pub const MAX_W_DIP: f32 = 360.0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    Command {
        id: u32,
        text: String,
        checked: bool,
    },
    Separator,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MenuLayout {
    /// Size of the whole menu, DIP.
    pub size: (f32, f32),
    /// One rectangle per entry (separators included), DIP, from the menu's top-left corner.
    pub rows: Vec<Rect>,
}

/// Stacks `entries`; the menu is as wide as its longest text (`text_w` measures one in DIPs)
/// plus the text insets, within [`MIN_W_DIP`] and [`MAX_W_DIP`].
pub fn layout(entries: &[Entry], text_w: &dyn Fn(&str) -> f32) -> MenuLayout {
    let widest = entries
        .iter()
        .filter_map(|e| match e {
            Entry::Command { text, .. } => Some(text_w(text)),
            Entry::Separator => None,
        })
        .fold(0.0, f32::max);
    let w = (TEXT_LEFT_DIP + widest + TEXT_RIGHT_DIP).clamp(MIN_W_DIP, MAX_W_DIP);
    let mut y = PAD_DIP;
    let rows = entries
        .iter()
        .map(|e| {
            let h = match e {
                Entry::Command { .. } => ROW_H_DIP,
                Entry::Separator => SEP_H_DIP,
            };
            let r = Rect {
                x: PAD_DIP,
                y,
                w: w - 2.0 * PAD_DIP,
                h,
            };
            y += h;
            r
        })
        .collect();
    MenuLayout {
        size: (w, y + PAD_DIP),
        rows,
    }
}

/// The command row under `pt_dip` (separators and padding give `None`).
pub fn hit(l: &MenuLayout, entries: &[Entry], pt_dip: (f32, f32)) -> Option<usize> {
    let (x, y) = pt_dip;
    l.rows.iter().zip(entries).position(|(r, e)| {
        matches!(e, Entry::Command { .. })
            && x >= r.x
            && x < r.right()
            && y >= r.y
            && y < r.bottom()
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavKey {
    Up,
    Down,
    Home,
    End,
}

/// The row selected after `key`, skipping separators and wrapping around.
pub fn step(entries: &[Entry], cur: Option<usize>, key: NavKey) -> Option<usize> {
    let cmds: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| matches!(e, Entry::Command { .. }))
        .map(|(i, _)| i)
        .collect();
    let (first, last) = (*cmds.first()?, *cmds.last()?);
    let at = cur.and_then(|c| cmds.iter().position(|&i| i == c));
    Some(match (key, at) {
        (NavKey::Home, _) | (NavKey::Down, None) => first,
        (NavKey::End, _) | (NavKey::Up, None) => last,
        (NavKey::Down, Some(p)) => cmds[(p + 1) % cmds.len()],
        (NavKey::Up, Some(p)) => cmds[(p + cmds.len() - 1) % cmds.len()],
    })
}

/// Top-left corner for a `size_px` menu opened at `at`: up-left of the point when it lies in
/// the lower / right half of `work` (so it opens away from the bar), else down-right; then
/// kept inside `work` (left, top, right, bottom).
pub fn place(at: (i32, i32), size_px: (i32, i32), work: (i32, i32, i32, i32)) -> (i32, i32) {
    let (l, t, r, b) = work;
    let x = if at.0 > (l + r) / 2 {
        at.0 - size_px.0
    } else {
        at.0
    };
    let y = if at.1 > (t + b) / 2 {
        at.1 - size_px.1
    } else {
        at.1
    };
    (x.min(r - size_px.0).max(l), y.min(b - size_px.1).max(t))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(id: u32, text: &str) -> Entry {
        Entry::Command {
            id,
            text: text.to_string(),
            checked: false,
        }
    }

    fn e() -> Vec<Entry> {
        vec![
            cmd(1, "Settings…"),
            Entry::Separator,
            cmd(5, "Start with Windows"),
            cmd(6, "Exit"),
        ]
    }

    #[test]
    fn rows_stack_with_padding() {
        let l = layout(&e(), &|_| 100.0);
        assert_eq!(l.rows[0].y, 4.0);
        assert_eq!(l.rows[1].h, 9.0);
        assert_eq!(l.rows[2].y, 4.0 + 32.0 + 9.0);
        assert_eq!(l.size, (180.0, 4.0 + 32.0 * 3.0 + 9.0 + 4.0));
    }

    #[test]
    fn width_follows_the_longest_text_within_bounds() {
        assert_eq!(layout(&e(), &|_| 200.0).size.0, 36.0 + 200.0 + 16.0);
        assert_eq!(layout(&e(), &|_| 900.0).size.0, 360.0);
    }

    #[test]
    fn separators_are_never_hit_or_selected() {
        let (en, l) = (e(), layout(&e(), &|_| 100.0));
        assert_eq!(hit(&l, &en, (50.0, 4.0 + 32.0 + 4.0)), None);
        assert_eq!(hit(&l, &en, (50.0, 10.0)), Some(0));
        assert_eq!(hit(&l, &en, (50.0, 1.0)), None, "padding");
        assert_eq!(step(&en, Some(0), NavKey::Down), Some(2));
        assert_eq!(step(&en, Some(2), NavKey::Up), Some(0));
        assert_eq!(step(&en, Some(3), NavKey::Down), Some(0));
        assert_eq!(step(&en, None, NavKey::Up), Some(3));
        assert_eq!(step(&en, Some(2), NavKey::Home), Some(0));
        assert_eq!(step(&en, Some(0), NavKey::End), Some(3));
        assert_eq!(step(&[], None, NavKey::Down), None);
    }

    #[test]
    fn opens_away_from_the_bar_and_stays_on_screen() {
        let work = (0, 0, 1920, 1040);
        assert_eq!(place((100, 1030), (200, 150), work), (100, 880));
        assert_eq!(place((1900, 1030), (200, 150), work), (1700, 880));
        assert_eq!(place((100, 10), (200, 150), work), (100, 10));
        // Down-right but too close to the right edge cannot happen; a tiny work area clamps.
        assert_eq!(place((100, 10), (200, 150), (0, 0, 150, 100)), (0, 0));
    }
}
