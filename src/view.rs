//! A single open document: layout, scrolling, zooming, text selection,
//! links, search highlights and the sidebar (outline + thumbnails).

use crate::worker::{DocId, Job, Link, LinkTarget, NRect, OutlineItem, TextChar};
use eframe::egui::{self, *};
use std::path::PathBuf;
use std::time::SystemTime;

/// Vertical gap between pages, in UI points.
const GAP: f32 = 10.0;
const MARGIN: f32 = 12.0;
/// UI points per PDF point at 100% zoom (96 DPI screen vs 72 DPI PDF).
const PT: f32 = 96.0 / 72.0;
const SCROLLBAR_W: f32 = 10.0;
const THUMB_W: f32 = 120.0;
pub const MIN_ZOOM: f32 = 0.1;
pub const MAX_ZOOM: f32 = 32.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fit {
    None,
    Width,
    Page,
}

impl Fit {
    pub fn to_u8(self) -> u8 {
        match self {
            Fit::None => 0,
            Fit::Width => 1,
            Fit::Page => 2,
        }
    }
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Fit::Width,
            2 => Fit::Page,
            _ => Fit::None,
        }
    }
}

pub enum LoadState {
    Loading,
    Ready,
    Failed(String),
    NeedsPassword { wrong: bool },
}

pub struct PageTex {
    pub tex: TextureHandle,
    pub scale: f32,
    pub rot: u8,
    pub invert: bool,
    /// Set after a reload: still drawn, but re-requested.
    pub stale: bool,
}

#[derive(Default)]
pub struct PageSlot {
    pub tex: Option<PageTex>,
    pub thumb: Option<PageTex>,
    pub chars: Option<Vec<TextChar>>,
    pub links: Vec<Link>,
}

#[derive(Clone, Copy, PartialEq)]
pub struct Selection {
    pub a: (usize, usize),
    pub b: (usize, usize),
}

impl Selection {
    fn ordered(&self) -> ((usize, usize), (usize, usize)) {
        if self.a <= self.b { (self.a, self.b) } else { (self.b, self.a) }
    }
}

#[derive(PartialEq)]
enum Drag {
    None,
    Pan,
    Select,
}

#[derive(Default)]
pub struct SearchState {
    pub open: bool,
    pub query: String,
    pub searched: String,
    pub generation: u64,
    pub hits: Vec<(usize, Vec<NRect>)>,
    pub current: Option<usize>,
    pub done: bool,
    pub progress: usize,
    pub start_page: usize,
    pub focus: bool,
}

/// Per-frame inputs from the app.
pub struct ViewCtx {
    pub invert: bool,
    pub dark: bool,
    pub max_tex: usize,
    pub ppp: f32,
}

pub struct Doc {
    pub id: DocId,
    pub path: PathBuf,
    pub name: String,
    pub title: Option<String>,
    pub state: LoadState,
    pub sizes: Vec<Vec2>,
    pub outline: Vec<OutlineItem>,
    pub slots: Vec<PageSlot>,
    pub zoom: f32,
    pub fit: Fit,
    pub rotation: u8,
    pub scroll: Vec2,
    tops: Vec<f32>,
    disp: Vec<Vec2>,
    content: Vec2,
    layout_dirty: bool,
    layout_key: (f32, u8),
    pub pending_goto: Option<(usize, f32)>,
    pub sel: Option<Selection>,
    drag: Drag,
    pub search: SearchState,
    pub mtime: Option<SystemTime>,
    /// Old worker id while a reload is in flight.
    pub reload_from: Option<DocId>,
    /// The file changed on disk and the user has not decided yet.
    pub disk_changed: bool,
    /// Reload without asking (chosen by the user for this document).
    pub auto_reload: bool,
    pub password: String,
    view: Vec2,
    pub thumbs_follow: bool,
}

impl Doc {
    pub fn new(id: DocId, path: PathBuf) -> Self {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        Doc {
            id,
            path,
            name,
            title: None,
            state: LoadState::Loading,
            sizes: Vec::new(),
            outline: Vec::new(),
            slots: Vec::new(),
            zoom: 1.0,
            fit: Fit::Width,
            rotation: 0,
            scroll: Vec2::ZERO,
            tops: Vec::new(),
            disp: Vec::new(),
            content: Vec2::ZERO,
            layout_dirty: true,
            layout_key: (0.0, 0),
            pending_goto: None,
            sel: None,
            drag: Drag::None,
            search: SearchState::default(),
            mtime,
            reload_from: None,
            disk_changed: false,
            auto_reload: false,
            password: String::new(),
            view: vec2(800.0, 600.0),
            thumbs_follow: true,
        }
    }

