//! PDF worker thread. PDFium is not thread-safe, so a single thread owns the
//! library and every open document. The UI talks to it through a command
//! channel plus a shared, priority-ordered job queue that the UI rewrites each
//! frame (so stale render requests are dropped automatically).

use eframe::egui;
use nopepdf::pdf::{OpenError, open_document};
use nopepdf::print::{Device, PageResult, PrintError};
use pdfium_render::prelude::*;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

pub type DocId = u64;

/// Rectangle in normalized page coordinates (0..1, origin top-left, page as
/// displayed with its own /Rotate applied, before any view rotation).
#[derive(Clone, Copy, Debug, Default)]
pub struct NRect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Job {
    Render {
        doc: DocId,
        page: usize,
        /// Pixels per PDF point.
        scale: f32,
        rotation: u8,
        invert: bool,
        thumb: bool,
    },
    PageData {
        doc: DocId,
        page: usize,
    },
}

pub enum Cmd {
    Open {
        doc: DocId,
        path: PathBuf,
        password: Option<String>,
    },
    Close {
        doc: DocId,
    },
    Search {
        doc: DocId,
        query: String,
        generation: u64,
    },
    CancelSearch {
        doc: DocId,
    },
    #[cfg_attr(not(windows), allow(dead_code))]
    Print {
        doc: DocId,
        job: nopepdf::print::PrintJob,
    },
    Wake,
}

pub struct OutlineItem {
    pub title: String,
    pub page: Option<usize>,
    pub children: Vec<OutlineItem>,
}

pub struct DocInfo {
    /// Page sizes in PDF points (width, height), with /Rotate applied.
    pub page_sizes: Vec<(f32, f32)>,
    pub outline: Vec<OutlineItem>,
    pub title: Option<String>,
}

pub struct TextChar {
    pub ch: char,
    pub rect: Option<NRect>,
}

pub enum LinkTarget {
    Page(usize),
    Uri(String),
}

pub struct Link {
    pub rect: NRect,
    pub target: LinkTarget,
}

pub enum Event {
    Opened { doc: DocId, info: DocInfo },
    OpenFailed { doc: DocId, error: OpenError },
    Rendered { doc: DocId, job: Job, width: usize, height: usize, rgba: Vec<u8> },
    PageData { doc: DocId, page: usize, chars: Vec<TextChar>, links: Vec<Link> },
    SearchHits { doc: DocId, generation: u64, page: usize, hits: Vec<Vec<NRect>> },
    SearchProgress { doc: DocId, generation: u64, page: usize, done: bool },
    PrintDone(Result<(), PrintError>),
}

#[derive(Default)]
pub struct Queue {
    jobs: Vec<Job>,
    /// The job the worker is executing right now (not re-queued by the UI).
    current: Option<Job>,
}

pub struct WorkerHandle {
    pub tx: mpsc::Sender<Cmd>,
    pub rx: mpsc::Receiver<Event>,
    queue: Arc<Mutex<Queue>>,
}

impl WorkerHandle {
    /// Replace the pending job queue; wakes the worker if the queue changed.
    pub fn set_queue(&self, mut jobs: Vec<Job>) {
        let mut q = self.queue.lock().unwrap();
        if let Some(c) = q.current {
            jobs.retain(|j| *j != c);
        }
        if q.jobs != jobs {
            q.jobs = jobs;
            drop(q);
            let _ = self.tx.send(Cmd::Wake);
        }
    }
}

pub fn spawn(pdfium: Pdfium, ctx: egui::Context) -> WorkerHandle {
    let (tx, cmd_rx) = mpsc::channel();
    let (ev_tx, rx) = mpsc::channel();
    let queue = Arc::new(Mutex::new(Queue::default()));
    let q = queue.clone();
    std::thread::Builder::new()
        .name("pdf-worker".into())
        .spawn(move || {
            let pdfium: &'static Pdfium = Box::leak(Box::new(pdfium));
            Worker {
                pdfium,
                docs: HashMap::new(),
                searches: Vec::new(),
                prints: Vec::new(),
                cmd_rx,
                ev_tx,
                queue: q,
                ctx,
            }
            .run();
        })
        .expect("spawn worker");
    WorkerHandle { tx, rx, queue }
}

struct Search {
    doc: DocId,
    query: String,
    generation: u64,
    next_page: usize,
}

/// A print job in progress: pages are rendered here one at a time and handed to the job's
/// print thread. Dropping the task closes the channel, which cancels an unfinished job.
struct PrintTask {
    doc: DocId,
    /// `PrintJob::pages`, and the index of the next one to render.
    pages: Vec<Option<usize>>,
    next: usize,
    device: Device,
    tx: mpsc::SyncSender<PageResult>,
    /// A rendered page the print thread was not ready to take yet.
    pending: Option<PageResult>,
}

