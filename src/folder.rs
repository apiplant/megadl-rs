//! Exported-folder tree fetch and decryption.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::json;

use crate::api::ApiClient;
use crate::crypto::{decrypt_attrs, decrypt_ecb_b64, sanitize_name, unpack_file_key};

#[derive(Deserialize)]
struct RawFNode {
    h: String,
    #[serde(default)]
    p: String,
    a: String,
    k: String,
    #[serde(default)]
    sk: String,
    t: i64,
    #[serde(default)]
    s: i64,
}

pub struct FNode {
    pub handle: String,
    pub parent_handle: String,
    pub name: String,
    pub is_dir: bool,
    pub size: i64,
    pub key: Vec<u8>,
    pub parent: Option<usize>, // index into FolderFs.nodes
    pub path: String,
}

pub struct FolderFs {
    pub root: usize,
    pub nodes: Vec<FNode>,
    pub by_handle: HashMap<String, usize>,
}

/// Fetches and decrypts an exported folder's node tree (API call "f"
/// with the link handle as the n= session parameter).
pub fn open_folder(api: &ApiClient, master_key_b64: &str, specific: Option<&str>) -> Result<FolderFs, String> {
    let master_key = crate::crypto::b64decode(master_key_b64)?;
    if master_key.len() != 16 {
        return Err("invalid folder key".into());
    }

    let res = api.call(json!({"a": "f", "c": 1, "r": 1}))?;
    let raw_nodes = res.get("f").and_then(|v| v.as_array()).ok_or("folder listing returned no nodes")?;
    if raw_nodes.is_empty() {
        return Err("folder listing returned no nodes".into());
    }

    let mut share_keys: HashMap<String, Vec<u8>> = HashMap::new();
    let mut parsed: Vec<FNode> = Vec::new();
    for (i, raw) in raw_nodes.iter().enumerate() {
        let rn: RawFNode = match serde_json::from_value(raw.clone()) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if i == 0 {
            share_keys.insert(rn.h.clone(), master_key.clone());
        }
        let mut n = match parse_fnode(&rn, &mut share_keys, &master_key) {
            Some(n) => n,
            None => continue,
        };
        if i == 0 {
            n.parent_handle.clear();
        }
        parsed.push(n);
    }
    if parsed.is_empty() {
        return Err("no decryptable nodes in folder".into());
    }

    let mut by_handle: HashMap<String, usize> = HashMap::new();
    for (i, n) in parsed.iter().enumerate() {
        by_handle.insert(n.handle.clone(), i);
    }
    for i in 0..parsed.len() {
        let ph = parsed[i].parent_handle.clone();
        if !ph.is_empty() {
            parsed[i].parent = by_handle.get(&ph).copied();
        }
    }

    let mut root = 0usize;
    if let Some(spec) = specific {
        root = *by_handle.get(spec).ok_or_else(|| format!("node not found: {spec}"))?;
        parsed[root].parent = None;
        parsed[root].parent_handle.clear();
    }

    // paths for everything reachable from root; the rest is dropped
    let mut children: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..parsed.len() {
        if i != root {
            if let Some(p) = parsed[i].parent {
                children.entry(p).or_default().push(i);
            }
        }
    }
    parsed[root].path = format!("/{}", parsed[root].name);
    let mut reach = vec![root];
    let mut idx = 0;
    while idx < reach.len() {
        let cur = reach[idx];
        if let Some(kids) = children.get(&cur) {
            let kids = kids.clone();
            for c in kids {
                let parent_path = parsed[cur].path.clone();
                parsed[c].path = format!("{parent_path}/{}", parsed[c].name);
                reach.push(c);
            }
        }
        idx += 1;
    }
    reach.sort_by(|&a, &b| sort_key(&parsed[a]).cmp(&sort_key(&parsed[b])));

    let mut reachable: HashMap<String, usize> = HashMap::new();
    for &i in &reach {
        reachable.insert(parsed[i].handle.clone(), i);
    }

    // Renumber nodes to just the reachable set, in `reach` order, so
    // `nodes[root]` is always index 0 (root's own index may have
    // shifted for deep links).
    let mut remap: HashMap<usize, usize> = HashMap::new();
    let mut nodes = Vec::with_capacity(reach.len());
    for (new_i, &old_i) in reach.iter().enumerate() {
        remap.insert(old_i, new_i);
    }
    for &old_i in &reach {
        let n = &parsed[old_i];
        nodes.push(FNode {
            handle: n.handle.clone(),
            parent_handle: n.parent_handle.clone(),
            name: n.name.clone(),
            is_dir: n.is_dir,
            size: n.size,
            key: n.key.clone(),
            parent: n.parent.and_then(|p| remap.get(&p).copied()),
            path: n.path.clone(),
        });
    }
    let new_root = *remap.get(&root).unwrap();
    let by_handle = nodes.iter().enumerate().map(|(i, n)| (n.handle.clone(), i)).collect();

    Ok(FolderFs { root: new_root, nodes, by_handle })
}

