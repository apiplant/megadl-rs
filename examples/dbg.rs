use std::time::Instant;
use std::thread;

fn main() {
    let url = "https://mega.nz/folder/XeJXXDzS#6u4QMJWD9QOoCLtLDRRRkA";
    // Reproduce the TUI's environment: raw mode + a stdin reader thread.
    let raw = megadl::term::RawMode::enable();
    eprintln!("raw mode: {:?}", raw.is_ok());
    thread::spawn(|| {
        while let Ok(k) = megadl::term::read_key() {
            eprintln!("key: {k:?}");
        }
    });
    thread::sleep(std::time::Duration::from_millis(300));
    let t = Instant::now();
    eprintln!("calling resolve...");
    match megadl::resolve(url, megadl::api::DEFAULT_API_URL) {
        Ok(r) => eprintln!("[{:?}] OK, {} entries", t.elapsed(), r.listing().len()),
        Err(e) => eprintln!("[{:?}] ERR {e}", t.elapsed()),
    }
}
