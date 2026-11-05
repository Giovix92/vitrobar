//! The settings window: a sidebar of categories and a glass page for each, applied to the
//! real bar as it is changed and saved at once (no OK / Cancel, like Windows Settings).
//!
//! [`SettingsCtl`] is the pure controller (pointer, wheel and keys in, a
//! [`SettingsAction`] out); [`apply_choice`] changes the settings and says what else the
//! change needs ([`Effect`]); [`SettingsWindow`] is the open window (a [`Panel`] with a
//! [`SettingsView`]). The app routes the window's messages here by `msg.hwnd`, as it does for
//! the Start panel.

use vitro_core::settings::Settings;
use vitro_core::settings_ui::{
    apply, build_page, first_focusable, focus_next, focus_prev, ControlId, ControlKind, Section,
    SettingsModel, ACCENT_SWATCHES, SECTIONS,
};
use vitro_core::style::MaterialStyle;
use vitro_render::compositor::Comp;
use vitro_render::flyout::PxRect;
use vitro_render::panel::Panel;
use vitro_render::settings_view::{
    close_rect, hit, max_scroll, nav_hit, page_w, reveal, viewport_h, Interaction, SettingsLook,
    SettingsView, Spot, CLOSE_W_DIP, DRAG_DIP, NAV_W_DIP, RADIUS_DIP,
};
use vitro_render::surfaces::Painter;
use windows::core::Result;
use windows::Win32::Foundation::HWND;

const VK_TAB: u32 = 0x09;
const VK_RETURN: u32 = 0x0D;
const VK_ESCAPE: u32 = 0x1B;
const VK_SPACE: u32 = 0x20;
const VK_PRIOR: u32 = 0x21;
const VK_NEXT: u32 = 0x22;
const VK_END: u32 = 0x23;
const VK_HOME: u32 = 0x24;
const VK_LEFT: u32 = 0x25;
const VK_UP: u32 = 0x26;
const VK_RIGHT: u32 = 0x27;
const VK_DOWN: u32 = 0x28;

/// The window's grow-in.
const OPEN_MS: u32 = 180;

/// One wheel notch (120) scrolls this far.
const WHEEL_STEP_DIP: f32 = 48.0;

/// Pointer, wheel and keyboard input (points in the window's client px).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsInput {
    Move((i32, i32)),
    Leave,
    Down((i32, i32)),
    Up((i32, i32)),
    /// `WM_MOUSEWHEEL` delta (120 per notch, positive away from the user).
    Wheel(i32),
    Key {
        vk: u32,
        shift: bool,
        /// Ctrl is held (Ctrl+Tab / Ctrl+PageDown switch category).
        ctrl: bool,
    },
}

/// What the app does after an input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsAction {
    /// Set control `id` to option / swatch `choice` (0/1 for a toggle): [`apply_choice`].
    Apply(ControlId, usize),
    OpenLink(&'static str),
    Close,
    Redraw,
    /// Another category was picked: rebuild the page from the settings
    /// ([`SettingsCtl::rebuild`]), then redraw.
    Rebuild,
    None,
}

/// What a changed setting needs beyond being saved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    /// The bars look different: new styles, then lay out and redraw (`App::relayout`).
    Relayout,
    /// The Windows key hook: `Keys::set_enabled(on)`.
    WinKey(bool),
    /// The `Run\Vitrobar` value: `apply_autostart(on)`.
    Autostart(bool),
    /// Read when Vitrobar next starts (the stock taskbar's mode is fixed while it is taken
    /// over).
    NextStart,
    /// The UI language: the window and the menus switch.
    Language,
    /// Read where it is used next (the Start style on the next open).
    Saved,
}

/// What changing `id` needs, with `s` already changed.
pub fn effect_of(id: ControlId, s: &Settings) -> Effect {
    match id {
        ControlId::Shape
        | ControlId::Material
        | ControlId::Hover
        | ControlId::IconSize
        | ControlId::Position
        | ControlId::Theme
        | ControlId::Accent
        | ControlId::ReserveFloating
        // The widget pills appear on or leave the bar.
        | ControlId::WidgetMusic
        | ControlId::WidgetClaude => Effect::Relayout,
        // Read when the quick panel next opens.
        ControlId::QuickTile(_) | ControlId::QuickPanelHint => Effect::Saved,
        ControlId::WinKey => Effect::WinKey(s.win_key_opens_start),
        ControlId::StartWithWindows => Effect::Autostart(s.start_with_windows),
        ControlId::StockTaskbar => Effect::NextStart,
        ControlId::Language => Effect::Language,
        ControlId::StartStyle
        | ControlId::ClaudeWidgetHint
        | ControlId::AboutVersion
        | ControlId::AboutLicense
        | ControlId::AboutSource => Effect::Saved,
    }
}

