//! The calendar panel controller: Vitrobar's own glass calendar above the bar's clock
//! (today's long date, a month grid with previous / next chevrons, a button to the stock
//! notification centre and one to the date and time settings).
//!
//! The month grid is a [`Row::Custom`] hole of the panel kit that this file draws into:
//! weekday initials, the day numbers (the neighbouring months' days dimmed) and an accent
//! circle behind today. While the panel is open one one-shot timer is armed for the next
//! midnight so "today" moves by itself (never a repeating timer).

use vitro_core::calendar::{
    self, action, cells, weekday_initials, Action, Date, Month, Weekday,
    CELL_H_DIP, ID_GRID, WEEKDAY_H_DIP,
};
use vitro_core::i18n::Lang;
use vitro_core::menu_model::NavKey;
use vitro_core::panel_kit::{focus_step, Part, Row};
use vitro_core::settings::Position;
use vitro_core::style::{MaterialStyle, Rgba};
use vitro_render::compositor::Comp;
use vitro_render::flyout::PxRect;
use vitro_render::panel_view::PanelView;
use vitro_render::surfaces::{FontKind, TextBox};
use vitro_render::{Vector2, Vector3};
use vitro_win::time_format;
use windows::core::Result;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    VK_DOWN, VK_END, VK_ESCAPE, VK_HOME, VK_LEFT, VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT,
    VK_SPACE, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    KillTimer, SetTimer, MSG, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
};
use windows::UI::Color;

use crate::bar_window::{mouse_pos, track_leave};
use crate::notify::TIMER_CALENDAR_MIDNIGHT;
use crate::quick::Gfx;

const WHEEL_DELTA: f32 = 120.0;
const WM_MOUSELEAVE: u32 = 0x02A3;
/// The timer fires a moment after midnight, never just before it.
const MIDNIGHT_SLACK_MS: u64 = 500;

const DAY_FONT_DIP: f32 = 14.0;
const INITIAL_FONT_DIP: f32 = 12.0;
const TODAY_DIAMETER_DIP: f32 = 32.0;
const DIM_ALPHA: u8 = 110;

/// What the app must do after an input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    None,
    Close,
    /// Close the panel and open the stock notification centre.
    Notifications,
    /// Close the panel and open this Windows Settings page.
    Open(String),
}

pub struct CalendarPanel {
    // Declared first: the window goes before the rest.
    view: PanelView,
    /// The monitor whose clock opened the panel.
    pub monitor: u64,
    /// The foreground window when the panel opened (see `QuickPanel::fg_at_open`).
    pub fg_at_open: usize,
    /// The panel took the foreground (it then hears keys and its deactivation).
    pub activated: bool,
    notify: HWND,
    lang: Lang,
    text: Rgba,
    accent: Rgba,
    first: Weekday,
    today: Date,
    shown: Month,
    rows: Vec<Row>,
    hot: Option<Part>,
    pressed: Option<Part>,
    focus: Option<Part>,
    tracking: bool,
}

fn ui_color(c: Rgba) -> Color {
    Color {
        A: c.3,
        R: c.0,
        G: c.1,
        B: c.2,
    }
}

/// Black or white text, whichever reads better on `bg`.
fn on_color(bg: Rgba) -> Rgba {
    let lum = 0.299 * f32::from(bg.0) + 0.587 * f32::from(bg.1) + 0.114 * f32::from(bg.2);
    if lum > 150.0 {
        Rgba(0, 0, 0, 255)
    } else {
        Rgba(255, 255, 255, 255)
    }
}

fn ok(r: Result<()>) -> Outcome {
    match r {
        Ok(()) => Outcome::None,
        Err(e) => {
            eprintln!("vitrobar: the calendar panel could not be drawn: {e}");
            Outcome::Close
        }
    }
}

fn local_today() -> (Date, u64) {
    let ((year, month, day), left) = time_format::today_and_midnight_ms();
    (Date { year, month, day }, left)
}