struct Worker {
    pdfium: &'static Pdfium,
    docs: HashMap<DocId, PdfDocument<'static>>,
    /// Running searches, one per document, stepped round-robin.
    searches: Vec<Search>,
    /// Running print jobs, stepped round-robin.
    prints: Vec<PrintTask>,
    cmd_rx: mpsc::Receiver<Cmd>,
    ev_tx: mpsc::Sender<Event>,
    queue: Arc<Mutex<Queue>>,
    ctx: egui::Context,
}

impl Worker {
    fn run(mut self) {
        loop {
            // Drain commands without blocking.
            loop {
                match self.cmd_rx.try_recv() {
                    Ok(cmd) => self.handle(cmd),
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => return,
                }
            }

            let job = {
                let mut q = self.queue.lock().unwrap();
                q.current = if q.jobs.is_empty() { None } else { Some(q.jobs.remove(0)) };
                q.current
            };

            if let Some(job) = job {
                self.do_job(job);
                self.queue.lock().unwrap().current = None;
                continue;
            }
            // Background work, one unit each, so view renders are checked again in between.
            let printed = self.print_step();
            let searched = self.search_step();
            if printed || searched {
                continue;
            }
            if self.prints.is_empty() {
                // Idle: block until something arrives.
                match self.cmd_rx.recv() {
                    Ok(cmd) => self.handle(cmd),
                    Err(_) => return,
                }
            } else {
                // Every print thread is busy (spooling, or a "Save as" dialog is open).
                match self.cmd_rx.recv_timeout(Duration::from_millis(20)) {
                    Ok(cmd) => self.handle(cmd),
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }
            }
        }
    }

    fn send(&self, ev: Event) {
        let _ = self.ev_tx.send(ev);
        self.ctx.request_repaint();
    }

    fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Wake => {}
            Cmd::Open { doc, path, password } => {
                let loaded = open_document(self.pdfium, &path, password.as_deref());
                match loaded {
                    Ok(d) => {
                        let info = doc_info(&d);
                        self.docs.insert(doc, d);
                        self.send(Event::Opened { doc, info });
                    }
                    Err(error) => self.send(Event::OpenFailed { doc, error }),
                }
            }
            Cmd::Close { doc } => {
                self.docs.remove(&doc);
                self.searches.retain(|s| s.doc != doc);
                self.prints.retain(|p| p.doc != doc);
            }
            Cmd::Search { doc, query, generation } => {
                self.searches.retain(|s| s.doc != doc);
                self.searches.push(Search { doc, query, generation, next_page: 0 });
            }
            Cmd::CancelSearch { doc } => self.searches.retain(|s| s.doc != doc),
            Cmd::Print { doc, job } => {
                if !self.docs.contains_key(&doc) {
                    self.send(Event::PrintDone(Err(PrintError::NotOpen)));
                    return;
                }
                // A rendezvous channel: at most one page waits here and one is being printed.
                let (tx, rx) = mpsc::sync_channel(0);
                let task = PrintTask { doc, pages: job.pages.clone(), next: 0, device: job.device, tx, pending: None };
                let (ev_tx, ctx) = (self.ev_tx.clone(), self.ctx.clone());
                nopepdf::print::spawn_printer(job, rx, move |res| {
                    let _ = ev_tx.send(Event::PrintDone(res));
                    ctx.request_repaint();
                });
                self.prints.push(task);
            }
        }
    }

    fn do_job(&mut self, job: Job) {
        match job {
            Job::Render { doc, page, scale, rotation, invert, .. } => {
                let Some(d) = self.docs.get(&doc) else { return };
                let Ok(p) = d.pages().get(page as PdfPageIndex) else { return };
                let rot = match rotation {
                    1 => PdfPageRenderRotation::Degrees90,
                    2 => PdfPageRenderRotation::Degrees180,
                    3 => PdfPageRenderRotation::Degrees270,
                    _ => PdfPageRenderRotation::None,
                };
                let cfg = PdfRenderConfig::new()
                    .scale_page_by_factor(scale)
                    .rotate(rot, true)
                    .render_form_data(true)
                    .set_clear_color(PdfColor::WHITE);
                if let Ok(bmp) = p.render_with_config(&cfg) {
                    let (w, h) = (bmp.width() as usize, bmp.height() as usize);
                    let mut rgba = bmp.as_rgba_bytes();
                    if invert {
                        // Soft inversion: white paper -> dark grey, black ink -> light grey.
                        let lut: [u8; 256] = std::array::from_fn(|v| (30 + (255 - v) * 190 / 255) as u8);
                        for px in rgba.chunks_exact_mut(4) {
                            px[0] = lut[px[0] as usize];
                            px[1] = lut[px[1] as usize];
                            px[2] = lut[px[2] as usize];
                        }
                    }
                    self.send(Event::Rendered { doc, job, width: w, height: h, rgba });
                }
            }
            Job::PageData { doc, page } => {
                let Some(d) = self.docs.get(&doc) else { return };
                let Ok(p) = d.pages().get(page as PdfPageIndex) else { return };
                let map = PageMap::new(&p);
                let mut chars = Vec::new();
                if let Ok(text) = p.text() {
                    for c in text.chars().iter() {
                        let ch = c.unicode_char().unwrap_or(' ');
                        let rect = c
                            .loose_bounds()
                            .ok()
                            .filter(|r| r.width().value > 0.0 && r.height().value > 0.0)
                            .map(|r| map.rect(&r));
                        chars.push(TextChar { ch, rect });
                    }
                }
                let mut links = Vec::new();
                for l in p.links().iter() {
                    let Ok(r) = l.rect() else { continue };
                    let target = if let Some(i) = l.destination().and_then(|d| d.page_index().ok()) {
                        Some(LinkTarget::Page(i as usize))
                    } else {
                        match l.action() {
                            Some(PdfAction::LocalDestination(a)) => a
                                .destination()
                                .ok()
                                .and_then(|d| d.page_index().ok())
                                .map(|i| LinkTarget::Page(i as usize)),
                            Some(PdfAction::Uri(a)) => a.uri().ok().map(LinkTarget::Uri),
                            _ => None,
                        }
                    };
                    if let Some(target) = target {
                        links.push(Link { rect: map.rect(&r), target });
                    }
                }
                self.send(Event::PageData { doc, page, chars, links });
            }
        }
    }

    /// Hands the next page of the first print job to its print thread (rendering it first if
    /// needed), then moves the job to the back of the line. Returns false when there was
    /// nothing to do or the print thread was not ready.
    fn print_step(&mut self) -> bool {
        if self.prints.is_empty() {
            return false;
        }
        let mut t = self.prints.remove(0);
        let page = match t.pending.take() {
            Some(p) => p,
            None => match (self.docs.get(&t.doc), t.pages.get(t.next)) {
                (Some(d), Some(&entry)) => {
                    let p = nopepdf::print::render_entry(d, entry, &t.device);
                    t.next += 1;
                    p
                }
                // Document closed, or nothing to print: dropping the task ends the job.
                _ => return true,
            },
        };
        let failed = page.is_err();
        match t.tx.try_send(page) {
            // Last page or an error: dropping the task closes the channel.
            Ok(()) if failed || t.next >= t.pages.len() => true,
            Ok(()) => {
                self.prints.push(t);
                true
            }
            Err(mpsc::TrySendError::Full(page)) => {
                t.pending = Some(page);
                self.prints.push(t);
                false
            }
            // The print thread stopped (error or cancelled dialog).
            Err(mpsc::TrySendError::Disconnected(_)) => true,
        }
    }

    /// Searches one page of the first running search, then moves it to the back of the line.
    /// Returns false when no search is running.
    fn search_step(&mut self) -> bool {
        if self.searches.is_empty() {
            return false;
        }
        let mut s = self.searches.remove(0);
        let Some(d) = self.docs.get(&s.doc) else { return true };
        let n = d.pages().len() as usize;
        let page = s.next_page;
        let (doc, generation) = (s.doc, s.generation);
        if page >= n {
            self.send(Event::SearchProgress { doc, generation, page, done: true });
            return true;
        }
        s.next_page += 1;
        let mut hits = Vec::new();
        if let Ok(p) = d.pages().get(page as PdfPageIndex) {
            let map = PageMap::new(&p);
            if let Ok(text) = p.text() {
                if let Ok(search) = text.search(&s.query, &PdfSearchOptions::new()) {
                    while let Some(segs) = search.find_next() {
                        let rects: Vec<NRect> = segs.iter().map(|sg| map.rect(&sg.bounds())).collect();
                        if !rects.is_empty() {
                            hits.push(rects);
                        }
                        if hits.len() > 5000 {
                            break;
                        }
                    }
                }
            }
        }
        if !hits.is_empty() {
            self.send(Event::SearchHits { doc, generation, page, hits });
        }
        if page.is_multiple_of(20) {
            self.send(Event::SearchProgress { doc, generation, page, done: false });
        }
        self.searches.push(s);
        true
    }
}