/// Applies `choice` to control `id` of `s` (`settings_ui::apply`). `None` when nothing
/// changed; else what the change needs besides saving.
pub fn apply_choice(s: &mut Settings, id: ControlId, choice: usize) -> Option<Effect> {
    apply(s, id, choice).then(|| effect_of(id, s))
}

/// The settings window's state: the controls, the scroll offset and the pointer.
#[derive(Debug, Clone)]
pub struct SettingsCtl {
    pub model: SettingsModel,
    /// The category shown.
    pub page: Section,
    /// Page scroll offset, DIP.
    pub scroll: f32,
    pub hover: Option<Spot>,
    pub pressed: Option<Spot>,
    /// The keyboard was used since the last click: the focus ring shows.
    pub keyboard: bool,
    /// px per DIP.
    pub scale: f32,
    /// Window width, DIP: [`vitro_render::settings_view::WINDOW_W_DIP`], or less on a narrow work area. The controls and
    /// the close button are laid out for it.
    pub width_dip: f32,
    /// Window height, DIP.
    pub height_dip: f32,
    /// The last pointer position (client px), to find the hover again after a scroll.
    last_pt: Option<(i32, i32)>,
}

impl SettingsCtl {
    pub fn new(s: &Settings, scale: f32, width_dip: f32, height_dip: f32) -> Self {
        let page = SECTIONS[0];
        Self {
            model: build_page(s, page_w(width_dip), Some(page)),
            page,
            scroll: 0.0,
            hover: None,
            pressed: None,
            keyboard: false,
            scale,
            width_dip,
            height_dip,
            last_pt: None,
        }
    }

    /// The settings changed, or the category did: show them. On the same page focus and
    /// scroll stay; a new page starts at its top.
    pub fn rebuild(&mut self, s: &Settings) {
        let same = self.model.page == Some(self.page);
        let focus = self.model.focus;
        self.model = build_page(s, page_w(self.width_dip), Some(self.page));
        if same {
            self.model.focus = focus.min(self.model.controls.len().saturating_sub(1));
            self.scroll = self.scroll.clamp(0.0, self.max_scroll());
        } else {
            self.model.focus = first_focusable(&self.model);
            self.scroll = 0.0;
        }
    }

    /// Shows category `i` of [`SECTIONS`] (wrapping); `Rebuild` if it changed.
    fn go_page(&mut self, i: usize) -> SettingsAction {
        let next = SECTIONS[i % SECTIONS.len()];
        if next == self.page {
            return SettingsAction::None;
        }
        self.page = next;
        SettingsAction::Rebuild
    }

    fn view_h(&self) -> f32 {
        viewport_h(self.height_dip)
    }

    fn max_scroll(&self) -> f32 {
        max_scroll(&self.model, self.view_h())
    }

    pub fn interaction(&self) -> Interaction {
        Interaction {
            hover: self.hover,
            pressed: self.pressed,
            ring: self.keyboard.then_some(self.model.focus),
        }
    }

    /// The part under client px `pt`.
    pub fn spot_at(&self, pt: (i32, i32)) -> Option<Spot> {
        let (x, y) = (pt.0 as f32 / self.scale, pt.1 as f32 / self.scale);
        let close = close_rect(self.width_dip);
        if x >= close.x && x < close.right() && y >= close.y && y < close.bottom() {
            return Some(Spot::Close);
        }
        if y < DRAG_DIP {
            return None;
        }
        if x < NAV_W_DIP {
            return nav_hit((x, y)).map(Spot::Nav);
        }
        hit(&self.model, (x - NAV_W_DIP, y - DRAG_DIP + self.scroll))
    }

    fn control_of(spot: Spot) -> Option<usize> {
        match spot {
            Spot::Close | Spot::Nav(_) => None,
            Spot::Option { control, .. } | Spot::Toggle(control) | Spot::Link(control) => {
                Some(control)
            }
        }
    }

