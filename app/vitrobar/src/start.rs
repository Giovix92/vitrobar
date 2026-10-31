//! The Vitrobar Start panel: the pure state machine ([`StartState::handle`]), its geometry in
//! panel px ([`Geom`]), the data it shows ([`StartData`]: the app catalog, recent files and
//! account name, read on a worker thread and cached across opens) and the open panel
//! ([`StartPanel`]: window, view and the items it draws).
//!
//! The app (`app.rs`) owns one [`StartData`] and, while the panel is open, one
//! [`StartPanel`]; it routes the panel's input here and carries out the [`StartAction`]s.

use std::collections::HashSet;
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use vitro_core::calc::format_result;
use vitro_core::i18n::{tr, Key, Lang};
use vitro_core::layout::Rect;
use vitro_core::search::{fold, search, search_grid, FileHit, ResultKind, SETTINGS_PAGES};
use vitro_core::settings::Pin;
use vitro_core::start_layout::{
    grid_layout, StartLayout, GRID_CELL_DIP, GRID_GAP_DIP, RESULT_ROW_H_DIP,
};
use vitro_core::style::{MaterialStyle, Rgba};
use vitro_core::text_input::TextInput;
use vitro_render::compositor::Comp;
use vitro_render::flyout::PxRect;
use vitro_render::panel::Panel;
use vitro_render::start_view::{Item, Layer, Part, StartView};
use vitro_render::surfaces::{FontKind, Painter};
use vitro_win::catalog::{list_all_apps, recent_docs_enabled, recent_files, RecentFile};
use vitro_win::event_queue::WakeQueue;
use vitro_win::file_search::FileSearch;
use vitro_win::power::PowerAction;
use windows::core::Result;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::icon_loader::{IconLoader, IconSrc, Lookup, MAX_ICON_PX};
use crate::launcher::Job;
use crate::notify::{WM_APP_FILE_SEARCH, WM_APP_START_DATA};

/// The panel's corner radius.
pub const PANEL_RADIUS_DIP: f32 = 12.0;
/// Icons in the panel are drawn this big.
pub const ICON_DIP: f32 = 32.0;
/// The catalog is read again on open when older than this.
pub const CATALOG_MAX_AGE: Duration = Duration::from_secs(60);
/// The account page the account button opens.
pub const ACCOUNT_URI: &str = "ms-settings:yourinfo";
/// Icons in the full-screen grid are drawn this big.
pub const GRID_ICON_DIP: f32 = 48.0;
/// Alpha (of 255) the full-screen grid's frosted tint gets: 0.12.
const GRID_TINT_ALPHA: u8 = 31;
/// Grid rows drawn beyond the visible ones (above and below).
const GRID_SLACK_ROWS: usize = 2;
/// Rows the wheel moves per notch.
const WHEEL_ROWS: f32 = 3.0;
/// All-apps rows drawn beyond the visible ones (above and below).
const LIST_SLACK_ROWS: usize = 8;

// Virtual-key codes the panel handles.
const VK_BACK: u32 = 0x08;
const VK_RETURN: u32 = 0x0D;
const VK_ESCAPE: u32 = 0x1B;
const VK_PRIOR: u32 = 0x21;
const VK_NEXT: u32 = 0x22;
const VK_END: u32 = 0x23;
const VK_HOME: u32 = 0x24;
const VK_LEFT: u32 = 0x25;
const VK_UP: u32 = 0x26;
const VK_RIGHT: u32 = 0x27;
const VK_DOWN: u32 = 0x28;
const VK_DELETE: u32 = 0x2E;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartMode {
    Home,
    AllApps,
    Search,
}

impl StartMode {
    pub fn name(self) -> &'static str {
        match self {
            StartMode::Home => "home",
            StartMode::AllApps => "all-apps",
            StartMode::Search => "search",
        }
    }
}

/// Input for the panel, in panel client px.
#[derive(Debug, Clone, PartialEq)]
pub enum StartInput {
    Char(char),
    /// A virtual key and whether Ctrl (without Alt) is down.
    Key(u32, bool),
    /// Clipboard text for Ctrl+V.
    Paste(String),
    /// Wheel delta (120 per notch, positive = away from the user).
    Wheel(i32),
    Move((i32, i32)),
    /// The cursor left the panel.
    Leave,
    Click((i32, i32)),
    RightClick((i32, i32)),
}

/// What the panel asks the app to do.
#[derive(Debug, Clone, PartialEq)]
pub enum StartAction {
    None,
    Close,
    /// Start an app (a Start pin or a catalog entry; see [`launch_job`]).
    Launch(Pin),
    /// Open a file or URI with the shell.
    Open(String),
    Power(PowerAction),
    /// Show the power menu at this panel point.
    PowerMenu((i32, i32)),
    /// Copy this quick-math result.
    CopyMath(String),
    /// Show the app menu of `item` at this panel point.
    Menu((i32, i32), MenuItem),
    Redraw,
}

/// The app a right-click menu is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuItem {
    /// Index into the Start pins.
    Pin(usize),
    /// Index into the app catalog.
    App(usize),
}

/// Something in the panel the cursor can be on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Search,
    /// "All apps ›" on Home, "‹ Back" in All apps.
    AllApps,
    Pin(usize),
    Recent(usize),
    Account,
    Power,
    Result(usize),
    /// A row of the All-apps list.
    Row(usize),
}

/// One row of the All-apps list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListRow {
    Header(char),
    /// Index into the app catalog.
    App(usize),
}

/// The letter an app is listed under: its folded first character, `#` for anything that is
/// not a letter.
fn letter(name: &str) -> char {
    fold(name)
        .chars()
        .next()
        .filter(|c| c.is_alphabetic())
        .map_or('#', |c| c.to_uppercase().next().unwrap_or(c))
}

/// The All-apps rows of `apps` (sorted by name): a header before each new letter.
pub fn list_rows(apps: &[Pin]) -> Vec<ListRow> {
    let mut rows = Vec::with_capacity(apps.len() + 27);
    let mut last = None;
    for (i, p) in apps.iter().enumerate() {
        let l = letter(&p.name);
        if last != Some(l) {
            rows.push(ListRow::Header(l));
            last = Some(l);
        }
        rows.push(ListRow::App(i));
    }
    rows
}

fn px_rect(r: Rect, origin: Rect, s: f32) -> PxRect {
    let p = |v: f32| (v * s).round() as i32;
    (
        p(r.x - origin.x),
        p(r.y - origin.y),
        p(r.right() - origin.x),
        p(r.bottom() - origin.y),
    )
}

fn contains(r: PxRect, (x, y): (i32, i32)) -> bool {
    x >= r.0 && x < r.2 && y >= r.1 && y < r.3
}

/// The panel's layout in panel client px.
#[derive(Debug, Clone, PartialEq)]
pub struct Geom {
    pub scale: f32,
    pub size: (i32, i32),
    pub search: PxRect,
    pub pins_title: PxRect,
    pub all_apps: PxRect,
    pub pins: Vec<PxRect>,
    pub recent_title: PxRect,
    pub recent: Vec<PxRect>,
    pub footer: PxRect,
    pub account: PxRect,
    pub power: PxRect,
    pub results: Vec<PxRect>,
    /// The search results' scrolling viewport (from the first result row to the footer).
    pub results_view: PxRect,
    /// The All-apps list viewport.
    pub list: PxRect,
    pub row_h: i32,
    /// Set in the full-screen grid layout (the panel fields above are then empty).
    pub grid: Option<GridGeom>,
}

/// The full-screen grid's geometry in panel client px.
#[derive(Debug, Clone, PartialEq)]
pub struct GridGeom {
    pub cols: usize,
    /// Cell side and the distance between cell origins.
    pub cell: i32,
    pub step: i32,
    /// Left edge of column 0 and top edge of row 0 (when no result rows are above the grid).
    pub x0: i32,
    pub top: i32,
}

/// The tint of the full-screen grid: the material's own with alpha 0.12 when it is frosted
/// (a solid material has nothing behind it to show through, so it stays as it is).
pub fn grid_style(style: &MaterialStyle) -> MaterialStyle {
    let mut s = *style;
    if s.backdrop_blur {
        s.tint.3 = GRID_TINT_ALPHA;
    }
    s
}

impl Geom {
    /// The full-screen grid's layout for a panel of `work` DIP (origin 0,0) at `scale`.
    pub fn new_grid(work: Rect, scale: f32) -> Self {
        let gl = grid_layout(work, 1);
        let p = |v: f32| (v * scale).round() as i32;
        let empty = (0, 0, 0, 0);
        let cell = gl.cells.first().copied().unwrap_or(Rect {
            x: 0.0,
            y: 0.0,
            w: GRID_CELL_DIP,
            h: GRID_CELL_DIP,
        });
        Self {
            scale,
            size: (p(work.w), p(work.h)),
            search: px_rect(gl.search, work, scale),
            pins_title: empty,
            all_apps: empty,
            pins: Vec::new(),
            recent_title: empty,
            recent: Vec::new(),
            footer: empty,
            account: empty,
            power: empty,
            results: Vec::new(),
            results_view: empty,
            list: empty,
            row_h: (RESULT_ROW_H_DIP * scale).round().max(1.0) as i32,
            grid: Some(GridGeom {
                cols: gl.cols,
                cell: p(GRID_CELL_DIP),
                step: p(GRID_CELL_DIP + GRID_GAP_DIP),
                x0: p(cell.x),
                top: p(cell.y),
            }),
        }
    }

    /// Top of the scrolling grid when `n_top` result rows sit above it.
    pub fn grid_view_top(&self, n_top: usize) -> i32 {
        self.grid.as_ref().map_or(0, |g| g.top) + n_top as i32 * self.row_h
    }

    /// Height of the scrolling grid viewport.
    pub fn grid_view_h(&self, n_top: usize) -> i32 {
        (self.size.1 - self.grid_view_top(n_top)).max(0)
    }

    /// Whole grid rows that fit the viewport (at least 1).
    pub fn grid_rows_visible(&self, n_top: usize) -> usize {
        let Some(g) = &self.grid else { return 1 };
        let gap = g.step - g.cell;
        (((self.grid_view_h(n_top) + gap) / g.step.max(1)) as usize).max(1)
    }

    pub fn new(l: &StartLayout, scale: f32) -> Self {
        let o = l.panel;
        let r = |x: Rect| px_rect(x, o, scale);
        let pins_title = r(l.pins_title);
        let footer = r(l.footer);
        let search = r(l.search);
        Self {
            scale,
            size: ((o.w * scale).round() as i32, (o.h * scale).round() as i32),
            search,
            pins_title,
            all_apps: r(l.all_apps_button),
            pins: l.pins.iter().map(|&p| r(p)).collect(),
            recent_title: r(l.recent_title),
            recent: l.recent.iter().map(|&p| r(p)).collect(),
            footer,
            account: r(l.account),
            power: r(l.power),
            results: l.results.iter().map(|&p| r(p)).collect(),
            results_view: {
                let v = r(l.list_view);
                (search.0, v.1, search.2, footer.1)
            },
            list: (
                search.0,
                pins_title.3 + (8.0 * scale).round() as i32,
                search.2,
                footer.1,
            ),
            row_h: (RESULT_ROW_H_DIP * scale).round().max(1.0) as i32,
            grid: None,
        }
    }

    fn list_h(&self) -> i32 {
        (self.list.3 - self.list.1).max(0)
    }

    fn results_h(&self) -> i32 {
        (self.results_view.3 - self.results_view.1).max(0)
    }

