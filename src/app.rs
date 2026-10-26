//! Application shell: tabs, toolbar, sidebar, shortcuts, file watching.

use crate::settings::{RecentFile, Settings};
use crate::view::{Doc, Fit, LoadState, PageTex, ViewCtx};
use crate::worker::{Cmd, DocId, Event, Job, WorkerHandle};
use eframe::egui::{self, *};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(PartialEq, Clone, Copy)]
enum SideTab {
    Outline,
    Thumbs,
}

pub struct App {
    worker: WorkerHandle,
    docs: Vec<Doc>,
    active: usize,
    next_id: DocId,
    settings: Settings,
    side_tab: SideTab,
    page_input: String,
    page_input_focus: bool,
    status: Option<(String, Instant)>,
    last_watch: Instant,
    fullscreen: bool,
    last_title: String,
    /// Files forwarded from later launches (single-instance).
    ipc: Option<std::sync::mpsc::Receiver<Vec<PathBuf>>>,
}

impl App {
    pub fn new(
        worker: WorkerHandle,
        settings: Settings,
        files: Vec<PathBuf>,
        ipc: Option<std::sync::mpsc::Receiver<Vec<PathBuf>>>,
    ) -> Self {
        let mut app = App {
            worker,
            docs: Vec::new(),
            active: 0,
            next_id: 1,
            settings,
            side_tab: SideTab::Outline,
            page_input: String::new(),
            page_input_focus: false,
            status: None,
            last_watch: Instant::now(),
            fullscreen: false,
            last_title: String::new(),
            ipc,
        };
        for f in files {
            app.open(f);
        }
        app
    }

    fn alloc_id(&mut self) -> DocId {
        self.next_id += 1;
        self.next_id
    }

    pub fn open(&mut self, path: PathBuf) {
        if path.is_dir() {
            return;
        }
        let path = std::fs::canonicalize(&path).map(strip_unc).unwrap_or(path);
        if let Some(i) = self.docs.iter().position(|d| d.path == path) {
            self.active = i;
            return;
        }
        let id = self.alloc_id();
        let mut doc = Doc::new(id, path.clone());
        if let Some(r) = self.settings.find(&path) {
            doc.zoom = r.zoom;
            doc.fit = Fit::from_u8(r.fit);
            doc.pending_goto = Some((r.page, 0.0));
        }
        let _ = self.worker.tx.send(Cmd::Open { doc: id, path, password: None });
        self.docs.push(doc);
        self.active = self.docs.len() - 1;
    }

    fn open_dialog(&mut self) {
        let mut dlg = rfd::FileDialog::new().add_filter("PDF", &["pdf"]).add_filter("Wszystkie pliki", &["*"]);
        if let Some(dir) = self.docs.get(self.active).and_then(|d| d.path.parent()) {
            dlg = dlg.set_directory(dir);
        }
        if let Some(files) = dlg.pick_files() {
            for f in files {
                self.open(f);
            }
        }
    }

    fn remember(&mut self, i: usize) {
        let d = &self.docs[i];
        if !d.is_ready() {
            return;
        }
        let (page, _) = d.position();
        self.settings.remember(RecentFile { path: d.path.clone(), page, zoom: d.zoom, fit: d.fit.to_u8() });
    }

    fn close(&mut self, i: usize) {
        if i >= self.docs.len() {
            return;
        }
        self.remember(i);
        let d = self.docs.remove(i);
        let _ = self.worker.tx.send(Cmd::Close { doc: d.id });
        if let Some(old) = d.reload_from {
            let _ = self.worker.tx.send(Cmd::Close { doc: old });
        }
        if self.active >= self.docs.len() {
            self.active = self.docs.len().saturating_sub(1);
        } else if self.active > i {
            self.active -= 1;
        }
        self.settings.save();
    }

    fn set_status(&mut self, s: impl Into<String>) {
        self.status = Some((s.into(), Instant::now()));
    }

    fn doc_by_id(&mut self, id: DocId) -> Option<&mut Doc> {
        self.docs.iter_mut().find(|d| d.id == id)
    }