    fn activate(&mut self, spot: Spot) -> SettingsAction {
        if let Some(i) = Self::control_of(spot) {
            self.model.focus = i;
        }
        match spot {
            Spot::Close => SettingsAction::Close,
            Spot::Nav(i) => self.go_page(i),
            Spot::Option { control, choice } => {
                SettingsAction::Apply(self.model.controls[control].id, choice)
            }
            Spot::Toggle(i) => self.toggle(i),
            Spot::Link(i) => match self.model.controls[i].kind {
                ControlKind::Link(url) => SettingsAction::OpenLink(url),
                _ => SettingsAction::None,
            },
        }
    }

    fn toggle(&self, i: usize) -> SettingsAction {
        match self.model.controls[i].kind {
            ControlKind::Toggle(on) => {
                SettingsAction::Apply(self.model.controls[i].id, usize::from(!on))
            }
            _ => SettingsAction::None,
        }
    }

    fn set_scroll(&mut self, s: f32) -> bool {
        let s = s.clamp(0.0, self.max_scroll());
        if s == self.scroll {
            return false;
        }
        self.scroll = s;
        if let Some(pt) = self.last_pt {
            self.hover = self.spot_at(pt);
        }
        true
    }

    fn redraw_if(changed: bool) -> SettingsAction {
        if changed {
            SettingsAction::Redraw
        } else {
            SettingsAction::None
        }
    }

    pub fn handle(&mut self, input: SettingsInput) -> SettingsAction {
        match input {
            SettingsInput::Move(pt) => {
                self.last_pt = Some(pt);
                let spot = self.spot_at(pt);
                let changed = spot != self.hover;
                self.hover = spot;
                Self::redraw_if(changed)
            }
            SettingsInput::Leave => {
                self.last_pt = None;
                let changed = self.hover.is_some() || self.pressed.is_some();
                self.hover = None;
                self.pressed = None;
                Self::redraw_if(changed)
            }
            SettingsInput::Down(pt) => {
                self.last_pt = Some(pt);
                self.hover = self.spot_at(pt);
                self.pressed = self.hover;
                // A click hides the focus ring until the keyboard is used again.
                self.keyboard = false;
                SettingsAction::Redraw
            }
            SettingsInput::Up(pt) => {
                self.last_pt = Some(pt);
                let spot = self.spot_at(pt);
                let pressed = self.pressed.take();
                self.hover = spot;
                match spot {
                    Some(s) if pressed == Some(s) => self.activate(s),
                    _ => SettingsAction::Redraw,
                }
            }
            SettingsInput::Wheel(delta) => {
                let step = delta as f32 / 120.0 * WHEEL_STEP_DIP;
                let changed = self.set_scroll(self.scroll - step);
                Self::redraw_if(changed)
            }
            SettingsInput::Key { vk, shift, ctrl } => {
                let n = SECTIONS.len();
                let at = self.page.index();
                match vk {
                    VK_TAB | VK_NEXT if ctrl && !(vk == VK_TAB && shift) => self.go_page(at + 1),
                    VK_TAB | VK_PRIOR if ctrl => self.go_page(at + n - 1),
                    _ => self.key(vk, shift),
                }
            }
        }
    }

    fn reveal_focus(&mut self) {
        let s = reveal(&self.model, self.model.focus, self.scroll, self.view_h());
        self.set_scroll(s);
    }