    /// Whole result rows that fit the results viewport (at least 1).
    pub fn result_rows(&self) -> usize {
        ((self.results_h() / self.row_h.max(1)) as usize).max(1)
    }
}

/// Makes sure the first screenful of results shows some files when there are any: if fewer
/// than `min(2, files)` file results fall within the first `rows`, app results are dropped
/// from the end of the apps until they do (the rest stays reachable by scrolling).
pub fn fit_results(mut items: Vec<ResultKind>, rows: usize) -> Vec<ResultKind> {
    let files = items
        .iter()
        .filter(|k| matches!(k, ResultKind::File(_)))
        .count();
    let want = files.min(2);
    loop {
        let visible = items
            .iter()
            .take(rows)
            .filter(|k| matches!(k, ResultKind::File(_)))
            .count();
        if visible >= want {
            return items;
        }
        let Some(last_app) = items.iter().rposition(|k| matches!(k, ResultKind::App(_))) else {
            return items;
        };
        items.remove(last_app);
    }
}

/// The full-screen grid's results: with no query every app in catalog order; otherwise the
/// Math/Settings/Files results first (they sit in rows above the grid), then every matching
/// app, best first.
pub fn grid_results(text: &str, apps: &[Pin], files: &[FileHit], lang: Lang) -> Vec<ResultKind> {
    if text.trim().is_empty() {
        return (0..apps.len()).map(ResultKind::App).collect();
    }
    let (matched, mut rows): (Vec<_>, Vec<_>) = search_grid(text, apps, files, lang)
        .items
        .into_iter()
        .partition(|k| matches!(k, ResultKind::App(_)));
    rows.extend(matched);
    rows
}

/// What [`StartState::handle`] needs to know besides the input: geometry and content.
pub struct StartCtx<'a> {
    pub geom: &'a Geom,
    pub start_pins: &'a [Pin],
    pub apps: &'a [Pin],
    pub rows: &'a [ListRow],
    pub recent: &'a [RecentFile],
    pub files: &'a [FileHit],
    /// The results, in display order (the list scrolls).
    pub results: &'a [ResultKind],
    pub lang: Lang,
}

impl StartCtx<'_> {
    fn pins_shown(&self) -> usize {
        self.start_pins.len().min(self.geom.pins.len())
    }

    /// Result rows (Math, Settings, Files) above the grid in the full-screen grid.
    fn top_rows(&self) -> usize {
        self.results
            .iter()
            .take_while(|k| !matches!(k, ResultKind::App(_)))
            .count()
    }

    /// How far the current mode's list can scroll.
    fn max_scroll(&self, mode: StartMode) -> f32 {
        let g = self.geom;
        if let Some(gg) = &g.grid {
            let n_top = self.top_rows();
            let cells = self.results.len() - n_top;
            let rows = cells.div_ceil(gg.cols.max(1)) as i32;
            let content = if rows == 0 {
                0
            } else {
                rows * gg.step - (gg.step - gg.cell)
            };
            return (content - g.grid_view_h(n_top)).max(0) as f32;
        }
        let (n, h) = match mode {
            StartMode::Search => (self.results.len(), g.results_h()),
            _ => (self.rows.len(), g.list_h()),
        };
        (n as i32 * g.row_h - h).max(0) as f32
    }
}

/// The panel's interaction state. Pure: no window, no system calls.
#[derive(Debug, Clone)]
pub struct StartState {
    pub mode: StartMode,
    pub input: TextInput,
    /// The highlighted search result (Enter runs it).
    pub selected: usize,
    /// Scroll offset of the All-apps list or of the search results, px.
    pub scroll: f32,
    pub hover: Option<Target>,
    /// Bumped on every change of the query text; file results carry the revision they
    /// were asked for.
    pub query_rev: u64,
    /// The revision of the file results accepted last, and how many there were.
    pub files_rev: u64,
    pub files: usize,
    /// The full-screen grid: `selected` indexes the results (rows first, then apps), the
    /// empty query lists every app (mode `AllApps`), arrows move the focus.
    pub grid: bool,
    /// A key moved the grid focus (Enter launches the focused app only then, while the query
    /// is empty).
    pub kbd: bool,
}

impl Default for StartState {
    fn default() -> Self {
        Self::new()
    }
}

impl StartState {
    pub fn new() -> Self {
        Self::with_rev(0)
    }

    /// A fresh state whose query revisions continue after `rev` (revisions keep growing
    /// across panels, so a late file result for an earlier panel never matches).
    pub fn with_rev(rev: u64) -> Self {
        Self {
            mode: StartMode::Home,
            input: TextInput::new(),
            selected: 0,
            scroll: 0.0,
            hover: None,
            query_rev: rev,
            files_rev: 0,
            files: 0,
            grid: false,
            kbd: false,
        }
    }

    /// The same state for the full-screen grid (all apps, no query).
    pub fn into_grid(mut self) -> Self {
        self.grid = true;
        self.mode = StartMode::AllApps;
        self
    }

    /// Scrolls the results (or the grid) so the selected item is fully visible.
    pub fn reveal_selected(&mut self, ctx: &StartCtx) {
        let g = ctx.geom;
        if let Some(gg) = &g.grid {
            let n_top = ctx.top_rows();
            if self.selected >= n_top {
                let row = ((self.selected - n_top) / gg.cols.max(1)) as i32;
                let top = (row * gg.step) as f32;
                let bottom = top + gg.cell as f32;
                let h = g.grid_view_h(n_top) as f32;
                if top < self.scroll {
                    self.scroll = top;
                } else if bottom > self.scroll + h {
                    self.scroll = bottom - h;
                }
            }
            self.scroll = self.scroll.clamp(0.0, ctx.max_scroll(self.mode));
            return;
        }
        let top = (self.selected as i32 * g.row_h) as f32;
        let bottom = top + g.row_h as f32;
        let h = g.results_h() as f32;
        if top < self.scroll {
            self.scroll = top;
        } else if bottom > self.scroll + h {
            self.scroll = bottom - h;
        }
        self.scroll = self.scroll.clamp(0.0, ctx.max_scroll(StartMode::Search));
    }

    /// File results for revision `rev` arrived: accepted (true) only if they answer the
    /// current query. The selection is left where it is (files come last in the results).
    pub fn files_arrived(&mut self, rev: u64, n: usize) -> bool {
        if rev != self.query_rev {
            return false;
        }
        self.files_rev = rev;
        self.files = n;
        true
    }

    /// The query text changed: Search while it has text, Home once it is empty.
    fn edited(&mut self) -> StartAction {
        self.query_rev += 1;
        self.selected = 0;
        self.files = 0;
        self.kbd = false;
        if self.input.is_empty() {
            self.mode = if self.grid {
                self.scroll = 0.0;
                StartMode::AllApps
            } else {
                StartMode::Home
            };
        } else {
            self.mode = StartMode::Search;
            self.scroll = 0.0;
        }
        StartAction::Redraw
    }

    pub fn handle(&mut self, ev: StartInput, ctx: &StartCtx) -> StartAction {
        match ev {
            StartInput::Char(c) => {
                if c.is_control() {
                    return StartAction::None;
                }
                self.input.insert(c.encode_utf8(&mut [0; 4]));
                self.edited()
            }
            StartInput::Paste(s) => {
                let before = self.input.text();
                self.input.insert(&s);
                if self.input.text() == before {
                    return StartAction::None;
                }
                self.edited()
            }
            StartInput::Key(vk, ctrl) => self.key(vk, ctrl, ctx),
            StartInput::Wheel(delta) => {
                if self.mode == StartMode::Home {
                    return StartAction::None;
                }
                let notch = match &ctx.geom.grid {
                    Some(gg) => gg.step as f32,
                    None => WHEEL_ROWS * ctx.geom.row_h as f32,
                };
                let step = notch * delta as f32 / 120.0;
                self.scroll_to(self.scroll - step, ctx)
            }
            StartInput::Move(pt) => {
                let hit = self.hit(pt, ctx);
                if hit == self.hover {
                    StartAction::None
                } else {
                    self.hover = hit;
                    StartAction::Redraw
                }
            }
            StartInput::Leave => {
                if self.hover.take().is_some() {
                    StartAction::Redraw
                } else {
                    StartAction::None
                }
            }
            StartInput::Click(pt) => match self.hit(pt, ctx) {
                Some(Target::AllApps) => {
                    self.mode = if self.mode == StartMode::AllApps {
                        StartMode::Home
                    } else {
                        StartMode::AllApps
                    };
                    self.scroll = 0.0;
                    self.hover = None;
                    StartAction::Redraw
                }
                Some(Target::Pin(i)) => ctx
                    .start_pins
                    .get(i)
                    .map_or(StartAction::None, |p| StartAction::Launch(p.clone())),
                Some(Target::Recent(i)) => ctx
                    .recent
                    .get(i)
                    .map_or(StartAction::None, |f| StartAction::Open(f.path.clone())),
                Some(Target::Account) => StartAction::Open(ACCOUNT_URI.to_string()),
                Some(Target::Power) => {
                    let r = ctx.geom.power;
                    StartAction::PowerMenu(((r.0 + r.2) / 2, r.1))
                }
                Some(Target::Result(i)) => self.run(i, ctx),
                Some(Target::Row(i)) => match ctx.rows.get(i) {
                    Some(ListRow::App(a)) => ctx
                        .apps
                        .get(*a)
                        .map_or(StartAction::None, |p| StartAction::Launch(p.clone())),
                    _ => StartAction::None,
                },
                Some(Target::Search) | None => StartAction::None,
            },
            StartInput::RightClick(pt) => match self.hit(pt, ctx) {
                Some(Target::Pin(i)) => StartAction::Menu(pt, MenuItem::Pin(i)),
                Some(Target::Row(i)) => match ctx.rows.get(i) {
                    Some(ListRow::App(a)) => StartAction::Menu(pt, MenuItem::App(*a)),
                    _ => StartAction::None,
                },
                Some(Target::Result(i)) => match ctx.results.get(i) {
                    Some(ResultKind::App(a)) => StartAction::Menu(pt, MenuItem::App(*a)),
                    _ => StartAction::None,
                },
                _ => StartAction::None,
            },
        }
    }

    /// Keys in the full-screen grid (Esc is handled by [`Self::key`]).
    fn grid_key(&mut self, vk: u32, ctrl: bool, ctx: &StartCtx) -> StartAction {
        let searching = self.mode == StartMode::Search;
        let n = ctx.results.len();
        match vk {
            VK_BACK | VK_DELETE if searching => {
                match (vk, ctrl) {
                    (VK_BACK, true) => self.input.backspace_word(),
                    (VK_BACK, false) => self.input.backspace(),
                    _ => self.input.delete(),
                }
                self.edited()
            }
            VK_RETURN if searching || self.kbd => self.run(self.selected, ctx),
            VK_LEFT | VK_RIGHT | VK_UP | VK_DOWN | VK_PRIOR | VK_NEXT | VK_HOME | VK_END
                if n > 0 =>
            {
                let cols = ctx.geom.grid.as_ref().map_or(1, |g| g.cols.max(1));
                let n_top = ctx.top_rows();
                let ngrid = n - n_top;
                let cur = self.selected.min(n - 1);
                let in_top = cur < n_top;
                let g = cur.saturating_sub(n_top);
                let page = ctx.geom.grid_rows_visible(n_top) * cols;
                let to = match vk {
                    VK_LEFT if !in_top && g > 0 => cur - 1,
                    VK_RIGHT if !in_top && g + 1 < ngrid => cur + 1,
                    VK_UP if in_top => cur.saturating_sub(1),
                    VK_UP if g >= cols => cur - cols,
                    VK_UP if n_top > 0 => n_top - 1,
                    VK_DOWN if in_top => (cur + 1).min(n - 1),
                    VK_DOWN if g + cols < ngrid => cur + cols,
                    // A short last row has nothing below this column: the last app.
                    VK_DOWN if g / cols < (ngrid - 1) / cols => n - 1,
                    VK_PRIOR if !in_top => n_top + g.saturating_sub(page),
                    VK_NEXT if in_top && ngrid > 0 => n_top,
                    VK_NEXT if !in_top => n_top + (g + page).min(ngrid - 1),
                    VK_HOME => 0,
                    VK_END => n - 1,
                    _ => cur,
                };
                if to == self.selected && self.kbd {
                    return StartAction::None;
                }
                self.selected = to;
                self.kbd = true;
                self.reveal_selected(ctx);
                StartAction::Redraw
            }
            _ => StartAction::None,
        }
    }

