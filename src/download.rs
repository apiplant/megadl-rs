//! Resumable, MAC-verified file download. Partial files persist as
//! `.megatmp.<handle>` next to the target and resume: the bytes already
//! on disk are plaintext, so a restart re-feeds them through the
//! chunked MAC and asks the server only for the remainder.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use aes::cipher::StreamCipher;

use crate::crypto::{ctr_at, ChunkedMac, FileKey};
use crate::http;

const FETCH_CHUNK_SIZE: i64 = 256 << 20;
const IDLE_TIMEOUT_SECS: u64 = 120;
const RETRY_WINDOW: Duration = Duration::from_secs(640 * 60);

/// One file to place at `local_path`.
pub struct FileJob<'a> {
    pub local_path: PathBuf,
    pub remote_path: String,
    pub size: i64,
    pub handle: String,
    pub key: FileKey,
    /// Resolves (or re-resolves, on retry) the transfer URL and size.
    pub get_url: Box<dyn Fn() -> Result<(String, i64), String> + 'a>,
}

/// Progress/lifecycle callbacks the CLI and TUI hook into.
#[allow(unused_variables)]
pub trait Sink {
    fn skip(&mut self, path: &Path) {}
    fn start(&mut self, path: &Path, remote: &str, size: i64) {}
    /// A partial was found on disk and `from` bytes are being reused.
    fn resume(&mut self, from: i64, total: i64) {}
    /// Absolute progress: `done` counts every byte on disk, including
    /// bytes carried over by a resume, against the full file size. It
    /// must stay absolute or a resumed transfer looks like a restart.
    fn progress(&mut self, done: i64, total: i64) {}
    fn verifying(&mut self) {}
    fn retry(&mut self, reason: &str, detail: &str, attempt: u32, delay: Duration) {}
    fn done(&mut self, path: &Path) {}
    fn error(&mut self, path: &Path, message: &str) {}
}

/// Downloads one file and emits lifecycle events around it via `sink`.
/// Existing files are skipped. Returns true on success.
pub fn sync_file(job: &FileJob, sink: &mut dyn Sink, cancel: &dyn Fn() -> bool) -> bool {
    if fs::symlink_metadata(&job.local_path).is_ok() {
        sink.skip(&job.local_path);
        return true;
    }
    if let Some(dir) = job.local_path.parent() {
        if !dir.as_os_str().is_empty() {
            if let Err(e) = fs::create_dir_all(dir) {
                sink.error(&job.local_path, &e.to_string());
                return false;
            }
        }
    }
    sink.start(&job.local_path, &job.remote_path, job.size);
    match fetch_file(job, sink, cancel) {
        Ok(()) => {
            sink.done(&job.local_path);
            true
        }
        // Cancelled mid-file: no error event, the partial stays resumable.
        Err(FetchErr::Cancelled) => false,
        Err(e) => {
            sink.error(&job.local_path, &e.message());
            false
        }
    }
}

enum FetchErr {
    Cancelled,
    /// Transient: worth another attempt (connection dropped, 5xx, stall).
    Retry(String),
    /// Permanent: retrying cannot help (local I/O, bad URL, bad key).
    Fatal(String),
}

impl FetchErr {
    fn message(&self) -> String {
        match self {
            FetchErr::Cancelled => "cancelled".into(),
            FetchErr::Retry(m) | FetchErr::Fatal(m) => m.clone(),
        }
    }
}

fn tmp_path(job: &FileJob) -> PathBuf {
    let dir = job.local_path.parent().unwrap_or_else(|| Path::new("."));
    dir.join(format!(".megatmp.{}", job.handle))
}

