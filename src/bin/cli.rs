//! `megadl`: a command-line downloader for public mega.nz links,
//! aiming for flag compatibility with megatools' `megadl`
//! (https://xff.cz/megatools/) rather than the Go megadl TUI's own
//! invocation style.
//!
//! Usage:
//!   megadl [--path=PATH] [--no-progress] [--print-names] URL...
//!   megadl --choose-files [--path=PATH] URL

use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use megadl::download::{sync_file, Sink};
use megadl::picker::{self, human_size};
use megadl::term;
use megadl::{api, resolve, Resolved};

struct Args {
    urls: Vec<String>,
    path: Option<PathBuf>,
    no_progress: bool,
    print_names: bool,
    choose_files: bool,
    /// Explicit selection (node handles), bypassing the picker.
    files: Option<Vec<String>>,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        urls: Vec::new(),
        path: None,
        no_progress: false,
        print_names: false,
        choose_files: false,
        files: None,
    };
    for arg in std::env::args().skip(1) {
        if let Some(v) = arg.strip_prefix("--path=") {
            a.path = Some(PathBuf::from(v));
        } else if let Some(v) = arg.strip_prefix("--files=") {
            a.files = Some(v.split(',').filter(|s| !s.is_empty()).map(str::to_string).collect());
        } else if arg == "--no-progress" {
            a.no_progress = true;
        } else if arg == "--print-names" {
            a.print_names = true;
        } else if arg == "--choose-files" {
            a.choose_files = true;
        } else if arg == "-h" || arg == "--help" {
            print_help();
            std::process::exit(0);
        } else if arg == "--version" {
            println!("megadl {}", env!("CARGO_PKG_VERSION"));
            std::process::exit(0);
        } else if arg.starts_with('-') {
            return Err(format!("unknown option: {arg}"));
        } else {
            a.urls.push(arg);
        }
    }
    if a.urls.is_empty() {
        return Err("no URL given".into());
    }
    Ok(a)
}

fn print_help() {
    println!(
        "megadl - download files and folders from mega.nz\n\n\
         Usage: megadl [OPTIONS] URL...\n\n\
         Options:\n  \
         --path=PATH       local destination directory or file name\n  \
         --choose-files    interactively pick which files to fetch from a folder link\n  \
         --files=H1,H2     fetch exactly these node handles (what --choose-files prints)\n  \
         --print-names     print each downloaded file's local path\n  \
         --no-progress     don't show a progress bar\n  \
         -h, --help        show this help"
    );
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("megadl: {e}");
            std::process::exit(1);
        }
    };

    let cancel = Arc::new(AtomicBool::new(false));
    {
        let cancel = cancel.clone();
        ctrlc_handler(move || cancel.store(true, Ordering::SeqCst));
    }

    let mut status = 0;
    for url in &args.urls {
        if let Err(e) = run_one(url, &args, &cancel) {
            eprintln!("megadl: {e}");
            status = 1;
        }
        if cancel.load(Ordering::SeqCst) {
            break;
        }
    }
    std::process::exit(status);
}

fn run_one(url: &str, args: &Args, cancel: &Arc<AtomicBool>) -> Result<(), String> {
    let resolved = resolve(url, api::DEFAULT_API_URL)?;

    let selected: Vec<String> = match &args.files {
        Some(handles) => handles.clone(),
        None if args.choose_files => {
            let picked = choose_files(&resolved)?;
            print_replay_command(url, args, &picked, &resolved);
            picked
        }
        None => Vec::new(),
    };

    let dest_dir = match &args.path {
        Some(p) => p.clone(),
        None => PathBuf::from("."),
    };
    if let Some(p) = &args.path {
        if !p.exists() && matches!(resolved, Resolved::Folder { .. }) {
            std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
        }
    }

    let jobs = resolved.plan(&dest_dir, &selected)?;
    if jobs.is_empty() {
        eprintln!("megadl: nothing selected to download");
        return Ok(());
    }

    let cancel_fn = || cancel.load(Ordering::SeqCst);
    for job in &jobs {
        let mut sink = CliSink { quiet: args.no_progress, print_names: args.print_names };
        if !sync_file(job, &mut sink, &cancel_fn) && !cancel_fn() {
            return Err(format!("failed to download {}", job.local_path.display()));
        }
        if cancel_fn() {
            break;
        }
    }
    Ok(())
}

/// Opens the full-screen checkbox picker over a link's contents.
/// Returns the chosen file handles, or an error if the user cancelled.
/// Falls back to a plain numbered prompt when stdin isn't a terminal.
fn choose_files(resolved: &Resolved) -> Result<Vec<String>, String> {
    let entries = resolved.listing();
    if entries.iter().all(|e| e.is_dir) {
        return Err("link has no files".into());
    }
    if !is_tty() {
        return choose_files_plain(&entries);
    }

    let rows = picker::rows_from_listing(&entries);
    let title = " megadl · choose files".to_string();

    let _raw = term::RawMode::enable().map_err(|e| e.to_string())?;
    term::enter_alt_screen();
    let picked = picker::pick(&rows, &title, &mut || term::read_key().ok());
    term::leave_alt_screen();
    drop(_raw);

    match picked {
        Some(handles) if handles.is_empty() => Err("nothing selected".into()),
        Some(handles) => Ok(handles),
        None => Err("cancelled".into()),
    }
}