    fn key(&mut self, vk: u32, ctrl: bool, ctx: &StartCtx) -> StartAction {
        if self.grid && vk != VK_ESCAPE {
            return self.grid_key(vk, ctrl, ctx);
        }
        let searching = self.mode == StartMode::Search;
        let all_apps = self.mode == StartMode::AllApps;
        match vk {
            VK_ESCAPE => {
                if !self.input.is_empty() {
                    self.input.clear();
                    self.edited()
                } else {
                    StartAction::Close
                }
            }
            VK_BACK | VK_DELETE if searching => {
                match (vk, ctrl) {
                    (VK_BACK, true) => self.input.backspace_word(),
                    (VK_BACK, false) => self.input.backspace(),
                    _ => self.input.delete(),
                }
                self.edited()
            }
            VK_LEFT | VK_RIGHT | VK_HOME | VK_END if searching => {
                match (vk, ctrl) {
                    (VK_LEFT, true) => self.input.word_left(),
                    (VK_LEFT, false) => self.input.left(),
                    (VK_RIGHT, true) => self.input.word_right(),
                    (VK_RIGHT, false) => self.input.right(),
                    (VK_HOME, _) => self.input.home(),
                    _ => self.input.end(),
                }
                StartAction::Redraw
            }
            VK_UP | VK_DOWN if searching => {
                let n = ctx.results.len();
                let next = if vk == VK_UP {
                    self.selected.saturating_sub(1)
                } else {
                    (self.selected + 1).min(n.saturating_sub(1))
                };
                if next == self.selected {
                    StartAction::None
                } else {
                    self.selected = next;
                    self.reveal_selected(ctx);
                    StartAction::Redraw
                }
            }
            VK_RETURN if searching => self.run(self.selected, ctx),
            VK_UP | VK_DOWN | VK_PRIOR | VK_NEXT | VK_HOME | VK_END if all_apps => {
                let row = ctx.geom.row_h as f32;
                let page = (ctx.geom.list_h() as f32 - row).max(row);
                let to = match vk {
                    VK_UP => self.scroll - row,
                    VK_DOWN => self.scroll + row,
                    VK_PRIOR => self.scroll - page,
                    VK_NEXT => self.scroll + page,
                    VK_HOME => 0.0,
                    _ => ctx.max_scroll(self.mode),
                };
                self.scroll_to(to, ctx)
            }
            _ => StartAction::None,
        }
    }

    fn scroll_to(&mut self, y: f32, ctx: &StartCtx) -> StartAction {
        let y = y.clamp(0.0, ctx.max_scroll(self.mode));
        if y == self.scroll {
            StartAction::None
        } else {
            self.scroll = y;
            StartAction::Redraw
        }
    }

    /// Runs search result `i`.
    fn run(&self, i: usize, ctx: &StartCtx) -> StartAction {
        match ctx.results.get(i) {
            Some(ResultKind::Math(v)) => StartAction::CopyMath(format_result(*v, ctx.lang)),
            Some(ResultKind::App(a)) => ctx
                .apps
                .get(*a)
                .map_or(StartAction::None, |p| StartAction::Launch(p.clone())),
            Some(ResultKind::Setting(s)) => SETTINGS_PAGES
                .get(*s)
                .map_or(StartAction::None, |p| StartAction::Open(p.uri.to_string())),
            Some(ResultKind::File(f)) => ctx
                .files
                .get(*f)
                .map_or(StartAction::None, |h| StartAction::Open(h.path.clone())),
            None => StartAction::None,
        }
    }

    /// What is under panel point `pt` in the current mode.
    pub fn hit(&self, pt: (i32, i32), ctx: &StartCtx) -> Option<Target> {
        let g = ctx.geom;
        if contains(g.search, pt) {
            return Some(Target::Search);
        }
        if let Some(gg) = &g.grid {
            let n_top = ctx.top_rows();
            let (x, y) = pt;
            if x >= g.search.0
                && x < g.search.2
                && y >= gg.top
                && y < gg.top + n_top as i32 * g.row_h
            {
                return Some(Target::Result(((y - gg.top) / g.row_h) as usize));
            }
            let view_top = g.grid_view_top(n_top);
            if y < view_top || y >= g.size.1 || x < gg.x0 {
                return None;
            }
            let (cx, cy) = (x - gg.x0, y - view_top + self.scroll.round() as i32);
            let (col, row) = ((cx / gg.step) as usize, (cy / gg.step) as usize);
            if cx % gg.step >= gg.cell || cy % gg.step >= gg.cell || col >= gg.cols {
                return None;
            }
            let idx = row * gg.cols + col;
            return (n_top + idx < ctx.results.len()).then_some(Target::Result(n_top + idx));
        }
        if contains(g.account, pt) {
            return Some(Target::Account);
        }
        if contains(g.power, pt) {
            return Some(Target::Power);
        }
        match self.mode {
            StartMode::Home => {
                if contains(g.all_apps, pt) {
                    return Some(Target::AllApps);
                }
                if let Some(i) = g.pins.iter().position(|&r| contains(r, pt)) {
                    return (i < ctx.pins_shown()).then_some(Target::Pin(i));
                }
                let n = ctx.recent.len().min(g.recent.len());
                g.recent[..n]
                    .iter()
                    .position(|&r| contains(r, pt))
                    .map(Target::Recent)
            }
            StartMode::AllApps => {
                if contains(g.all_apps, pt) {
                    return Some(Target::AllApps);
                }
                if !contains(g.list, pt) {
                    return None;
                }
                let y = pt.1 - g.list.1 + self.scroll.round() as i32;
                let i = (y / g.row_h) as usize;
                (y >= 0 && i < ctx.rows.len()).then_some(Target::Row(i))
            }
            StartMode::Search => {
                let v = g.results_view;
                if !contains(v, pt) {
                    return None;
                }
                let y = pt.1 - v.1 + self.scroll.round() as i32;
                let i = (y / g.row_h.max(1)) as usize;
                (y >= 0 && i < ctx.results.len()).then_some(Target::Result(i))
            }
        }
    }
}

/// How a pin from the Start panel starts: a pin with a shortcut or an executable (one that
/// came from the bar) through [`vitro_win::launch::launch_pin`]; a catalog entry (only an
/// `AppsFolder` parsing name, which `launch_pin` cannot start for desktop apps) through
/// `shell:AppsFolder\<id>`.
pub fn launch_job(p: &Pin) -> Option<Job> {
    let set = |v: &Option<String>| v.as_deref().is_some_and(|s| !s.is_empty());
    if set(&p.link) || set(&p.path) {
        return Some(Job::Pin(p.clone()));
    }
    let id = p.aumid.as_deref().filter(|a| !a.is_empty())?;
    Some(Job::Open(format!("shell:AppsFolder\\{id}")))
}

/// Whether two pins are the same app: any identity field they both have matches (ignoring
/// case).
pub fn same_app(a: &Pin, b: &Pin) -> bool {
    let eq = |x: &Option<String>, y: &Option<String>| match (x.as_deref(), y.as_deref()) {
        (Some(x), Some(y)) => !x.is_empty() && x.eq_ignore_ascii_case(y),
        _ => false,
    };
    eq(&a.aumid, &b.aumid) || eq(&a.path, &b.path) || eq(&a.link, &b.link)
}

/// The icon the panel shows for a pin.
pub fn pin_icon(p: &Pin) -> Option<IconSrc> {
    let some = |v: &Option<String>| v.as_deref().filter(|s| !s.is_empty()).map(str::to_owned);
    some(&p.aumid)
        .map(IconSrc::Aumid)
        .or_else(|| some(&p.path).map(IconSrc::Path))
        .or_else(|| some(&p.link).map(IconSrc::Path))
}

// ---------------------------------------------------------------------------------------
// Data: catalog, recent files and account name, read on a worker
// ---------------------------------------------------------------------------------------

enum Request {
    Apps,
    Recent,
    Account,
}

enum Answer {
    Apps(Vec<Pin>),
    /// `None`: Windows keeps no recent files (`Start_TrackDocs` = 0).
    Recent(Option<Vec<RecentFile>>),
    Account(String),
}

/// The worker thread ("vitrobar-start", COM STA) that reads what the panel lists. Dropping
/// it closes the channel; the thread ends after its current read (not joined: the first
/// catalog read can take many seconds).
struct DataWorker {
    tx: Sender<Request>,
    answers: Arc<WakeQueue<Answer>>,
}

impl DataWorker {
    fn start(notify: HWND) -> std::io::Result<Self> {
        let (tx, rx) = channel::<Request>();
        let answers = Arc::new(WakeQueue::new());
        let queue = Arc::clone(&answers);
        let notify = notify.0 as isize;
        std::thread::Builder::new()
            .name("vitrobar-start".into())
            .spawn(move || {
                // SAFETY: initialises COM for this worker thread only; balanced below.
                let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
                while let Ok(req) = rx.recv() {
                    let answer = match req {
                        Request::Apps => Answer::Apps(list_all_apps()),
                        Request::Recent => {
                            Answer::Recent(recent_docs_enabled().then(|| recent_files(RECENT_MAX)))
                        }
                        Request::Account => Answer::Account(vitro_win::account::display_name()),
                    };
                    if queue.push(answer) && notify != 0 {
                        // SAFETY: a payload-free post; a destroyed window just fails.
                        let posted = unsafe {
                            PostMessageW(
                                Some(HWND(notify as *mut _)),
                                WM_APP_START_DATA,
                                WPARAM(0),
                                LPARAM(0),
                            )
                        };
                        if posted.is_err() {
                            queue.wake_failed();
                        }
                    }
                }
                if com {
                    // SAFETY: balances the successful CoInitializeEx above.
                    unsafe { CoUninitialize() };
                }
            })?;
        Ok(Self { tx, answers })
    }
}

/// Recent files read per open (the panel shows at most four).
const RECENT_MAX: usize = 4;

/// What the Start panel shows, cached across opens. The workers start on the first open.
pub struct StartData {
    pub apps: Vec<Pin>,
    pub rows: Vec<ListRow>,
    apps_at: Option<Instant>,
    apps_pending: bool,
    pub recent: Vec<RecentFile>,
    /// Windows keeps recent files; false shows "No recent files".
    pub recent_enabled: bool,
    pub account: String,
    worker: Option<DataWorker>,
    pub files: Option<FileSearch>,
    /// The last query revision a panel used: the next panel continues after it, so file
    /// results of a closed panel can never match a new one.
    pub last_rev: u64,
}

impl Default for StartData {
    fn default() -> Self {
        Self::new()
    }
}