fn sort_key(n: &FNode) -> String {
    if n.is_dir {
        format!("{}/", n.path)
    } else {
        n.path.clone()
    }
}

/// Decrypts and validates one raw node. Undecryptable or malformed
/// nodes are skipped (None).
fn parse_fnode(rn: &RawFNode, share_keys: &mut HashMap<String, Vec<u8>>, master_key: &[u8]) -> Option<FNode> {
    if rn.h.is_empty() || rn.a.is_empty() || rn.k.is_empty() {
        return None;
    }
    if rn.t != 0 && rn.t != 1 {
        return None; // only files and folders
    }
    let is_dir = rn.t == 1;

    if !rn.sk.is_empty() && rn.sk.len() <= 22 {
        if let Ok(sk) = decrypt_ecb_b64(master_key, &rn.sk) {
            if sk.len() == 16 {
                share_keys.insert(rn.h.clone(), sk);
            }
        }
    }

    let mut share_key: Option<&Vec<u8>> = None;
    let mut enc_key = "";
    for part in rn.k.split('/') {
        if let Some((kh, kv)) = part.split_once(':') {
            if let Some(sk) = share_keys.get(kh) {
                share_key = Some(sk);
                enc_key = kv;
                break;
            }
        }
    }
    let share_key = share_key?;
    if enc_key.is_empty() || enc_key.len() >= 46 {
        return None; // >=46 chars would be an RSA key
    }
    let node_key = decrypt_ecb_b64(share_key, enc_key).ok()?;
    if (is_dir && node_key.len() != 16) || (!is_dir && node_key.len() != 32) {
        return None;
    }

    let attr_key: [u8; 16] = if is_dir {
        node_key.clone().try_into().ok()?
    } else {
        unpack_file_key(&node_key).ok()?.aes
    };
    let name = decrypt_attrs(&attr_key, &rn.a).ok()?;
    let name = sanitize_name(&name).ok()?;

    Some(FNode {
        handle: rn.h.clone(),
        parent_handle: rn.p.clone(),
        name,
        is_dir,
        size: rn.s,
        key: node_key,
        parent: None,
        path: String::new(),
    })
}

impl FolderFs {
    /// Asks the API for a node's transfer URL and size.
    pub fn download_url(&self, api: &ApiClient, handle: &str) -> Result<(String, i64), String> {
        let res = api.call(json!({"a": "g", "g": 1, "ssl": 0, "n": handle}))?;
        let g = res.get("g").and_then(|v| v.as_str()).unwrap_or("");
        let s = res.get("s").and_then(|v| v.as_i64()).unwrap_or(-1);
        if g.is_empty() || s < 0 {
            return Err("can't determine download url".into());
        }
        Ok((g.to_string(), s))
    }

    /// Every file in n's subtree (n included if a file), in listing order.
    pub fn files_under(&self, n: usize) -> Vec<usize> {
        let prefix = format!("{}/", self.nodes[n].path);
        self.nodes
            .iter()
            .enumerate()
            .filter(|(i, c)| !c.is_dir && (*i == n || c.path.starts_with(&prefix)))
            .map(|(i, _)| i)
            .collect()
    }

    pub fn rel_to_root(&self, n: usize) -> String {
        if n == self.root {
            self.nodes[n].name.clone()
        } else {
            let root_path = &self.nodes[self.root].path;
            self.nodes[n]
                .path
                .strip_prefix(&format!("{root_path}/"))
                .unwrap_or(&self.nodes[n].path)
                .to_string()
        }
    }
}
