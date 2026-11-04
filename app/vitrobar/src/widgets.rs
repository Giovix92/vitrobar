//! The bar's widgets (spec §5.7). Claude limits: a pill with the 5-hour and weekly usage that
//! Claude Code's status line saved (ruling P1) and a detail panel with a bar for each. Music: a pill with the current track (its glyph toggles
//! play / pause, its text opens the detail panel) and a glass detail panel with the artwork,
//! title, artist, the transport buttons and the player's name.
//!
//! A disabled widget loads nothing (P4): [`Music`] owns the media service and exists only
//! while the widget is on. The artwork is read only while the detail panel is open.

use vitro_core::i18n::Lang;
use vitro_core::image::BgraImage;
use vitro_core::menu_model::NavKey;
use vitro_core::panel_kit::{focus_step, Part, Row};
use vitro_core::settings::Position;
use vitro_core::style::{MaterialStyle, Rgba};
use vitro_core::widgets::{
    bar_fill, claude_pill_text, claude_rows, music_action, music_glyph, music_pill_text,
    music_rows, pill_width, ClaudeUsage, MusicAction, MusicView, ART_DIP, CLAUDE_PILL_MAX_DIP,
    G_CLAUDE, ID_ART, ID_BAR_5H, ID_BAR_7D, MUSIC_PILL_MAX_CHARS, MUSIC_PILL_MAX_DIP,
    PILL_FONT_DIP, next_pill_change,
};
use vitro_render::bar_visual::WidgetPill;
use vitro_render::compositor::Comp;
use vitro_render::flyout::PxRect;
use vitro_render::panel_view::PanelView;
use vitro_render::surfaces::{FontKind, Painter, TextBox};
use vitro_render::{Vector2, Vector3};
use vitro_win::claude_usage::{self, Watcher};
use vitro_win::media::{Change, Media, MediaSnapshot};
use vitro_win::time_format;
use windows::core::Result;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    VK_DOWN, VK_END, VK_ESCAPE, VK_HOME, VK_LEFT, VK_RETURN, VK_RIGHT, VK_SPACE, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{MSG, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE};
use windows::UI::Color;

use crate::bar_window::{mouse_pos, track_leave};
use crate::notify::{WM_APP_CLAUDE, WM_APP_MEDIA};
use crate::quick::Gfx;

const WM_MOUSELEAVE: u32 = 0x02A3;
/// Segoe Fluent Icons "MusicInfo": the artwork's stand-in.
const G_NO_ART: char = '\u{E8D6}';
const TITLE_FONT_DIP: f32 = 15.0;
const ARTIST_FONT_DIP: f32 = 13.0;
const ART_RADIUS_DIP: f32 = 6.0;
const ART_GAP_DIP: f32 = 12.0;
const DIM_ALPHA: u8 = 150;
const NO_ART_ALPHA: u8 = 28;

pub fn view_of(s: MediaSnapshot) -> MusicView {
    MusicView {
        app: s.app,
        title: s.title,
        artist: s.artist,
        playing: s.playing,
        can_prev: s.can_prev,
        can_next: s.can_next,
        can_play_pause: s.can_play_pause,
    }
}

/// What a media wake-up changed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Wake {
    /// The pill's text, glyph or width changed: lay the bars out again.
    pub pill: bool,
    /// The session changed (the open panel shows it anew).
    pub session: bool,
    /// The artwork the panel asked for is ready.
    pub thumbnail: bool,
}

/// The music widget while it is enabled.
pub struct Music {
    media: Media,
    pub view: Option<MusicView>,
    /// `None`: no session, the pill hides.
    pub pill: Option<WidgetPill>,
    /// DIP; 0 while hidden.
    pub width: f32,
}

impl Music {
    pub fn open(notify: HWND) -> Result<Self> {
        Ok(Self {
            media: Media::open(notify, WM_APP_MEDIA)?,
            view: None,
            pill: None,
            width: 0.0,
        })
    }