    fn handle_events(&mut self, ctx: &Context) {
        while let Ok(ev) = self.worker.rx.try_recv() {
            match ev {
                Event::Opened { doc, info } => {
                    let Some(d) = self.doc_by_id(doc) else {
                        let _ = self.worker.tx.send(Cmd::Close { doc });
                        continue;
                    };
                    let old = d.reload_from.take();
                    let reloaded = old.is_some();
                    d.on_opened(info.page_sizes, info.outline, info.title);
                    if reloaded {
                        d.reset_search_results();
                    }
                    if let Some(old) = old {
                        let _ = self.worker.tx.send(Cmd::Close { doc: old });
                    }
                    if self.docs.iter().any(|d| d.id == doc && d.outline.is_empty()) {
                        self.side_tab = SideTab::Thumbs;
                    }
                }
                Event::OpenFailed { doc, error, needs_password } => {
                    let Some(d) = self.doc_by_id(doc) else { continue };
                    if let Some(old) = d.reload_from.take() {
                        // Reload failed (file probably mid-write): keep showing the old version.
                        d.id = old;
                        if !d.auto_reload {
                            d.disk_changed = true;
                            self.set_status("Nie udało się przeładować — plik może być jeszcze zapisywany");
                        }
                        continue;
                    }
                    d.state = if needs_password {
                        LoadState::NeedsPassword { wrong: !d.password.is_empty() }
                    } else {
                        LoadState::Failed(error)
                    };
                }
                Event::Rendered { doc, job, width, height, rgba } => {
                    let Job::Render { page, scale, rotation, invert, thumb, .. } = job else { continue };
                    let Some(d) = self.doc_by_id(doc) else { continue };
                    let Some(slot) = d.slots.get_mut(page) else { continue };
                    let img = ColorImage::from_rgba_unmultiplied([width, height], &rgba);
                    let tex = ctx.load_texture(format!("{doc}-{page}-{thumb}"), img, TextureOptions::LINEAR);
                    let t = PageTex { tex, scale, rot: rotation, invert, stale: false };
                    if thumb {
                        slot.thumb = Some(t);
                    } else {
                        slot.tex = Some(t);
                    }
                }
                Event::PageData { doc, page, chars, links } => {
                    if let Some(slot) = self.doc_by_id(doc).and_then(|d| d.slots.get_mut(page)) {
                        slot.chars = Some(chars);
                        slot.links = links;
                    }
                }
                Event::SearchHits { doc, generation, page, hits } => {
                    if let Some(d) = self.doc_by_id(doc).filter(|d| d.search.generation == generation) {
                        d.add_hits(page, hits);
                    }
                }
                Event::SearchProgress { doc, generation, page, done } => {
                    if let Some(d) = self.doc_by_id(doc).filter(|d| d.search.generation == generation) {
                        d.search.progress = page;
                        if done {
                            d.finish_search();
                        }
                    }
                }
                Event::PrintDone(res) => match res {
                    Ok(()) => self.set_status("Wysłano do drukarki"),
                    Err(e) => self.set_status(format!("Błąd drukowania: {e}")),
                },
            }
        }
    }

    /// Re-read document `i` from disk, keeping the old version visible until the new one opens.
    fn reload(&mut self, i: usize) {
        let id = self.alloc_id();
        let d = &mut self.docs[i];
        if d.reload_from.is_some() || !d.is_ready() {
            return;
        }
        d.disk_changed = false;
        d.mtime = std::fs::metadata(&d.path).and_then(|m| m.modified()).ok();
        d.reload_from = Some(d.id);
        d.id = id;
        let pw = Some(d.password.clone()).filter(|p| !p.is_empty());
        let _ = self.worker.tx.send(Cmd::Open { doc: id, path: d.path.clone(), password: pw });
    }

