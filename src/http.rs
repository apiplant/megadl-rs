//! Minimal blocking HTTP/1.1 client: just enough POST support for the
//! MEGA API and its data-transfer servers. Speaks TLS via rustls and
//! plain HTTP (MEGA hands out `http://` transfer URLs, whose payload is
//! already AES-encrypted). No connection reuse, no async runtime.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};

const IO_TIMEOUT: Duration = Duration::from_secs(120);

/// Either a TLS session or a bare socket, so the HTTP layer above does
/// not care which scheme it is talking.
pub enum Stream {
    Plain(TcpStream),
    Tls(Box<StreamOwned<ClientConnection, TcpStream>>),
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.read(buf),
            Stream::Tls(s) => s.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.write(buf),
            Stream::Tls(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Stream::Plain(s) => s.flush(),
            Stream::Tls(s) => s.flush(),
        }
    }
}

pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Body,
}

impl Response {
    /// Case-insensitive header lookup.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

pub enum Body {
    Buffered(Vec<u8>),
    Stream(BufReader<Stream>, BodyKind),
}

#[derive(Clone, Copy)]
pub enum BodyKind {
    ContentLength(u64),
    Chunked,
    UntilClose,
}

fn config() -> Arc<ClientConfig> {
    static CONFIG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let mut roots = RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            Arc::new(ClientConfig::builder().with_root_certificates(roots).with_no_client_auth())
        })
        .clone()
}

struct Target {
    tls: bool,
    host: String,
    port: u16,
    path: String,
}

/// Splits an http(s) URL into its connection parts.
fn split_url(url: &str) -> io::Result<Target> {
    let (tls, rest) = if let Some(r) = url.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (false, r)
    } else {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("unsupported URL scheme: {url}")));
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], rest[i..].to_string()),
        None => (rest, "/".to_string()),
    };
    let default_port = if tls { 443 } else { 80 };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().unwrap_or(default_port)),
        None => (authority.to_string(), default_port),
    };
    Ok(Target { tls, host, port, path })
}

fn connect(target: &Target) -> io::Result<Stream> {
    let sock = TcpStream::connect((target.host.as_str(), target.port))?;
    sock.set_nodelay(true).ok();
    sock.set_read_timeout(Some(IO_TIMEOUT)).ok();
    sock.set_write_timeout(Some(IO_TIMEOUT)).ok();
    if !target.tls {
        return Ok(Stream::Plain(sock));
    }
    let server_name = ServerName::try_from(target.host.clone())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "bad host name"))?;
    let conn = ClientConnection::new(config(), server_name).map_err(io::Error::other)?;
    Ok(Stream::Tls(Box::new(StreamOwned::new(conn, sock))))
}

/// Performs one HTTP POST, returning the status and a body reader.
/// `stream_body` selects whether the caller wants the whole body read
/// up front (API calls) or handed back as a streaming reader (data
/// downloads).
pub fn post(
    url: &str,
    extra_headers: &[(&str, &str)],
    body: &[u8],
    stream_body: bool,
) -> io::Result<Response> {
    let target = split_url(url)?;
    let mut stream = connect(&target)?;

    let mut req = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nContent-Length: {len}\r\n",
        path = target.path,
        host = target.host,
        len = body.len()
    );
    for (k, v) in extra_headers {
        req.push_str(k);
        req.push_str(": ");
        req.push_str(v);
        req.push_str("\r\n");
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes())?;
    if !body.is_empty() {
        stream.write_all(body)?;
    }
    stream.flush()?;

    let mut reader = BufReader::with_capacity(64 * 1024, stream);
    let (status, kind, headers) = read_headers(&mut reader)?;

    if stream_body {
        Ok(Response { status, headers, body: Body::Stream(reader, kind) })
    } else {
        let mut buf = Vec::new();
        read_body_into(&mut reader, kind, &mut buf)?;
        Ok(Response { status, headers, body: Body::Buffered(buf) })
    }
}

type Headers = Vec<(String, String)>;

fn read_headers(r: &mut BufReader<Stream>) -> io::Result<(u16, BodyKind, Headers)> {
    let mut line = String::new();
    r.read_line(&mut line)?;
    let status: u16 = line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad status line"))?;

    let mut content_length: Option<u64> = None;
    let mut chunked = false;
    let mut headers: Headers = Vec::new();
    loop {
        let mut hline = String::new();
        let n = r.read_line(&mut hline)?;
        if n == 0 || hline == "\r\n" || hline == "\n" {
            break;
        }
        if let Some((k, v)) = hline.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
        let lower = hline.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            content_length = v.trim().parse().ok();
        } else if let Some(v) = lower.strip_prefix("transfer-encoding:") {
            if v.trim().contains("chunked") {
                chunked = true;
            }
        }
    }
    let kind = if chunked {
        BodyKind::Chunked
    } else if let Some(len) = content_length {
        BodyKind::ContentLength(len)
    } else {
        BodyKind::UntilClose
    };
    Ok((status, kind, headers))
}