    /// [`WM_APP_MEDIA`] arrived: reads what changed and re-measures the pill.
    pub fn on_wake(&mut self, painter: &Painter) -> Wake {
        let mut w = Wake::default();
        for c in self.media.drain_changes() {
            match c {
                Change::Session => w.session = true,
                Change::Thumbnail => w.thumbnail = true,
            }
        }
        if w.session {
            self.view = self.media.snapshot().map(view_of);
            w.pill = self.remeasure(painter);
        }
        w
    }

    /// Recomputes the pill from the current view; true if it changed.
    pub fn remeasure(&mut self, painter: &Painter) -> bool {
        let text = music_pill_text(self.view.as_ref(), MUSIC_PILL_MAX_CHARS);
        let pill = (!text.is_empty()).then(|| WidgetPill {
            glyph: music_glyph(self.view.as_ref().is_some_and(|v| v.playing)),
            text: text.clone(),
        });
        let text_w = painter
            .measure(&text, FontKind::Text, PILL_FONT_DIP)
            .map(|(w, _)| w.ceil())
            .unwrap_or(text.chars().count() as f32 * PILL_FONT_DIP * 0.6);
        let width = pill_width(&text, text_w, MUSIC_PILL_MAX_DIP).round();
        let changed = pill != self.pill || width != self.width;
        self.pill = pill;
        self.width = width;
        changed
    }

    pub fn act(&self, a: MusicAction) {
        match a {
            MusicAction::Prev => self.media.prev(),
            MusicAction::PlayPause => self.media.play_pause(),
            MusicAction::Next => self.media.next(),
        }
    }

    pub fn request_thumbnail(&self, px: u32) {
        self.media.request_thumbnail(px);
    }

    pub fn take_thumbnail(&self) -> Option<BgraImage> {
        self.media.take_thumbnail()
    }
}

// ---- the detail panel ---------------------------------------------------------------------

/// What the app must do after an input on the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    None,
    Close,
    Act(MusicAction),
}

fn ui_color(c: Rgba) -> Color {
    Color {
        A: c.3,
        R: c.0,
        G: c.1,
        B: c.2,
    }
}

fn ok(r: Result<()>) -> Outcome {
    match r {
        Ok(()) => Outcome::None,
        Err(e) => {
            eprintln!("vitrobar: the music panel could not be drawn: {e}");
            Outcome::Close
        }
    }
}

pub struct MusicPanel {
    // Declared first: the window goes before the rest.
    view: PanelView,
    /// The foreground window when the panel opened (see `QuickPanel::fg_at_open`).
    pub fg_at_open: usize,
    /// The panel took the foreground (it then hears keys and its deactivation).
    pub activated: bool,
    data: MusicView,
    art: Option<BgraImage>,
    /// The track changed since the artwork was last asked for.
    art_wanted: bool,
    lang: Lang,
    text: Rgba,
    rows: Vec<Row>,
    hot: Option<Part>,
    pressed: Option<Part>,
    focus: Option<Part>,
    tracking: bool,
}