    pub fn page_count(&self) -> usize {
        self.sizes.len()
    }

    pub fn is_ready(&self) -> bool {
        !self.sizes.is_empty()
    }

    /// Called when the worker has (re)opened the document.
    pub fn on_opened(&mut self, sizes: Vec<(f32, f32)>, outline: Vec<OutlineItem>, title: Option<String>) {
        let reload = !self.sizes.is_empty();
        self.sizes = sizes.into_iter().map(|(w, h)| vec2(w.max(1.0), h.max(1.0))).collect();
        self.outline = outline;
        self.title = title;
        self.slots.resize_with(self.sizes.len(), PageSlot::default);
        for s in &mut self.slots {
            if let Some(t) = &mut s.tex {
                t.stale = true;
            }
            if let Some(t) = &mut s.thumb {
                t.stale = true;
            }
            s.chars = None;
            s.links.clear();
        }
        if reload {
            self.sel = None;
            if !self.search.searched.is_empty() {
                // Hits may have moved; the app restarts the search.
                self.search.searched.clear();
            }
        }
        self.state = LoadState::Ready;
        self.layout_dirty = true;
    }

    fn page_disp_pts(&self, i: usize) -> Vec2 {
        let s = self.sizes[i];
        if self.rotation % 2 == 1 { vec2(s.y, s.x) } else { s }
    }

    fn ensure_layout(&mut self) {
        let key = (self.zoom, self.rotation);
        if !self.layout_dirty && key == self.layout_key && self.tops.len() == self.sizes.len() {
            return;
        }
        self.layout_dirty = false;
        self.layout_key = key;
        let s = self.zoom * PT;
        self.tops.clear();
        self.disp.clear();
        let mut y = MARGIN;
        let mut maxw: f32 = 0.0;
        for i in 0..self.sizes.len() {
            let d = self.page_disp_pts(i) * s;
            self.tops.push(y);
            self.disp.push(d);
            y += d.y + GAP;
            maxw = maxw.max(d.x);
        }
        self.content = vec2(maxw + 2.0 * MARGIN, y - GAP + MARGIN);
    }

    fn page_at(&self, y: f32) -> usize {
        self.tops.partition_point(|&t| t <= y).saturating_sub(1).min(self.tops.len().saturating_sub(1))
    }

    pub fn current_page(&self) -> usize {
        if self.tops.is_empty() {
            return 0;
        }
        self.page_at(self.scroll.y + self.view.y * 0.4)
    }

    /// Left edge of page `i` in content coordinates.
    fn page_left(&self, i: usize) -> f32 {
        (self.content.x.max(self.view.x) - self.disp[i].x) / 2.0
    }

    fn max_scroll(&self) -> Vec2 {
        vec2((self.content.x - self.view.x).max(0.0), (self.content.y - self.view.y).max(0.0))
    }

    fn clamp_scroll(&mut self) {
        let m = self.max_scroll();
        self.scroll = self.scroll.clamp(Vec2::ZERO, m);
    }

    pub fn goto_page(&mut self, i: usize) {
        if self.tops.is_empty() {
            self.pending_goto = Some((i, 0.0));
            return;
        }
        let i = i.min(self.tops.len() - 1);
        self.scroll.y = self.tops[i] - MARGIN;
        self.clamp_scroll();
    }

    /// Position to persist: (page, fraction into the page).
    pub fn position(&self) -> (usize, f32) {
        if self.tops.is_empty() {
            return self.pending_goto.unwrap_or((0, 0.0));
        }
        let y = self.scroll.y + MARGIN;
        let p = self.page_at(y);
        (p, ((y - self.tops[p]) / self.disp[p].y).clamp(0.0, 1.0))
    }

    pub fn scroll_by(&mut self, d: Vec2) {
        self.scroll += d;
        self.clamp_scroll();
    }

    pub fn page_step(&self) -> f32 {
        (self.view.y - 40.0).max(40.0)
    }

    pub fn scroll_to_end(&mut self, end: bool) {
        self.scroll.y = if end { f32::MAX } else { 0.0 };
        self.clamp_scroll();
    }

