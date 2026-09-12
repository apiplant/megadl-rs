//! `megadl-tui`: a terminal download manager for mega.nz links.
//!
//! Deliberately a different shape from the Go megadl's Bubble Tea app
//! (no SQLite history, no mouse, no folding file tree) — a queue you
//! feed links into, downloaded one at a time, rendered with plain ANSI
//! and no TUI framework.
//!
//! All keystrokes arrive through one reader thread and a single channel,
//! including those consumed by the modal URL input and the file picker.
//! Nothing else may read stdin, or the two readers race and swallow each
//! other's keys.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use megadl::download::{sync_file, Sink};
use megadl::picker::{self, human_size};
use megadl::term::{self, Key};
use megadl::{api, resolve, Resolved};

const ACCENT: &str = "\x1b[38;5;44m";
const DIM: &str = "\x1b[90m";
const GREEN: &str = "\x1b[32m";
const RED: &str = "\x1b[31m";
const YELLOW: &str = "\x1b[33m";
const BOLD: &str = "\x1b[1m";
const OFF: &str = "\x1b[0m";

#[derive(Clone)]
enum Status {
    Queued,
    Active { done: i64, total: i64, rate: f64, note: String },
    Verifying,
    Done,
    Error(String),
}

struct Item {
    id: u64,
    dest: PathBuf,
    name: String,
    files: usize,
    status: Status,
    last_sample: Option<(Instant, i64)>,
}

enum Event {
    Key(Key),
    Progress { id: u64, done: i64, total: i64 },
    Verifying { id: u64 },
    Note { id: u64, note: String },
    Started { id: u64, name: String },
    Finished { id: u64 },
    Failed { id: u64, message: String },
}

/// What the worker thread pulls from. The condvar means the worker
/// parks without holding the mutex — a sleep inside the lock would
/// starve the UI thread trying to enqueue.
struct Queue {
    pending: Mutex<VecDeque<Job>>,
    ready: Condvar,
}

impl Queue {
    fn push(&self, job: Job) {
        self.pending.lock().unwrap().push_back(job);
        self.ready.notify_one();
    }

    fn remove(&self, id: u64) {
        self.pending.lock().unwrap().retain(|j| j.id != id);
    }

    /// Blocks until a job is available or `quit` is set.
    fn pop(&self, quit: &AtomicBool) -> Option<Job> {
        let mut pending = self.pending.lock().unwrap();
        loop {
            if quit.load(Ordering::SeqCst) {
                return None;
            }
            if let Some(job) = pending.pop_front() {
                return Some(job);
            }
            // Waiting releases the mutex until someone enqueues.
            let (guard, _) = self.ready.wait_timeout(pending, Duration::from_millis(200)).unwrap();
            pending = guard;
        }
    }
}

#[derive(Clone)]
struct Job {
    id: u64,
    url: String,
    dest: PathBuf,
    selected: Vec<String>,
}

