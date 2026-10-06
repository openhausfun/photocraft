//! Native-menu behaviour for the menu bar's menus (#138): a menu or submenu taller than the
//! window scrolls (mouse wheel / trackpad, and the ▲ / ▼ arrows at its top and bottom that scroll
//! while the pointer rests on them, like macOS and Windows menus on a small display), and the
//! keyboard drives an open menu: ↑ / ↓ move the highlight (and keep it in view), → opens a
//! submenu or the next menu, ← closes the submenu or opens the previous menu, ↩ / Space run the
//! highlighted item, Esc closes.
//!
//! The highlight is view state only (egui memory), per top-level menu; it follows the pointer too,
//! so the mouse and the keyboard share one highlighted row like a native menu.

use egui::containers::menu::{MenuState, SubMenu, find_menu_root};
use egui::{Id, Key, Modifiers, Rect, Sense, Ui, pos2, vec2};

/// Height of a scroll-arrow strip.
pub const ARROW: f32 = 16.0;
/// Gap kept between a menu and the window edge.
const EDGE: f32 = 6.0;
/// How fast resting on a scroll arrow scrolls, in points per second.
const SPEED: f32 = 600.0;

/// One actionable row of a menu level: a command or a submenu.
#[derive(Clone, Debug)]
pub struct Row {
    /// The row's widget id (a submenu's popup id derives from it).
    pub widget: Id,
    pub enabled: bool,
    /// The command it runs; `None` for a submenu.
    pub command: Option<String>,
    /// Where it was drawn (scrolled out of view when outside its level's `views` rect).
    pub rect: Rect,
}

/// Keyboard / highlight state of the open menu.
#[derive(Clone, Debug, Default)]
pub struct Nav {
    /// The open top-level menu (index into the menu bar).
    top: Option<usize>,
    /// Popup ids of the top-level menus, from the last frame.
    tops: Vec<Id>,
    /// The level the arrow keys move in (0 = the top-level menu).
    pub(crate) level: usize,
    /// Highlighted row per level.
    hi: Vec<Option<usize>>,
    /// This frame's rows per level (deeper levels are dropped when their parent redraws).
    pub(crate) rows: Vec<Vec<Row>>,
    /// Menu-root id per level (the owner of egui's `MenuState` for that level).
    roots: Vec<Id>,
    /// Bottom of the menu bar: where top-level menus hang from.
    pub bar_bottom: Option<f32>,
    /// The visible (scrolled) part of each level.
    pub(crate) views: Vec<Rect>,
    /// The full height of each level's rows (taller than its view when it scrolls).
    pub(crate) contents: Vec<f32>,
    /// Per level: the row its open submenu hangs from (level 0 has none). From the frame
    /// before, since a submenu is drawn while its row is.
    anchors: Vec<Option<Rect>>,
    /// Scroll the highlighted row of `level` into view when it is next drawn.
    reveal: bool,
    /// Highlight the first enabled row of this level once it has been drawn (a keyboard-opened
    /// submenu).
    first_at: Option<usize>,
}

fn nav_id() -> Id {
    Id::new("pc-menu-nav")
}

fn open_id() -> Id {
    Id::new("pc-menu-open")
}

/// Is one of the menu bar's menus open? (Shortcuts leave the keys to it meanwhile.)
pub fn is_open(ctx: &egui::Context) -> bool {
    // Only while the menu bar is still drawn (Full Screen Mode hides it with a menu open).
    let pass = ctx.cumulative_pass_nr();
    ctx.data(|d| d.get_temp::<u64>(open_id())).is_some_and(|seen| seen + 2 >= pass)
}

impl Nav {
    /// Last frame's state, reset when another top-level menu (or none) is open now.
    pub fn load(ctx: &egui::Context) -> Nav {
        let mut nav: Nav = ctx.data(|d| d.get_temp(nav_id())).unwrap_or_default();
        let top = nav.tops.iter().position(|id| egui::Popup::is_id_open(ctx, *id));
        if top != nav.top {
            nav = Nav { top, tops: std::mem::take(&mut nav.tops), ..Default::default() };
        }
        nav
    }

    /// The state as the last frame left it (tests, inspection).
    pub fn current(ctx: &egui::Context) -> Nav {
        ctx.data(|d| d.get_temp(nav_id())).unwrap_or_default()
    }

    /// The highlighted row of `level`, if any.
    pub fn highlighted(&self, level: usize) -> Option<usize> {
        self.hi.get(level).copied().flatten()
    }

    fn set_hi(&mut self, level: usize, row: Option<usize>) {
        if self.hi.len() <= level {
            self.hi.resize(level + 1, None);
        }
        self.hi[level] = row;
    }