impl CalendarPanel {
    /// Opens the panel above `anchor_px` (below it for a top bar).
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        g: &Gfx,
        style: &MaterialStyle,
        dpi: u32,
        anchor_px: PxRect,
        position: Position,
        accent: Rgba,
        lang: Lang,
        notify: HWND,
        monitor: u64,
    ) -> Result<Self> {
        let first = Weekday::from_index(time_format::first_day_of_week());
        let (today, left) = local_today();
        let shown = today.month();
        let rows = calendar::rows(shown, today, lang);
        let view = PanelView::show(
            g.comp, g.painter, style, dpi, anchor_px, position, accent, &rows,
        )?;
        let mut p = Self {
            view,
            monitor,
            fg_at_open: 0,
            activated: false,
            notify,
            lang,
            text: style.text,
            accent,
            first,
            today,
            shown,
            rows,
            hot: None,
            pressed: None,
            focus: None,
            tracking: false,
        };
        p.draw_grid(g)?;
        p.arm_midnight(left);
        Ok(p)
    }

    pub fn hwnd(&self) -> HWND {
        self.view.hwnd()
    }

    pub fn rect_px(&self) -> PxRect {
        self.view.rect_px()
    }

    /// Takes the foreground; false if Windows refused.
    pub fn activate(&mut self) -> bool {
        self.activated |= self.view.activate();
        self.activated
    }

    // ---- drawing -----------------------------------------------------------------------------

    /// Shows the rows for the current month and today, and draws the grid into its hole.
    fn render(&mut self, g: &Gfx) -> Result<()> {
        self.rows = calendar::rows(self.shown, self.today, self.lang);
        self.view.update(g.comp, g.painter, &self.rows)?;
        self.draw_grid(g)
    }

    fn render_or_close(&mut self, g: &Gfx) -> Outcome {
        match self.render(g) {
            Ok(()) => Outcome::None,
            Err(e) => {
                eprintln!("vitrobar: the calendar panel could not be drawn: {e}");
                Outcome::Close
            }
        }
    }

    fn draw_grid(&mut self, g: &Gfx) -> Result<()> {
        let (Some(slot), Some(r)) = (
            self.view.custom_slot(ID_GRID),
            self.view.custom_rect_px(ID_GRID),
        ) else {
            return Ok(());
        };
        let comp: &windows::UI::Composition::Compositor = g.comp;
        let s = self.view.scale();
        let (w, h) = ((r.2 - r.0).max(1) as f32, (r.3 - r.1).max(1) as f32);
        let kids = slot.Children()?;
        kids.RemoveAll()?;
        let cell_w = w / 7.0;
        let head_h = (WEEKDAY_H_DIP * s).round();
        let cell_h = (CELL_H_DIP * s).round();
        let cx = |c: usize| c as f32 * cell_w;
        let cy = |r: usize| head_h + r as f32 * cell_h;
        // Text centred horizontally in its cell (the painter left-aligns).
        let centred = |t: &str, size: f32, x: f32, y: f32, bh: f32| -> (f32, f32, f32, f32) {
            let tw = g
                .painter
                .measure(t, FontKind::Text, size)
                .map(|(tw, _)| tw.ceil())
                .unwrap_or(cell_w / 2.0);
            (x + ((cell_w - tw) / 2.0).max(0.0), y, (cell_w - 2.0).max(1.0), bh)
        };

        let initials = weekday_initials(self.first, self.lang);
        let grid = cells(self.shown, self.first);
        let numbers: Vec<Vec<String>> = grid
            .iter()
            .map(|row| row.iter().map(|c| c.date.day.to_string()).collect())
            .collect();
        let text = self.text;
        let dim = Rgba(text.0, text.1, text.2, DIM_ALPHA);
        let (mut heads, mut inside, mut outside, mut now): (
            Vec<TextBox>,
            Vec<TextBox>,
            Vec<TextBox>,
            Vec<TextBox>,
        ) = Default::default();
        let size = DAY_FONT_DIP * s;
        let small = INITIAL_FONT_DIP * s;
        for (c, t) in initials.iter().enumerate() {
            heads.push((t, FontKind::Text, centred(t, small, cx(c), 0.0, head_h)));
        }
        let mut today_at = None;
        for (ri, row) in grid.iter().enumerate() {
            for (ci, cell) in row.iter().enumerate() {
                let t = numbers[ri][ci].as_str();
                let b = (t, FontKind::Text, centred(t, size, cx(ci), cy(ri), cell_h));
                if cell.in_month && cell.date == self.today {
                    today_at = Some((ci, ri));
                    now.push(b);
                } else if cell.in_month {
                    inside.push(b);
                } else {
                    outside.push(b);
                }
            }
        }

        if let Some((ci, ri)) = today_at {
            let d = (TODAY_DIAMETER_DIP * s).round();
            let geom = comp.CreateRoundedRectangleGeometry()?;
            geom.SetSize(Vector2::new(d, d))?;
            geom.SetCornerRadius(Vector2::new(d / 2.0, d / 2.0))?;
            let shape = comp.CreateSpriteShapeWithGeometry(&geom)?;
            shape.SetFillBrush(&comp.CreateColorBrushWithColor(ui_color(self.accent))?)?;
            let v = comp.CreateShapeVisual()?;
            v.SetSize(Vector2::new(d, d))?;
            v.Shapes()?.Append(&shape)?;
            v.SetOffset(Vector3::new(
                (cx(ci) + (cell_w - d) / 2.0).round(),
                (cy(ri) + (cell_h - d) / 2.0).round(),
                0.0,
            ))?;
            kids.InsertAtTop(&v)?;
        }
        let (pw, ph) = (w.ceil() as u32, h.ceil() as u32);
        for (boxes, px, color) in [
            (&heads, small, dim),
            (&outside, size, dim),
            (&inside, size, text),
            (&now, size, on_color(self.accent)),
        ] {
            if boxes.is_empty() {
                continue;
            }
            let surface = g.painter.text_boxes(boxes, px, color, pw, ph)?;
            let sprite = comp.CreateSpriteVisual()?;
            sprite.SetBrush(&comp.CreateSurfaceBrushWithSurface(&surface)?)?;
            sprite.SetSize(Vector2::new(pw as f32, ph as f32))?;
            kids.InsertAtTop(&sprite)?;
        }
        Ok(())
    }

    // ---- input -------------------------------------------------------------------------------

    /// A mouse message on the panel window (client px in `lParam`).
    pub fn on_mouse(&mut self, msg: &MSG, g: &Gfx) -> Outcome {
        let pt = mouse_pos(msg.lParam);
        match msg.message {
            WM_MOUSEMOVE => {
                if !self.tracking {
                    track_leave(self.hwnd());
                    self.tracking = true;
                }
                let hot = self.view.hit_px(pt);
                self.hot = hot;
                ok(self.view.set_hot(hot))
            }
            WM_MOUSELEAVE => {
                self.tracking = false;
                self.hot = None;
                ok(self.view.set_hot(None))
            }
            WM_LBUTTONDOWN => {
                if !self.activated {
                    // A click on our own window may take the foreground.
                    self.activate();
                }
                let part = self.view.hit_px(pt);
                self.pressed = part;
                ok(self.view.set_pressed(part))
            }
            WM_LBUTTONUP => {
                let pressed = self.pressed.take();
                if let Outcome::Close = ok(self.view.set_pressed(None)) {
                    return Outcome::Close;
                }
                match (pressed, self.view.hit_px(pt)) {
                    (Some(p), Some(q)) if p == q => self.activate_part(p, g),
                    _ => Outcome::None,
                }
            }
            _ => Outcome::None,
        }
    }

    /// A key on the panel (`WM_KEYDOWN`).
    pub fn on_key(&mut self, vk: u16, shift: bool, g: &Gfx) -> Outcome {
        let k = windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY(vk);
        match k {
            VK_ESCAPE => Outcome::Close,
            VK_TAB => self.move_focus(if shift { NavKey::Up } else { NavKey::Down }),
            VK_UP | VK_LEFT => self.move_focus(NavKey::Up),
            VK_DOWN | VK_RIGHT => self.move_focus(NavKey::Down),
            VK_END => self.move_focus(NavKey::End),
            VK_PRIOR => self.set_month(self.shown.prev(), g),
            VK_NEXT => self.set_month(self.shown.next(), g),
            // Back to today's month.
            VK_HOME => self.set_month(self.today.month(), g),
            VK_RETURN | VK_SPACE => match self.focus {
                Some(p) => self.activate_part(p, g),
                None => Outcome::None,
            },
            _ => Outcome::None,
        }
    }

    /// `WM_MOUSEWHEEL` (`screen_pt` in screen px): over the grid one notch up is the previous
    /// month, one down the next.
    pub fn on_wheel(&mut self, delta: i16, screen_pt: (i32, i32), g: &Gfx) -> Outcome {
        let r = self.rect_px();
        let pt = (screen_pt.0 - r.0, screen_pt.1 - r.1);
        if self.view.hit_px(pt) != Some(Part::Custom(ID_GRID)) {
            return Outcome::None;
        }
        let notches = f32::from(delta) / WHEEL_DELTA;
        let mut m = self.shown;
        for _ in 0..(notches.abs().round() as u32).clamp(1, 12) {
            m = if notches > 0.0 { m.prev() } else { m.next() };
        }
        self.set_month(m, g)
    }

    fn move_focus(&mut self, key: NavKey) -> Outcome {
        let f = focus_step(self.view.layout(), self.focus, key);
        self.focus = f;
        ok(self.view.set_focus(f))
    }

    fn set_month(&mut self, m: Month, g: &Gfx) -> Outcome {
        if m == self.shown {
            return Outcome::None;
        }
        self.shown = m;
        self.render_or_close(g)
    }

    fn activate_part(&mut self, p: Part, g: &Gfx) -> Outcome {
        match action(p) {
            Some(Action::PrevMonth) => self.set_month(self.shown.prev(), g),
            Some(Action::NextMonth) => self.set_month(self.shown.next(), g),
            Some(Action::Notifications) => Outcome::Notifications,
            Some(Action::DateSettings) => Outcome::Open("ms-settings:dateandtime".into()),
            None => Outcome::None,
        }
    }

    // ---- today -------------------------------------------------------------------------------

    /// Reads today again (midnight passed or the system clock changed): the highlighted day
    /// moves, and a panel showing the old today's month follows to the new one. Re-arms the
    /// midnight timer.
    pub fn refresh_today(&mut self, g: &Gfx) -> Outcome {
        let (today, left) = local_today();
        self.arm_midnight(left);
        if today == self.today {
            return Outcome::None;
        }
        if self.shown == self.today.month() {
            self.shown = today.month();
        }
        self.today = today;
        self.render_or_close(g)
    }

    /// One one-shot timer for the next midnight, replacing the previous one.
    fn arm_midnight(&self, left_ms: u64) {
        let ms = (left_ms + MIDNIGHT_SLACK_MS).min(u64::from(u32::MAX)) as u32;
        // SAFETY: one-shot timer on the app's notify window (killed when it fires or when
        // the panel closes); setting the same id again replaces it.
        unsafe {
            SetTimer(Some(self.notify), TIMER_CALENDAR_MIDNIGHT, ms, None);
        }
    }

    /// `WM_TIMER` for the midnight timer; `None` for any other id.
    pub fn on_timer(&mut self, id: usize, g: &Gfx) -> Option<Outcome> {
        if id != TIMER_CALENDAR_MIDNIGHT {
            return None;
        }
        // SAFETY: timer on the app's notify window.
        unsafe {
            let _ = KillTimer(Some(self.notify), TIMER_CALENDAR_MIDNIGHT);
        }
        Some(self.refresh_today(g))
    }

    // ---- layout and look ---------------------------------------------------------------------

    /// The bar was laid out anew: the panel follows the clock.
    pub fn reanchor(&mut self, comp: &Comp, anchor_px: PxRect) -> Result<()> {
        self.view.reanchor(comp, anchor_px)
    }

    /// Theme, material or accent changed while open.
    pub fn restyle(&mut self, g: &Gfx, style: &MaterialStyle, accent: Rgba) -> Result<()> {
        self.text = style.text;
        self.accent = accent;
        self.view
            .restyle(g.comp, g.painter, style, accent, &self.rows)?;
        self.draw_grid(g)
    }
}

impl Drop for CalendarPanel {
    fn drop(&mut self) {
        // SAFETY: timer on the app's notify window.
        unsafe {
            let _ = KillTimer(Some(self.notify), TIMER_CALENDAR_MIDNIGHT);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vitro_core::calendar::ms_until_midnight;

    #[test]
    fn text_on_the_accent_is_readable() {
        assert_eq!(on_color(Rgba(0, 120, 215, 255)), Rgba(255, 255, 255, 255));
        assert_eq!(on_color(Rgba(255, 220, 0, 255)), Rgba(0, 0, 0, 255));
    }

    #[test]
    fn the_next_midnight_is_never_zero_ms_away() {
        assert!(ms_until_midnight(23, 59, 59, 999) >= 1);
    }

    #[test]
    fn the_grid_hole_fits_six_weeks_and_the_heading() {
        assert_eq!(calendar::GRID_H_DIP, WEEKDAY_H_DIP + 6.0 * CELL_H_DIP);
    }
}