    /// Change zoom keeping the document point under `anchor` (viewport-relative) fixed.
    pub fn set_zoom(&mut self, z: f32, anchor: Option<Vec2>) {
        let z = z.clamp(MIN_ZOOM, MAX_ZOOM);
        if self.tops.is_empty() {
            self.zoom = z;
            return;
        }
        let anchor = anchor.unwrap_or(self.view / 2.0);
        let dy = self.scroll.y + anchor.y;
        let p = self.page_at(dy);
        let fy = (dy - self.tops[p]) / self.disp[p].y;
        let fx = (self.scroll.x + anchor.x - self.page_left(p)) / self.disp[p].x;
        self.zoom = z;
        self.ensure_layout();
        self.scroll.y = self.tops[p] + fy * self.disp[p].y - anchor.y;
        self.scroll.x = self.page_left(p) + fx * self.disp[p].x - anchor.x;
        self.clamp_scroll();
    }

    pub fn zoom_step(&mut self, up: bool) {
        const STEPS: [f32; 17] =
            [0.1, 0.25, 0.33, 0.5, 0.67, 0.75, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 3.0, 4.0, 8.0, 16.0];
        let z = self.zoom;
        let next = if up {
            STEPS.iter().copied().find(|&s| s > z * 1.01).unwrap_or(MAX_ZOOM)
        } else {
            STEPS.iter().rev().copied().find(|&s| s < z * 0.99).unwrap_or(MIN_ZOOM)
        };
        self.fit = Fit::None;
        self.set_zoom(next, None);
    }

    fn fit_zoom(&self) -> Option<f32> {
        if self.sizes.is_empty() {
            return None;
        }
        let avail = vec2(self.view.x - 2.0 * MARGIN - SCROLLBAR_W, self.view.y - 2.0 * MARGIN).max(vec2(50.0, 50.0));
        match self.fit {
            Fit::None => None,
            Fit::Width => {
                let maxw = (0..self.sizes.len()).map(|i| self.page_disp_pts(i).x).fold(0.0, f32::max);
                Some(avail.x / (maxw * PT))
            }
            Fit::Page => {
                let d = self.page_disp_pts(self.current_page());
                Some((avail.x / (d.x * PT)).min(avail.y / (d.y * PT)))
            }
        }
    }

    pub fn set_fit(&mut self, fit: Fit) {
        let page = self.current_page();
        self.fit = fit;
        if let Some(z) = self.fit_zoom() {
            self.set_zoom(z, Some(vec2(self.view.x / 2.0, 0.0)));
        }
        if fit == Fit::Page && !self.tops.is_empty() {
            let p = page.min(self.tops.len() - 1);
            self.scroll.y = self.tops[p] - ((self.view.y - self.disp[p].y) / 2.0).max(0.0);
            self.clamp_scroll();
        }
    }

    pub fn rotate(&mut self, cw: bool) {
        let p = self.current_page();
        self.rotation = (self.rotation + if cw { 1 } else { 3 }) % 4;
        self.layout_dirty = true;
        if let Some(z) = self.fit_zoom() {
            self.zoom = z.clamp(MIN_ZOOM, MAX_ZOOM);
        }
        self.ensure_layout();
        self.goto_page(p);
    }

    // ----- coordinate helpers -----

    fn page_rect(&self, i: usize, view: Rect) -> Rect {
        Rect::from_min_size(
            view.min + vec2(self.page_left(i) - self.scroll.x, self.tops[i] - self.scroll.y),
            self.disp[i],
        )
    }

    /// Page-normalized rect → displayed (rotated) normalized rect.
    fn to_disp(&self, r: &NRect) -> Rect {
        let f = |x: f32, y: f32| -> Pos2 {
            match self.rotation {
                1 => pos2(1.0 - y, x),
                2 => pos2(1.0 - x, 1.0 - y),
                3 => pos2(y, 1.0 - x),
                _ => pos2(x, y),
            }
        };
        Rect::from_two_pos(f(r.x0, r.y0), f(r.x1, r.y1))
    }

    fn from_disp(&self, p: Pos2) -> Pos2 {
        match self.rotation {
            1 => pos2(p.y, 1.0 - p.x),
            2 => pos2(1.0 - p.x, 1.0 - p.y),
            3 => pos2(1.0 - p.y, p.x),
            _ => p,
        }
    }

    fn screen_rect(&self, page_rect: Rect, r: &NRect) -> Rect {
        let d = self.to_disp(r);
        Rect::from_min_max(
            page_rect.min + d.min.to_vec2() * page_rect.size(),
            page_rect.min + d.max.to_vec2() * page_rect.size(),
        )
    }