enum Mode {
    List,
    AskUrl { buf: String },
    AskDest { url: String, buf: String },
    Message(String),
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("megadl-tui {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    let (tx, rx): (Sender<Event>, Receiver<Event>) = channel();
    let quit = Arc::new(AtomicBool::new(false));
    let queue = Arc::new(Queue { pending: Mutex::new(VecDeque::new()), ready: Condvar::new() });

    // The one and only stdin reader.
    {
        let tx = tx.clone();
        thread::spawn(move || {
            while let Ok(k) = term::read_key() {
                if tx.send(Event::Key(k)).is_err() {
                    return;
                }
            }
        });
    }

    {
        let queue = queue.clone();
        let tx = tx.clone();
        let quit = quit.clone();
        thread::spawn(move || worker_loop(queue, tx, quit));
    }

    let _raw = term::RawMode::enable().expect("enable raw mode");
    term::enter_alt_screen();

    let mut items: Vec<Item> = Vec::new();
    let mut cursor = 0usize;
    let mut mode = Mode::List;
    let mut next_id = 1u64;
    let mut deferred: Vec<Event> = Vec::new();

    render(&items, cursor, &mode);

    'outer: loop {
        for ev in deferred.drain(..) {
            apply(&mut items, ev);
        }

        let ev = match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(ev) => ev,
            Err(RecvTimeoutError::Timeout) => {
                render(&items, cursor, &mode);
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => break 'outer,
        };

        let key = match ev {
            Event::Key(k) => k,
            other => {
                apply(&mut items, other);
                render(&items, cursor, &mode);
                continue;
            }
        };

        match &mut mode {
            Mode::Message(_) => mode = Mode::List,
            Mode::AskUrl { buf } => match key {
                Key::Esc | Key::CtrlC => mode = Mode::List,
                Key::Enter => {
                    let url = buf.trim().to_string();
                    mode = if url.is_empty() {
                        Mode::List
                    } else {
                        Mode::AskDest { url, buf: String::new() }
                    };
                }
                Key::Backspace => {
                    buf.pop();
                }
                Key::Char(c) => buf.push(c),
                _ => {}
            },
            Mode::AskDest { url, buf } => match key {
                Key::Esc | Key::CtrlC => mode = Mode::List,
                Key::Backspace => {
                    buf.pop();
                }
                Key::Char(c) => buf.push(c),
                Key::Enter => {
                    let url = url.clone();
                    let dest = if buf.trim().is_empty() { PathBuf::from(".") } else { PathBuf::from(buf.trim()) };
                    mode = Mode::Message("resolving link…".into());
                    render(&items, cursor, &mode);

                    match resolve(&url, api::DEFAULT_API_URL) {
                        Err(e) => mode = Mode::Message(format!("{RED}{e}{OFF}  (any key)")),
                        Ok(resolved) => {
                            let name = megadl::default_dest_name(&resolved);
                            let entries = resolved.listing();
                            let file_count = entries.iter().filter(|e| !e.is_dir).count();

                            // Folder links get the picker; a single file
                            // needs no choosing.
                            let selected = if matches!(resolved, Resolved::Folder { .. }) && file_count > 1 {
                                let rows = picker::rows_from_listing(&entries);
                                let title = format!(" megadl · choose files from {name}");
                                let mut src = || pull_key(&rx, &mut deferred);
                                match picker::pick(&rows, &title, &mut src) {
                                    Some(sel) => Some(sel),
                                    None => None, // cancelled
                                }
                            } else {
                                Some(Vec::new())
                            };

                            match selected {
                                None => mode = Mode::List,
                                Some(sel) => {
                                    let picked = if sel.is_empty() { file_count } else { sel.len() };
                                    if picked == 0 {
                                        mode = Mode::Message(format!("{YELLOW}nothing selected{OFF}  (any key)"));
                                    } else {
                                        let id = next_id;
                                        next_id += 1;
                                        items.push(Item {
                                            id,
                                            dest: dest.clone(),
                                            name,
                                            files: picked,
                                            status: Status::Queued,
                                            last_sample: None,
                                        });
                                        queue.push(Job { id, url, dest, selected: sel });
                                        cursor = items.len() - 1;
                                        mode = Mode::List;
                                    }
                                }
                            }
                        }
                    }
                }
                _ => {}
            },
            Mode::List => match key {
                Key::CtrlC | Key::Char('q') => break 'outer,
                Key::Up | Key::Char('k') => cursor = cursor.saturating_sub(1),
                Key::Down | Key::Char('j') => {
                    if cursor + 1 < items.len() {
                        cursor += 1;
                    }
                }
                Key::Char('a') => mode = Mode::AskUrl { buf: String::new() },
                Key::Char('d') => {
                    if let Some(it) = items.get(cursor) {
                        if !matches!(it.status, Status::Active { .. } | Status::Verifying) {
                            let id = it.id;
                            items.remove(cursor);
                            queue.remove(id);
                            if cursor > 0 && cursor >= items.len() {
                                cursor -= 1;
                            }
                        }
                    }
                }
                _ => {}
            },
        }
        render(&items, cursor, &mode);
    }

    quit.store(true, Ordering::SeqCst);
    term::leave_alt_screen();
}