impl StartData {
    pub fn new() -> Self {
        Self {
            apps: Vec::new(),
            rows: Vec::new(),
            apps_at: None,
            apps_pending: false,
            recent: Vec::new(),
            recent_enabled: true,
            account: String::new(),
            worker: None,
            files: None,
            last_rev: 0,
        }
    }

    /// On open: start the workers (first time), refresh the catalog when it is older than
    /// [`CATALOG_MAX_AGE`] (or was never read), and the recent files every time.
    pub fn refresh(&mut self, notify: HWND) {
        if self.worker.is_none() {
            match DataWorker::start(notify) {
                Ok(w) => {
                    let _ = w.tx.send(Request::Account);
                    self.worker = Some(w);
                }
                Err(e) => eprintln!("vitrobar: the Start data worker could not start: {e}"),
            }
        }
        if self.files.is_none() {
            self.files = FileSearch::start(notify, WM_APP_FILE_SEARCH)
                .inspect_err(|e| eprintln!("vitrobar: file search could not start: {e}"))
                .ok();
        }
        let Some(w) = &self.worker else { return };
        let stale = self.apps_at.is_none_or(|t| t.elapsed() >= CATALOG_MAX_AGE);
        if stale && !self.apps_pending && w.tx.send(Request::Apps).is_ok() {
            self.apps_pending = true;
        }
        let _ = w.tx.send(Request::Recent);
    }

    /// Takes the worker's answers. Returns (catalog changed, anything changed).
    pub fn drain(&mut self) -> (bool, bool) {
        let Some(w) = &self.worker else {
            return (false, false);
        };
        let mut apps_changed = false;
        let mut any = false;
        for a in w.answers.drain() {
            any = true;
            match a {
                Answer::Apps(apps) => {
                    self.apps_pending = false;
                    self.apps_at = Some(Instant::now());
                    // An empty read (the shell was not ready) keeps the old list.
                    if !apps.is_empty() && apps != self.apps {
                        self.rows = list_rows(&apps);
                        self.apps = apps;
                        apps_changed = true;
                    }
                }
                Answer::Recent(r) => {
                    self.recent_enabled = r.is_some();
                    self.recent = r.unwrap_or_default();
                }
                Answer::Account(a) => self.account = a,
            }
        }
        (apps_changed, any)
    }
}

// ---------------------------------------------------------------------------------------
// The open panel
// ---------------------------------------------------------------------------------------

/// Colours and strings the panel is drawn with.
pub struct Look<'a> {
    pub style: &'a MaterialStyle,
    pub accent: Rgba,
    pub lang: Lang,
}

/// The open Start panel. Dropping it destroys its visuals and window.
pub struct StartPanel {
    // Field order = drop order: the visuals before the window they are in.
    view: StartView,
    panel: Panel,
    pub state: StartState,
    pub geom: Geom,
    /// The monitor it opened on.
    pub monitor: u64,
    /// `Panel::activate` succeeded (it hears the keyboard).
    pub activated: bool,
    /// The foreground window when it opened (if activation failed, the next change of the
    /// foreground away from it closes the panel).
    pub fg_at_open: usize,
    /// Results for the current query, in display order (the list scrolls).
    pub results: Vec<ResultKind>,
    /// File hits of the last accepted file search.
    pub files: Vec<FileHit>,
    /// The query revision `results` were computed for.
    searched: u64,
    /// The cursor is over the panel (tracked for WM_MOUSELEAVE).
    pub tracking: bool,
    /// Icon sources requested while open (kept by the icon trim, forgotten on close).
    pub icon_srcs: HashSet<IconSrc>,
    /// First query character shown in the search box (scrolled so the caret stays inside).
    text_first: std::cell::Cell<usize>,
}

impl StartPanel {
    /// Opens the panel at `rect_px` (laid out by `layout`) on a monitor of `dpi`.
    pub fn open(
        comp: &Comp,
        layout: &StartLayout,
        rect_px: PxRect,
        dpi: u32,
        style: &MaterialStyle,
        monitor: u64,
        title: &str,
    ) -> Result<Self> {
        let panel = Panel::show(comp, rect_px, dpi, style, PANEL_RADIUS_DIP, title, false)?;
        let geom = Geom::new(layout, panel.scale());
        Self::with_panel(panel, geom, monitor)
    }

    /// Opens the full-screen grid over `rect_px` (a monitor's work area) on a monitor of
    /// `dpi`: square corners, the frosted tint of [`grid_style`].
    pub fn open_grid(
        comp: &Comp,
        rect_px: PxRect,
        dpi: u32,
        style: &MaterialStyle,
        monitor: u64,
        title: &str,
    ) -> Result<Self> {
        let style = grid_style(style);
        let panel = Panel::show(comp, rect_px, dpi, &style, 0.0, title, false)?;
        let scale = panel.scale();
        let work = Rect {
            x: 0.0,
            y: 0.0,
            w: (rect_px.2 - rect_px.0) as f32 / scale,
            h: (rect_px.3 - rect_px.1) as f32 / scale,
        };
        let geom = Geom::new_grid(work, scale);
        let mut sp = Self::with_panel(panel, geom, monitor)?;
        sp.state = StartState::new().into_grid();
        Ok(sp)
    }

    /// Fades the panel in (and scales it from 98 %) over `ms`.
    pub fn animate_open(&self, ms: u32) -> Result<()> {
        self.panel.animate_open(ms)
    }

    fn with_panel(panel: Panel, geom: Geom, monitor: u64) -> Result<Self> {
        let view = StartView::new(&panel)?;
        Ok(Self {
            view,
            panel,
            state: StartState::new(),
            geom,
            monitor,
            activated: false,
            fg_at_open: 0,
            results: Vec::new(),
            files: Vec::new(),
            searched: u64::MAX,
            tracking: false,
            icon_srcs: HashSet::new(),
            text_first: std::cell::Cell::new(0),
        })
    }

    pub fn hwnd(&self) -> HWND {
        self.panel.hwnd()
    }

    pub fn rect_px(&self) -> PxRect {
        self.panel.rect_px()
    }

    pub fn activate(&mut self) -> bool {
        self.activated = self.panel.activate();
        self.activated
    }

    /// Runs `ev` through the state machine with the current content.
    pub fn handle(
        &mut self,
        ev: StartInput,
        data: &StartData,
        start_pins: &[Pin],
        lang: Lang,
    ) -> StartAction {
        let ctx = StartCtx {
            geom: &self.geom,
            start_pins,
            apps: &data.apps,
            rows: &data.rows,
            recent: &data.recent,
            files: &self.files,
            results: &self.results,
            lang,
        };
        self.state.handle(ev, &ctx)
    }

    /// The query text changed (or the catalog did): search again. Returns the query to hand
    /// to the file search when the text changed since the last search.
    pub fn research(&mut self, data: &StartData, lang: Lang, force: bool) -> Option<(u64, String)> {
        let keep = self.key_of(self.state.selected, data);
        self.research_keeping(data, lang, force, keep)
    }

    /// Identity of result `i` (stable when the results are ranked again).
    fn key_of(&self, i: usize, data: &StartData) -> Option<String> {
        Some(match self.results.get(i)? {
            ResultKind::Math(_) => "m".into(),
            ResultKind::App(a) => {
                let p = data.apps.get(*a)?;
                format!("a:{}:{}", p.aumid.as_deref().unwrap_or(""), p.name)
            }
            ResultKind::Setting(s) => format!("s:{s}"),
            ResultKind::File(f) => format!("f:{}", self.files.get(*f)?.path),
        })
    }

    fn research_keeping(
        &mut self,
        data: &StartData,
        lang: Lang,
        force: bool,
        keep: Option<String>,
    ) -> Option<(u64, String)> {
        let rev = self.state.query_rev;
        let changed = rev != self.searched;
        if !changed && !force {
            return None;
        }
        if changed {
            // Files of an older query no longer apply.
            self.files.clear();
        }
        self.searched = rev;
        let text = self.state.input.text();
        self.results = if self.state.grid {
            grid_results(&text, &data.apps, &self.files, lang)
        } else {
            let items = search(&text, &data.apps, &self.files, lang).items;
            fit_results(items, self.geom.result_rows())
        };
        if !changed {
            // Same query ranked again (files or the catalog arrived): the selection stays
            // on the same item.
            if let Some(k) = keep {
                let found = (0..self.results.len())
                    .find(|&i| self.key_of(i, data).as_deref() == Some(k.as_str()));
                if let Some(i) = found {
                    self.state.selected = i;
                }
            }
        }
        let n = self.results.len();
        if self.state.selected >= n {
            self.state.selected = n.saturating_sub(1);
        }
        if self.state.mode == StartMode::Search || self.state.grid {
            let ctx = StartCtx {
                geom: &self.geom,
                start_pins: &[],
                apps: &data.apps,
                rows: &data.rows,
                recent: &data.recent,
                files: &self.files,
                results: &self.results,
                lang,
            };
            self.state.reveal_selected(&ctx);
        }
        changed.then_some((rev, text))
    }

    /// File hits arrived for `rev`; true if they belong to the current query.
    pub fn files_arrived(
        &mut self,
        rev: u64,
        hits: Vec<FileHit>,
        data: &StartData,
        lang: Lang,
    ) -> bool {
        if !self.state.files_arrived(rev, hits.len()) {
            return false;
        }
        let keep = self.key_of(self.state.selected, data);
        self.files = hits;
        self.research_keeping(data, lang, true, keep);
        true
    }

    fn icon(
        &mut self,
        icons: &mut IconLoader,
        src: Option<IconSrc>,
    ) -> Option<Arc<vitro_core::image::BgraImage>> {
        self.icon_at(icons, src, ICON_DIP)
    }

    fn icon_at(
        &mut self,
        icons: &mut IconLoader,
        src: Option<IconSrc>,
        dip: f32,
    ) -> Option<Arc<vitro_core::image::BgraImage>> {
        let src = src?;
        let px = ((dip * self.geom.scale).round() as u32).clamp(1, MAX_ICON_PX);
        let img = match icons.get(&src, px) {
            Lookup::Ready(img) => Some(Arc::clone(img)),
            _ => None,
        };
        self.icon_srcs.insert(src);
        img
    }

    /// Draws the current state (only what changed is redrawn).
    pub fn render(
        &mut self,
        painter: &Painter,
        icons: &mut IconLoader,
        data: &StartData,
        start_pins: &[Pin],
        look: &Look,
    ) -> Result<()> {
        let chrome = self.chrome_items(painter, data, look);
        if self.geom.grid.is_some() {
            let (page, list, viewport) = self.grid_items(icons, data, look);
            self.view.set_layer(Layer::Chrome, painter, chrome)?;
            self.view.set_layer(Layer::Page, painter, page)?;
            self.view.set_viewport(Some(viewport))?;
            self.view.set_scroll(self.state.scroll)?;
            return self.view.set_layer(Layer::List, painter, list);
        }
        let (page, list) = match self.state.mode {
            StartMode::Home => (self.home_items(icons, data, start_pins, look), None),
            StartMode::AllApps => {
                let (page, list) = self.all_apps_items(icons, data, look);
                (page, Some(list))
            }
            StartMode::Search => {
                let (page, list) = self.search_items(icons, data, look);
                (page, Some(list))
            }
        };
        self.view.set_layer(Layer::Chrome, painter, chrome)?;
        self.view.set_layer(Layer::Page, painter, page)?;
        match list {
            Some(list) => {
                let viewport = if self.state.mode == StartMode::Search {
                    self.geom.results_view
                } else {
                    self.geom.list
                };
                self.view.set_viewport(Some(viewport))?;
                self.view.set_scroll(self.state.scroll)?;
                self.view.set_layer(Layer::List, painter, list)?;
            }
            None => {
                self.view.set_viewport(None)?;
                self.view.set_layer(Layer::List, painter, Vec::new())?;
            }
        }
        Ok(())
    }