    /// Screen position → (page, page-normalized point). Clamps to the nearest page.
    fn hit_page(&self, pos: Pos2, view: Rect) -> (usize, Pos2, bool) {
        let p = self.page_at(pos.y - view.min.y + self.scroll.y);
        // Prefer the next page if we're in the gap below this one.
        let mut best = p;
        if p + 1 < self.tops.len() {
            let r0 = self.page_rect(p, view);
            let r1 = self.page_rect(p + 1, view);
            if (pos.y - r1.top()).abs() < (pos.y - r0.bottom()).abs() && pos.y > r0.bottom() {
                best = p + 1;
            }
        }
        let r = self.page_rect(best, view);
        let inside = r.contains(pos);
        let n = ((pos - r.min) / r.size()).to_pos2();
        (best, self.from_disp(n), inside)
    }

    fn char_at(&self, page: usize, n: Pos2, exact: bool) -> Option<usize> {
        let chars = self.slots.get(page)?.chars.as_ref()?;
        let tol = 0.002;
        let mut best = None;
        let mut best_d = f32::MAX;
        for (i, c) in chars.iter().enumerate() {
            let Some(r) = c.rect else { continue };
            let dx = (r.x0 - n.x).max(n.x - r.x1).max(0.0);
            let dy = (r.y0 - n.y).max(n.y - r.y1).max(0.0);
            if exact {
                if dx <= tol && dy <= tol {
                    return Some(i);
                }
            } else {
                let d = dx + dy * 4.0;
                if d < best_d {
                    best_d = d;
                    best = Some(i);
                }
            }
        }
        best
    }

    fn link_at(&self, page: usize, n: Pos2) -> Option<&Link> {
        self.slots
            .get(page)?
            .links
            .iter()
            .find(|l| n.x >= l.rect.x0 && n.x <= l.rect.x1 && n.y >= l.rect.y0 && n.y <= l.rect.y1)
    }

    pub fn selected_text(&self) -> Option<String> {
        let (a, b) = self.sel?.ordered();
        let mut out = String::new();
        for p in a.0..=b.0 {
            let Some(chars) = self.slots.get(p).and_then(|s| s.chars.as_ref()) else { continue };
            if chars.is_empty() {
                continue;
            }
            let s = if p == a.0 { a.1 } else { 0 };
            let e = if p == b.0 { b.1 } else { chars.len() - 1 };
            for c in &chars[s.min(chars.len() - 1)..=e.min(chars.len() - 1)] {
                if c.ch != '\r' && c.ch != '\0' {
                    out.push(c.ch);
                }
            }
            if p != b.0 && !out.ends_with('\n') {
                out.push('\n');
            }
        }
        Some(out).filter(|s| !s.is_empty())
    }

    fn select_word(&mut self, page: usize, ci: usize) {
        let Some(chars) = self.slots.get(page).and_then(|s| s.chars.as_ref()) else { return };
        let is_word = |i: usize| chars[i].ch.is_alphanumeric() || chars[i].ch == '_';
        if !is_word(ci) {
            self.sel = Some(Selection { a: (page, ci), b: (page, ci) });
            return;
        }
        let mut s = ci;
        while s > 0 && is_word(s - 1) {
            s -= 1;
        }
        let mut e = ci;
        while e + 1 < chars.len() && is_word(e + 1) {
            e += 1;
        }
        self.sel = Some(Selection { a: (page, s), b: (page, e) });
    }

    /// Scroll so that the current search hit is visible.
    pub fn reveal_hit(&mut self) {
        let Some(i) = self.search.current else { return };
        let Some((p, rects)) = self.search.hits.get(i) else { return };
        let (p, r) = (*p, rects[0]);
        if self.tops.is_empty() {
            self.pending_goto = Some((p, 0.0));
            return;
        }
        let d = self.to_disp(&r);
        let y = self.tops[p] + d.min.y * self.disp[p].y;
        let x = self.page_left(p) + d.min.x * self.disp[p].x;
        if y < self.scroll.y + 40.0 || y > self.scroll.y + self.view.y - 60.0 {
            self.scroll.y = y - self.view.y / 3.0;
        }
        if x < self.scroll.x + 20.0 || x > self.scroll.x + self.view.x - 60.0 {
            self.scroll.x = x - self.view.x / 3.0;
        }
        self.clamp_scroll();
    }

    pub fn reset_search_results(&mut self) {
        self.search.hits.clear();
        self.search.current = None;
        self.search.done = false;
        self.search.progress = 0;
    }