/// Blocks for the next keypress, stashing any worker events that arrive
/// meanwhile so the caller can apply them once it regains control.
fn pull_key(rx: &Receiver<Event>, deferred: &mut Vec<Event>) -> Option<Key> {
    loop {
        match rx.recv() {
            Ok(Event::Key(k)) => return Some(k),
            Ok(other) => deferred.push(other),
            Err(_) => return None,
        }
    }
}

fn apply(items: &mut [Item], ev: Event) {
    match ev {
        Event::Key(_) => {}
        Event::Started { id, name } => {
            if let Some(it) = find(items, id) {
                it.name = name;
                it.status = Status::Active { done: 0, total: 0, rate: 0.0, note: String::new() };
                it.last_sample = None;
            }
        }
        Event::Verifying { id } => {
            if let Some(it) = find(items, id) {
                it.status = Status::Verifying;
            }
        }
        Event::Progress { id, done, total } => {
            if let Some(it) = find(items, id) {
                let done = done.max(0);
                let note = match &it.status {
                    Status::Active { note, .. } => note.clone(),
                    _ => String::new(),
                };
                // Smooth the rate so the readout doesn't jitter.
                let mut rate = match &it.status {
                    Status::Active { rate, .. } => *rate,
                    _ => 0.0,
                };
                if let Some((t0, b0)) = it.last_sample {
                    let dt = t0.elapsed().as_secs_f64();
                    if dt >= 0.4 {
                        let sample = (done - b0) as f64 / dt;
                        rate = if rate == 0.0 { sample } else { rate * 0.7 + sample * 0.3 };
                        it.last_sample = Some((Instant::now(), done));
                    }
                } else {
                    it.last_sample = Some((Instant::now(), done));
                }
                it.status = Status::Active { done, total, rate, note };
            }
        }
        Event::Note { id, note } => {
            if let Some(it) = find(items, id) {
                if let Status::Active { done, total, rate, .. } = it.status {
                    it.status = Status::Active { done, total, rate, note };
                }
            }
        }
        Event::Finished { id } => {
            if let Some(it) = find(items, id) {
                it.status = Status::Done;
            }
        }
        Event::Failed { id, message } => {
            if let Some(it) = find(items, id) {
                it.status = Status::Error(message);
            }
        }
    }
}

fn find(items: &mut [Item], id: u64) -> Option<&mut Item> {
    items.iter_mut().find(|i| i.id == id)
}

fn worker_loop(queue: Arc<Queue>, tx: Sender<Event>, quit: Arc<AtomicBool>) {
    loop {
        if quit.load(Ordering::SeqCst) {
            return;
        }
        let job = match queue.pop(&quit) {
            Some(j) => j,
            None => return,
        };

        let resolved = match resolve(&job.url, api::DEFAULT_API_URL) {
            Ok(r) => r,
            Err(e) => {
                let _ = tx.send(Event::Failed { id: job.id, message: e });
                continue;
            }
        };
        let _ = tx.send(Event::Started { id: job.id, name: megadl::default_dest_name(&resolved) });

        let jobs = match resolved.plan(&job.dest, &job.selected) {
            Ok(j) => j,
            Err(e) => {
                let _ = tx.send(Event::Failed { id: job.id, message: e });
                continue;
            }
        };

        let q = quit.clone();
        let cancel = move || q.load(Ordering::SeqCst);
        let mut ok = true;
        // Progress is reported across the whole item, not per file, so a
        // multi-file link shows one bar that advances to the end.
        let grand_total: i64 = jobs.iter().map(|j| j.size).sum();
        let mut completed: i64 = 0;
        for one in &jobs {
            let mut sink = TuiSink {
                id: job.id,
                tx: tx.clone(),
                base: completed,
                grand_total,
                last: Instant::now() - Duration::from_secs(1),
            };
            if !sync_file(one, &mut sink, &cancel) {
                ok = false;
                break;
            }
            completed += one.size;
        }
        if ok {
            let _ = tx.send(Event::Finished { id: job.id });
        }
        if quit.load(Ordering::SeqCst) {
            return;
        }
    }
}

struct TuiSink {
    id: u64,
    tx: Sender<Event>,
    base: i64,
    grand_total: i64,
    last: Instant,
}