    fn sz(&self, dip: f32) -> f32 {
        dip * self.geom.scale
    }

    fn px(&self, dip: f32) -> i32 {
        (dip * self.geom.scale).round() as i32
    }

    fn text(&self, s: &str, rect: PxRect, size_dip: f32, color: Rgba, centred: bool) -> Item {
        Item {
            rect,
            part: Part::Text {
                text: s.to_string(),
                font: FontKind::Text,
                size_px: self.sz(size_dip),
                color,
                centred,
            },
        }
    }

    fn glyph(&self, g: char, rect: PxRect, size_dip: f32, color: Rgba) -> Item {
        Item {
            rect,
            part: Part::Text {
                text: g.to_string(),
                font: FontKind::Glyph,
                size_px: self.sz(size_dip),
                color,
                centred: true,
            },
        }
    }

    fn plate(&self, rect: PxRect, color: Rgba, radius_dip: f32) -> Item {
        Item {
            rect,
            part: Part::Plate {
                color,
                radius_px: self.sz(radius_dip),
            },
        }
    }

    fn image(
        &self,
        rect: PxRect,
        img: Option<Arc<vitro_core::image::BgraImage>>,
        look: &Look,
    ) -> Item {
        self.image_at(rect, img, look, ICON_DIP)
    }

    fn image_at(
        &self,
        rect: PxRect,
        img: Option<Arc<vitro_core::image::BgraImage>>,
        look: &Look,
        dip: f32,
    ) -> Item {
        let t = look.style.text;
        // An icon-sized square centred in `rect` (images are centred in it anyway; the
        // placeholder of an icon still loading takes its size).
        let side = self.px(dip).min(rect.2 - rect.0).min(rect.3 - rect.1);
        let x = (rect.0 + rect.2 - side) / 2;
        let y = (rect.1 + rect.3 - side) / 2;
        Item {
            rect: (x, y, x + side, y + side),
            part: Part::Image {
                image: img,
                placeholder: Rgba(t.0, t.1, t.2, 40),
            },
        }
    }

    /// A hover plate for `t` when the cursor is on it.
    fn hover_plate(&self, t: Target, rect: PxRect, look: &Look) -> Option<Item> {
        let c = look.style.text;
        (self.state.hover == Some(t)).then(|| self.plate(rect, Rgba(c.0, c.1, c.2, 22), 6.0))
    }

    fn chrome_items(&self, painter: &Painter, data: &StartData, look: &Look) -> Vec<Item> {
        let t = look.style.text;
        let dim = Rgba(t.0, t.1, t.2, 150);
        let g = &self.geom;
        let mut items = Vec::new();
        // Search box.
        let s = g.search;
        items.push(self.plate(s, Rgba(t.0, t.1, t.2, 18), 6.0));
        let pad = self.px(12.0);
        let glyph_w = self.px(20.0);
        items.push(self.glyph(
            '\u{E721}',
            (s.0 + pad, s.1, s.0 + pad + glyph_w, s.3),
            14.0,
            dim,
        ));
        let tx = s.0 + 2 * pad + glyph_w;
        let text_box = (tx, s.1, s.2 - pad, s.3);
        let full = self.state.input.text();
        // Scroll the text so the caret stays in the box: drop leading characters until the
        // part before the caret fits, and bring them back when there is room again.
        let caret = self.state.input.caret();
        let box_w = (text_box.2 - text_box.0 - self.px(2.0)).max(1) as f32;
        let chars: Vec<char> = full.chars().collect();
        let width = |from: usize| -> f32 {
            let s: String = chars[from..].iter().collect();
            painter
                .caret_x(&s, FontKind::Text, self.sz(14.0), caret - from)
                .unwrap_or(0.0)
        };
        let mut first = self.text_first.get().min(caret);
        while first < caret && width(first) > box_w {
            first += 1;
        }
        while first > 0 && width(first - 1) <= box_w {
            first -= 1;
        }
        self.text_first.set(first);
        let query: String = chars[first..].iter().collect();
        let caret_in_shown = caret - first;
        if full.is_empty() {
            items.push(self.text(
                tr(look.lang, Key::SearchPlaceholder),
                text_box,
                14.0,
                dim,
                false,
            ));
        } else {
            items.push(self.text(&query, text_box, 14.0, t, false));
        }
        // Non-blinking caret (no timers).
        let x = painter
            .caret_x(&query, FontKind::Text, self.sz(14.0), caret_in_shown)
            .unwrap_or(0.0)
            .round() as i32;
        let cx = (tx + x).min(text_box.2);
        let ch = self.px(18.0);
        let cy = (s.1 + s.3 - ch) / 2;
        items.push(self.plate((cx, cy, cx + self.px(1.5).max(1), cy + ch), t, 0.0));
        if g.grid.is_some() {
            return items;
        }
        // Footer: account and power.
        let a = g.account;
        if let Some(p) = self.hover_plate(Target::Account, a, look) {
            items.push(p);
        }
        let d = self.px(32.0);
        let circle = (
            a.0 + self.px(4.0),
            (a.1 + a.3 - d) / 2,
            a.0 + self.px(4.0) + d,
            (a.1 + a.3 + d) / 2,
        );
        items.push(self.plate(circle, look.accent, 16.0));
        let initial: String = data
            .account
            .chars()
            .find(|c| c.is_alphanumeric())
            .map(|c| c.to_uppercase().collect())
            .unwrap_or_default();
        items.push(self.text(&initial, circle, 14.0, Rgba(255, 255, 255, 255), true));
        items.push(self.text(
            &data.account,
            (circle.2 + self.px(10.0), a.1, a.2, a.3),
            13.0,
            t,
            false,
        ));
        let p = g.power;
        if let Some(pl) = self.hover_plate(Target::Power, p, look) {
            items.push(pl);
        }
        items.push(self.glyph('\u{E7E8}', p, 16.0, t));
        items
    }

    fn home_items(
        &mut self,
        icons: &mut IconLoader,
        data: &StartData,
        start_pins: &[Pin],
        look: &Look,
    ) -> Vec<Item> {
        let t = look.style.text;
        let dim = Rgba(t.0, t.1, t.2, 150);
        let g = self.geom.clone();
        let mut items = Vec::new();
        items.push(self.text(tr(look.lang, Key::Pinned), g.pins_title, 14.0, t, false));
        if let Some(p) = self.hover_plate(Target::AllApps, g.all_apps, look) {
            items.push(p);
        }
        let label = format!("{} \u{203A}", tr(look.lang, Key::AllApps));
        items.push(self.text(&label, g.all_apps, 12.0, t, true));
        let icon_box = self.px(40.0);
        for (i, (r, pin)) in g.pins.iter().zip(start_pins).enumerate() {
            if let Some(p) = self.hover_plate(Target::Pin(i), *r, look) {
                items.push(p);
            }
            let top = r.1 + self.px(8.0);
            let img = self.icon(icons, pin_icon(pin));
            items.push(self.image((r.0, top, r.2, top + icon_box), img, look));
            let lt = top + icon_box + self.px(2.0);
            let inset = self.px(4.0);
            items.push(self.text(
                &pin.name,
                (r.0 + inset, lt, r.2 - inset, lt + self.px(22.0)),
                12.0,
                t,
                true,
            ));
        }
        // Recent (skipped when the layout has no room: its title is then an empty box).
        let title = g.recent_title;
        if !g.recent.is_empty() {
            items.push(self.text(tr(look.lang, Key::Recent), title, 14.0, t, false));
        }
        if !data.recent_enabled || data.recent.is_empty() {
            if let Some(r) = g.recent.first() {
                items.push(self.text(tr(look.lang, Key::NoRecent), *r, 12.0, dim, false));
            }
        } else {
            for (i, (r, f)) in g.recent.iter().zip(&data.recent).enumerate() {
                if let Some(p) = self.hover_plate(Target::Recent(i), *r, look) {
                    items.push(p);
                }
                let img = self.icon(icons, Some(IconSrc::Path(f.path.clone())));
                let folder = std::path::Path::new(&f.path)
                    .parent()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();
                items.extend(self.two_line_row(*r, img, &f.name, &folder, look));
            }
        }
        items
    }

    /// Icon at the left, title above, subtitle below.
    fn two_line_row(
        &self,
        r: PxRect,
        img: Option<Arc<vitro_core::image::BgraImage>>,
        title: &str,
        sub: &str,
        look: &Look,
    ) -> Vec<Item> {
        let t = look.style.text;
        let dim = Rgba(t.0, t.1, t.2, 150);
        let icon_w = self.px(44.0);
        let mid = (r.1 + r.3) / 2;
        let x = r.0 + icon_w + self.px(4.0);
        let right = r.2 - self.px(8.0);
        vec![
            self.image((r.0, r.1, r.0 + icon_w, r.3), img, look),
            self.text(
                title,
                (x, mid - self.px(19.0), right, mid + self.px(1.0)),
                14.0,
                t,
                false,
            ),
            self.text(sub, (x, mid, right, mid + self.px(17.0)), 12.0, dim, false),
        ]
    }

    /// The page ("No results" when empty) and the result rows (content px of the scrolling
    /// results viewport).
    fn search_items(
        &mut self,
        icons: &mut IconLoader,
        data: &StartData,
        look: &Look,
    ) -> (Vec<Item>, Vec<Item>) {
        let t = look.style.text;
        let dim = Rgba(t.0, t.1, t.2, 150);
        let g = self.geom.clone();
        let mut page = Vec::new();
        let mut items = Vec::new();
        if self.results.is_empty() {
            if let Some(r) = g.results.first() {
                page.push(self.text(tr(look.lang, Key::NoResults), *r, 14.0, dim, false));
            }
            return (page, items);
        }
        let results = self.results.clone();
        let w = g.results_view.2 - g.results_view.0;
        let rows: Vec<PxRect> = (0..results.len() as i32)
            .map(|i| (0, i * g.row_h, w, (i + 1) * g.row_h))
            .collect();
        for (i, (r, kind)) in rows.iter().zip(&results).enumerate() {
            if i == self.state.selected {
                items.push(self.plate(*r, Rgba(t.0, t.1, t.2, 30), 6.0));
            } else if let Some(p) = self.hover_plate(Target::Result(i), *r, look) {
                items.push(p);
            }
            items.extend(self.result_items(icons, data, look, *r, kind));
        }
        (page, items)
    }