    pub fn search_step(&mut self, forward: bool) {
        let n = self.search.hits.len();
        if n == 0 {
            return;
        }
        self.search.current = Some(match self.search.current {
            None => 0,
            Some(c) if forward => (c + 1) % n,
            Some(c) => (c + n - 1) % n,
        });
        self.reveal_hit();
    }

    /// Called as hits stream in; picks the first hit at or after where the search started.
    pub fn add_hits(&mut self, page: usize, hits: Vec<Vec<NRect>>) {
        for h in hits {
            self.search.hits.push((page, h));
        }
        if self.search.current.is_none() && page >= self.search.start_page {
            self.search.current = self.search.hits.iter().position(|(p, _)| *p >= self.search.start_page);
            self.reveal_hit();
        }
    }

    pub fn finish_search(&mut self) {
        self.search.done = true;
        if self.search.current.is_none() && !self.search.hits.is_empty() {
            self.search.current = Some(0);
            self.reveal_hit();
        }
    }

    // ----- main view -----

    pub fn show(&mut self, ui: &mut Ui, cx: &ViewCtx, jobs: &mut Vec<Job>) {
        let full = ui.available_rect_before_wrap();
        let resp = ui.allocate_rect(full, Sense::click_and_drag());
        let view = Rect::from_min_max(full.min, pos2(full.max.x - SCROLLBAR_W, full.max.y));
        self.view = view.size();
        let painter = ui.painter_at(full);
        let bg = if cx.dark { Color32::from_gray(38) } else { Color32::from_gray(222) };
        painter.rect_filled(full, 0.0, bg);

        if self.sizes.is_empty() {
            return;
        }

        if let Some(z) = self.fit_zoom() {
            if (z - self.zoom).abs() > 0.0005 {
                self.set_zoom(z, Some(vec2(self.view.x / 2.0, 0.0)));
            }
        }
        self.ensure_layout();
        if let Some((p, f)) = self.pending_goto.take() {
            let p = p.min(self.tops.len() - 1);
            self.scroll.y = self.tops[p] + f * self.disp[p].y - MARGIN;
        }

        // --- input ---
        let pointer = ui.input(|i| i.pointer.hover_pos());
        if resp.hovered() || resp.contains_pointer() {
            let (sd, zd) = ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta()));
            if zd != 1.0 {
                self.fit = Fit::None;
                self.set_zoom(self.zoom * zd, pointer.map(|p| p - view.min));
            }
            if sd != Vec2::ZERO {
                self.scroll -= sd;
            }
        }

        let primary_down = ui.input(|i| i.pointer.primary_down());
        if resp.drag_started() {
            let start = ui.input(|i| i.pointer.press_origin()).or(pointer);
            let middle = ui.input(|i| i.pointer.middle_down());
            self.drag = Drag::Pan;
            if let (Some(pos), false) = (start, middle) {
                let (p, n, inside) = self.hit_page(pos, view);
                if inside {
                    if let Some(ci) = self.char_at(p, n, true) {
                        let shift = ui.input(|i| i.modifiers.shift);
                        let a = match (shift, self.sel) {
                            (true, Some(s)) => s.a,
                            _ => (p, ci),
                        };
                        self.sel = Some(Selection { a, b: (p, ci) });
                        self.drag = Drag::Select;
                    }
                }
            }
        }
        if resp.dragged() {
            match self.drag {
                Drag::Pan => self.scroll -= resp.drag_delta(),
                Drag::Select => {
                    if let Some(pos) = pointer {
                        let (p, n, _) = self.hit_page(pos, view);
                        if let Some(ci) = self.char_at(p, n, false) {
                            if let Some(s) = &mut self.sel {
                                s.b = (p, ci);
                            }
                        }
                        // Auto-scroll when dragging past the edges.
                        let edge = 30.0;
                        if pos.y < view.top() + edge {
                            self.scroll.y -= (view.top() + edge - pos.y) * 0.5;
                            ui.ctx().request_repaint();
                        } else if pos.y > view.bottom() - edge {
                            self.scroll.y += (pos.y - view.bottom() + edge) * 0.5;
                            ui.ctx().request_repaint();
                        }
                    }
                }
                Drag::None => {}
            }
        }
        if resp.drag_stopped() || (!primary_down && self.drag == Drag::Select) {
            self.drag = Drag::None;
        }

        let mut hover_link = false;
        if let Some(pos) = pointer.filter(|p| view.contains(*p)) {
            let (p, n, inside) = self.hit_page(pos, view);
            if inside && self.drag == Drag::None {
                if let Some(link) = self.link_at(p, n) {
                    hover_link = true;
                    if let LinkTarget::Uri(u) = &link.target {
                        resp.clone().on_hover_text_at_pointer(u.as_str());
                    }
                } else if self.char_at(p, n, true).is_some() {
                    ui.ctx().set_cursor_icon(CursorIcon::Text);
                }
            }
            if resp.clicked() {
                let target = if inside {
                    self.link_at(p, n).map(|l| match &l.target {
                        LinkTarget::Page(i) => LinkTarget::Page(*i),
                        LinkTarget::Uri(u) => LinkTarget::Uri(u.clone()),
                    })
                } else {
                    None
                };
                match target {
                    Some(LinkTarget::Page(i)) => self.goto_page(i),
                    Some(LinkTarget::Uri(u)) => {
                        let lower = u.to_ascii_lowercase();
                        if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:")
                        {
                            let _ = open::that_detached(&u);
                        }
                    }
                    None => self.sel = None,
                }
            }
            if resp.double_clicked() && inside {
                if let Some(ci) = self.char_at(p, n, true) {
                    self.select_word(p, ci);
                }
            }
        }
        if hover_link {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        } else if self.drag == Drag::Pan {
            ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        }

        self.clamp_scroll();

        // --- draw pages ---
        let first = self.page_at(self.scroll.y);
        let last = self.page_at(self.scroll.y + self.view.y);
        let sel = self.sel.map(|s| s.ordered());
        let shadow = if cx.dark { Color32::from_black_alpha(120) } else { Color32::from_black_alpha(40) };
        let placeholder = if cx.invert { Color32::from_gray(30) } else { Color32::WHITE };
        let hl = Color32::from_rgba_unmultiplied(255, 200, 0, 90);
        let hl_cur = Color32::from_rgba_unmultiplied(255, 120, 0, 150);
        let sel_col = Color32::from_rgba_unmultiplied(40, 120, 255, 80);

        for p in first..=last {
            let r = self.page_rect(p, view);
            painter.rect_filled(r.translate(vec2(0.0, 2.0)).expand(1.0), 2.0, shadow);
            match &self.slots[p].tex {
                Some(t) => painter.image(t.tex.id(), r, Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), Color32::WHITE),
                None => painter.rect_filled(r, 0.0, placeholder),
            };

            // Search hits.
            let hits = &self.search.hits;
            let lo = hits.partition_point(|(hp, _)| *hp < p);
            for (k, (hp, rects)) in hits.iter().enumerate().skip(lo) {
                if *hp != p {
                    break;
                }
                let col = if Some(k) == self.search.current { hl_cur } else { hl };
                for nr in rects {
                    painter.rect_filled(self.screen_rect(r, nr).expand(1.0), 1.0, col);
                }
            }

            // Selection, merged into per-line rectangles.
            if let (Some((a, b)), Some(chars)) = (sel, self.slots[p].chars.as_ref()) {
                if p >= a.0 && p <= b.0 && !chars.is_empty() {
                    let s = if p == a.0 { a.1 } else { 0 };
                    let e = if p == b.0 { b.1 } else { chars.len() - 1 };
                    let mut line: Option<NRect> = None;
                    for c in &chars[s.min(chars.len() - 1)..=e.min(chars.len() - 1)] {
                        let Some(cr) = c.rect else { continue };
                        line = match line {
                            Some(l) if same_line(&l, &cr) => Some(NRect {
                                x0: l.x0.min(cr.x0),
                                y0: l.y0.min(cr.y0),
                                x1: l.x1.max(cr.x1),
                                y1: l.y1.max(cr.y1),
                            }),
                            Some(l) => {
                                painter.rect_filled(self.screen_rect(r, &l), 0.0, sel_col);
                                Some(cr)
                            }
                            None => Some(cr),
                        };
                    }
                    if let Some(l) = line {
                        painter.rect_filled(self.screen_rect(r, &l), 0.0, sel_col);
                    }
                }
            }
        }

        // --- scrollbar ---
        self.scrollbar(ui, full, view, cx.dark);

        // --- render jobs ---
        let target_scale = |d: Vec2| -> f32 {
            let px = d * cx.ppp;
            let mut k = 1.0f32;
            let max_side = (cx.max_tex as f32).min(8192.0);
            if px.max_elem() > max_side {
                k = max_side / px.max_elem();
            }
            let area = px.x * px.y * k * k;
            if area > 48e6 {
                k *= (48e6 / area).sqrt();
            }
            self.zoom * PT * cx.ppp * k
        };
        let needs = |slot: &PageSlot, scale: f32| match &slot.tex {
            None => true,
            Some(t) => {
                t.stale || t.rot != self.rotation || t.invert != cx.invert || (t.scale / scale - 1.0).abs() > 0.02
            }
        };
        let job = |p: usize, scale: f32| Job::Render {
            doc: self.id,
            page: p,
            scale,
            rotation: self.rotation,
            invert: cx.invert,
            thumb: false,
        };
        // Quick low-res preview for big, empty visible pages.
        for p in first..=last {
            let sc = target_scale(self.disp[p]);
            let px = self.disp[p] * cx.ppp;
            if self.slots[p].tex.is_none() && px.x * px.y > 1.5e6 {
                jobs.push(job(p, sc * 0.3));
            }
        }
        for p in first..=last {
            let sc = target_scale(self.disp[p]);
            if needs(&self.slots[p], sc) {
                jobs.push(job(p, sc));
            }
        }
        for p in first..=last {
            if self.slots[p].chars.is_none() {
                jobs.push(Job::PageData { doc: self.id, page: p });
            }
        }
        let n = self.sizes.len();
        for p in [last + 1, last + 2, first.wrapping_sub(1)] {
            if p < n {
                let sc = target_scale(self.disp[p]);
                if needs(&self.slots[p], sc) {
                    jobs.push(job(p, sc));
                }
            }
        }
        // Evict textures far from the viewport.
        let keep = first.saturating_sub(2)..=(last + 2);
        for (p, s) in self.slots.iter_mut().enumerate() {
            if !keep.contains(&p) {
                s.tex = None;
            }
        }
    }

    fn scrollbar(&mut self, ui: &mut Ui, full: Rect, view: Rect, dark: bool) {
        if self.content.y <= self.view.y {
            return;
        }
        let track = Rect::from_min_max(pos2(view.max.x, full.min.y), full.max);
        let frac = self.view.y / self.content.y;
        let h = (track.height() * frac).max(30.0);
        let max = self.max_scroll().y;
        let t = if max > 0.0 { self.scroll.y / max } else { 0.0 };
        let thumb = Rect::from_min_size(
            pos2(track.min.x + 2.0, track.min.y + (track.height() - h) * t),
            vec2(SCROLLBAR_W - 4.0, h),
        );
        let id = ui.scope_id().with("vscroll");
        let resp = ui.interact(track, id, Sense::click_and_drag());
        if resp.dragged() {
            let dy = resp.drag_delta().y;
            let usable = (track.height() - h).max(1.0);
            self.scroll.y += dy / usable * max;
        } else if resp.clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                let f = ((p.y - track.min.y - h / 2.0) / (track.height() - h).max(1.0)).clamp(0.0, 1.0);
                self.scroll.y = f * max;
            }
        }
        self.clamp_scroll();
        let active = resp.hovered() || resp.dragged();
        let col = match (dark, active) {
            (true, true) => Color32::from_gray(150),
            (true, false) => Color32::from_gray(95),
            (false, true) => Color32::from_gray(110),
            (false, false) => Color32::from_gray(165),
        };
        ui.painter().rect_filled(thumb, 3.0, col);
    }

    // ----- sidebar -----

    pub fn show_outline(&mut self, ui: &mut Ui) {
        if self.outline.is_empty() {
            ui.weak("Brak spisu treści");
            return;
        }
        let mut goto = None;
        ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            outline_items(ui, &self.outline, &mut goto, 0);
        });
        if let Some(p) = goto {
            self.goto_page(p);
        }
    }

    pub fn show_thumbs(&mut self, ui: &mut Ui, cx: &ViewCtx, jobs: &mut Vec<Job>) {
        if self.sizes.is_empty() {
            return;
        }
        let label_h = 18.0;
        let heights: Vec<f32> = (0..self.sizes.len())
            .map(|i| {
                let d = self.page_disp_pts(i);
                THUMB_W * d.y / d.x + label_h + 8.0
            })
            .collect();
        let current = self.current_page();
        let mut goto = None;
        let follow = self.thumbs_follow;
        let mut area = ScrollArea::vertical().auto_shrink([false, false]);
        if follow {
            let off: f32 = heights[..current].iter().sum();
            area = area.vertical_scroll_offset((off - 100.0).max(0.0));
            self.thumbs_follow = false;
        }
        area.show_viewport(ui, |ui, vp| {
            let total: f32 = heights.iter().sum();
            let w = ui.available_width();
            let (rect, _) = ui.allocate_exact_size(vec2(w, total), Sense::hover());
            let mut y = 0.0;
            for (i, h) in heights.iter().enumerate() {
                let top = y;
                y += h;
                if y < vp.min.y || top > vp.max.y {
                    continue;
                }
                let d = self.page_disp_pts(i);
                let th = THUMB_W * d.y / d.x;
                let r = Rect::from_min_size(
                    pos2(rect.center().x - THUMB_W / 2.0, rect.min.y + top + 4.0),
                    vec2(THUMB_W, th),
                );
                let resp = ui.interact(r.expand(3.0), ui.scope_id().with(("thumb", i)), Sense::click());
                if i == current {
                    ui.painter().rect_stroke(
                        r.expand(3.0),
                        3.0,
                        Stroke::new(2.0, ui.visuals().selection.bg_fill),
                        StrokeKind::Outside,
                    );
                } else if resp.hovered() {
                    ui.painter().rect_stroke(
                        r.expand(3.0),
                        3.0,
                        Stroke::new(1.0, ui.visuals().weak_text_color()),
                        StrokeKind::Outside,
                    );
                }
                let scale = THUMB_W * cx.ppp / d.x;
                match &self.slots[i].thumb {
                    Some(t) => {
                        ui.painter().image(
                            t.tex.id(),
                            r,
                            Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    }
                    None => {
                        ui.painter().rect_filled(
                            r,
                            0.0,
                            if cx.invert { Color32::from_gray(30) } else { Color32::WHITE },
                        );
                    }
                }
                let stale = match &self.slots[i].thumb {
                    None => true,
                    Some(t) => {
                        t.stale
                            || t.rot != self.rotation
                            || t.invert != cx.invert
                            || (t.scale / scale - 1.0).abs() > 0.05
                    }
                };
                if stale {
                    jobs.push(Job::Render {
                        doc: self.id,
                        page: i,
                        scale,
                        rotation: self.rotation,
                        invert: cx.invert,
                        thumb: true,
                    });
                }
                ui.painter().text(
                    pos2(rect.center().x, r.max.y + 2.0),
                    Align2::CENTER_TOP,
                    format!("{}", i + 1),
                    FontId::proportional(11.0),
                    ui.visuals().text_color(),
                );
                if resp.clicked() {
                    goto = Some(i);
                }
            }
            // Drop thumbnails that are far off-screen.
            let mut acc = 0.0;
            for (i, h) in heights.iter().enumerate() {
                if acc + h < vp.min.y - 3000.0 || acc > vp.max.y + 3000.0 {
                    self.slots[i].thumb = None;
                }
                acc += h;
            }
        });
        if let Some(p) = goto {
            self.goto_page(p);
        }
    }
}