    /// Notice documents whose file changed on disk; ask before reloading unless
    /// the user chose to always reload that document.
    fn watch_files(&mut self) {
        if self.last_watch.elapsed() < Duration::from_millis(800) {
            return;
        }
        self.last_watch = Instant::now();
        for i in 0..self.docs.len() {
            let (path, mtime, busy) = {
                let d = &self.docs[i];
                (d.path.clone(), d.mtime, d.reload_from.is_some() || !d.is_ready())
            };
            if busy {
                continue;
            }
            let Ok(m) = std::fs::metadata(&path).and_then(|m| m.modified()) else { continue };
            if Some(m) != mtime {
                let d = &mut self.docs[i];
                d.mtime = Some(m);
                if d.auto_reload {
                    self.reload(i);
                } else {
                    d.disk_changed = true;
                }
            }
        }
    }

    fn start_search(&mut self) {
        let Some(d) = self.docs.get_mut(self.active) else { return };
        let q = d.search.query.trim().to_string();
        if q.is_empty() {
            return;
        }
        d.search.generation += 1;
        d.reset_search_results();
        d.search.searched = d.search.query.clone();
        d.search.start_page = d.current_page();
        let _ = self.worker.tx.send(Cmd::Search { doc: d.id, query: q, generation: d.search.generation });
    }

    fn search_next(&mut self, forward: bool) {
        let Some(d) = self.docs.get_mut(self.active) else { return };
        if d.search.query != d.search.searched || d.search.searched.is_empty() {
            self.start_search();
        } else {
            d.search_step(forward);
        }
    }

    fn close_search(&mut self) {
        if let Some(d) = self.docs.get_mut(self.active) {
            d.search.open = false;
            d.search.searched.clear();
            d.reset_search_results();
            let _ = self.worker.tx.send(Cmd::CancelSearch);
        }
    }

    fn print(&mut self) {
        let Some(d) = self.docs.get(self.active).filter(|d| d.is_ready()) else { return };
        if !cfg!(windows) {
            self.set_status("Drukowanie nie jest jeszcze obsługiwane na tej platformie");
            return;
        }
        if let Some(job) = crate::print::show_dialog(d.page_count(), d.current_page(), &d.name) {
            let _ = self.worker.tx.send(Cmd::Print { doc: d.id, job });
            self.set_status("Drukowanie…");
        }
    }