    fn key(&mut self, vk: u32, shift: bool) -> SettingsAction {
        let i = self.model.focus;
        match vk {
            VK_ESCAPE => SettingsAction::Close,
            VK_TAB => {
                // The first Tab only shows where the focus is.
                if self.keyboard {
                    if shift {
                        focus_prev(&mut self.model);
                    } else {
                        focus_next(&mut self.model);
                    }
                }
                self.keyboard = true;
                self.reveal_focus();
                SettingsAction::Redraw
            }
            VK_LEFT | VK_RIGHT => {
                self.keyboard = true;
                self.reveal_focus();
                let forward = vk == VK_RIGHT;
                let c = &self.model.controls[i];
                let (selected, n) = match &c.kind {
                    ControlKind::Choice { options, selected } => (*selected, options.len()),
                    // A hand-set colour (an extra swatch past the list) steps into the list.
                    ControlKind::Swatches { selected, .. } => (*selected, ACCENT_SWATCHES.len()),
                    _ => return SettingsAction::Redraw,
                };
                if n == 0 {
                    return SettingsAction::Redraw;
                }
                let next = match (selected >= n, forward) {
                    (true, true) => 0,
                    (true, false) => n - 1,
                    (false, true) => (selected + 1) % n,
                    (false, false) => (selected + n - 1) % n,
                };
                SettingsAction::Apply(c.id, next)
            }
            VK_SPACE | VK_RETURN => {
                self.keyboard = true;
                match self.model.controls[i].kind {
                    ControlKind::Toggle(_) => self.toggle(i),
                    ControlKind::Link(url) => SettingsAction::OpenLink(url),
                    _ => SettingsAction::Redraw,
                }
            }
            VK_UP | VK_DOWN | VK_PRIOR | VK_NEXT | VK_HOME | VK_END => {
                let page = self.view_h() - WHEEL_STEP_DIP;
                let target = match vk {
                    VK_UP => self.scroll - WHEEL_STEP_DIP,
                    VK_DOWN => self.scroll + WHEEL_STEP_DIP,
                    VK_PRIOR => self.scroll - page,
                    VK_NEXT => self.scroll + page,
                    VK_HOME => 0.0,
                    _ => self.max_scroll(),
                };
                let changed = self.set_scroll(target);
                Self::redraw_if(changed)
            }
            _ => SettingsAction::None,
        }
    }
}

/// The open settings window. Dropping it destroys its visuals, then its window.
pub struct SettingsWindow {
    // Field order = drop order: the visuals before the window they are in.
    view: SettingsView,
    panel: Panel,
    pub ctl: SettingsCtl,
    pub dpi: u32,
    /// The cursor is over the window (tracked for WM_MOUSELEAVE).
    pub tracking: bool,
}

impl SettingsWindow {
    /// Opens the window at `rect_px` on a monitor of `dpi`, titled `title`.
    pub fn open(
        comp: &Comp,
        rect_px: PxRect,
        dpi: u32,
        style: &MaterialStyle,
        title: &str,
        settings: &Settings,
    ) -> Result<Self> {
        let mut panel = Panel::show(comp, rect_px, dpi, style, RADIUS_DIP, title, true)?;
        if vitro_render::panel_view::animations() {
            if let Err(e) = panel.animate_open(OPEN_MS, vitro_render::panel::Motion::Grow) {
                eprintln!("vitrobar: the settings window could not animate in: {e}");
                panel.show_static();
            }
        }
        panel.set_drag_height(DRAG_DIP);
        panel.set_drag_exclude_right(CLOSE_W_DIP);
        let view = SettingsView::new(&panel)?;
        let scale = panel.scale();
        let width_dip = (rect_px.2 - rect_px.0) as f32 / scale;
        let height_dip = (rect_px.3 - rect_px.1) as f32 / scale;
        Ok(Self {
            view,
            panel,
            ctl: SettingsCtl::new(settings, scale, width_dip, height_dip),
            dpi,
            tracking: false,
        })
    }

    pub fn hwnd(&self) -> HWND {
        self.panel.hwnd()
    }

    pub fn rect_px(&self) -> PxRect {
        self.panel.rect_px()
    }

    /// Brings the window to the front (restoring it when minimised).
    pub fn focus(&self) -> bool {
        self.panel.restore_and_activate()
    }

    pub fn set_title(&self, title: &str) {
        self.panel.set_title(title);
    }

    /// Moves / resizes the window (a DPI change) or only redraws its glass (`style`); the
    /// controls of `settings` are laid out again for the new width.
    pub fn reshape(
        &mut self,
        comp: &Comp,
        rect_px: PxRect,
        dpi: u32,
        style: &MaterialStyle,
        settings: &Settings,
    ) -> Result<()> {
        self.panel.move_to(comp, rect_px, dpi, style)?;
        self.dpi = dpi;
        self.ctl.scale = self.panel.scale();
        self.ctl.width_dip = (rect_px.2 - rect_px.0) as f32 / self.ctl.scale;
        self.ctl.height_dip = (rect_px.3 - rect_px.1) as f32 / self.ctl.scale;
        self.ctl.rebuild(settings);
        Ok(())
    }

