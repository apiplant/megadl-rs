//! MEGA link parsing (no regex dependency — hand-rolled matching over
//! the handful of URL shapes MEGA actually issues).

#[derive(Debug, Clone)]
pub struct Link {
    pub kind: Kind,
    pub handle: String,
    pub key: String,
    /// Folder deep links: handle of the node to rebase to.
    pub specific: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Folder,
}

fn is_b64url(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

fn strip_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    if s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&s[prefix.len()..])
    } else {
        None
    }
}

fn take_while<'a>(s: &'a str, f: impl Fn(char) -> bool) -> (&'a str, &'a str) {
    let end = s.find(|c: char| !f(c)).unwrap_or(s.len());
    (&s[..end], &s[end..])
}

/// Parses a mega.nz URL into its typed, handle/key components.
pub fn parse_link(raw: &str) -> Result<Link, String> {
    let raw = percent_decode(raw.trim());

    // https://mega.nz/#!<handle>!<key>  or  https://mega.co.nz/#!<handle>!<key>
    if let Some(rest) = strip_scheme_host(&raw, "#!") {
        let (handle, rest) = take_while(rest, is_b64url);
        if handle.len() == 8 {
            if let Some(rest) = rest.strip_prefix('!') {
                let (key, rest) = take_while(rest, |c| is_b64url(c) || c == '=');
                if key.len() >= 43 && key.len() <= 45 && rest.is_empty() {
                    return Ok(Link { kind: Kind::File, handle: handle.to_string(), key: key.to_string(), specific: None });
                }
            }
        }
    }

    // https://mega.nz/file/<handle>#<key>
    if let Some(rest) = strip_scheme_host(&raw, "file/") {
        let (handle, rest) = take_while(rest, is_b64url);
        if handle.len() == 8 {
            if let Some(rest) = rest.strip_prefix('#') {
                let (key, rest) = take_while(rest, |c| is_b64url(c) || c == '=');
                if key.len() >= 43 && key.len() <= 45 && rest.is_empty() {
                    return Ok(Link { kind: Kind::File, handle: handle.to_string(), key: key.to_string(), specific: None });
                }
            }
        }
    }

    // https://mega.nz/#F!<handle>!<key>[!?<specific>]
    if let Some(rest) = strip_scheme_host(&raw, "#F!") {
        let (handle, rest) = take_while(rest, is_b64url);
        if handle.len() == 8 {
            if let Some(rest) = rest.strip_prefix('!') {
                let (key, rest) = take_while(rest, is_b64url);
                if key.len() == 22 {
                    let specific = match rest.strip_prefix('!').or_else(|| rest.strip_prefix('?')) {
                        Some(r) => {
                            let (h, r2) = take_while(r, is_b64url);
                            if h.len() == 8 && r2.is_empty() {
                                Some(h.to_string())
                            } else if rest.is_empty() {
                                None
                            } else {
                                return Err(format!("invalid mega download link: {raw}"));
                            }
                        }
                        None => None,
                    };
                    if rest.is_empty() || specific.is_some() {
                        return Ok(Link { kind: Kind::Folder, handle: handle.to_string(), key: key.to_string(), specific });
                    }
                }
            }
        }
    }

    // https://mega.nz/folder/<handle>#<key>[/file/<h> | /folder/<h>]
    if let Some(rest) = strip_scheme_host(&raw, "folder/") {
        let (handle, rest) = take_while(rest, is_b64url);
        if handle.len() == 8 {
            if let Some(rest) = rest.strip_prefix('#') {
                let (key, rest) = take_while(rest, is_b64url);
                if key.len() == 22 {
                    if rest.is_empty() {
                        return Ok(Link { kind: Kind::Folder, handle: handle.to_string(), key: key.to_string(), specific: None });
                    }
                    for tag in ["/file/", "/folder/"] {
                        if let Some(r) = rest.strip_prefix(tag) {
                            let (h, r2) = take_while(r, is_b64url);
                            if h.len() == 8 && r2.is_empty() {
                                return Ok(Link { kind: Kind::Folder, handle: handle.to_string(), key: key.to_string(), specific: Some(h.to_string()) });
                            }
                        }
                    }
                }
            }
        }
    }

    Err(format!("invalid mega download link: {raw}"))
}

/// Strips `https://mega.nz/` or `https://mega.co.nz/` (case-insensitive)
/// followed by `marker`, returning what's left.
fn strip_scheme_host<'a>(raw: &'a str, marker: &str) -> Option<&'a str> {
    for scheme in ["https://", "http://"] {
        let rest = strip_ci(raw, scheme)?;
        for host in ["mega.co.nz/", "mega.nz/"] {
            if let Some(rest) = strip_ci(rest, host) {
                if let Some(rest) = strip_ci(rest, marker) {
                    return Some(rest);
                }
            }
        }
        return None;
    }
    None
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_hash_bang() {
        let l = parse_link("https://mega.nz/#!AAAAAAAA!012345678901234567890123456789012345678901a").unwrap();
        assert_eq!(l.kind, Kind::File);
        assert_eq!(l.handle, "AAAAAAAA");
    }

    #[test]
    fn file_new_style() {
        let l = parse_link("https://mega.nz/file/AAAAAAAA#012345678901234567890123456789012345678901a").unwrap();
        assert_eq!(l.kind, Kind::File);
    }

    #[test]
    fn folder_new_style() {
        let l = parse_link("https://mega.nz/folder/AAAAAAAA#0123456789012345678901").unwrap();
        assert_eq!(l.kind, Kind::Folder);
        assert!(l.specific.is_none());
    }

    #[test]
    fn folder_deep_link() {
        let l = parse_link("https://mega.nz/folder/AAAAAAAA#0123456789012345678901/file/BBBBBBBB").unwrap();
        assert_eq!(l.kind, Kind::Folder);
        assert_eq!(l.specific.as_deref(), Some("BBBBBBBB"));
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_link("https://example.com/").is_err());
    }
}