fn read_body_into(r: &mut BufReader<Stream>, kind: BodyKind, out: &mut Vec<u8>) -> io::Result<()> {
    let mut reader = BodyReader { inner: r, kind, remaining_in_chunk: 0, remaining_total: None, done: false };
    if let BodyKind::ContentLength(n) = kind {
        reader.remaining_total = Some(n);
    }
    reader.read_to_end(out)?;
    Ok(())
}

impl Body {
    pub fn into_buffered(self) -> Vec<u8> {
        match self {
            Body::Buffered(b) => b,
            Body::Stream(mut reader, kind) => {
                let mut out = Vec::new();
                let _ = read_body_into(&mut reader, kind, &mut out);
                out
            }
        }
    }

    /// Streaming body reader. Only valid on a response fetched with
    /// `stream_body = true`.
    pub fn into_reader(self) -> Option<OwnedBodyReader> {
        match self {
            Body::Stream(reader, kind) => {
                let remaining_total = match kind {
                    BodyKind::ContentLength(n) => Some(n),
                    _ => None,
                };
                Some(OwnedBodyReader { inner: reader, kind, remaining_in_chunk: 0, remaining_total, done: false })
            }
            Body::Buffered(_) => None,
        }
    }
}

/// Un-chunking reader over a borrowed stream.
struct BodyReader<'a> {
    inner: &'a mut BufReader<Stream>,
    kind: BodyKind,
    remaining_in_chunk: u64,
    remaining_total: Option<u64>,
    done: bool,
}

/// Un-chunking reader that owns its stream, handed to download code.
pub struct OwnedBodyReader {
    inner: BufReader<Stream>,
    kind: BodyKind,
    remaining_in_chunk: u64,
    remaining_total: Option<u64>,
    done: bool,
}

impl Read for BodyReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        read_body(self.inner, self.kind, &mut self.remaining_in_chunk, &mut self.remaining_total, &mut self.done, buf)
    }
}

impl Read for OwnedBodyReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        read_body(&mut self.inner, self.kind, &mut self.remaining_in_chunk, &mut self.remaining_total, &mut self.done, buf)
    }
}

fn read_body(
    inner: &mut BufReader<Stream>,
    kind: BodyKind,
    remaining_in_chunk: &mut u64,
    remaining_total: &mut Option<u64>,
    done: &mut bool,
    buf: &mut [u8],
) -> io::Result<usize> {
    if *done || buf.is_empty() {
        return Ok(0);
    }
    match kind {
        BodyKind::ContentLength(_) | BodyKind::UntilClose => {
            if let Some(rem) = *remaining_total {
                if rem == 0 {
                    *done = true;
                    return Ok(0);
                }
                let want = buf.len().min(rem as usize);
                let n = inner.read(&mut buf[..want])?;
                if n == 0 {
                    *done = true;
                }
                *remaining_total = Some(rem - n as u64);
                Ok(n)
            } else {
                let n = inner.read(buf)?;
                if n == 0 {
                    *done = true;
                }
                Ok(n)
            }
        }
        BodyKind::Chunked => {
            if *remaining_in_chunk == 0 {
                let mut size_line = String::new();
                inner.read_line(&mut size_line)?;
                let size_str = size_line.trim().split(';').next().unwrap_or("").trim();
                let size = u64::from_str_radix(size_str, 16)
                    .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad chunk size"))?;
                if size == 0 {
                    loop {
                        let mut l = String::new();
                        let n = inner.read_line(&mut l)?;
                        if n == 0 || l == "\r\n" || l == "\n" {
                            break;
                        }
                    }
                    *done = true;
                    return Ok(0);
                }
                *remaining_in_chunk = size;
            }
            let want = buf.len().min(*remaining_in_chunk as usize);
            let n = inner.read(&mut buf[..want])?;
            if n == 0 {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "chunked body truncated"));
            }
            *remaining_in_chunk -= n as u64;
            if *remaining_in_chunk == 0 {
                let mut crlf = [0u8; 2];
                inner.read_exact(&mut crlf)?;
            }
            Ok(n)
        }
    }
}