    /// Draws the current state (only what changed is drawn again).
    pub fn render(&mut self, painter: &Painter, look: &SettingsLook) -> Result<()> {
        let (l, t, r, b) = self.panel.rect_px();
        self.view.render(
            painter,
            &self.ctl.model,
            look,
            &self.ctl.interaction(),
            self.ctl.scroll,
            (r - l, b - t),
            self.ctl.scale,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vitro_core::settings::{Material, Shape};
    use vitro_render::settings_view::{parts, WINDOW_H_DIP, WINDOW_W_DIP};

    fn ctl() -> SettingsCtl {
        SettingsCtl::new(&Settings::default(), 1.5, WINDOW_W_DIP, WINDOW_H_DIP)
    }

    fn index(c: &SettingsCtl, id: ControlId) -> usize {
        c.model.controls.iter().position(|x| x.id == id).unwrap()
    }

    /// Client px of the centre of `spot` (content DIP rect, scrolled, below the caption).
    fn point_of(c: &SettingsCtl, spot: Spot) -> (i32, i32) {
        let i = SettingsCtl::control_of(spot).unwrap();
        let (_, r) = parts(&c.model.controls[i], i)
            .into_iter()
            .find(|(s, _)| *s == spot)
            .unwrap();
        let x = (r.x + r.w / 2.0 + NAV_W_DIP) * c.scale;
        let y = (r.y + r.h / 2.0 + DRAG_DIP - c.scroll) * c.scale;
        (x.round() as i32, y.round() as i32)
    }

    fn click(c: &mut SettingsCtl, pt: (i32, i32)) -> SettingsAction {
        c.handle(SettingsInput::Move(pt));
        c.handle(SettingsInput::Down(pt));
        c.handle(SettingsInput::Up(pt))
    }

    fn key(c: &mut SettingsCtl, vk: u32) -> SettingsAction {
        c.handle(SettingsInput::Key {
            vk,
            shift: false,
            ctrl: false,
        })
    }

    /// Shows category `sec`, as a click on its sidebar row would.
    fn show(c: &mut SettingsCtl, sec: Section) {
        let r = vitro_render::settings_view::nav_rect(sec.index());
        let pt = (
            ((r.x + r.w / 2.0) * c.scale).round() as i32,
            ((r.y + r.h / 2.0) * c.scale).round() as i32,
        );
        let a = click(c, pt);
        if sec != c.model.page.unwrap_or(sec) {
            assert_eq!(a, SettingsAction::Rebuild);
        }
        c.rebuild(&Settings::default());
        assert_eq!(c.model.page, Some(sec));
    }

    #[test]
    fn the_sidebar_and_ctrl_tab_switch_category() {
        let mut c = ctl();
        assert_eq!(c.model.page, Some(Section::Appearance));
        show(&mut c, Section::About);
        assert!(c.model.controls.iter().all(|x| x.section == Section::About));
        assert_eq!(c.scroll, 0.0);
        // Ctrl+Tab wraps to the first category, Ctrl+Shift+Tab goes back.
        let ctrl = |vk, shift| SettingsInput::Key {
            vk,
            shift,
            ctrl: true,
        };
        assert_eq!(c.handle(ctrl(VK_TAB, false)), SettingsAction::Rebuild);
        assert_eq!(c.page, Section::Appearance);
        assert_eq!(c.handle(ctrl(VK_TAB, true)), SettingsAction::Rebuild);
        assert_eq!(c.page, Section::About);
        assert_eq!(c.handle(ctrl(VK_PRIOR, false)), SettingsAction::Rebuild);
        assert_eq!(c.page, Section::Language);
        // Clicking the current category does nothing.
        c.rebuild(&Settings::default());
        let r = vitro_render::settings_view::nav_rect(Section::Language.index());
        let pt = ((r.x + 20.0) as i32 * 3 / 2, ((r.y + 10.0) * 1.5) as i32);
        assert_eq!(click(&mut c, pt), SettingsAction::None);
    }

    #[test]
    fn the_quick_panel_page_switches_tiles() {
        let mut c = ctl();
        show(&mut c, Section::QuickPanel);
        let i = index(
            &c,
            ControlId::QuickTile(vitro_core::settings::QuickTile::Lock),
        );
        let pt = point_of(&c, Spot::Toggle(i));
        assert_eq!(
            click(&mut c, pt),
            SettingsAction::Apply(
                ControlId::QuickTile(vitro_core::settings::QuickTile::Lock),
                1
            )
        );
        assert_eq!(c.model.focus, i);
    }

    #[test]
    fn clicking_full_applies_the_shape_and_relayouts() {
        let mut c = ctl();
        let shape = index(&c, ControlId::Shape);
        let pt = point_of(
            &c,
            Spot::Option {
                control: shape,
                choice: 2,
            },
        );
        let action = click(&mut c, pt);
        assert_eq!(action, SettingsAction::Apply(ControlId::Shape, 2));
        let mut s = Settings::default();
        assert_eq!(
            apply_choice(&mut s, ControlId::Shape, 2),
            Some(Effect::Relayout)
        );
        assert_eq!(s.shape, Shape::Full);
        // The same choice again changes nothing.
        assert_eq!(apply_choice(&mut s, ControlId::Shape, 2), None);
        c.rebuild(&s);
        assert!(matches!(
            c.model.controls[shape].kind,
            ControlKind::Choice { selected: 2, .. }
        ));
    }

    #[test]
    fn a_press_released_elsewhere_does_nothing() {
        let mut c = ctl();
        let shape = index(&c, ControlId::Shape);
        let a = point_of(
            &c,
            Spot::Option {
                control: shape,
                choice: 0,
            },
        );
        let b = point_of(
            &c,
            Spot::Option {
                control: shape,
                choice: 1,
            },
        );
        c.handle(SettingsInput::Down(a));
        assert_eq!(c.handle(SettingsInput::Up(b)), SettingsAction::Redraw);
    }

    #[test]
    fn left_right_cycle_the_material_within_its_four_options() {
        let mut c = ctl();
        let mut s = Settings::default();
        c.model.focus = index(&c, ControlId::Material);
        let mut seen = Vec::new();
        for _ in 0..4 {
            let SettingsAction::Apply(ControlId::Material, k) = key(&mut c, VK_RIGHT) else {
                panic!("not applied");
            };
            assert!(k < 4);
            apply_choice(&mut s, ControlId::Material, k);
            c.rebuild(&s);
            seen.push(s.material);
        }
        assert_eq!(
            seen,
            [
                Material::Smoked,
                Material::SolidBlack,
                Material::SolidWhite,
                Material::Frosted
            ]
        );
        // Left from the first wraps to the last.
        assert_eq!(
            key(&mut c, VK_LEFT),
            SettingsAction::Apply(ControlId::Material, 3)
        );
        assert!(c.keyboard, "arrows show the focus ring");
    }

    #[test]
    fn space_on_the_windows_key_toggle_disables_the_hook() {
        let mut c = ctl();
        show(&mut c, Section::Behaviour);
        let mut s = Settings::default();
        assert!(s.win_key_opens_start);
        c.model.focus = index(&c, ControlId::WinKey);
        assert_eq!(
            key(&mut c, VK_SPACE),
            SettingsAction::Apply(ControlId::WinKey, 0)
        );
        assert_eq!(
            apply_choice(&mut s, ControlId::WinKey, 0),
            Some(Effect::WinKey(false))
        );
        c.rebuild(&s);
        assert_eq!(
            key(&mut c, VK_SPACE),
            SettingsAction::Apply(ControlId::WinKey, 1)
        );
    }

    #[test]
    fn escape_and_the_close_button_close() {
        let mut c = ctl();
        assert_eq!(key(&mut c, VK_ESCAPE), SettingsAction::Close);
        let close = close_rect(WINDOW_W_DIP);
        let pt = (
            ((close.x + close.w / 2.0) * c.scale) as i32,
            ((close.h / 2.0) * c.scale) as i32,
        );
        assert_eq!(c.spot_at(pt), Some(Spot::Close));
        assert_eq!(click(&mut c, pt), SettingsAction::Close);
        // The rest of the caption is the drag band: nothing to click.
        assert_eq!(c.spot_at((30, 30)), None);
    }

    #[test]
    fn a_narrow_window_lays_out_and_hit_tests_at_its_own_width() {
        // A 600 DIP work area (less than the 720 DIP the window wants), at 150 %.
        let mut c = SettingsCtl::new(&Settings::default(), 1.5, 600.0, WINDOW_H_DIP);
        // Every control fits the window, none is clipped at 720.
        for ctrl in &c.model.controls {
            assert!(ctrl.rect.right() <= 600.0, "{:?} is clipped", ctrl.id);
            for (_, r) in parts(ctrl, 0) {
                assert!(r.right() <= 600.0, "a part of {:?} is clipped", ctrl.id);
            }
        }
        // The close button is where the view draws it: at the right edge of 600 DIP ...
        let close = close_rect(600.0);
        let pt = (
            ((close.x + close.w / 2.0) * c.scale) as i32,
            ((close.h / 2.0) * c.scale) as i32,
        );
        assert_eq!(c.spot_at(pt), Some(Spot::Close));
        assert_eq!(click(&mut c, pt), SettingsAction::Close);
        // ... and just left of it is the drag band (the window's own width, not 720 DIP,
        // decides where the button starts).
        let x = ((close.x - 4.0) * c.scale) as i32;
        assert_eq!(c.spot_at((x, pt.1)), None);
        // A setting change keeps the narrow layout.
        c.rebuild(&Settings::default());
        assert!(c.model.controls.iter().all(|k| k.rect.right() <= 600.0));
    }

    #[test]
    fn effects_follow_the_brief() {
        let s = Settings {
            start_with_windows: false,
            ..Settings::default()
        };
        assert_eq!(
            effect_of(ControlId::StartWithWindows, &s),
            Effect::Autostart(false)
        );
        assert_eq!(effect_of(ControlId::Theme, &s), Effect::Relayout);
        assert_eq!(effect_of(ControlId::Accent, &s), Effect::Relayout);
        assert_eq!(effect_of(ControlId::StockTaskbar, &s), Effect::NextStart);
        assert_eq!(effect_of(ControlId::Language, &s), Effect::Language);
        assert_eq!(effect_of(ControlId::StartStyle, &s), Effect::Saved);
        let mut s = Settings::default();
        assert_eq!(apply_choice(&mut s, ControlId::AboutLicense, 0), None);
    }

    #[test]
    fn tab_moves_focus_and_scrolls_it_into_view() {
        let mut c = ctl();
        assert_eq!(key(&mut c, VK_TAB), SettingsAction::Redraw);
        assert_eq!(c.model.focus, 0, "the first Tab shows the ring");
        assert_eq!(c.interaction().ring, Some(0));
        key(&mut c, VK_TAB);
        assert_eq!(c.model.focus, 1);
        let shift_tab = SettingsInput::Key {
            vk: VK_TAB,
            shift: true,
            ctrl: false,
        };
        c.handle(shift_tab);
        c.handle(shift_tab);
        // Wrapped to the last control of the page, shown whole.
        assert_eq!(c.model.controls[c.model.focus].id, ControlId::Accent);
        let r = c.model.controls[c.model.focus].rect;
        assert!(r.bottom() <= c.scroll + viewport_h(c.height_dip));
        // A click hides the ring.
        c.handle(SettingsInput::Down((5, 5)));
        assert_eq!(c.interaction().ring, None);
    }

    #[test]
    fn enter_opens_links_and_the_wheel_scrolls_within_bounds() {
        // Short enough for the About page to scroll.
        let mut c = SettingsCtl::new(&Settings::default(), 1.5, WINDOW_W_DIP, 200.0);
        show(&mut c, Section::About);
        c.model.focus = index(&c, ControlId::AboutLicense);
        assert!(matches!(
            key(&mut c, VK_RETURN),
            SettingsAction::OpenLink(u) if u.contains("gpl-3.0")
        ));
        assert_eq!(c.handle(SettingsInput::Wheel(120)), SettingsAction::None);
        assert_eq!(c.handle(SettingsInput::Wheel(-120)), SettingsAction::Redraw);
        assert_eq!(c.scroll, WHEEL_STEP_DIP);
        c.handle(SettingsInput::Wheel(-120 * 100));
        assert_eq!(c.scroll, c.max_scroll());
        assert_eq!(key(&mut c, VK_HOME), SettingsAction::Redraw);
        assert_eq!(c.scroll, 0.0);
    }

    #[test]
    fn swatch_keys_step_into_the_list_from_a_custom_colour() {
        let s = Settings {
            accent: Some(0x123456),
            ..Settings::default()
        };
        let mut c = SettingsCtl::new(&s, 1.0, WINDOW_W_DIP, WINDOW_H_DIP);
        c.model.focus = index(&c, ControlId::Accent);
        assert_eq!(
            key(&mut c, VK_RIGHT),
            SettingsAction::Apply(ControlId::Accent, 0)
        );
        assert_eq!(
            key(&mut c, VK_LEFT),
            SettingsAction::Apply(ControlId::Accent, ACCENT_SWATCHES.len() - 1)
        );
    }
}
