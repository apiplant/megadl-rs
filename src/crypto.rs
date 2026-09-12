//! MEGA's crypto primitives: url-safe base64, AES-ECB key unwrap,
//! AES-CBC attribute decryption, AES-CTR data stream, and the chunked
//! CBC-MAC used for integrity verification.

use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit, KeyIvInit, StreamCipher};
use aes::Aes128;
use ctr::Ctr128BE;

pub type Error = String;

/// Decodes MEGA's url-safe base64 (no padding; stray '=' tolerated).
pub fn b64decode(s: &str) -> Result<Vec<u8>, Error> {
    let s = s.trim_end_matches('=');
    b64_url_decode(s)
}

pub fn b64encode(data: &[u8]) -> String {
    b64_url_encode(data)
}

const B64_ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn b64_url_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        let n = ((b0 as u32) << 16) | ((b1 as u32) << 8) | (b2 as u32);
        out.push(B64_ALPHABET[((n >> 18) & 0x3f) as usize] as char);
        out.push(B64_ALPHABET[((n >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(B64_ALPHABET[((n >> 6) & 0x3f) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(B64_ALPHABET[(n & 0x3f) as usize] as char);
        }
    }
    out
}

fn b64_url_decode(s: &str) -> Result<Vec<u8>, Error> {
    fn val(c: u8) -> Result<u32, Error> {
        match c {
            b'A'..=b'Z' => Ok((c - b'A') as u32),
            b'a'..=b'z' => Ok((c - b'a' + 26) as u32),
            b'0'..=b'9' => Ok((c - b'0' + 52) as u32),
            b'-' => Ok(62),
            b'_' => Ok(63),
            _ => Err(format!("invalid base64 byte {c}")),
        }
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4 + 3);
    let mut i = 0;
    while i < bytes.len() {
        let n = (bytes.len() - i).min(4);
        let mut acc = 0u32;
        for k in 0..4 {
            acc <<= 6;
            if k < n {
                acc |= val(bytes[i + k])?;
            }
        }
        let b = acc.to_be_bytes();
        if n >= 2 {
            out.push(b[1]);
        }
        if n >= 3 {
            out.push(b[2]);
        }
        if n >= 4 {
            out.push(b[3]);
        }
        i += n;
    }
    Ok(out)
}

/// A 32-byte MEGA node key, unpacked into its parts.
#[derive(Clone, Copy)]
pub struct FileKey {
    pub aes: [u8; 16],
    pub nonce: [u8; 8],
    pub meta_mac: [u8; 8],
}

pub fn unpack_file_key(node_key: &[u8]) -> Result<FileKey, Error> {
    if node_key.len() != 32 {
        return Err(format!("file key is {} bytes, want 32", node_key.len()));
    }
    let mut aes = [0u8; 16];
    for i in 0..16 {
        aes[i] = node_key[i] ^ node_key[i + 16];
    }
    let mut nonce = [0u8; 8];
    nonce.copy_from_slice(&node_key[16..24]);
    let mut meta_mac = [0u8; 8];
    meta_mac.copy_from_slice(&node_key[24..32]);
    Ok(FileKey { aes, nonce, meta_mac })
}

/// base64-decodes and AES-128-ECB-decrypts a key blob.
pub fn decrypt_ecb_b64(key: &[u8], s: &str) -> Result<Vec<u8>, Error> {
    let data = b64decode(s)?;
    if data.is_empty() || data.len() % 16 != 0 {
        return Err(format!("cipher length {} not a block multiple", data.len()));
    }
    let cipher = Aes128::new_from_slice(key).map_err(|e| e.to_string())?;
    let mut out = data;
    for block in out.chunks_mut(16) {
        let ga = GenericArray::from_mut_slice(block);
        cipher.decrypt_block(ga);
    }
    Ok(out)
}

/// Decrypts a node attribute blob (AES-128-CBC, zero IV, "MEGA{json}"
/// plaintext) and returns the node name.
pub fn decrypt_attrs(key: &[u8; 16], at: &str) -> Result<String, Error> {
    let mut data = b64decode(at)?;
    if data.is_empty() || data.len() % 16 != 0 {
        return Err(format!("attribute length {} not a block multiple", data.len()));
    }
    let cipher = Aes128::new_from_slice(key).map_err(|e| e.to_string())?;
    let mut prev = [0u8; 16];
    for block in data.chunks_mut(16) {
        let mut ct = [0u8; 16];
        ct.copy_from_slice(block);
        let ga = GenericArray::from_mut_slice(block);
        cipher.decrypt_block(ga);
        for i in 0..16 {
            block[i] ^= prev[i];
        }
        prev = ct;
    }

    if !data.starts_with(b"MEGA") {
        return Err("malformed attributes".into());
    }
    let mut j = &data[4..];
    if let Some(i) = j.iter().position(|&b| b == 0) {
        j = &j[..i];
    }
    let attrs: serde_json::Value = serde_json::from_slice(j).map_err(|e| format!("malformed attributes: {e}"))?;
    let name = attrs
        .get("n")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("attributes are missing the node name")?;
    Ok(name.to_string())
}

/// Makes a remote node name safe as one local path component.
pub fn sanitize_name(name: &str) -> Result<String, Error> {
    let name = name.replace('/', "_");
    if name.is_empty() || name == "." || name == ".." {
        return Err(format!("invalid remote file name {name:?}"));
    }
    Ok(name)
}

type Aes128Ctr = Ctr128BE<Aes128>;

/// Returns an AES-128-CTR keystream cipher positioned at byte offset
/// `off` (iv = nonce || big-endian block counter, mid-block offsets
/// consumed so the caller can XOR starting exactly at `off`).
pub fn ctr_at(key: [u8; 16], nonce: [u8; 8], off: u64) -> Aes128Ctr {
    let mut iv = [0u8; 16];
    iv[..8].copy_from_slice(&nonce);
    iv[8..].copy_from_slice(&(off / 16).to_be_bytes());
    let mut cipher = Aes128Ctr::new(&key.into(), &iv.into());
    let pad = (off % 16) as usize;
    if pad > 0 {
        let mut scratch = [0u8; 16];
        cipher.apply_keystream(&mut scratch[..pad]);
    }
    cipher
}

/// Size of the idx-th MAC chunk: 128 KiB, 256 KiB, ... growing to
/// 1 MiB, then 1 MiB forever.
fn mac_chunk_size(idx: i64) -> i64 {
    if idx < 8 {
        (idx + 1) * 128 * 1024
    } else {
        8 * 128 * 1024
    }
}

/// MEGA's chunked CBC-MAC over the file plaintext: a CBC-MAC (iv =
/// nonce||nonce) per chunk, chunk MACs folded into a meta-MAC that
/// condenses to the 8 bytes stored in the node key.
pub struct ChunkedMac {
    cipher: Aes128,
    chunk_idx: i64,
    next_boundary: i64,
    pos: i64,
    iv: [u8; 16],
    chunk: [u8; 16],
    meta: [u8; 16],
}

impl ChunkedMac {
    pub fn new(key: [u8; 16], nonce: [u8; 8]) -> Self {
        let mut iv = [0u8; 16];
        iv[..8].copy_from_slice(&nonce);
        iv[8..].copy_from_slice(&nonce);
        ChunkedMac {
            cipher: Aes128::new(&key.into()),
            chunk_idx: 0,
            next_boundary: mac_chunk_size(0),
            pos: 0,
            iv,
            chunk: iv,
            meta: [0u8; 16],
        }
    }

    fn encrypt_block(&self, block: &mut [u8; 16]) {
        let ga = GenericArray::from_mut_slice(block);
        self.cipher.encrypt_block(ga);
    }

    fn close_chunk(&mut self) {
        for i in 0..16 {
            self.meta[i] ^= self.chunk[i];
        }
        let mut meta = self.meta;
        self.encrypt_block(&mut meta);
        self.meta = meta;
        self.chunk = self.iv;
        self.chunk_idx += 1;
        self.next_boundary += mac_chunk_size(self.chunk_idx);
    }

    pub fn update(&mut self, mut data: &[u8]) {
        while !data.is_empty() {
            if self.pos % 16 == 0 && data.len() >= 16 && self.pos + 16 <= self.next_boundary {
                for i in 0..16 {
                    self.chunk[i] ^= data[i];
                }
                let mut blk = self.chunk;
                self.encrypt_block(&mut blk);
                self.chunk = blk;
                self.pos += 16;
                data = &data[16..];
            } else {
                self.chunk[(self.pos % 16) as usize] ^= data[0];
                self.pos += 1;
                if self.pos % 16 == 0 {
                    let mut blk = self.chunk;
                    self.encrypt_block(&mut blk);
                    self.chunk = blk;
                }
                data = &data[1..];
            }
            if self.pos == self.next_boundary {
                self.close_chunk();
            }
        }
    }

    /// Zero-pads the last block, folds any unfinished chunk and
    /// condenses the meta-MAC to the 8 bytes kept in the node key.
    pub fn finish(mut self) -> [u8; 8] {
        if self.pos % 16 != 0 {
            self.pos += 16 - self.pos % 16;
            let mut blk = self.chunk;
            self.encrypt_block(&mut blk);
            self.chunk = blk;
        }
        if self.pos > self.next_boundary - mac_chunk_size(self.chunk_idx) {
            self.close_chunk();
        }
        let mut out = [0u8; 8];
        for i in 0..4 {
            out[i] = self.meta[i] ^ self.meta[i + 4];
            out[i + 4] = self.meta[i + 8] ^ self.meta[i + 12];
        }
        out
    }
}