impl Sink for TuiSink {
    fn verifying(&mut self) {
        let _ = self.tx.send(Event::Verifying { id: self.id });
    }
    fn resume(&mut self, from: i64, _total: i64) {
        let _ = self.tx.send(Event::Note { id: self.id, note: format!("resuming at {}", human_size(from)) });
    }
    fn progress(&mut self, done: i64, _total: i64) {
        if self.last.elapsed() < Duration::from_millis(150) {
            return;
        }
        self.last = Instant::now();
        let _ = self.tx.send(Event::Progress { id: self.id, done: self.base + done, total: self.grand_total });
    }
    fn retry(&mut self, reason: &str, _detail: &str, attempt: u32, delay: Duration) {
        let note = format!("{reason} · retry {attempt} in {}s", delay.as_secs());
        let _ = self.tx.send(Event::Note { id: self.id, note });
    }
    fn skip(&mut self, _path: &std::path::Path) {
        let _ = self.tx.send(Event::Note { id: self.id, note: "already on disk".into() });
    }
    fn error(&mut self, _path: &std::path::Path, message: &str) {
        let _ = self.tx.send(Event::Failed { id: self.id, message: message.to_string() });
    }
}

fn bar(frac: f64, width: usize) -> String {
    let frac = frac.clamp(0.0, 1.0);
    let filled = (frac * width as f64).floor() as usize;
    let rem = frac * width as f64 - filled as f64;
    let partial = match (rem * 8.0) as usize {
        0 => "",
        1 => "\u{258F}",
        2 => "\u{258E}",
        3 => "\u{258D}",
        4 => "\u{258C}",
        5 => "\u{258B}",
        6 => "\u{258A}",
        _ => "\u{2589}",
    };
    let mut s = "\u{2588}".repeat(filled.min(width));
    if filled < width {
        s.push_str(partial);
    }
    let used = filled + if partial.is_empty() { 0 } else { 1 };
    s.push_str(&"\u{2591}".repeat(width.saturating_sub(used)));
    s
}

