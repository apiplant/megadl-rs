//! Full-screen interactive file selector: a checkbox tree over a
//! link's contents, driven by arrow keys. Used by the CLI's
//! `--choose-files` and by the TUI when adding a folder link.
//!
//! Keys come from an injected source rather than being read directly,
//! so the TUI (which already owns a single stdin reader thread) and the
//! CLI (which reads stdin itself) can share one implementation without
//! two readers competing for keystrokes.

use std::io::{self, Write};

use crate::term::{self, Key};

pub struct Row {
    pub label: String,
    pub depth: usize,
    pub is_dir: bool,
    pub size: i64,
    pub handle: String,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Mark {
    Off,
    On,
    Partial,
}

/// Runs the picker. Returns the chosen file handles, or `None` if the
/// user cancelled. Everything is selected initially.
pub fn pick(rows: &[Row], title: &str, keys: &mut dyn FnMut() -> Option<Key>) -> Option<Vec<String>> {
    if rows.is_empty() {
        return Some(Vec::new());
    }
    let mut chosen: Vec<bool> = rows.iter().map(|r| !r.is_dir).collect();
    let mut cursor = 0usize;
    let mut top = 0usize;

    loop {
        let (_, term_rows) = term::terminal_size();
        let view = (term_rows as usize).saturating_sub(6).max(3);
        if cursor < top {
            top = cursor;
        } else if cursor >= top + view {
            top = cursor + 1 - view;
        }
        draw(rows, &chosen, cursor, top, view, title);

        let key = keys()?;
        match key {
            Key::Up | Key::Char('k') => cursor = cursor.saturating_sub(1),
            Key::Down | Key::Char('j') => {
                if cursor + 1 < rows.len() {
                    cursor += 1;
                }
            }
            Key::Char('g') => cursor = 0,
            Key::Char('G') => cursor = rows.len() - 1,
            Key::Space => toggle(rows, &mut chosen, cursor),
            Key::Char('a') => chosen = rows.iter().map(|r| !r.is_dir).collect(),
            Key::Char('n') => chosen = vec![false; rows.len()],
            Key::Enter => {
                let picked: Vec<String> = rows
                    .iter()
                    .zip(&chosen)
                    .filter(|(r, &c)| c && !r.is_dir)
                    .map(|(r, _)| r.handle.clone())
                    .collect();
                return Some(picked);
            }
            Key::Esc | Key::Char('q') | Key::CtrlC => return None,
            _ => {}
        }
    }
}

/// Descendants of `i`: the run of following rows deeper than it. Rows
/// arrive in path-sorted order, so a directory's subtree is contiguous.
fn descendants(rows: &[Row], i: usize) -> std::ops::Range<usize> {
    let depth = rows[i].depth;
    let mut end = i + 1;
    while end < rows.len() && rows[end].depth > depth {
        end += 1;
    }
    (i + 1)..end
}

fn toggle(rows: &[Row], chosen: &mut [bool], i: usize) {
    if rows[i].is_dir {
        let range = descendants(rows, i);
        let turn_on = mark_of(rows, chosen, i) != Mark::On;
        for d in range {
            if !rows[d].is_dir {
                chosen[d] = turn_on;
            }
        }
    } else {
        chosen[i] = !chosen[i];
    }
}

fn mark_of(rows: &[Row], chosen: &[bool], i: usize) -> Mark {
    if !rows[i].is_dir {
        return if chosen[i] { Mark::On } else { Mark::Off };
    }
    let mut any = false;
    let mut all = true;
    for d in descendants(rows, i) {
        if rows[d].is_dir {
            continue;
        }
        if chosen[d] {
            any = true;
        } else {
            all = false;
        }
    }
    match (any, all) {
        (false, _) => Mark::Off,
        (true, true) => Mark::On,
        (true, false) => Mark::Partial,
    }
}

fn draw(rows: &[Row], chosen: &[bool], cursor: usize, top: usize, view: usize, title: &str) {
    let (cols, _) = term::terminal_size();
    let cols = (cols as usize).clamp(40, 200);
    let mut out = String::new();
    out.push_str("\x1b[H\x1b[2J");

    let selected_files = rows.iter().zip(chosen).filter(|(r, &c)| c && !r.is_dir).count();
    let total_bytes: i64 = rows.iter().zip(chosen).filter(|(r, &c)| c && !r.is_dir).map(|(r, _)| r.size).sum();

    out.push_str(&format!("\x1b[1;36m{title}\x1b[0m\r\n"));
    out.push_str(&format!(
        "\x1b[90m{} file(s) selected · {}\x1b[0m\r\n",
        selected_files,
        human_size(total_bytes)
    ));
    out.push_str(&format!("\x1b[90m{}\x1b[0m\r\n", "\u{2500}".repeat(cols)));

    for (i, row) in rows.iter().enumerate().skip(top).take(view) {
        let mark = match mark_of(rows, chosen, i) {
            Mark::On => "\x1b[32m[x]\x1b[0m",
            Mark::Off => "[ ]",
            Mark::Partial => "\x1b[33m[~]\x1b[0m",
        };
        let indent = "  ".repeat(row.depth);
        let name = if row.is_dir {
            format!("\x1b[1;34m{}/\x1b[0m", row.label)
        } else {
            row.label.clone()
        };
        let size = if row.is_dir { String::new() } else { human_size(row.size) };
        let line = format!("{mark} {indent}{name}");
        let plain_len = visible_len(&line);
        let pad = cols.saturating_sub(plain_len + size.len() + 3);
        if i == cursor {
            out.push_str(&format!("\x1b[7m \x1b[0m{line}{}\x1b[90m{size}\x1b[0m\r\n", " ".repeat(pad)));
        } else {
            out.push_str(&format!(" {line}{}\x1b[90m{size}\x1b[0m\r\n", " ".repeat(pad)));
        }
    }

    out.push_str(&format!("\x1b[90m{}\x1b[0m\r\n", "\u{2500}".repeat(cols)));
    out.push_str(
        " \x1b[1mspace\x1b[0m toggle  \x1b[1ma\x1b[0m all  \x1b[1mn\x1b[0m none  \
         \x1b[1m\u{2191}\u{2193}/jk\x1b[0m move  \x1b[1menter\x1b[0m download  \x1b[1mq\x1b[0m cancel\r\n",
    );

    let mut stdout = io::stdout();
    stdout.write_all(out.as_bytes()).ok();
    stdout.flush().ok();
}

/// Character count ignoring ANSI escape sequences.
fn visible_len(s: &str) -> usize {
    let mut n = 0;
    let mut in_esc = false;
    for c in s.chars() {
        if in_esc {
            if c == 'm' {
                in_esc = false;
            }
        } else if c == '\x1b' {
            in_esc = true;
        } else {
            n += 1;
        }
    }
    n
}

pub fn human_size(n: i64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// /root, /root/a.bin, /root/sub, /root/sub/b.bin, /root/sub/c.bin
    fn tree() -> Vec<Row> {
        let spec: &[(&str, usize, bool)] = &[
            ("root", 0, true),
            ("a.bin", 1, false),
            ("sub", 1, true),
            ("b.bin", 2, false),
            ("c.bin", 2, false),
        ];
        spec.iter()
            .enumerate()
            .map(|(i, (label, depth, is_dir))| Row {
                label: label.to_string(),
                depth: *depth,
                is_dir: *is_dir,
                size: 10,
                handle: format!("h{i}"),
            })
            .collect()
    }

    #[test]
    fn descendants_span_the_subtree() {
        let rows = tree();
        assert_eq!(descendants(&rows, 0), 1..5); // root covers everything
        assert_eq!(descendants(&rows, 2), 3..5); // sub covers b and c
        assert_eq!(descendants(&rows, 1), 2..2); // a file has none
    }

    #[test]
    fn toggling_a_dir_clears_then_sets_its_files() {
        let rows = tree();
        let mut chosen: Vec<bool> = rows.iter().map(|r| !r.is_dir).collect();
        assert_eq!(mark_of(&rows, &chosen, 0), Mark::On);

        toggle(&rows, &mut chosen, 0); // everything off
        assert_eq!(chosen, vec![false; 5]);
        assert_eq!(mark_of(&rows, &chosen, 0), Mark::Off);

        toggle(&rows, &mut chosen, 2); // just sub's files back on
        assert_eq!(chosen, vec![false, false, false, true, true]);
        assert_eq!(mark_of(&rows, &chosen, 2), Mark::On);
        assert_eq!(mark_of(&rows, &chosen, 0), Mark::Partial);
    }

    #[test]
    fn toggling_one_file_makes_its_parent_partial() {
        let rows = tree();
        let mut chosen: Vec<bool> = rows.iter().map(|r| !r.is_dir).collect();
        toggle(&rows, &mut chosen, 3);
        assert!(!chosen[3]);
        assert_eq!(mark_of(&rows, &chosen, 2), Mark::Partial);
    }

    #[test]
    fn scripted_keys_return_only_checked_files() {
        let rows = tree();
        // space (clear root) → down,down,down (to b.bin) → space → enter
        let mut script = vec![Key::Enter, Key::Space, Key::Down, Key::Down, Key::Down, Key::Space];
        let picked = pick(&rows, "t", &mut || script.pop()).unwrap();
        assert_eq!(picked, vec!["h3".to_string()]);
    }

    #[test]
    fn cancelling_returns_none() {
        let rows = tree();
        let mut script = vec![Key::Esc];
        assert!(pick(&rows, "t", &mut || script.pop()).is_none());
    }
}

/// Builds picker rows from a resolved link's listing.
pub fn rows_from_listing(entries: &[crate::Entry]) -> Vec<Row> {
    entries
        .iter()
        .map(|e| Row {
            label: e.name.clone(),
            depth: e.path.matches('/').count().saturating_sub(1),
            is_dir: e.is_dir,
            size: e.size,
            handle: e.handle.clone(),
        })
        .collect()
}