/// Non-interactive fallback: list files, read indices from stdin.
fn choose_files_plain(entries: &[megadl::Entry]) -> Result<Vec<String>, String> {
    let files: Vec<_> = entries.iter().filter(|e| !e.is_dir).collect();
    println!("Files in this link:");
    for f in &files {
        println!("  {:>4}  {:>10}  {}", f.index, human_size(f.size), f.path);
    }
    print!("Enter numbers to download (blank = all): ");
    io::stdout().flush().ok();
    let mut line = String::new();
    io::stdin().read_line(&mut line).map_err(|e| e.to_string())?;
    let line = line.trim();
    if line.is_empty() {
        return Ok(Vec::new());
    }
    let mut handles = Vec::new();
    for tok in line.split(|c: char| c == ',' || c.is_whitespace()).filter(|s| !s.is_empty()) {
        let n: usize = tok.parse().map_err(|_| format!("not a number: {tok}"))?;
        let entry = files.iter().find(|f| f.index == n).ok_or_else(|| format!("no such file: {n}"))?;
        handles.push(entry.handle.clone());
    }
    Ok(handles)
}

fn is_tty() -> bool {
    unsafe { libc::isatty(libc::STDIN_FILENO) == 1 }
}

/// After an interactive pick, echo the equivalent non-interactive
/// command. Re-running it resumes the same selection without walking
/// the picker again, since partials persist and node handles are stable.
fn print_replay_command(url: &str, args: &Args, picked: &[String], resolved: &Resolved) {
    let names: Vec<String> = {
        let entries = resolved.listing();
        picked
            .iter()
            .filter_map(|h| entries.iter().find(|e| &e.handle == h))
            .map(|e| e.name.clone())
            .collect()
    };
    let total: i64 = {
        let entries = resolved.listing();
        picked
            .iter()
            .filter_map(|h| entries.iter().find(|e| &e.handle == h))
            .map(|e| e.size)
            .sum()
    };

    let program = std::env::args().next().unwrap_or_else(|| "megadl".into());
    let mut cmd = format!("{} --files={}", shell_quote(&program), picked.join(","));
    if let Some(p) = &args.path {
        cmd.push_str(&format!(" --path={}", shell_quote(&p.display().to_string())));
    }
    if args.print_names {
        cmd.push_str(" --print-names");
    }
    if args.no_progress {
        cmd.push_str(" --no-progress");
    }
    cmd.push(' ');
    cmd.push_str(&shell_quote(url));

    eprintln!();
    eprintln!("Selected {} file(s), {}:", picked.len(), human_size(total));
    for n in names.iter().take(10) {
        eprintln!("  - {n}");
    }
    if names.len() > 10 {
        eprintln!("  … and {} more", names.len() - 10);
    }
    eprintln!();
    eprintln!("To resume this download with the current selection, you can just run this command:");
    eprintln!();
    eprintln!("  {cmd}");
    eprintln!();
}

/// Single-quotes an argument for POSIX shells (URLs carry `#`, paths
/// carry spaces).
fn shell_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "._-/=:".contains(c)) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', r"'\''"))
}

struct CliSink {
    quiet: bool,
    print_names: bool,
}

impl Sink for CliSink {
    fn skip(&mut self, path: &std::path::Path) {
        eprintln!("megadl: skipping existing file {}", path.display());
    }
    fn start(&mut self, path: &std::path::Path, _remote: &str, size: i64) {
        if !self.quiet {
            eprintln!("Downloading {} ({})", path.display(), human_size(size));
        }
    }
    fn resume(&mut self, from: i64, total: i64) {
        if !self.quiet {
            let pct = if total > 0 { from as f64 / total as f64 * 100.0 } else { 0.0 };
            eprintln!("Resuming at {} ({pct:.1}% already on disk)", human_size(from));
        }
    }
    fn progress(&mut self, done: i64, total: i64) {
        if self.quiet {
            return;
        }
        let pct = if total > 0 { done as f64 / total as f64 * 100.0 } else { 100.0 };
        eprint!("\r{:>6.1}%  {} / {}   ", pct, human_size(done), human_size(total));
        io::stderr().flush().ok();
    }
    fn verifying(&mut self) {
        if !self.quiet {
            eprint!("\rverifying integrity…                    ");
            io::stderr().flush().ok();
        }
    }
    fn retry(&mut self, reason: &str, _detail: &str, attempt: u32, delay: Duration) {
        eprintln!("\nmegadl: {reason}, retrying in {}s (attempt {attempt})", delay.as_secs());
    }
    fn done(&mut self, path: &std::path::Path) {
        if !self.quiet {
            eprintln!();
        }
        if self.print_names {
            println!("{}", path.display());
        }
    }
    fn error(&mut self, _path: &std::path::Path, message: &str) {
        eprintln!("\nmegadl: {message}");
    }
}

/// Minimal SIGINT handler with no extra dependency: a small signal
/// trampoline via libc.
fn ctrlc_handler<F: Fn() + Send + Sync + 'static>(f: F) {
    use std::sync::OnceLock;
    static HANDLER: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();
    let _ = HANDLER.set(Box::new(f));
    extern "C" fn on_sigint(_: libc::c_int) {
        if let Some(h) = HANDLER.get() {
            h();
        }
    }
    unsafe {
        libc::signal(libc::SIGINT, on_sigint as *const () as libc::sighandler_t);
    }
}