fn render(items: &[Item], cursor: usize, mode: &Mode) {
    let (cols, rows) = term::terminal_size();
    let width = (cols as usize).clamp(48, 160);
    let mut out = String::new();
    out.push_str("\x1b[H\x1b[2J");

    // Header
    let active = items.iter().filter(|i| matches!(i.status, Status::Active { .. } | Status::Verifying)).count();
    let done = items.iter().filter(|i| matches!(i.status, Status::Done)).count();
    let failed = items.iter().filter(|i| matches!(i.status, Status::Error(_))).count();
    let summary = format!("{active} active · {done} done · {failed} failed");
    out.push_str(&format!("{ACCENT}\u{256D}{}\u{256E}{OFF}\r\n", "\u{2500}".repeat(width - 2)));
    let title = format!("{BOLD}megadl{OFF}{DIM} · mega.nz download manager{OFF}");
    let pad = width.saturating_sub(2 + 6 + 28 + summary.len());
    out.push_str(&format!(
        "{ACCENT}\u{2502}{OFF} {title}{}{DIM}{summary}{OFF} {ACCENT}\u{2502}{OFF}\r\n",
        " ".repeat(pad)
    ));
    out.push_str(&format!("{ACCENT}\u{2570}{}\u{256F}{OFF}\r\n", "\u{2500}".repeat(width - 2)));

    // Body
    if items.is_empty() {
        out.push_str(&format!("\r\n  {DIM}queue is empty — press{OFF} {BOLD}a{OFF} {DIM}to add a mega.nz link{OFF}\r\n"));
    }
    let room = (rows as usize).saturating_sub(9) / 2;
    for (i, it) in items.iter().enumerate().take(room.max(1)) {
        let sel = i == cursor;
        let pointer = if sel { format!("{ACCENT}\u{25B8}{OFF}") } else { " ".into() };
        let (glyph, name_color) = match &it.status {
            Status::Queued => (format!("{DIM}\u{22EF}{OFF}"), DIM),
            Status::Active { .. } => (format!("{ACCENT}\u{25BC}{OFF}"), ""),
            Status::Verifying => (format!("{YELLOW}\u{2726}{OFF}"), ""),
            Status::Done => (format!("{GREEN}\u{2713}{OFF}"), DIM),
            Status::Error(_) => (format!("{RED}\u{2717}{OFF}"), RED),
        };
        let files = if it.files > 1 { format!("{DIM} ({} files){OFF}", it.files) } else { String::new() };
        out.push_str(&format!("\r\n {pointer} {glyph} {name_color}{}{OFF}{files}\r\n", it.name));

        let bar_width = width.saturating_sub(34).clamp(10, 60);
        match &it.status {
            Status::Queued => {
                out.push_str(&format!("     {DIM}waiting · \u{2192} {}{OFF}\r\n", it.dest.display()));
            }
            Status::Active { done, total, rate, note } => {
                let frac = if *total > 0 { *done as f64 / *total as f64 } else { 0.0 };
                let speed = if *rate > 1.0 { format!("{}/s", human_size(*rate as i64)) } else { "—".into() };
                out.push_str(&format!(
                    "     {ACCENT}{}{OFF} {:>3.0}%  {DIM}{} / {}  {speed}{OFF}\r\n",
                    bar(frac, bar_width),
                    frac * 100.0,
                    human_size(*done),
                    human_size(*total)
                ));
                if !note.is_empty() {
                    out.push_str(&format!("     {YELLOW}{note}{OFF}\r\n"));
                }
            }
            Status::Verifying => {
                out.push_str(&format!("     {YELLOW}verifying integrity (MAC)…{OFF}\r\n"));
            }
            Status::Done => {
                out.push_str(&format!("     {GREEN}{}{OFF} {DIM}complete \u{2192} {}{OFF}\r\n", bar(1.0, bar_width), it.dest.display()));
            }
            Status::Error(msg) => {
                out.push_str(&format!("     {RED}{}{OFF}\r\n", truncate(msg, width.saturating_sub(8))));
            }
        }
    }

    // Modal / footer
    out.push_str("\r\n");
    match mode {
        Mode::AskUrl { buf } => out.push_str(&modal("paste a mega.nz link", buf, width, "enter next · esc cancel")),
        Mode::AskDest { buf, .. } => out.push_str(&modal("destination directory", buf, width, "enter start · esc cancel (blank = .)")),
        Mode::Message(m) => out.push_str(&format!(" {m}\r\n")),
        Mode::List => out.push_str(&format!(
            " {BOLD}a{OFF}{DIM} add{OFF}   {BOLD}d{OFF}{DIM} remove{OFF}   \
             {BOLD}\u{2191}\u{2193}/jk{OFF}{DIM} move{OFF}   {BOLD}q{OFF}{DIM} quit{OFF}\r\n"
        )),
    }

    let mut stdout = io::stdout();
    stdout.write_all(out.as_bytes()).ok();
    stdout.flush().ok();
}

fn modal(label: &str, buf: &str, width: usize, hint: &str) -> String {
    let inner = width.saturating_sub(4);
    let shown = if buf.chars().count() > inner.saturating_sub(2) {
        buf.chars().skip(buf.chars().count() - inner.saturating_sub(2)).collect::<String>()
    } else {
        buf.to_string()
    };
    let mut s = String::new();
    s.push_str(&format!("{ACCENT}\u{256D}\u{2500} {BOLD}{label}{OFF} {ACCENT}{}\u{256E}{OFF}\r\n",
        "\u{2500}".repeat(width.saturating_sub(label.len() + 6))));
    s.push_str(&format!("{ACCENT}\u{2502}{OFF} {shown}{ACCENT}\u{2588}{OFF}{}{ACCENT}\u{2502}{OFF}\r\n",
        " ".repeat(inner.saturating_sub(shown.chars().count() + 1))));
    s.push_str(&format!("{ACCENT}\u{2570}{}\u{256F}{OFF}\r\n", "\u{2500}".repeat(width - 2)));
    s.push_str(&format!(" {DIM}{hint}{OFF}\r\n"));
    s
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max || max == 0 {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('\u{2026}');
        t
    }
}
