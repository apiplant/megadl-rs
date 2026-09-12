pub mod api;
pub mod crypto;
pub mod download;
pub mod folder;
pub mod http;
pub mod link;
pub mod picker;
pub mod term;

use std::path::{Path, PathBuf};

use serde_json::json;

use api::ApiClient;
use crypto::{decrypt_attrs, sanitize_name, unpack_file_key, FileKey};
use download::FileJob;
use folder::FolderFs;
use link::{Kind, Link};

/// One entry in a listed link: a file or a folder, with everything
/// needed to queue it for download.
#[derive(Clone)]
pub struct Entry {
    pub index: usize,
    pub path: String,
    pub name: String,
    pub is_dir: bool,
    pub size: i64,
    pub handle: String,
    pub parent: Option<String>,
}

/// A resolved link: either a single file or an open folder tree.
pub enum Resolved {
    File { info: FileInfo, handle: String, api: ApiClient },
    Folder { fs: FolderFs, api: ApiClient },
}

pub struct FileInfo {
    pub name: String,
    pub size: i64,
    pub url: String,
    pub key: FileKey,
}

pub fn resolve(raw_url: &str, api_url: &str) -> Result<Resolved, String> {
    let l = link::parse_link(raw_url)?;
    match l.kind {
        Kind::File => {
            let api = ApiClient::new(api_url, "");
            let info = prepare_file_link(&api, &l)?;
            Ok(Resolved::File { info, handle: l.handle, api })
        }
        Kind::Folder => {
            let api = ApiClient::new(api_url, &l.handle);
            let fs = folder::open_folder(&api, &l.key, l.specific.as_deref())?;
            Ok(Resolved::Folder { fs, api })
        }
    }
}

fn prepare_file_link(api: &ApiClient, l: &Link) -> Result<FileInfo, String> {
    let key_raw = crypto::b64decode(&l.key)?;
    if key_raw.len() != 32 {
        return Err("can't retrieve file key".into());
    }
    let key = unpack_file_key(&key_raw)?;

    let res = api.call(json!({"a": "g", "g": 1, "ssl": 0, "p": l.handle}))?;
    let g = res.get("g").and_then(|v| v.as_str()).unwrap_or("");
    let s = res.get("s").and_then(|v| v.as_i64()).unwrap_or(-1);
    let at = res.get("at").and_then(|v| v.as_str()).unwrap_or("");
    if s < 0 || g.is_empty() || at.is_empty() {
        return Err("incomplete file info from server".into());
    }
    let name = decrypt_attrs(&key.aes, at).map_err(|_| "invalid key".to_string())?;
    let name = sanitize_name(&name)?;
    Ok(FileInfo { name, size: s, url: g.to_string(), key })
}

impl Resolved {
    /// Listing entries for `--choose-files` style UIs and the TUI's
    /// folder tree.
    pub fn listing(&self) -> Vec<Entry> {
        match self {
            Resolved::File { info, handle, .. } => vec![Entry {
                index: 1,
                path: format!("/{}", info.name),
                name: info.name.clone(),
                is_dir: false,
                size: info.size,
                handle: handle.clone(),
                parent: None,
            }],
            Resolved::Folder { fs, .. } => fs
                .nodes
                .iter()
                .enumerate()
                .map(|(i, n)| Entry {
                    index: i + 1,
                    path: n.path.clone(),
                    name: n.name.clone(),
                    is_dir: n.is_dir,
                    size: n.size,
                    handle: n.handle.clone(),
                    parent: if i == fs.root { None } else { Some(n.parent_handle.clone()) },
                })
                .collect(),
        }
    }

    /// Builds the file jobs to run: `selected` handles (empty = the
    /// whole link), rooted at `dest_dir`. Folders in the selection
    /// expand to every file beneath them; nodes covered by a chosen
    /// ancestor are pruned.
    pub fn plan<'a>(&'a self, dest_dir: &Path, selected: &[String]) -> Result<Vec<FileJob<'a>>, String> {
        match self {
            Resolved::File { info, handle, .. } => {
                let mut path = dest_dir.to_path_buf();
                if path.is_dir() {
                    path = path.join(&info.name);
                }
                let url = info.url.clone();
                let size = info.size;
                let key = info.key;
                Ok(vec![FileJob {
                    local_path: path,
                    remote_path: format!("/{}", info.name),
                    size,
                    handle: handle.clone(),
                    key,
                    get_url: Box::new(move || Ok((url.clone(), size))),
                }])
            }
            Resolved::Folder { fs, api } => {
                let chosen = plan_folder_nodes(fs, selected)?;
                let mut jobs = Vec::new();
                let mut added = std::collections::HashSet::new();
                for &n in &chosen {
                    for f in fs.files_under(n) {
                        let handle = fs.nodes[f].handle.clone();
                        if !added.insert(handle.clone()) {
                            continue;
                        }
                        let local = dest_dir.join(fs.rel_to_root(f));
                        let key = unpack_file_key(&fs.nodes[f].key)?;
                        jobs.push(FileJob {
                            local_path: local,
                            remote_path: fs.nodes[f].path.clone(),
                            size: fs.nodes[f].size,
                            handle: handle.clone(),
                            key,
                            get_url: Box::new(move || fs.download_url(api, &handle)),
                        });
                    }
                }
                Ok(jobs)
            }
        }
    }
}

fn plan_folder_nodes(fs: &FolderFs, selected: &[String]) -> Result<Vec<usize>, String> {
    if selected.is_empty() {
        return Ok(vec![fs.root]);
    }
    let mut seen = std::collections::HashSet::new();
    let mut chosen = Vec::new();
    for h in selected {
        if h.is_empty() || !seen.insert(h.clone()) {
            continue;
        }
        match fs.by_handle.get(h) {
            Some(&i) => chosen.push(i),
            None => eprintln!("warning: handle not found: {h}"),
        }
    }
    chosen.sort_by(|&a, &b| sort_key(fs, a).cmp(&sort_key(fs, b)));
    Ok(prune_children(fs, chosen))
}

fn sort_key(fs: &FolderFs, i: usize) -> String {
    if fs.nodes[i].is_dir {
        format!("{}/", fs.nodes[i].path)
    } else {
        fs.nodes[i].path.clone()
    }
}

/// Drops nodes that have an ancestor already in the list.
fn prune_children(fs: &FolderFs, nodes: Vec<usize>) -> Vec<usize> {
    let set: std::collections::HashSet<usize> = nodes.iter().copied().collect();
    nodes
        .into_iter()
        .filter(|&n| {
            let mut p = fs.nodes[n].parent;
            while let Some(pi) = p {
                if set.contains(&pi) {
                    return false;
                }
                p = fs.nodes[pi].parent;
            }
            true
        })
        .collect()
}

pub fn default_dest_name(resolved: &Resolved) -> String {
    match resolved {
        Resolved::File { info, .. } => info.name.clone(),
        Resolved::Folder { fs, .. } => fs.nodes[fs.root].name.clone(),
    }
}

pub fn dest_path_for(dir: &Path, name: &str) -> PathBuf {
    dir.join(name)
}