/// Affine map from PDF page space to normalized display coordinates,
/// derived from PDFium's own page-to-device transform (handles /Rotate and
/// non-zero crop box origins).
struct PageMap {
    ox: f32,
    oy: f32,
    ax: (f32, f32),
    ay: (f32, f32),
}

impl PageMap {
    const N: i32 = 100_000;

    fn new(p: &PdfPage) -> Self {
        let w = p.width().value.max(1.0);
        let h = p.height().value.max(1.0);
        let cfg = PdfRenderConfig::new().set_target_size(Self::N, (Self::N as f32 * h / w) as i32);
        let hpx = Self::N as f32 * h / w;
        let pt = |x: f32, y: f32| -> (f32, f32) {
            p.points_to_pixels(PdfPoints::new(x), PdfPoints::new(y), &cfg)
                .map(|(a, b)| (a as f32 / Self::N as f32, b as f32 / hpx))
                .unwrap_or((0.0, 0.0))
        };
        let o = pt(0.0, 0.0);
        let x = pt(1000.0, 0.0);
        let y = pt(0.0, 1000.0);
        PageMap {
            ox: o.0,
            oy: o.1,
            ax: ((x.0 - o.0) / 1000.0, (x.1 - o.1) / 1000.0),
            ay: ((y.0 - o.0) / 1000.0, (y.1 - o.1) / 1000.0),
        }
    }

    fn point(&self, x: f32, y: f32) -> (f32, f32) {
        (self.ox + self.ax.0 * x + self.ay.0 * y, self.oy + self.ax.1 * x + self.ay.1 * y)
    }

    fn rect(&self, r: &PdfRect) -> NRect {
        let a = self.point(r.left().value, r.top().value);
        let b = self.point(r.right().value, r.bottom().value);
        NRect { x0: a.0.min(b.0), y0: a.1.min(b.1), x1: a.0.max(b.0), y1: a.1.max(b.1) }
    }
}

fn doc_info(d: &PdfDocument) -> DocInfo {
    let page_sizes = d
        .pages()
        .page_sizes()
        .map(|v| v.iter().map(|r| (r.width().value, r.height().value)).collect())
        .unwrap_or_default();
    let mut budget = 20_000;
    let outline = d.bookmarks().root().map(|b| walk(b, 0, &mut budget)).unwrap_or_default();
    let title = d
        .metadata()
        .get(PdfDocumentMetadataTagType::Title)
        .map(|t| t.value().trim().to_string())
        .filter(|t| !t.is_empty());
    DocInfo { page_sizes, outline, title }
}

