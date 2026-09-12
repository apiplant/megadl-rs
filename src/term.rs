//! Bare-bones raw-terminal handling for the TUI: no crossterm/termion,
//! just libc termios plus hand-rolled ANSI escapes.

use std::io::{self, Read, Write};
use std::mem::MaybeUninit;

pub struct RawMode {
    original: libc::termios,
}

impl RawMode {
    pub fn enable() -> io::Result<Self> {
        unsafe {
            let mut term = MaybeUninit::<libc::termios>::uninit();
            if libc::tcgetattr(libc::STDIN_FILENO, term.as_mut_ptr()) != 0 {
                return Err(io::Error::last_os_error());
            }
            let original = term.assume_init();
            let mut raw = original;
            libc::cfmakeraw(&mut raw);
            if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(RawMode { original })
        }
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.original);
        }
    }
}

pub fn enter_alt_screen() {
    print!("\x1b[?1049h\x1b[2J\x1b[H\x1b[?25l");
    io::stdout().flush().ok();
}

pub fn leave_alt_screen() {
    print!("\x1b[?25h\x1b[?1049l");
    io::stdout().flush().ok();
}

pub fn terminal_size() -> (u16, u16) {
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_col > 0 {
            (ws.ws_col, ws.ws_row)
        } else {
            (80, 24)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Backspace,
    Esc,
    Up,
    Down,
    Left,
    Right,
    Space,
    Tab,
    CtrlC,
}

/// Reads one key from stdin, blocking. Handles the two-/three-byte
/// escape sequences for arrow keys and drops anything else
/// unrecognized silently.
pub fn read_key() -> io::Result<Key> {
    let mut buf = [0u8; 1];
    loop {
        io::stdin().read_exact(&mut buf)?;
        match buf[0] {
            3 => return Ok(Key::CtrlC),
            b'\r' | b'\n' => return Ok(Key::Enter),
            0x7f | 0x08 => return Ok(Key::Backspace),
            b'\t' => return Ok(Key::Tab),
            b' ' => return Ok(Key::Space),
            0x1b => {
                let mut seq = [0u8; 2];
                if io::stdin().read_exact(&mut seq).is_err() {
                    return Ok(Key::Esc);
                }
                if seq[0] == b'[' {
                    match seq[1] {
                        b'A' => return Ok(Key::Up),
                        b'B' => return Ok(Key::Down),
                        b'C' => return Ok(Key::Right),
                        b'D' => return Ok(Key::Left),
                        _ => continue,
                    }
                }
                return Ok(Key::Esc);
            }
            c if c.is_ascii() => return Ok(Key::Char(c as char)),
            _ => continue,
        }
    }
}