    /// Start drawing `level`: forget its rows from before, and every deeper level's.
    fn begin_level(&mut self, level: usize, root: Id) {
        self.rows.truncate(level);
        self.rows.resize(level + 1, Vec::new());
        self.roots.truncate(level);
        self.roots.resize(level + 1, root);
        self.views.truncate(level);
        self.views.resize(level + 1, Rect::NOTHING);
        self.contents.truncate(level);
        self.contents.resize(level + 1, 0.0);
    }

    /// Draw a row of `level` with `add`, highlighted like a hovered row when the keyboard (or the
    /// pointer) is on it, and register it for the keys. `add` returns the row's response.
    pub fn row<R>(&mut self, ui: &mut Ui, level: usize, enabled: bool, command: Option<&str>, add: impl FnOnce(&mut Ui, &mut Nav) -> (egui::Response, R)) -> R {
        let i = self.rows.get(level).map_or(usize::MAX, Vec::len);
        let lit = enabled && self.highlighted(level) == Some(i);
        let saved = ui.visuals().widgets.inactive;
        if lit {
            ui.visuals_mut().widgets.inactive = ui.visuals().widgets.hovered;
        }
        let (r, out) = add(ui, self);
        ui.visuals_mut().widgets.inactive = saved;
        if let Some(rows) = self.rows.get_mut(level) {
            rows.push(Row { widget: r.id, enabled, command: command.map(str::to_string), rect: r.rect });
            self.after_row(level, i, lit, &r);
        }
        out
    }

    /// After a row is drawn: the pointer moving onto it highlights it; a keyboard move scrolls
    /// the highlighted row into view.
    fn after_row(&mut self, level: usize, i: usize, lit: bool, r: &egui::Response) {
        let moved = r.ctx.input(|inp| inp.pointer.delta() != egui::Vec2::ZERO);
        if moved && r.hovered() && !lit {
            self.set_hi(level, Some(i));
            self.level = level;
            r.ctx.request_repaint();
        }
        if lit && self.reveal && level == self.level {
            r.scroll_to_me(None);
            self.reveal = false;
        }
    }

    fn enabled(&self, level: usize) -> Vec<usize> {
        self.rows.get(level).map(|rows| rows.iter().enumerate().filter(|(_, r)| r.enabled).map(|(i, _)| i).collect()).unwrap_or_default()
    }

    /// ↑ / ↓: the next enabled row (wrapping), or the first / last one.
    fn step(&mut self, level: usize, down: bool) {
        let on = self.enabled(level);
        let (Some(&first), Some(&last)) = (on.first(), on.last()) else { return };
        let next = match self.highlighted(level) {
            None => {
                if down {
                    first
                } else {
                    last
                }
            }
            Some(cur) if down => on.iter().copied().find(|&i| i > cur).unwrap_or(first),
            Some(cur) => on.iter().rev().copied().find(|&i| i < cur).unwrap_or(last),
        };
        self.set_hi(level, Some(next));
        self.hi.truncate(level + 1);
        self.level = level;
        self.reveal = true;
    }

    /// Open the highlighted submenu of `level` and move the keyboard into it.
    fn open_sub(&mut self, ctx: &egui::Context, level: usize, widget: Id) {
        let Some(&root) = self.roots.get(level) else { return };
        let sub = SubMenu::id_from_widget_id(widget);
        // egui drops an open item it has never drawn: mark it shown so it survives to the next frame.
        MenuState::mark_shown(ctx, sub);
        MenuState::from_id(ctx, root, |s| s.open_item = Some(sub));
        self.level = level + 1;
        self.hi.truncate(level + 1);
        self.first_at = Some(level + 1);
    }

    /// Remember the row the submenu of `level` (1 = a top-level menu's submenu) hangs from.
    pub fn set_anchor(&mut self, level: usize, row: Rect) {
        if self.anchors.len() <= level {
            self.anchors.resize(level + 1, None);
        }
        if let Some(a) = self.anchors.get_mut(level) {
            *a = Some(row);
        }
    }