fn fetch_file(job: &FileJob, sink: &mut dyn Sink, cancel: &dyn Fn() -> bool) -> Result<(), FetchErr> {
    let (url, size) = (job.get_url)().map_err(FetchErr::Retry)?;

    let tmp = tmp_path(job);
    let mut f = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&tmp)
        .map_err(|e| FetchErr::Fatal(format!("can't open {}: {e}", tmp.display())))?;

    let mut mac = ChunkedMac::new(job.key.aes, job.key.nonce);

    // Resume: everything already in the partial is plaintext, so feed it
    // through the MAC to rebuild the running state, then fetch the rest.
    let mut pos: i64 = 0;
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| FetchErr::Fatal(format!("can't read {} for resume: {e}", tmp.display())))?;
        if n == 0 {
            break;
        }
        mac.update(&buf[..n]);
        pos += n as i64;
    }
    if pos > size {
        return Err(FetchErr::Fatal(format!(
            "unfinished download {} is larger than the remote file (remove it to fix this)",
            tmp.display()
        )));
    }

    if pos > 0 {
        sink.resume(pos, size);
    }
    sink.progress(pos, size);

    let deadline = Instant::now() + RETRY_WINDOW;
    let mut tries: u32 = 0;
    let mut last_report = Instant::now() - Duration::from_secs(1);

    while pos < size {
        let before = pos;
        let end = size.min(pos + FETCH_CHUNK_SIZE);
        let attempt = fetch_range(job, &url, &mut f, &mut mac, &mut pos, end, size, cancel, sink, &mut last_report);
        match attempt {
            Ok(()) => {
                tries = 0;
            }
            Err(FetchErr::Cancelled) => return Err(FetchErr::Cancelled),
            Err(FetchErr::Fatal(msg)) => return Err(FetchErr::Fatal(msg)),
            Err(FetchErr::Retry(msg)) => {
                if cancel() {
                    return Err(FetchErr::Cancelled);
                }
                if Instant::now() > deadline {
                    return Err(FetchErr::Fatal(format!("data download failed: {msg}")));
                }
                // Progress was made, so don't escalate the backoff.
                if pos > before {
                    tries = 0;
                }
                if tries < 8 {
                    tries += 1;
                }
                let delay = Duration::from_secs(1u64 << tries);
                sink.retry(&retry_reason(&msg), &msg, tries, delay);
                if !wait_or_cancel(delay, cancel) {
                    return Err(FetchErr::Cancelled);
                }
            }
        }
    }

    sink.verifying();

    if mac.finish() != job.key.meta_mac {
        drop(f);
        let _ = fs::remove_file(&tmp);
        return Err(FetchErr::Fatal("MAC mismatch".into()));
    }
    f.sync_all().map_err(|e| FetchErr::Fatal(e.to_string()))?;
    drop(f);
    fs::rename(&tmp, &job.local_path).map_err(|e| FetchErr::Fatal(e.to_string()))?;
    Ok(())
}

fn retry_reason(msg: &str) -> String {
    if msg.contains("509") {
        "transfer quota exceeded".into()
    } else if msg.contains("500") || msg.contains("502") || msg.contains("503") || msg.contains("504") {
        "server busy".into()
    } else if msg.contains("no data") {
        "no data from server".into()
    } else {
        "connection failed".into()
    }
}

fn wait_or_cancel(delay: Duration, cancel: &dyn Fn() -> bool) -> bool {
    let step = Duration::from_millis(200);
    let mut waited = Duration::ZERO;
    while waited < delay {
        if cancel() {
            return false;
        }
        let s = step.min(delay - waited);
        thread::sleep(s);
        waited += s;
    }
    !cancel()
}

/// Streams `[*pos, end)` of the encrypted file, decrypting, MAC-ing and
/// persisting as it goes; `*pos` tracks every byte safely on disk so a
/// retry continues exactly where this attempt stopped.
#[allow(clippy::too_many_arguments)]
fn fetch_range(
    job: &FileJob,
    url: &str,
    f: &mut File,
    mac: &mut ChunkedMac,
    pos: &mut i64,
    end: i64,
    size: i64,
    cancel: &dyn Fn() -> bool,
    sink: &mut dyn Sink,
    last_report: &mut Instant,
) -> Result<(), FetchErr> {
    let from = *pos;
    let range_url = format!("{url}/{}-{}", from, end - 1);
    f.seek(SeekFrom::Start(from as u64))
        .map_err(|e| FetchErr::Fatal(format!("seek {}: {e}", job.local_path.display())))?;

    let resp = match http::post(&range_url, &[], b"", true) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::InvalidInput => {
            return Err(FetchErr::Fatal(e.to_string()));
        }
        Err(e) => return Err(FetchErr::Retry(e.to_string())),
    };
    if resp.status != 200 && resp.status != 201 {
        return Err(FetchErr::Retry(http_status_msg(resp.status)));
    }
    let mut body = resp
        .body
        .into_reader()
        .ok_or_else(|| FetchErr::Fatal("expected a streaming body".into()))?;

    let mut stream = ctr_at(job.key.aes, job.key.nonce, from as u64);
    let mut buf = vec![0u8; 128 * 1024];
    loop {
        if cancel() {
            return Err(FetchErr::Cancelled);
        }
        let n = match body.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                return Err(FetchErr::Retry(format!("no data received for {IDLE_TIMEOUT_SECS}s")));
            }
            Err(e) => return Err(FetchErr::Retry(e.to_string())),
        };
        stream.apply_keystream(&mut buf[..n]);
        f.write_all(&buf[..n])
            .map_err(|e| FetchErr::Fatal(format!("write {}: {e}", job.local_path.display())))?;
        mac.update(&buf[..n]);
        *pos += n as i64;
        if last_report.elapsed() >= Duration::from_millis(250) {
            *last_report = Instant::now();
            sink.progress(*pos, size);
        }
    }
    if *pos != end {
        // Flush what we did get so a retry resumes from it.
        let _ = f.flush();
        return Err(FetchErr::Retry(format!(
            "server closed the connection early ({} of {} bytes)",
            *pos - from,
            end - from
        )));
    }
    sink.progress(*pos, size);
    Ok(())
}

fn http_status_msg(status: u16) -> String {
    match status {
        509 => "Server returned 509 (over quota)".into(),
        500 => "Server returned 500 (probably busy)".into(),
        other => format!("Server returned {other}"),
    }
}