    fn shortcuts(&mut self, ctx: &Context) {
        let sc = |m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(m, k)));
        let cmd = Modifiers::COMMAND;
        let cmd_shift = Modifiers::COMMAND | Modifiers::SHIFT;

        if sc(cmd, Key::O) {
            self.open_dialog();
        }
        if sc(Modifiers::NONE, Key::F11) {
            self.fullscreen = !self.fullscreen;
            ctx.send_viewport_cmd(ViewportCommand::Fullscreen(self.fullscreen));
        }
        if sc(cmd, Key::B) {
            self.settings.sidebar = !self.settings.sidebar;
        }
        if self.docs.is_empty() {
            return;
        }
        if sc(cmd, Key::W) || sc(cmd, Key::F4) {
            self.close(self.active);
            return;
        }
        if sc(cmd_shift, Key::Tab) {
            self.active = (self.active + self.docs.len() - 1) % self.docs.len();
        }
        if sc(cmd, Key::Tab) {
            self.active = (self.active + 1) % self.docs.len();
        }
        if sc(cmd, Key::F) {
            let d = &mut self.docs[self.active];
            d.search.open = true;
            d.search.focus = true;
        }
        if sc(Modifiers::SHIFT, Key::F3) {
            self.search_next(false);
        }
        if sc(Modifiers::NONE, Key::F3) {
            self.search_next(true);
        }
        if sc(cmd, Key::P) {
            self.print();
        }
        if sc(Modifiers::NONE, Key::F5) {
            self.reload(self.active);
        }
        if sc(cmd, Key::G) {
            self.page_input_focus = true;
        }

        let d = &mut self.docs[self.active];
        let copy = ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy)));
        if copy || sc(cmd, Key::C) {
            if let Some(t) = d.selected_text() {
                ctx.copy_text(t);
            }
        }
        if sc(cmd, Key::Plus) || sc(cmd, Key::Equals) || sc(cmd_shift, Key::Equals) {
            d.zoom_step(true);
        }
        if sc(cmd, Key::Minus) {
            d.zoom_step(false);
        }
        if sc(cmd, Key::Num0) {
            d.set_fit(Fit::Page);
        }
        if sc(cmd, Key::Num1) {
            d.fit = Fit::None;
            d.set_zoom(1.0, None);
        }
        if sc(cmd, Key::Num2) {
            d.set_fit(Fit::Width);
        }
        if sc(cmd_shift, Key::R) {
            d.rotate(false);
        }
        if sc(cmd, Key::R) {
            d.rotate(true);
        }

        if ctx.egui_wants_keyboard_input() {
            if d.search.open && sc(Modifiers::NONE, Key::Escape) {
                self.close_search();
            }
            return;
        }
        if sc(Modifiers::NONE, Key::Escape) {
            if self.fullscreen {
                self.fullscreen = false;
                ctx.send_viewport_cmd(ViewportCommand::Fullscreen(false));
            } else if d.search.open {
                self.close_search();
            } else {
                d.sel = None;
            }
            return;
        }
        let step = 60.0;
        let page = d.page_step();
        let (down, up, left, right, pgdn, pgup, home, end, space, shift_space) = ctx.input(|i| {
            (
                i.key_pressed(Key::ArrowDown),
                i.key_pressed(Key::ArrowUp),
                i.key_pressed(Key::ArrowLeft),
                i.key_pressed(Key::ArrowRight),
                i.key_pressed(Key::PageDown),
                i.key_pressed(Key::PageUp),
                i.key_pressed(Key::Home),
                i.key_pressed(Key::End),
                i.key_pressed(Key::Space) && !i.modifiers.shift,
                i.key_pressed(Key::Space) && i.modifiers.shift,
            )
        });
        if down {
            d.scroll_by(vec2(0.0, step));
        }
        if up {
            d.scroll_by(vec2(0.0, -step));
        }
        if left {
            d.scroll_by(vec2(-step, 0.0));
        }
        if right {
            d.scroll_by(vec2(step, 0.0));
        }
        if pgdn || space {
            d.scroll_by(vec2(0.0, page));
        }
        if pgup || shift_space {
            d.scroll_by(vec2(0.0, -page));
        }
        if home {
            d.scroll_to_end(false);
        }
        if end {
            d.scroll_to_end(true);
        }
    }

    fn tab_bar(&mut self, ui: &mut Ui) {
        let mut close = None;
        ScrollArea::horizontal().id_salt("tabs").show(ui, |ui| {
            ui.horizontal(|ui| {
                for (i, d) in self.docs.iter().enumerate() {
                    let label = if d.disk_changed { format!("● {}", d.name) } else { d.name.clone() };
                    let r = ui
                        .add(Button::selectable(i == self.active, label).truncate())
                        .on_hover_text(d.path.display().to_string());
                    if r.clicked() {
                        self.active = i;
                    }
                    if r.middle_clicked() {
                        close = Some(i);
                    }
                    if ui.add(Button::new("✕").frame(false).small()).on_hover_text("Zamknij (Ctrl+W)").clicked() {
                        close = Some(i);
                    }
                    ui.separator();
                }
            });
        });
        if let Some(i) = close {
            self.close(i);
        }
    }

    fn toolbar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            if ui.button("📂").on_hover_text("Otwórz (Ctrl+O)").clicked() {
                self.open_dialog();
            }
            if ui.selectable_label(self.settings.sidebar, "☰").on_hover_text("Panel boczny (Ctrl+B)").clicked() {
                self.settings.sidebar = !self.settings.sidebar;
            }
            ui.separator();

            let mut do_print = false;
            let mut search_toggle = false;
            if let Some(d) = self.docs.get_mut(self.active).filter(|d| d.is_ready()) {
                let n = d.page_count();
                let cur = d.current_page();
                if ui.button("⏶").on_hover_text("Poprzednia strona").clicked() && cur > 0 {
                    d.goto_page(cur - 1);
                }
                let te = TextEdit::singleline(&mut self.page_input)
                    .hint_text(format!("{}", cur + 1))
                    .desired_width(36.0)
                    .align(Align2::CENTER_CENTER);
                let r = ui.add(te);
                if self.page_input_focus {
                    r.request_focus();
                    self.page_input_focus = false;
                }
                if r.gained_focus() {
                    self.page_input.clear();
                }
                if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    if let Ok(p) = self.page_input.trim().parse::<usize>() {
                        d.goto_page(p.clamp(1, n) - 1);
                    }
                }
                if !r.has_focus() {
                    self.page_input = format!("{}", cur + 1);
                }
                ui.label(format!("/ {n}"));
                if ui.button("⏷").on_hover_text("Następna strona").clicked() && cur + 1 < n {
                    d.goto_page(cur + 1);
                }
                ui.separator();

                if ui.button("−").on_hover_text("Pomniejsz (Ctrl+-)").clicked() {
                    d.zoom_step(false);
                }
                ui.menu_button(format!("{:.0}%", d.zoom * 100.0), |ui| {
                    for z in [0.5, 0.75, 1.0, 1.25, 1.5, 2.0, 3.0, 4.0] {
                        if ui.button(format!("{:.0}%", z * 100.0)).clicked() {
                            d.fit = Fit::None;
                            d.set_zoom(z, None);
                            ui.close();
                        }
                    }
                });
                if ui.button("+").on_hover_text("Powiększ (Ctrl++)").clicked() {
                    d.zoom_step(true);
                }
                if ui.selectable_label(d.fit == Fit::Width, "↔").on_hover_text("Dopasuj szerokość (Ctrl+2)").clicked()
                {
                    d.set_fit(Fit::Width);
                }
                if ui.selectable_label(d.fit == Fit::Page, "⬜").on_hover_text("Cała strona (Ctrl+0)").clicked() {
                    d.set_fit(Fit::Page);
                }
                if ui.button("⟳").on_hover_text("Obróć w prawo (Ctrl+R)").clicked() {
                    d.rotate(true);
                }
                ui.separator();
                if ui.selectable_label(d.search.open, "🔍").on_hover_text("Szukaj (Ctrl+F)").clicked() {
                    search_toggle = true;
                }
                if ui.button("🖶").on_hover_text("Drukuj (Ctrl+P)").clicked() {
                    do_print = true;
                }
            }
            if search_toggle {
                let open = self.docs[self.active].search.open;
                if open {
                    self.close_search();
                } else {
                    let d = &mut self.docs[self.active];
                    d.search.open = true;
                    d.search.focus = true;
                }
            }
            if do_print {
                self.print();
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let theme = if self.settings.dark { "☀" } else { "🌙" };
                if ui.button(theme).on_hover_text("Tryb jasny / ciemny").clicked() {
                    self.settings.dark = !self.settings.dark;
                    apply_theme(ui.ctx(), self.settings.dark);
                }
                if ui
                    .selectable_label(self.settings.invert_pages, "◐")
                    .on_hover_text("Odwróć kolory stron (do czytania nocą)")
                    .clicked()
                {
                    self.settings.invert_pages = !self.settings.invert_pages;
                }
            });
        });
    }

    fn disk_bar(&mut self, ui: &mut Ui) {
        let (mut reload, mut always, mut ignore) = (false, false, false);
        ui.add_space(3.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("⚠").color(ui.visuals().warn_fg_color));
            ui.label("Plik został zmieniony na dysku. Przeładować?");
            reload = ui.button("Przeładuj (F5)").clicked();
            always = ui.button("Zawsze przeładowuj ten plik").on_hover_text("Do zamknięcia karty").clicked();
            ignore = ui.button("Ignoruj").clicked();
        });
        ui.add_space(3.0);
        let i = self.active;
        if always {
            self.docs[i].auto_reload = true;
        }
        if reload || always {
            self.reload(i);
        } else if ignore {
            self.docs[i].disk_changed = false;
        }
    }

    fn search_bar(&mut self, ui: &mut Ui) {
        let mut action = None;
        let d = &mut self.docs[self.active];
        ui.horizontal(|ui| {
            ui.label("Szukaj:");
            let r = ui.add(TextEdit::singleline(&mut d.search.query).desired_width(240.0).hint_text("tekst…"));
            if d.search.focus {
                r.request_focus();
                d.search.focus = false;
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                action = Some(!ui.input(|i| i.modifiers.shift));
                r.request_focus();
            }
            if ui.button("⏶").on_hover_text("Poprzedni (Shift+Enter)").clicked() {
                action = Some(false);
            }
            if ui.button("⏷").on_hover_text("Następny (Enter)").clicked() {
                action = Some(true);
            }
            let s = &d.search;
            if !s.searched.is_empty() {
                let n = s.hits.len();
                let txt = match (s.current, s.done) {
                    (_, false) => format!("{n} wyników… (str. {})", s.progress + 1),
                    (_, true) if n == 0 => "Brak wyników".to_string(),
                    (Some(c), true) => format!("{} z {n}", c + 1),
                    (None, true) => format!("{n} wyników"),
                };
                ui.weak(txt);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add(Button::new("✕").frame(false)).on_hover_text("Zamknij (Esc)").clicked() {
                    action = None;
                    d.search.open = false;
                }
            });
        });
        if !self.docs[self.active].search.open {
            self.close_search();
        } else if let Some(fwd) = action {
            self.search_next(fwd);
        }
    }

    fn welcome(&mut self, ui: &mut Ui) {
        let mut open_path = None;
        let mut open_dialog = false;
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * 0.18);
            ui.heading(RichText::new("NopePDF").size(32.0));
            ui.add_space(6.0);
            ui.weak("Lekki czytnik PDF");
            ui.add_space(24.0);
            if ui
                .add(Button::new(RichText::new("📂  Otwórz plik…  (Ctrl+O)").size(16.0)).min_size(vec2(240.0, 36.0)))
                .clicked()
            {
                open_dialog = true;
            }
            ui.add_space(8.0);
            ui.weak("albo upuść plik PDF w tym oknie");
            let recent: Vec<_> = self.settings.recent.iter().filter(|r| r.path.exists()).take(12).cloned().collect();
            if !recent.is_empty() {
                ui.add_space(28.0);
                ui.label(RichText::new("Ostatnio otwierane").strong());
                ui.add_space(4.0);
                for r in recent {
                    let name = r.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    if ui
                        .add(Button::new(format!("{name}   —   str. {}", r.page + 1)).frame(false))
                        .on_hover_text(r.path.display().to_string())
                        .clicked()
                    {
                        open_path = Some(r.path.clone());
                    }
                }
            }
        });
        if open_dialog {
            self.open_dialog();
        }
        if let Some(p) = open_path {
            self.open(p);
        }
    }

    fn update_title(&mut self, ctx: &Context) {
        let t = match self.docs.get(self.active) {
            Some(d) => match &d.title {
                Some(t) => format!("{} — {} — NopePDF", d.name, t),
                None => format!("{} — NopePDF", d.name),
            },
            None => "NopePDF".to_string(),
        };
        if t != self.last_title {
            ctx.send_viewport_cmd(ViewportCommand::Title(t.clone()));
            self.last_title = t;
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.handle_events(&ctx);
        let forwarded: Vec<Vec<PathBuf>> = self.ipc.as_ref().map(|rx| rx.try_iter().collect()).unwrap_or_default();
        for files in forwarded {
            for f in files {
                self.open(f);
            }
            ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(ViewportCommand::Focus);
        }
        self.watch_files();
        ctx.request_repaint_after(Duration::from_secs(1));

        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect()
        });
        for p in dropped {
            self.open(p);
        }
        self.shortcuts(&ctx);
        self.update_title(&ctx);

        let cx = ViewCtx {
            invert: self.settings.invert_pages,
            dark: self.settings.dark,
            max_tex: ctx.input(|i| i.max_texture_side),
            ppp: ctx.pixels_per_point(),
        };
        let mut jobs = Vec::new();
        let mut thumb_jobs = Vec::new();

        if !self.fullscreen {
            Panel::top("toolbar").show(ui, |ui| {
                if self.docs.len() > 1 {
                    ui.add_space(2.0);
                    self.tab_bar(ui);
                }
                ui.add_space(2.0);
                self.toolbar(ui);
                ui.add_space(2.0);
            });
            if self.docs.get(self.active).is_some_and(|d| d.disk_changed) {
                Panel::top("disk_changed").show(ui, |ui| self.disk_bar(ui));
            }
            if self.docs.get(self.active).is_some_and(|d| d.search.open) {
                Panel::top("search").show(ui, |ui| {
                    ui.add_space(2.0);
                    self.search_bar(ui);
                    ui.add_space(2.0);
                });
            }
            if self.settings.sidebar && self.docs.get(self.active).is_some_and(|d| d.is_ready()) {
                Panel::left("sidebar").resizable(true).default_size(200.0).size_range(120.0..=500.0).show(ui, |ui| {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut self.side_tab, SideTab::Outline, "Spis treści");
                        if ui.selectable_value(&mut self.side_tab, SideTab::Thumbs, "Miniatury").clicked() {
                            self.docs[self.active].thumbs_follow = true;
                        }
                    });
                    ui.separator();
                    let d = &mut self.docs[self.active];
                    match self.side_tab {
                        SideTab::Outline => d.show_outline(ui),
                        SideTab::Thumbs => d.show_thumbs(ui, &cx, &mut thumb_jobs),
                    }
                });
            }
        }

        CentralPanel::default().frame(Frame::NONE).show(ui, |ui| {
            if self.docs.is_empty() {
                self.welcome(ui);
                return;
            }
            let d = &mut self.docs[self.active];
            if !matches!(d.state, LoadState::Ready) {
                ui.painter().rect_filled(ui.max_rect(), 0.0, ui.visuals().panel_fill);
            }
            match &d.state {
                LoadState::Ready => d.show(ui, &cx, &mut jobs),
                LoadState::Loading => {
                    ui.centered_and_justified(|ui| ui.spinner());
                }
                LoadState::Failed(e) => {
                    let e = e.clone();
                    ui.vertical_centered(|ui| {
                        ui.add_space(80.0);
                        ui.heading("Nie udało się otworzyć pliku");
                        ui.add_space(8.0);
                        ui.label(d.path.display().to_string());
                        ui.weak(e);
                    });
                }
                LoadState::NeedsPassword { wrong } => {
                    let wrong = *wrong;
                    let mut submit = false;
                    ui.vertical_centered(|ui| {
                        ui.add_space(80.0);
                        ui.heading("Dokument jest chroniony hasłem");
                        ui.add_space(8.0);
                        if wrong {
                            ui.colored_label(ui.visuals().error_fg_color, "Nieprawidłowe hasło");
                        }
                        let r = ui.add(TextEdit::singleline(&mut d.password).password(true).desired_width(200.0));
                        r.request_focus();
                        if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                            submit = true;
                        }
                        if ui.button("Otwórz").clicked() {
                            submit = true;
                        }
                    });
                    if submit {
                        d.state = LoadState::Loading;
                        let _ = self.worker.tx.send(Cmd::Open {
                            doc: d.id,
                            path: d.path.clone(),
                            password: Some(d.password.clone()),
                        });
                    }
                }
            }
        });

        jobs.extend(thumb_jobs);
        self.worker.set_queue(jobs);

        if let Some((msg, t)) = &self.status {
            if t.elapsed() > Duration::from_secs(4) {
                self.status = None;
            } else {
                let msg = msg.clone();
                Area::new(Id::unique("status")).anchor(Align2::LEFT_BOTTOM, vec2(12.0, -12.0)).show(&ctx, |ui| {
                    Frame::popup(ui.style()).show(ui, |ui| ui.label(msg));
                });
                ctx.request_repaint_after(Duration::from_millis(500));
            }
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        for i in 0..self.docs.len() {
            self.remember(i);
        }
        self.settings.save();
    }
}

pub fn apply_theme(ctx: &Context, dark: bool) {
    ctx.set_theme(if dark { Theme::Dark } else { Theme::Light });
}

/// `canonicalize` on Windows yields `\\?\C:\...`; strip it for display and history.
fn strip_unc(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => Path::new(rest).to_path_buf(),
        _ => p,
    }
}