    /// Handle this frame's navigation keys (after the menus were drawn). `tops` are the menu
    /// bar's popup ids; an activated command lands in `clicked`.
    pub fn keys(&mut self, ctx: &egui::Context, tops: &[Id], clicked: &mut Option<String>) {
        self.tops = tops.to_vec();
        let Some(top) = self.top.filter(|_| !tops.is_empty()) else { return };
        let level = self.level.min(self.rows.len().saturating_sub(1));
        self.level = level;
        let press = |k: Key| ctx.input_mut(|i| i.consume_key(Modifiers::NONE, k));
        let current = self.highlighted(level).and_then(|i| self.rows.get(level)?.get(i)).cloned();
        let switch = |by: usize| {
            let n = tops.len();
            if let Some(&id) = tops.get((top + by) % n) {
                egui::Popup::open_id(ctx, id);
            }
        };
        if press(Key::Escape) {
            egui::Popup::close_all(ctx);
        } else if press(Key::ArrowDown) {
            self.step(level, true);
        } else if press(Key::ArrowUp) {
            self.step(level, false);
        } else if press(Key::ArrowRight) {
            match current {
                Some(Row { widget, enabled: true, command: None, .. }) => self.open_sub(ctx, level, widget),
                _ => switch(1),
            }
        } else if press(Key::ArrowLeft) {
            if level > 0 {
                if let Some(&root) = self.roots.get(level - 1) {
                    MenuState::from_id(ctx, root, |s| s.open_item = None);
                }
                self.hi.truncate(level);
                self.level = level - 1;
            } else {
                switch(tops.len() - 1);
            }
        } else if press(Key::Enter) || press(Key::Space) {
            match current {
                Some(Row { enabled: true, command: Some(c), .. }) => {
                    *clicked = Some(c);
                    egui::Popup::close_all(ctx);
                }
                Some(Row { widget, enabled: true, command: None, .. }) => self.open_sub(ctx, level, widget),
                _ => {}
            }
        } else {
            return;
        }
        ctx.request_repaint();
    }

    pub fn store(self, ctx: &egui::Context) {
        let pass = ctx.cumulative_pass_nr();
        ctx.data_mut(|d| {
            if self.top.is_some() {
                d.insert_temp(open_id(), pass);
            } else {
                d.remove::<u64>(open_id());
            }
            d.insert_temp(nav_id(), self);
        });
    }
}

/// Is the pointer over `rect` on this ui's layer (not under a submenu drawn on top)?
fn pointer_on(ui: &Ui, rect: Rect) -> bool {
    ui.ctx().pointer_hover_pos().is_some_and(|p| rect.contains(p) && ui.ctx().layer_id_at(p) == Some(ui.layer_id()))
}

/// The height a menu level's rows may take. Every level stays below the menu bar (#319): a
/// top-level menu hangs from it, and a submenu that is too tall for the window between its row
/// and the bottom, or (opening upward, as egui does when that fits) between the bar and its row,
/// scrolls instead of egui sliding it up over the menu bar, where it hid the menu titles.
/// `anchor` is the submenu's row (unknown on its first frame); `frame` the popup's margins.
pub fn level_room(screen: Rect, bar_bottom: Option<f32>, depth: usize, anchor: Option<Rect>, frame: f32) -> f32 {
    level_room_in(screen, screen, bar_bottom, depth, anchor, None, frame)
}

/// [`level_room`] when only `visible`, a part of the window's content rect `screen`, can be seen
/// (#315: a window taller than its display runs under the taskbar). egui still places popups
/// within the whole window, so a submenu opens upward only when downward doesn't fit the window.
/// `rows` is the level's rows' height when known (the popup shrinks to it).
pub fn level_room_in(screen: Rect, visible: Rect, bar_bottom: Option<f32>, depth: usize, anchor: Option<Rect>, rows: Option<f32>, frame: f32) -> f32 {
    let top = bar_bottom.unwrap_or(visible.top()).max(visible.top()) + EDGE;
    let bottom = visible.bottom().min(screen.bottom()) - EDGE;
    let below_bar = bottom - top;
    let room = match anchor.filter(|_| depth > 1) {
        Some(row) => {
            // Downward from the row, or upward from it, whichever leaves more room. egui opens a
            // submenu upward only when the downward one doesn't fit the window (it hangs from the
            // row's top less half the frame margin).
            let (down, up) = (bottom - row.top(), row.bottom() - top);
            let opens_up = |h: f32| row.top() - (frame - 2.0) / 2.0 + h + frame > screen.bottom();
            let want = rows.unwrap_or(f32::INFINITY);
            if want > down && up > down && opens_up(want.min(up)) { up } else { down }.min(below_bar)
        }
        None => below_bar,
    } - frame;
    if room.is_finite() { room.max(4.0 * ARROW) } else { 4.0 * ARROW }
}