    /// The icon (or glyph) and the two text lines of one search result in the row `r`.
    fn result_items(
        &mut self,
        icons: &mut IconLoader,
        data: &StartData,
        look: &Look,
        r: PxRect,
        kind: &ResultKind,
    ) -> Vec<Item> {
        let t = look.style.text;
        let icon_w = self.px(44.0);
        let glyph_box = (r.0, r.1, r.0 + icon_w, r.3);
        let lang = look.lang;
        let (title, sub, icon) = match kind {
            ResultKind::Math(v) => (
                format!("= {}", format_result(*v, lang)),
                format!(
                    "{} \u{00B7} {}",
                    tr(lang, Key::Calculator),
                    tr(lang, Key::CopyResult)
                ),
                Err('\u{E8EF}'),
            ),
            ResultKind::App(a) => {
                let Some(p) = data.apps.get(*a) else {
                    return Vec::new();
                };
                (
                    p.name.clone(),
                    tr(lang, Key::Apps).to_string(),
                    Ok(pin_icon(p)),
                )
            }
            ResultKind::Setting(s) => {
                let Some(p) = SETTINGS_PAGES.get(*s) else {
                    return Vec::new();
                };
                (
                    tr(lang, p.key).to_string(),
                    tr(lang, Key::SettingsPages).to_string(),
                    Err('\u{E713}'),
                )
            }
            ResultKind::File(f) => {
                let Some(h) = self.files.get(*f) else {
                    return Vec::new();
                };
                (
                    h.name.clone(),
                    h.path.clone(),
                    Ok(Some(IconSrc::Path(h.path.clone()))),
                )
            }
        };
        match icon {
            Ok(src) => {
                let img = self.icon(icons, src);
                self.two_line_row(r, img, &title, &sub, look)
            }
            Err(glyph) => {
                let mut v = vec![self.glyph(glyph, glyph_box, 18.0, t)];
                v.extend(
                    self.two_line_row(r, None, &title, &sub, look)
                        .into_iter()
                        .skip(1),
                );
                v
            }
        }
    }

    /// The full-screen grid: the result rows above it (page layer, panel px), the app cells
    /// near the viewport (list layer, content px) and the viewport itself.
    fn grid_items(
        &mut self,
        icons: &mut IconLoader,
        data: &StartData,
        look: &Look,
    ) -> (Vec<Item>, Vec<Item>, PxRect) {
        let t = look.style.text;
        let dim = Rgba(t.0, t.1, t.2, 150);
        let g = self.geom.clone();
        let Some(gg) = g.grid.clone() else {
            return (Vec::new(), Vec::new(), (0, 0, 0, 0));
        };
        let results = self.results.clone();
        let n_top = results
            .iter()
            .take_while(|k| !matches!(k, ResultKind::App(_)))
            .count();
        let focus = self.state.mode == StartMode::Search || self.state.kbd;
        let mut page = Vec::new();
        if results.is_empty() && self.state.mode == StartMode::Search {
            let r = (g.search.0, gg.top, g.search.2, gg.top + g.row_h);
            page.push(self.text(tr(look.lang, Key::NoResults), r, 14.0, dim, false));
        }
        for (i, kind) in results.iter().take(n_top).enumerate() {
            let y = gg.top + i as i32 * g.row_h;
            let r = (g.search.0, y, g.search.2, y + g.row_h);
            if i == self.state.selected && focus {
                page.push(self.plate(r, Rgba(t.0, t.1, t.2, 30), 6.0));
            } else if let Some(p) = self.hover_plate(Target::Result(i), r, look) {
                page.push(p);
            }
            page.extend(self.result_items(icons, data, look, r, kind));
        }
        let view = (0, g.grid_view_top(n_top), g.size.0, g.size.1);
        let cols = gg.cols.max(1);
        let cells = results.len() - n_top;
        let total_rows = cells.div_ceil(cols);
        let rows_visible = g.grid_rows_visible(n_top);
        let first_row = (self.state.scroll as i32 / gg.step.max(1)) as usize;
        let start_row = first_row.saturating_sub(GRID_SLACK_ROWS);
        let end_row = (first_row + rows_visible + GRID_SLACK_ROWS + 1).min(total_rows);
        let pad = self.px(4.0);
        let mut list = Vec::new();
        for idx in (start_row * cols)..(end_row * cols).min(cells) {
            let ri = n_top + idx;
            let ResultKind::App(a) = results[ri] else {
                continue;
            };
            let Some(p) = data.apps.get(a) else { continue };
            let (c, row) = ((idx % cols) as i32, (idx / cols) as i32);
            let x = gg.x0 + c * gg.step;
            let y = row * gg.step;
            let r = (x, y, x + gg.cell, y + gg.cell);
            if ri == self.state.selected && focus {
                list.push(self.plate(r, Rgba(t.0, t.1, t.2, 30), 8.0));
                list.push(Item {
                    rect: r,
                    part: Part::Outline {
                        color: look.accent,
                        radius_px: self.sz(8.0),
                        thickness_px: self.sz(2.0).max(1.0),
                    },
                });
            } else if let Some(h) = self.hover_plate(Target::Result(ri), r, look) {
                list.push(h);
            }
            let icon_box = self.px(GRID_ICON_DIP);
            let top = y + self.px(8.0);
            let img = self.icon_at(icons, pin_icon(p), GRID_ICON_DIP);
            list.push(self.image_at(
                (x, top, x + gg.cell, top + icon_box),
                img,
                look,
                GRID_ICON_DIP,
            ));
            let ny = top + icon_box + self.px(4.0);
            list.push(Item {
                rect: (x + pad, ny, x + gg.cell - pad, y + gg.cell),
                part: Part::Wrapped {
                    text: p.name.clone(),
                    font: FontKind::Text,
                    size_px: self.sz(12.0),
                    color: t,
                    max_lines: 2,
                },
            });
        }
        (page, list, view)
    }

    /// The page (title and Back button) and the list layer's rows near the viewport.
    fn all_apps_items(
        &mut self,
        icons: &mut IconLoader,
        data: &StartData,
        look: &Look,
    ) -> (Vec<Item>, Vec<Item>) {
        let t = look.style.text;
        let g = self.geom.clone();
        let mut page = Vec::new();
        page.push(self.text(tr(look.lang, Key::AllApps), g.pins_title, 14.0, t, false));
        if let Some(p) = self.hover_plate(Target::AllApps, g.all_apps, look) {
            page.push(p);
        }
        let label = format!("\u{2039} {}", tr(look.lang, Key::Back));
        page.push(self.text(&label, g.all_apps, 12.0, t, true));
        // Rows from a little above to a little below the viewport, in steps of the slack so
        // small scrolls leave the set (and so the layer) unchanged.
        let row_h = g.row_h.max(1);
        let w = g.list.2 - g.list.0;
        let first = (self.state.scroll as i32 / row_h) as usize;
        let start = first.saturating_sub(LIST_SLACK_ROWS) / LIST_SLACK_ROWS * LIST_SLACK_ROWS;
        let visible = (g.list_h() / row_h) as usize + 1;
        let end = (start + visible + 3 * LIST_SLACK_ROWS).min(data.rows.len());
        let mut list = Vec::new();
        for i in start..end {
            let y = i as i32 * row_h;
            let r = (0, y, w, y + row_h);
            if let Some(p) = self.hover_plate(Target::Row(i), r, look) {
                list.push(p);
            }
            match data.rows[i] {
                ListRow::Header(c) => {
                    let pad = self.px(8.0);
                    list.push(self.text(
                        &c.to_string(),
                        (r.0 + pad, r.1, r.2, r.3),
                        14.0,
                        look.accent,
                        false,
                    ));
                }
                ListRow::App(a) => {
                    let Some(p) = data.apps.get(a) else { continue };
                    let icon_w = self.px(44.0);
                    let img = self.icon(icons, pin_icon(p));
                    list.push(self.image((r.0, r.1, r.0 + icon_w, r.3), img, look));
                    let x = r.0 + icon_w + self.px(4.0);
                    list.push(self.text(
                        &p.name,
                        (x, r.1, r.2 - self.px(8.0), r.3),
                        14.0,
                        t,
                        false,
                    ));
                }
            }
        }
        (page, list)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vitro_core::start_layout::{panel_layout, PANEL_H_DIP, PANEL_W_DIP};

    fn geom() -> Geom {
        let l = panel_layout(Rect {
            x: 100.0,
            y: 50.0,
            w: PANEL_W_DIP,
            h: PANEL_H_DIP,
        });
        Geom::new(&l, 1.0)
    }

    fn pin(name: &str) -> Pin {
        Pin {
            aumid: Some(format!("{name}!App")),
            path: None,
            name: name.into(),
            link: None,
        }
    }

    struct Fixture {
        geom: Geom,
        pins: Vec<Pin>,
        apps: Vec<Pin>,
        rows: Vec<ListRow>,
        recent: Vec<RecentFile>,
        files: Vec<FileHit>,
        results: Vec<ResultKind>,
    }

    impl Fixture {
        fn new() -> Self {
            let apps: Vec<Pin> = (0..60).map(|i| pin(&format!("App {i:02}"))).collect();
            Self {
                geom: geom(),
                pins: vec![pin("Paint"), pin("Calculator")],
                rows: list_rows(&apps),
                apps,
                recent: Vec::new(),
                files: Vec::new(),
                results: Vec::new(),
            }
        }

        fn ctx(&self) -> StartCtx<'_> {
            StartCtx {
                geom: &self.geom,
                start_pins: &self.pins,
                apps: &self.apps,
                rows: &self.rows,
                recent: &self.recent,
                files: &self.files,
                results: &self.results,
                lang: Lang::En,
            }
        }
    }

    fn centre(r: PxRect) -> (i32, i32) {
        ((r.0 + r.2) / 2, (r.1 + r.3) / 2)
    }

    #[test]
    fn typing_switches_home_to_search() {
        let f = Fixture::new();
        let mut s = StartState::new();
        assert_eq!(s.mode, StartMode::Home);
        assert_eq!(
            s.handle(StartInput::Char('c'), &f.ctx()),
            StartAction::Redraw
        );
        assert_eq!(s.mode, StartMode::Search);
        assert_eq!(s.input.text(), "c");
        assert_eq!(s.query_rev, 1);
        // From All apps too.
        let mut s = StartState::new();
        s.mode = StartMode::AllApps;
        s.handle(StartInput::Char('x'), &f.ctx());
        assert_eq!(s.mode, StartMode::Search);
        // Control characters do nothing.
        let mut s = StartState::new();
        assert_eq!(
            s.handle(StartInput::Char('\u{8}'), &f.ctx()),
            StartAction::None
        );
        assert_eq!(s.mode, StartMode::Home);
    }

    #[test]
    fn esc_clears_then_closes() {
        let f = Fixture::new();
        let mut s = StartState::new();
        s.handle(StartInput::Char('a'), &f.ctx());
        assert_eq!(
            s.handle(StartInput::Key(VK_ESCAPE, false), &f.ctx()),
            StartAction::Redraw
        );
        assert!(s.input.is_empty());
        assert_eq!(s.mode, StartMode::Home);
        assert_eq!(
            s.handle(StartInput::Key(VK_ESCAPE, false), &f.ctx()),
            StartAction::Close
        );
    }

    #[test]
    fn backspace_to_empty_returns_home() {
        let f = Fixture::new();
        let mut s = StartState::new();
        s.handle(StartInput::Char('a'), &f.ctx());
        s.handle(StartInput::Key(VK_BACK, false), &f.ctx());
        assert_eq!(s.mode, StartMode::Home);
        s.handle(StartInput::Paste("open file".into()), &f.ctx());
        s.handle(StartInput::Key(VK_BACK, true), &f.ctx());
        assert_eq!(s.input.text(), "open ");
        assert_eq!(s.mode, StartMode::Search);
    }

    #[test]
    fn arrows_clamp() {
        let mut f = Fixture::new();
        f.results = vec![
            ResultKind::App(0),
            ResultKind::App(1),
            ResultKind::Setting(0),
        ];
        let mut s = StartState::new();
        s.handle(StartInput::Char('a'), &f.ctx());
        assert_eq!(
            s.handle(StartInput::Key(VK_UP, false), &f.ctx()),
            StartAction::None
        );
        assert_eq!(s.selected, 0);
        for _ in 0..5 {
            s.handle(StartInput::Key(VK_DOWN, false), &f.ctx());
        }
        assert_eq!(s.selected, 2);
        s.handle(StartInput::Key(VK_UP, false), &f.ctx());
        assert_eq!(s.selected, 1);
    }