fn walk(first: PdfBookmark, depth: usize, budget: &mut usize) -> Vec<OutlineItem> {
    let mut out = Vec::new();
    let mut cur = Some(first);
    while let Some(b) = cur {
        if *budget == 0 {
            break;
        }
        *budget -= 1;
        let page = b
            .destination()
            .and_then(|d| d.page_index().ok())
            .or_else(|| match b.action() {
                Some(PdfAction::LocalDestination(a)) => a.destination().ok().and_then(|d| d.page_index().ok()),
                _ => None,
            })
            .map(|i| i as usize);
        let children = if depth < 32 {
            b.first_child().map(|c| walk(c, depth + 1, budget)).unwrap_or_default()
        } else {
            Vec::new()
        };
        out.push(OutlineItem { title: b.title().unwrap_or_default(), page, children });
        cur = b.next_sibling();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{lock, pdfium, sample};
    #[cfg(windows)]
    use nopepdf::print::{PrintRequest, open_printer};
    #[cfg(windows)]
    use std::time::Instant;

    /// A job for "Microsoft Print to PDF" writing to `out`; never a physical printer.
    #[cfg(windows)]
    fn pdf_job(pages: Vec<usize>, out: &std::path::Path) -> nopepdf::print::PrintJob {
        let _ = std::fs::remove_file(out);
        let req =
            PrintRequest { output: Some(out.to_path_buf()), ..PrintRequest::new("Microsoft Print to PDF", pages) };
        open_printer(&req).unwrap()
    }

    fn next_event(w: &WorkerHandle) -> Event {
        w.rx.recv_timeout(Duration::from_secs(60)).expect("no event from the worker")
    }

    fn open(w: &WorkerHandle, doc: DocId, path: PathBuf) -> Result<DocInfo, OpenError> {
        w.tx.send(Cmd::Open { doc, path, password: None }).unwrap();
        loop {
            match next_event(w) {
                Event::Opened { doc: d, info } if d == doc => return Ok(info),
                Event::OpenFailed { doc: d, error } if d == doc => return Err(error),
                _ => {}
            }
        }
    }

    /// Commands are handled in order, so once this answers, everything sent before it is done.
    /// Tests call it before dropping the worker, so PDFium is idle when the lock is released.
    fn finish(w: WorkerHandle) {
        let missing = std::env::temp_dir().join("nopepdf-no-such-file.pdf");
        let _ = open(&w, u64::MAX, missing);
    }

    #[test]
    fn open_errors_are_classified() {
        let _g = lock();
        let Some(pdfium) = pdfium() else { return };
        let w = spawn(pdfium, egui::Context::default());
        let missing = std::env::temp_dir().join("nopepdf-no-such-file.pdf");
        assert!(matches!(open(&w, 1, missing), Err(OpenError::Read(_))));
        let not_pdf = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        assert_eq!(open(&w, 2, not_pdf).err(), Some(OpenError::InvalidPdf));
        finish(w);
    }

    #[test]
    fn search_counts_hits() {
        let _g = lock();
        let (Some(input), Some(pdfium)) = (sample(), pdfium()) else { return };
        let w = spawn(pdfium, egui::Context::default());
        let info = open(&w, 1, input).unwrap();
        assert!(info.page_sizes.len() >= 3);
        w.tx.send(Cmd::Search { doc: 1, query: "attention".into(), generation: 7 }).unwrap();
        let mut hits = 0;
        loop {
            match next_event(&w) {
                Event::SearchHits { generation: 7, hits: h, .. } => hits += h.len(),
                Event::SearchProgress { generation: 7, done: true, .. } => break,
                _ => {}
            }
        }
        assert!(hits > 0);
        finish(w);
    }

    #[cfg(windows)]
    #[test]
    fn renders_while_printing() {
        let _g = lock();
        let (Some(input), Some(pdfium)) = (sample(), pdfium()) else { return };
        let w = spawn(pdfium, egui::Context::default());
        let n = open(&w, 1, input).unwrap().page_sizes.len();
        let out = std::env::temp_dir().join("nopepdf-worker-print-test.pdf");
        w.tx.send(Cmd::Print { doc: 1, job: pdf_job((0..n).collect(), &out) }).unwrap();
        // The view asks for a page while the job is running; it must not wait for the whole job.
        std::thread::sleep(Duration::from_millis(500));
        let render = Job::Render { doc: 1, page: n / 2, scale: 1.0, rotation: 0, invert: false, thumb: false };
        w.set_queue(vec![render]);
        let started = Instant::now();
        let mut rendered_during_print = false;
        let result = loop {
            match next_event(&w) {
                Event::Rendered { job, .. } if job == render => rendered_during_print = true,
                Event::PrintDone(r) => break r,
                _ => {}
            }
        };
        println!("printed {n} pages in {:?}", started.elapsed());
        assert_eq!(result, Ok(()));
        assert!(rendered_during_print, "the view render waited for the whole print job");
        let printed =
            crate::testutil::pdfium().unwrap().load_pdf_from_file(&out, None).map(|d| d.pages().len() as usize);
        assert_eq!(printed.ok(), Some(n));
        finish(w);
    }

    #[cfg(windows)]
    #[test]
    fn closing_the_document_cancels_printing() {
        let _g = lock();
        let (Some(input), Some(pdfium)) = (sample(), pdfium()) else { return };
        let w = spawn(pdfium, egui::Context::default());
        let n = open(&w, 1, input).unwrap().page_sizes.len();
        let out = std::env::temp_dir().join("nopepdf-worker-cancel-test.pdf");
        w.tx.send(Cmd::Print { doc: 1, job: pdf_job((0..n).collect(), &out) }).unwrap();
        w.tx.send(Cmd::Close { doc: 1 }).unwrap();
        let result = loop {
            if let Event::PrintDone(r) = next_event(&w) {
                break r;
            }
        };
        assert_eq!(result, Err(PrintError::Cancelled));
        finish(w);
    }

    #[cfg(windows)]
    #[test]
    fn printing_a_closed_document_fails() {
        let _g = lock();
        let Some(pdfium) = pdfium() else { return };
        let w = spawn(pdfium, egui::Context::default());
        let out = std::env::temp_dir().join("nopepdf-worker-notopen-test.pdf");
        w.tx.send(Cmd::Print { doc: 42, job: pdf_job(vec![0], &out) }).unwrap();
        let result = loop {
            if let Event::PrintDone(r) = next_event(&w) {
                break r;
            }
        };
        assert_eq!(result, Err(PrintError::NotOpen));
        finish(w);
    }
}