/// Draw one level of a menu, bounded by the window: when its rows don't fit they scroll, with
/// scroll arrows at the top and bottom. `depth` is 1 for a top-level menu.
pub fn level(ui: &mut Ui, depth: usize, nav: &mut Nav, rows: impl FnOnce(&mut Ui, &mut Nav)) {
    let ctx = ui.ctx().clone();
    let level = depth.saturating_sub(1);
    nav.begin_level(level, find_menu_root(ui).id);
    let screen = ctx.content_rect();
    // Only the part of the window on its monitor, clear of the taskbar, can be seen (#315).
    let visible = crate::work_area::visible_rect(&ctx);
    // The menu frame's margin and stroke around the rows.
    let frame = ui.spacing().menu_margin.sum().y + 2.0;
    let anchor = nav.anchors.get(level).copied().flatten();
    let key = ui.id().with(("pc-menu-level", depth));
    // Rows' height from the last frame: does this level overflow?
    let content: Option<f32> = ctx.data(|d| d.get_temp(key));
    let room = level_room_in(screen, visible, nav.bar_bottom, depth, anchor, content, frame);
    let over = content.is_some_and(|h| h > room + 0.5);
    let up = over.then(|| ui.allocate_exact_size(vec2(0.0, ARROW), Sense::hover()).0);
    let height = if over { room - 2.0 * ARROW } else { room };
    // Wheel, scroll bar and dragging the rows all scroll (#160).
    // The popup's `Ui` is only as tall as the popup was last frame (egui's `default_area_size`, 400
    // pt, on the first): a scroll area never grows past that, so every menu stuck at that height
    // and scrolled even on a tall window (#235). Ask for the room the window has; auto-shrink then
    // fits the area to its rows, so a menu scrolls only when they don't fit.
    let out = egui::ScrollArea::vertical()
        .id_salt(("menu-level", depth))
        .max_height(height)
        .min_scrolled_height(height)
        .scroll_source(egui::containers::scroll_area::ScrollSource::ALL)
        .show(ui, |ui| rows(ui, nav));
    let down = over.then(|| ui.allocate_exact_size(vec2(0.0, ARROW), Sense::hover()).0);
    ctx.data_mut(|d| d.insert_temp(key, out.content_size.y));
    if let Some(v) = nav.views.get_mut(level) {
        // `inner_rect` is the size asked for, before auto-shrink fits it to the rows.
        *v = Rect::from_min_size(out.inner_rect.min, vec2(out.inner_rect.width(), out.inner_rect.height().min(out.content_size.y)));
    }
    if let Some(c) = nav.contents.get_mut(level) {
        *c = out.content_size.y;
    }
    if nav.first_at == Some(level) {
        nav.first_at = None;
        nav.step(level, true);
        ctx.request_repaint();
    }
    let (Some(up), Some(down)) = (up, down) else { return };
    let t = crate::theme::Tokens::get(&ctx);
    let x = ui.min_rect().x_range();
    let max = (out.content_size.y - out.inner_rect.height()).max(0.0);
    let offset = out.state.offset.y;
    let dt = ctx.input(|i| i.stable_dt).clamp(0.0, 0.1);
    let mut target = offset;
    for (strip, dir, live) in [(up, -1.0, offset > 0.5), (down, 1.0, offset < max - 0.5)] {
        let strip = Rect::from_x_y_ranges(x, strip.y_range());
        let c = strip.center();
        let (h, w) = (4.5, 6.0);
        let tri = if dir < 0.0 {
            vec![pos2(c.x - w, c.y + h * 0.5), pos2(c.x + w, c.y + h * 0.5), pos2(c.x, c.y - h * 0.5)]
        } else {
            vec![pos2(c.x - w, c.y - h * 0.5), pos2(c.x + w, c.y - h * 0.5), pos2(c.x, c.y + h * 0.5)]
        };
        ui.painter().add(egui::Shape::convex_polygon(tri, if live { t.text } else { t.text_faint.gamma_multiply(0.5) }, egui::Stroke::NONE));
        let id = key.with(dir < 0.0);
        let r = ui.interact(strip, id, Sense::hover());
        r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, live, if dir < 0.0 { "Scroll menu up" } else { "Scroll menu down" }));
        if live && pointer_on(ui, strip) {
            target = (offset + dir * SPEED * dt.max(1.0 / 120.0)).clamp(0.0, max);
            ctx.request_repaint();
        }
        // The wheel scrolls over the arrows too.
        if pointer_on(ui, strip) {
            let wheel = ctx.input(|i| i.smooth_scroll_delta.y);
            if wheel != 0.0 {
                target = (target - wheel).clamp(0.0, max);
            }
        }
    }
    if target != offset {
        let mut st = out.state;
        st.offset.y = target;
        st.store(&ctx, out.id);
    }
}

#[cfg(test)]
#[path = "menu_nav_tests.rs"]
mod tests;