impl MusicPanel {
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
        data: MusicView,
    ) -> Result<Self> {
        let rows = music_rows(&data, lang);
        let view = PanelView::show(
            g.comp, g.painter, style, dpi, anchor_px, position, accent, &rows,
        )?;
        let mut p = Self {
            view,
            fg_at_open: 0,
            activated: false,
            data,
            art: None,
            art_wanted: true,
            lang,
            text: style.text,
            rows,
            hot: None,
            pressed: None,
            focus: None,
            tracking: false,
        };
        p.draw_art(g)?;
        Ok(p)
    }

    pub fn hwnd(&self) -> HWND {
        self.view.hwnd()
    }

    pub fn rect_px(&self) -> PxRect {
        self.view.rect_px()
    }

    /// The artwork's size in px on this panel's monitor.
    pub fn art_px(&self) -> u32 {
        (ART_DIP * self.view.scale()).round().max(1.0) as u32
    }

    /// True once per new track (and at open): the caller asks the service for the artwork.
    pub fn take_art_wanted(&mut self) -> bool {
        std::mem::take(&mut self.art_wanted)
    }

    /// Takes the foreground; false if Windows refused.
    pub fn activate(&mut self) -> bool {
        self.activated |= self.view.activate();
        self.activated
    }

    /// The session changed: new rows (buttons come and go) and a new title block. A new
    /// track drops the old artwork; the caller asks for the new one.
    pub fn set_data(&mut self, g: &Gfx, data: MusicView) -> Outcome {
        if data == self.data {
            return Outcome::None;
        }
        if (&data.title, &data.artist, &data.app) != (&self.data.title, &self.data.artist, &self.data.app) {
            self.art = None;
            self.art_wanted = true;
        }
        self.data = data;
        self.rows = music_rows(&self.data, self.lang);
        if let Err(e) = self.view.update(g.comp, g.painter, &self.rows) {
            return ok(Err(e));
        }
        // A button that went away may have held the focus or the hover.
        let l = self.view.layout();
        let still = |p: Option<Part>| p.filter(|p| l.parts.iter().any(|(q, _)| q == p));
        self.focus = still(self.focus);
        self.hot = still(self.hot);
        let _ = self.view.set_focus(self.focus);
        let _ = self.view.set_hot(self.hot);
        ok(self.draw_art(g))
    }

    pub fn set_art(&mut self, g: &Gfx, art: Option<BgraImage>) -> Outcome {
        self.art = art;
        ok(self.draw_art(g))
    }

    /// The artwork (or its stand-in) and the title and artist beside it, into the hole.
    fn draw_art(&mut self, g: &Gfx) -> Result<()> {
        let (Some(slot), Some(r)) = (
            self.view.custom_slot(ID_ART),
            self.view.custom_rect_px(ID_ART),
        ) else {
            return Ok(());
        };
        let comp: &windows::UI::Composition::Compositor = g.comp;
        let s = self.view.scale();
        let (w, h) = ((r.2 - r.0).max(1) as f32, (r.3 - r.1).max(1) as f32);
        let kids = slot.Children()?;
        kids.RemoveAll()?;
        let side = self.art_px() as f32;
        let top = ((h - side) / 2.0).round();
        let radius = ART_RADIUS_DIP * s;

        let geom = comp.CreateRoundedRectangleGeometry()?;
        geom.SetSize(Vector2::new(side, side))?;
        geom.SetCornerRadius(Vector2::new(radius, radius))?;
        match self.art.as_ref().map(|a| g.painter.image(a)) {
            Some(Ok(surface)) => {
                let v = comp.CreateSpriteVisual()?;
                v.SetBrush(&comp.CreateSurfaceBrushWithSurface(&surface)?)?;
                v.SetSize(Vector2::new(side, side))?;
                v.SetOffset(Vector3::new(0.0, top, 0.0))?;
                v.SetClip(&comp.CreateGeometricClipWithGeometry(&geom)?)?;
                kids.InsertAtTop(&v)?;
            }
            _ => {
                let shape = comp.CreateSpriteShapeWithGeometry(&geom)?;
                let t = self.text;
                shape.SetFillBrush(
                    &comp.CreateColorBrushWithColor(ui_color(Rgba(t.0, t.1, t.2, NO_ART_ALPHA)))?,
                )?;
                let v = comp.CreateShapeVisual()?;
                v.SetSize(Vector2::new(side, side))?;
                v.Shapes()?.Append(&shape)?;
                v.SetOffset(Vector3::new(0.0, top, 0.0))?;
                kids.InsertAtTop(&v)?;
                let glyph = G_NO_ART.to_string();
                let boxes: [TextBox; 1] = [(glyph.as_str(), FontKind::Glyph, (0.0, top, side, side))];
                self.text_sprite(g, &boxes, 24.0 * s, self.dim(), w, h)?;
            }
        }

        let x = side + (ART_GAP_DIP * s).round();
        let tw = (w - x).max(1.0);
        let line = (h / 2.0).round();
        let title: [TextBox; 1] = [(self.data.title.as_str(), FontKind::Text, (x, top, tw, line - top))];
        self.text_sprite(g, &title, TITLE_FONT_DIP * s, self.text, w, h)?;
        let artist: [TextBox; 1] = [(
            self.data.artist.as_str(),
            FontKind::Text,
            (x, line, tw, top + side - line),
        )];
        self.text_sprite(g, &artist, ARTIST_FONT_DIP * s, self.dim(), w, h)
    }

    fn dim(&self) -> Rgba {
        Rgba(self.text.0, self.text.1, self.text.2, DIM_ALPHA)
    }

    fn text_sprite(&self, g: &Gfx, boxes: &[TextBox], px: f32, color: Rgba, w: f32, h: f32) -> Result<()> {
        let Some(slot) = self.view.custom_slot(ID_ART) else {
            return Ok(());
        };
        if boxes.iter().all(|b| b.0.is_empty()) {
            return Ok(());
        }
        let comp: &windows::UI::Composition::Compositor = g.comp;
        let (pw, ph) = (w.ceil() as u32, h.ceil() as u32);
        let surface = g.painter.text_boxes(boxes, px, color, pw, ph)?;
        let sprite = comp.CreateSpriteVisual()?;
        sprite.SetBrush(&comp.CreateSurfaceBrushWithSurface(&surface)?)?;
        sprite.SetSize(Vector2::new(pw as f32, ph as f32))?;
        slot.Children()?.InsertAtTop(&sprite)
    }

    // ---- input -------------------------------------------------------------------------------

    /// A mouse message on the panel window (client px in `lParam`).
    pub fn on_mouse(&mut self, msg: &MSG) -> Outcome {
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
                    (Some(p), Some(q)) if p == q => Self::activate_part(p),
                    _ => Outcome::None,
                }
            }
            _ => Outcome::None,
        }
    }

    /// A key on the panel (`WM_KEYDOWN`).
    pub fn on_key(&mut self, vk: u16, shift: bool) -> Outcome {
        let k = windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY(vk);
        match k {
            VK_ESCAPE => Outcome::Close,
            VK_TAB => self.move_focus(if shift { NavKey::Up } else { NavKey::Down }),
            VK_UP | VK_LEFT => self.move_focus(NavKey::Up),
            VK_DOWN | VK_RIGHT => self.move_focus(NavKey::Down),
            VK_HOME => self.move_focus(NavKey::Home),
            VK_END => self.move_focus(NavKey::End),
            VK_RETURN | VK_SPACE => match self.focus {
                Some(p) => Self::activate_part(p),
                None => Outcome::None,
            },
            _ => Outcome::None,
        }
    }

    fn move_focus(&mut self, key: NavKey) -> Outcome {
        let f = focus_step(self.view.layout(), self.focus, key);
        self.focus = f;
        ok(self.view.set_focus(f))
    }

    fn activate_part(p: Part) -> Outcome {
        match music_action(p) {
            Some(a) => Outcome::Act(a),
            None => Outcome::None,
        }
    }

    // ---- layout and look ---------------------------------------------------------------------

    /// The bar was laid out anew: the panel follows the pill.
    pub fn reanchor(&mut self, comp: &Comp, anchor_px: PxRect) -> Result<()> {
        self.view.reanchor(comp, anchor_px)
    }

    /// Theme, material or accent changed while open.
    pub fn restyle(&mut self, g: &Gfx, style: &MaterialStyle, accent: Rgba) -> Result<()> {
        self.text = style.text;
        self.view
            .restyle(g.comp, g.painter, style, accent, &self.rows)?;
        self.draw_art(g)
    }
}