fn same_line(a: &NRect, b: &NRect) -> bool {
    let overlap = a.y1.min(b.y1) - a.y0.max(b.y0);
    let h = (a.y1 - a.y0).min(b.y1 - b.y0).max(1e-6);
    overlap / h > 0.5 && b.x0 >= a.x0 - 0.01
}

fn outline_items(ui: &mut Ui, items: &[OutlineItem], goto: &mut Option<usize>, depth: usize) {
    for (k, it) in items.iter().enumerate() {
        let title = if it.title.trim().is_empty() { "(bez tytułu)" } else { it.title.trim() };
        if it.children.is_empty() {
            // Align leaf titles with the text of collapsible siblings (past their arrow).
            let indent = ui.spacing().icon_width + ui.spacing().icon_spacing;
            let r = ui
                .horizontal(|ui| {
                    ui.add_space(indent);
                    ui.add(Button::selectable(false, title).truncate()).on_hover_text(title)
                })
                .inner;
            if r.clicked() {
                *goto = it.page;
            }
        } else {
            let id = ui.make_persistent_id(("outline", depth, k, &it.title));
            egui::collapsing_header::CollapsingState::load_with_default_open(
                ui.ctx(),
                id,
                depth == 0 && items.len() < 12,
            )
            .show_header(ui, |ui| {
                let r = ui.add(Button::selectable(false, title).truncate()).on_hover_text(title);
                if r.clicked() {
                    *goto = it.page;
                }
            })
            .body(|ui| outline_items(ui, &it.children, goto, depth + 1));
        }
    }
}