    #[test]
    fn enter_on_math_copies_and_enter_on_nothing_does_nothing() {
        let mut f = Fixture::new();
        let mut s = StartState::new();
        s.handle(StartInput::Paste("2+2".into()), &f.ctx());
        assert_eq!(
            s.handle(StartInput::Key(VK_RETURN, false), &f.ctx()),
            StartAction::None
        );
        f.results = search("2+2", &f.apps, &f.files, Lang::En).items;
        assert_eq!(
            s.handle(StartInput::Key(VK_RETURN, false), &f.ctx()),
            StartAction::CopyMath("4".into())
        );
    }

    #[test]
    fn enter_runs_apps_settings_and_files() {
        let mut f = Fixture::new();
        f.files = vec![FileHit {
            name: "a.txt".into(),
            path: r"C:\a.txt".into(),
        }];
        f.results = vec![
            ResultKind::App(3),
            ResultKind::Setting(1),
            ResultKind::File(0),
        ];
        let mut s = StartState::new();
        s.handle(StartInput::Char('a'), &f.ctx());
        assert_eq!(
            s.handle(StartInput::Key(VK_RETURN, false), &f.ctx()),
            StartAction::Launch(f.apps[3].clone())
        );
        s.handle(StartInput::Key(VK_DOWN, false), &f.ctx());
        assert_eq!(
            s.handle(StartInput::Key(VK_RETURN, false), &f.ctx()),
            StartAction::Open(SETTINGS_PAGES[1].uri.into())
        );
        s.handle(StartInput::Key(VK_DOWN, false), &f.ctx());
        assert_eq!(
            s.handle(StartInput::Key(VK_RETURN, false), &f.ctx()),
            StartAction::Open(r"C:\a.txt".into())
        );
    }

    #[test]
    fn wheel_in_all_apps_scrolls_and_clamps() {
        let f = Fixture::new();
        let mut s = StartState::new();
        // Not in Home.
        assert_eq!(
            s.handle(StartInput::Wheel(-120), &f.ctx()),
            StartAction::None
        );
        s.mode = StartMode::AllApps;
        assert_eq!(
            s.handle(StartInput::Wheel(-120), &f.ctx()),
            StartAction::Redraw
        );
        assert_eq!(s.scroll, 3.0 * f.geom.row_h as f32);
        assert_eq!(
            s.handle(StartInput::Wheel(120 * 10), &f.ctx()),
            StartAction::Redraw
        );
        assert_eq!(s.scroll, 0.0);
        assert_eq!(
            s.handle(StartInput::Wheel(120), &f.ctx()),
            StartAction::None
        );
        let max = f.ctx().max_scroll(StartMode::AllApps);
        assert!(max > 0.0);
        s.handle(StartInput::Wheel(-120 * 1000), &f.ctx());
        assert_eq!(s.scroll, max);
        assert_eq!(
            max,
            (f.rows.len() as i32 * f.geom.row_h - f.geom.list_h()) as f32
        );
    }

    #[test]
    fn right_click_on_a_pin_or_listed_app_opens_its_menu() {
        let f = Fixture::new();
        let mut s = StartState::new();
        let pt = centre(f.geom.pins[1]);
        assert_eq!(
            s.handle(StartInput::RightClick(pt), &f.ctx()),
            StartAction::Menu(pt, MenuItem::Pin(1))
        );
        // An empty pin cell has no menu.
        let empty = centre(f.geom.pins[5]);
        assert_eq!(
            s.handle(StartInput::RightClick(empty), &f.ctx()),
            StartAction::None
        );
        // All apps: row 0 is the "A" header, row 1 the first app.
        s.mode = StartMode::AllApps;
        let l = f.geom.list;
        let row1 = (l.0 + 20, l.1 + f.geom.row_h + 5);
        assert_eq!(f.rows[0], ListRow::Header('A'));
        assert_eq!(
            s.handle(StartInput::RightClick(row1), &f.ctx()),
            StartAction::Menu(row1, MenuItem::App(0))
        );
        let row0 = (l.0 + 20, l.1 + 5);
        assert_eq!(
            s.handle(StartInput::RightClick(row0), &f.ctx()),
            StartAction::None
        );
    }

    #[test]
    fn paste_sanitises_newlines() {
        let f = Fixture::new();
        let mut s = StartState::new();
        assert_eq!(
            s.handle(StartInput::Paste("a\nb".into()), &f.ctx()),
            StartAction::Redraw
        );
        assert_eq!(s.input.text(), "a b");
        assert_eq!(s.mode, StartMode::Search);
        // Pasting nothing changes nothing.
        assert_eq!(
            s.handle(StartInput::Paste("\u{7}".into()), &f.ctx()),
            StartAction::None
        );
    }

    #[test]
    fn stale_file_generation_is_ignored() {
        let f = Fixture::new();
        let mut s = StartState::new();
        s.handle(StartInput::Char('r'), &f.ctx());
        let old = s.query_rev;
        s.handle(StartInput::Char('e'), &f.ctx());
        s.selected = 2;
        assert!(!s.files_arrived(old, 3));
        assert_eq!(s.files, 0);
        assert!(s.files_arrived(s.query_rev, 3));
        assert_eq!((s.files, s.files_rev), (3, s.query_rev));
        // The selection above the files stays where it is.
        assert_eq!(s.selected, 2);
    }

    #[test]
    fn clicks_launch_open_and_switch_modes() {
        let mut f = Fixture::new();
        f.recent = vec![RecentFile {
            name: "r.txt".into(),
            path: r"C:\r.txt".into(),
        }];
        let mut s = StartState::new();
        let c = f.ctx();
        assert_eq!(
            s.handle(StartInput::Click(centre(f.geom.pins[0])), &c),
            StartAction::Launch(f.pins[0].clone())
        );
        assert_eq!(
            s.handle(StartInput::Click(centre(f.geom.recent[0])), &c),
            StartAction::Open(r"C:\r.txt".into())
        );
        assert_eq!(
            s.handle(StartInput::Click(centre(f.geom.account)), &c),
            StartAction::Open(ACCOUNT_URI.into())
        );
        assert!(matches!(
            s.handle(StartInput::Click(centre(f.geom.power)), &c),
            StartAction::PowerMenu(_)
        ));
        assert_eq!(
            s.handle(StartInput::Click(centre(f.geom.all_apps)), &c),
            StartAction::Redraw
        );
        assert_eq!(s.mode, StartMode::AllApps);
        let l = f.geom.list;
        assert_eq!(
            s.handle(StartInput::Click((l.0 + 20, l.1 + f.geom.row_h + 5)), &c),
            StartAction::Launch(f.apps[0].clone())
        );
        s.handle(StartInput::Click(centre(f.geom.all_apps)), &c);
        assert_eq!(s.mode, StartMode::Home);
    }

    #[test]
    fn hover_changes_redraw_once() {
        let f = Fixture::new();
        let mut s = StartState::new();
        let pt = centre(f.geom.pins[0]);
        assert_eq!(
            s.handle(StartInput::Move(pt), &f.ctx()),
            StartAction::Redraw
        );
        assert_eq!(s.handle(StartInput::Move(pt), &f.ctx()), StartAction::None);
        assert_eq!(s.hover, Some(Target::Pin(0)));
        assert_eq!(s.handle(StartInput::Leave, &f.ctx()), StartAction::Redraw);
        assert_eq!(s.hover, None);
    }

    /// A panel whose results viewport holds exactly 8 rows (at 1x).
    fn short_geom() -> Geom {
        let l = panel_layout(Rect {
            x: 0.0,
            y: 0.0,
            w: PANEL_W_DIP,
            h: 500.0,
        });
        Geom::new(&l, 1.0)
    }

    fn sixteen_results() -> Vec<ResultKind> {
        let mut r = vec![ResultKind::Math(4.0)];
        r.extend((0..6).map(ResultKind::App));
        r.extend((0..3).map(ResultKind::Setting));
        r.extend((0..6).map(ResultKind::File));
        r
    }

    #[test]
    fn every_result_is_reachable_and_the_selection_stays_visible() {
        let mut f = Fixture::new();
        f.geom = short_geom();
        assert_eq!(f.geom.result_rows(), 8);
        f.files = (0..6)
            .map(|i| FileHit {
                name: format!("f{i}.txt"),
                path: format!("C:\\f{i}.txt"),
            })
            .collect();
        f.results = sixteen_results();
        let mut s = StartState::new();
        s.handle(StartInput::Char('a'), &f.ctx());
        let h = f.geom.results_h() as f32;
        let row = f.geom.row_h as f32;
        for i in 1..16 {
            assert_eq!(
                s.handle(StartInput::Key(VK_DOWN, false), &f.ctx()),
                StartAction::Redraw
            );
            assert_eq!(s.selected, i);
            let top = i as f32 * row;
            assert!(
                top >= s.scroll && top + row <= s.scroll + h,
                "row {i} hidden"
            );
        }
        // The last one (a file) runs, and is under the cursor at its scrolled position.
        assert_eq!(
            s.handle(StartInput::Key(VK_RETURN, false), &f.ctx()),
            StartAction::Open("C:\\f5.txt".into())
        );
        let v = f.geom.results_view;
        let y = (15.0 * row - s.scroll) as i32 + v.1 + 5;
        assert_eq!(s.hit((v.0 + 20, y), &f.ctx()), Some(Target::Result(15)));
        // Back up to the top scrolls back.
        for _ in 0..15 {
            s.handle(StartInput::Key(VK_UP, false), &f.ctx());
        }
        assert_eq!((s.selected, s.scroll), (0, 0.0));
        // The wheel scrolls the results too, clamped.
        s.handle(StartInput::Wheel(-120 * 10), &f.ctx());
        assert_eq!(s.scroll, f.ctx().max_scroll(StartMode::Search));
        assert_eq!(s.scroll, 16.0 * row - h);
    }

    #[test]
    fn files_show_on_a_short_panel() {
        let fitted = fit_results(sixteen_results(), 8);
        let files_visible = fitted
            .iter()
            .take(8)
            .filter(|k| matches!(k, ResultKind::File(_)))
            .count();
        assert_eq!(files_visible, 2);
        // Four apps made room (math and 3 settings precede the files); every file is kept.
        assert_eq!(
            fitted
                .iter()
                .filter(|k| matches!(k, ResultKind::App(_)))
                .count(),
            2
        );
        assert_eq!(fitted.len(), 12);
        assert!(fitted.contains(&ResultKind::Setting(2)));
        assert!(fitted.contains(&ResultKind::File(5)));
        // Room enough, or no files: unchanged.
        assert_eq!(fit_results(sixteen_results(), 16), sixteen_results());
        let no_files: Vec<ResultKind> = (0..12).map(ResultKind::App).collect();
        assert_eq!(fit_results(no_files.clone(), 4), no_files);
        // One file needs one row.
        let one = vec![
            ResultKind::App(0),
            ResultKind::App(1),
            ResultKind::App(2),
            ResultKind::File(0),
        ];
        assert_eq!(
            fit_results(one, 2),
            vec![ResultKind::App(0), ResultKind::File(0)]
        );
    }

    #[test]
    fn revisions_continue_across_panels() {
        let f = Fixture::new();
        let mut old = StartState::new();
        old.handle(StartInput::Char('x'), &f.ctx());
        let stale = old.query_rev;
        let mut s = StartState::with_rev(stale + 1);
        assert!(!s.files_arrived(stale, 2));
        s.handle(StartInput::Char('y'), &f.ctx());
        assert!(s.query_rev > stale + 1);
        assert!(s.files_arrived(s.query_rev, 2));
    }