// ---- Claude limits ------------------------------------------------------------------------

fn now_s() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// The Claude limits widget while it is enabled: the watch on the bridge's file (P4).
pub struct Claude {
    _watcher: Watcher,
    pub usage: Option<ClaudeUsage>,
    pub pill: Option<WidgetPill>,
    /// DIP.
    pub width: f32,
}

impl Claude {
    pub fn open(notify: HWND) -> std::io::Result<Self> {
        Ok(Self {
            _watcher: claude_usage::watch(notify, WM_APP_CLAUDE)?,
            usage: claude_usage::read(),
            pill: None,
            width: 0.0,
        })
    }

    /// The bridge wrote the file: read it again. True if the pill changed.
    pub fn reload(&mut self, painter: &Painter, lang: Lang) -> bool {
        self.usage = claude_usage::read();
        self.remeasure(painter, lang)
    }

    /// Recomputes the pill for now; true if it changed.
    pub fn remeasure(&mut self, painter: &Painter, lang: Lang) -> bool {
        let text = claude_pill_text(self.usage.as_ref(), now_s(), lang);
        let text_w = painter
            .measure(&text, FontKind::Text, PILL_FONT_DIP)
            .map(|(w, _)| w.ceil())
            .unwrap_or(text.chars().count() as f32 * PILL_FONT_DIP * 0.6);
        let pill = Some(WidgetPill {
            glyph: G_CLAUDE,
            text: text.clone(),
        });
        let width = pill_width(&text, text_w, CLAUDE_PILL_MAX_DIP).round();
        let changed = pill != self.pill || width != self.width;
        self.pill = pill;
        self.width = width;
        changed
    }

    /// Milliseconds until the pill's text changes by itself (a window resets, or the data
    /// turns "old"); `None` when nothing is ahead.
    pub fn ms_until_change(&self) -> Option<u32> {
        let now = now_s();
        let at = next_pill_change(&self.usage?, now)?;
        Some(((at - now).max(1) as u64 * 1000).min(u64::from(u32::MAX)) as u32)
    }
}

pub struct ClaudePanel {
    // Declared first: the window goes before the rest.
    view: PanelView,
    pub fg_at_open: usize,
    pub activated: bool,
    usage: Option<ClaudeUsage>,
    lang: Lang,
    text: Rgba,
    accent: Rgba,
    rows: Vec<Row>,
    tracking: bool,
}

const BAR_THICK_DIP: f32 = 6.0;
const TRACK_ALPHA: u8 = 40;

fn claude_rows_now(usage: Option<&ClaudeUsage>, lang: Lang) -> Vec<Row> {
    let at = usage.map(|u| time_format::local_short_time(u.at)).unwrap_or_default();
    claude_rows(usage, now_s(), &at, lang)
}

impl ClaudePanel {
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        g: &Gfx,
        style: &MaterialStyle,
        dpi: u32,
        anchor_px: PxRect,
        position: Position,
        accent: Rgba,
        lang: Lang,
        usage: Option<ClaudeUsage>,
    ) -> Result<Self> {
        let rows = claude_rows_now(usage.as_ref(), lang);
        let view = PanelView::show(
            g.comp, g.painter, style, dpi, anchor_px, position, accent, &rows,
        )?;
        let mut p = Self {
            view,
            fg_at_open: 0,
            activated: false,
            usage,
            lang,
            text: style.text,
            accent,
            rows,
            tracking: false,
        };
        p.draw_bars(g)?;
        Ok(p)
    }

    pub fn hwnd(&self) -> HWND {
        self.view.hwnd()
    }

    pub fn rect_px(&self) -> PxRect {
        self.view.rect_px()
    }

    pub fn activate(&mut self) -> bool {
        self.activated |= self.view.activate();
        self.activated
    }

    /// New data, or a minute passed ("resets in" counts down): rows and bars anew.
    pub fn refresh(&mut self, g: &Gfx, usage: Option<ClaudeUsage>) -> Outcome {
        self.usage = usage;
        let rows = claude_rows_now(self.usage.as_ref(), self.lang);
        if rows != self.rows {
            self.rows = rows;
            if let Err(e) = self.view.update(g.comp, g.painter, &self.rows) {
                return ok(Err(e));
            }
        }
        ok(self.draw_bars(g))
    }

    /// A track and an accent fill for each window's progress bar hole.
    fn draw_bars(&mut self, g: &Gfx) -> Result<()> {
        let comp: &windows::UI::Composition::Compositor = g.comp;
        let s = self.view.scale();
        let now = now_s();
        let windows = self
            .usage
            .map_or([None, None], |u| [u.five_hour.map(|w| (ID_BAR_5H, w)), u.seven_day.map(|w| (ID_BAR_7D, w))]);
        for (id, w) in windows.into_iter().flatten() {
            let (Some(slot), Some(r)) = (self.view.custom_slot(id), self.view.custom_rect_px(id))
            else {
                continue;
            };
            let kids = slot.Children()?;
            kids.RemoveAll()?;
            let (bw, bh) = ((r.2 - r.0).max(1) as f32, (r.3 - r.1).max(1) as f32);
            let thick = (BAR_THICK_DIP * s).round().max(1.0);
            let top = ((bh - thick) / 2.0).round();
            let t = self.text;
            let fill = (bw * bar_fill(&w, now)).round();
            for (width, color) in [
                (bw, Rgba(t.0, t.1, t.2, TRACK_ALPHA)),
                (fill, self.accent),
            ] {
                if width < 1.0 {
                    continue;
                }
                let geom = comp.CreateRoundedRectangleGeometry()?;
                geom.SetSize(Vector2::new(width, thick))?;
                geom.SetCornerRadius(Vector2::new(thick / 2.0, thick / 2.0))?;
                let shape = comp.CreateSpriteShapeWithGeometry(&geom)?;
                shape.SetFillBrush(&comp.CreateColorBrushWithColor(ui_color(color))?)?;
                let v = comp.CreateShapeVisual()?;
                v.SetSize(Vector2::new(width, thick))?;
                v.Shapes()?.Append(&shape)?;
                v.SetOffset(Vector3::new(0.0, top, 0.0))?;
                kids.InsertAtTop(&v)?;
            }
        }
        Ok(())
    }

    pub fn on_mouse(&mut self, msg: &MSG) -> Outcome {
        match msg.message {
            WM_MOUSEMOVE if !self.tracking => {
                track_leave(self.hwnd());
                self.tracking = true;
                Outcome::None
            }
            WM_MOUSELEAVE => {
                self.tracking = false;
                Outcome::None
            }
            WM_LBUTTONDOWN if !self.activated => {
                self.activate();
                Outcome::None
            }
            _ => Outcome::None,
        }
    }

    pub fn on_key(&mut self, vk: u16) -> Outcome {
        if windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY(vk) == VK_ESCAPE {
            Outcome::Close
        } else {
            Outcome::None
        }
    }

    pub fn reanchor(&mut self, comp: &Comp, anchor_px: PxRect) -> Result<()> {
        self.view.reanchor(comp, anchor_px)
    }

    pub fn restyle(&mut self, g: &Gfx, style: &MaterialStyle, accent: Rgba) -> Result<()> {
        self.text = style.text;
        self.accent = accent;
        self.view
            .restyle(g.comp, g.painter, style, accent, &self.rows)?;
        self.draw_bars(g)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_snapshot_becomes_the_core_view() {
        let v = view_of(MediaSnapshot {
            app: "Spotify.exe".into(),
            title: "T".into(),
            artist: "A".into(),
            playing: true,
            can_prev: false,
            can_next: true,
            can_play_pause: true,
        });
        assert_eq!(
            v,
            MusicView {
                app: "Spotify.exe".into(),
                title: "T".into(),
                artist: "A".into(),
                playing: true,
                can_prev: false,
                can_next: true,
                can_play_pause: true,
            }
        );
    }
}