    #[test]
    fn list_rows_have_letter_headers() {
        let apps = vec![
            pin("Alpha"),
            pin("apple"),
            pin("Écran"),
            pin("7-Zip"),
            pin("Zeta"),
        ];
        let rows = list_rows(&apps);
        assert_eq!(
            rows,
            vec![
                ListRow::Header('A'),
                ListRow::App(0),
                ListRow::App(1),
                ListRow::Header('E'),
                ListRow::App(2),
                ListRow::Header('#'),
                ListRow::App(3),
                ListRow::Header('Z'),
                ListRow::App(4),
            ]
        );
    }

    #[test]
    fn catalog_entries_open_through_apps_folder_and_bar_pins_launch() {
        let catalog = Pin {
            aumid: Some(r"{6D809377-6AF0-444B-8957-A3773F02200E}\Tool\tool.exe".into()),
            path: None,
            name: "Tool".into(),
            link: None,
        };
        match launch_job(&catalog) {
            Some(Job::Open(s)) => assert_eq!(
                s,
                r"shell:AppsFolder\{6D809377-6AF0-444B-8957-A3773F02200E}\Tool\tool.exe"
            ),
            other => panic!("{other:?}"),
        }
        let bar = Pin {
            aumid: Some("App!Id".into()),
            path: Some(r"C:\a.exe".into()),
            name: "A".into(),
            link: None,
        };
        assert!(matches!(launch_job(&bar), Some(Job::Pin(_))));
        assert!(launch_job(&Pin::default()).is_none());
    }

    #[test]
    fn same_app_matches_any_shared_identity() {
        let a = Pin {
            aumid: Some("X!App".into()),
            ..Default::default()
        };
        let b = Pin {
            aumid: Some("x!app".into()),
            path: Some("p".into()),
            ..Default::default()
        };
        assert!(same_app(&a, &b));
        assert!(!same_app(&a, &Pin::default()));
        assert!(!same_app(&Pin::default(), &Pin::default()));
    }

    #[test]
    fn geom_hit_targets_are_inside_the_panel() {
        let g = geom();
        assert_eq!(g.size, (640, 720));
        for r in g.pins.iter().chain(&g.results).chain(&g.recent) {
            assert!(r.0 >= 0 && r.1 >= 0 && r.2 <= g.size.0 && r.3 <= g.size.1);
        }
        assert!(g.list.1 > g.pins_title.3 && g.list.3 == g.footer.1);
    }

    // ---- full-screen grid ----

    /// 1920x1040 at 1x: 8 columns.
    fn grid_geom() -> Geom {
        Geom::new_grid(
            Rect {
                x: 0.0,
                y: 0.0,
                w: 1920.0,
                h: 1040.0,
            },
            1.0,
        )
    }

    fn grid_fixture(n: usize) -> Fixture {
        let mut f = Fixture::new();
        f.apps = (0..n).map(|i| pin(&format!("App {i:03}"))).collect();
        f.rows = list_rows(&f.apps);
        f.geom = grid_geom();
        f.results = grid_results("", &f.apps, &[], Lang::En);
        f
    }

    fn key(s: &mut StartState, f: &Fixture, vk: u32) -> StartAction {
        s.handle(StartInput::Key(vk, false), &f.ctx())
    }

    #[test]
    fn grid_arrows_move_by_one_and_by_columns_and_clamp() {
        let f = grid_fixture(20);
        let cols = f.geom.grid.as_ref().unwrap().cols;
        assert_eq!(cols, 8);
        let mut s = StartState::new().into_grid();
        assert_eq!(s.mode, StartMode::AllApps);
        // Enter does nothing until a key moved the focus.
        assert_eq!(key(&mut s, &f, VK_RETURN), StartAction::None);
        // The first key shows the focus on app 0; the left edge clamps.
        assert_eq!(key(&mut s, &f, VK_LEFT), StartAction::Redraw);
        assert_eq!(s.selected, 0);
        assert_eq!(key(&mut s, &f, VK_LEFT), StartAction::None);
        assert_eq!(key(&mut s, &f, VK_RIGHT), StartAction::Redraw);
        assert_eq!(s.selected, 1);
        assert_eq!(key(&mut s, &f, VK_DOWN), StartAction::Redraw);
        assert_eq!(s.selected, 1 + cols);
        assert_eq!(key(&mut s, &f, VK_UP), StartAction::Redraw);
        assert_eq!(s.selected, 1);
        assert_eq!(key(&mut s, &f, VK_UP), StartAction::None);
        // 20 apps: rows of 8, 8, 4. Down from column 5 of row 1 has no cell below: last app.
        s.selected = cols + 5;
        key(&mut s, &f, VK_DOWN);
        assert_eq!(s.selected, 19);
        assert_eq!(key(&mut s, &f, VK_DOWN), StartAction::None);
        assert_eq!(key(&mut s, &f, VK_RIGHT), StartAction::None);
        assert_eq!(s.selected, 19);
        // Enter launches the focused app.
        assert_eq!(
            key(&mut s, &f, VK_RETURN),
            StartAction::Launch(f.apps[19].clone())
        );
    }

    #[test]
    fn grid_page_down_moves_by_a_screenful_of_rows() {
        let f = grid_fixture(200);
        let cols = f.geom.grid.as_ref().unwrap().cols;
        let rows = f.geom.grid_rows_visible(0);
        assert!(rows >= 2, "{rows}");
        let mut s = StartState::new().into_grid();
        key(&mut s, &f, VK_NEXT);
        assert_eq!(s.selected, rows * cols);
        key(&mut s, &f, VK_NEXT);
        assert_eq!(s.selected, 2 * rows * cols);
        // The focused row stays inside the viewport.
        let gg = f.geom.grid.as_ref().unwrap();
        let top = (s.selected / cols) as i32 * gg.step;
        let h = f.geom.grid_view_h(0);
        assert!(s.scroll as i32 <= top && top + gg.cell <= s.scroll as i32 + h);
        key(&mut s, &f, VK_PRIOR);
        assert_eq!(s.selected, rows * cols);
        key(&mut s, &f, VK_END);
        assert_eq!(s.selected, 199);
        key(&mut s, &f, VK_NEXT);
        assert_eq!(s.selected, 199);
        key(&mut s, &f, VK_HOME);
        assert_eq!(s.selected, 0);
        assert_eq!(s.scroll, 0.0);
    }

    #[test]
    fn grid_typing_lists_matching_apps_only_with_the_other_kinds_above() {
        let mut apps: Vec<Pin> = (0..30).map(|i| pin(&format!("Tool {i:02}"))).collect();
        apps.push(pin("Paint"));
        apps.push(pin("Paint 3D"));
        let all = grid_results("", &apps, &[], Lang::En);
        assert_eq!(all.len(), 32);
        let r = grid_results("paint", &apps, &[], Lang::En);
        let names: Vec<&str> = r
            .iter()
            .filter_map(|k| match k {
                ResultKind::App(a) => Some(apps[*a].name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(names, ["Paint", "Paint 3D"]);
        // More matches than the panel's six-app cap are all listed.
        let r = grid_results("tool", &apps, &[], Lang::En);
        let n = r.iter().filter(|k| matches!(k, ResultKind::App(_))).count();
        assert_eq!(n, 30);
        let r = grid_results("display", &apps, &[], Lang::En);
        assert!(matches!(r.first(), Some(ResultKind::Setting(_))));
        let r = grid_results("2+3", &apps, &[], Lang::En);
        assert!(matches!(r.first(), Some(ResultKind::Math(_))));
        assert!(r.iter().skip(1).all(|k| matches!(k, ResultKind::App(_))));
    }

    #[test]
    fn grid_rows_above_the_grid_are_reached_with_arrows_and_run_with_enter() {
        let mut f = grid_fixture(40);
        f.results = vec![ResultKind::Math(5.0), ResultKind::Setting(0)];
        f.results.extend((0..40).map(ResultKind::App));
        let cols = f.geom.grid.as_ref().unwrap().cols;
        let mut s = StartState::new().into_grid();
        s.handle(StartInput::Char('x'), &f.ctx());
        assert_eq!(s.mode, StartMode::Search);
        assert_eq!(s.selected, 0);
        // Down walks the rows into the first app, Up from the first grid row walks back.
        key(&mut s, &f, VK_DOWN);
        key(&mut s, &f, VK_DOWN);
        assert_eq!(s.selected, 2);
        key(&mut s, &f, VK_DOWN);
        assert_eq!(s.selected, 2 + cols);
        key(&mut s, &f, VK_UP);
        key(&mut s, &f, VK_UP);
        assert_eq!(s.selected, 1);
        assert!(matches!(key(&mut s, &f, VK_RETURN), StartAction::Open(_)));
        // Backspace edits the query; the arrows never touch the caret.
        assert_eq!(key(&mut s, &f, VK_BACK), StartAction::Redraw);
        assert_eq!(s.mode, StartMode::AllApps);
        assert!(!s.kbd);
    }

    #[test]
    fn grid_clicks_and_wheel() {
        let f = grid_fixture(200);
        let gg = f.geom.grid.clone().unwrap();
        let mut s = StartState::new().into_grid();
        let cell = |i: usize, s: &StartState| {
            let (c, r) = ((i % gg.cols) as i32, (i / gg.cols) as i32);
            (
                gg.x0 + c * gg.step + gg.cell / 2,
                gg.top + r * gg.step - s.scroll as i32 + gg.cell / 2,
            )
        };
        let a = s.handle(StartInput::Click(cell(10, &s)), &f.ctx());
        assert_eq!(a, StartAction::Launch(f.apps[10].clone()));
        // The gap between cells is not a cell.
        let gap = (gg.x0 + gg.cell + 2, gg.top + 2);
        assert_eq!(s.hit(gap, &f.ctx()), None);
        // One notch scrolls one row; the wheel clamps at both ends.
        s.handle(StartInput::Wheel(-120), &f.ctx());
        assert_eq!(s.scroll, gg.step as f32);
        let a = s.handle(StartInput::Click(cell(40, &s)), &f.ctx());
        assert_eq!(a, StartAction::Launch(f.apps[40].clone()));
        s.handle(StartInput::Wheel(120 * 50), &f.ctx());
        assert_eq!(s.scroll, 0.0);
        s.handle(StartInput::Wheel(-120 * 500), &f.ctx());
        assert!(s.scroll > 0.0 && s.scroll == f.ctx().max_scroll(s.mode));
        // Esc clears the query first, then closes.
        s.handle(StartInput::Char('a'), &f.ctx());
        assert_eq!(key(&mut s, &f, VK_ESCAPE), StartAction::Redraw);
        assert_eq!(s.mode, StartMode::AllApps);
        assert_eq!(key(&mut s, &f, VK_ESCAPE), StartAction::Close);
    }

    #[test]
    fn grid_tint_is_the_materials_with_alpha_012() {
        let mut m = vitro_core::style::material_style(
            vitro_core::settings::Material::Frosted,
            vitro_core::style::Theme::Dark,
            true,
        );
        assert!(m.backdrop_blur);
        let g = grid_style(&m);
        assert_eq!(g.tint.3, 31);
        assert_eq!(
            (g.tint.0, g.tint.1, g.tint.2),
            (m.tint.0, m.tint.1, m.tint.2)
        );
        m.backdrop_blur = false;
        assert_eq!(grid_style(&m), m);
    }
}
