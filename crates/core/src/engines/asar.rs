//! Electron `app.asar` archives (TyranoScript V5 and other Electron games).
//!
//! ```text
//! u32 4   u32 header_size   u32 header_size - 4   u32 json_len   json   pad to 4
//! file data (offsets in the JSON count from here)
//! ```
//!
//! The JSON is a tree: `{"files": {"name": {"files": {...}} | {"size": n,
//! "offset": "123", "integrity": {...}} | {"size": n, "unpacked": true}}}`.
//! Unpacked files live in `app.asar.unpacked/` next to the archive.

use crate::error::{user, Error, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const BLOCK: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct AsarFile {
    /// '/'-separated.
    pub name: String,
    pub offset: u64,
    pub size: u64,
    pub unpacked: bool,
}

pub struct Asar {
    pub path: PathBuf,
    header: Value,
    data_start: u64,
    pub files: Vec<AsarFile>,
}

fn bad(path: &Path, why: &str) -> Error {
    Error::User(format!(
        "{} isn't an app.asar archive Yakusu can read ({why}).",
        path.file_name().unwrap_or_default().to_string_lossy()
    ))
}

fn walk(node: &Value, prefix: &str, out: &mut Vec<AsarFile>) {
    let Some(files) = node.get("files").and_then(Value::as_object) else { return };
    for (name, v) in files {
        let full = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
        if v.get("files").is_some() {
            walk(v, &full, out);
        } else if v.get("link").is_none() {
            out.push(AsarFile {
                name: full,
                offset: v.get("offset").and_then(Value::as_str).and_then(|s| s.parse().ok()).unwrap_or(0),
                size: v.get("size").and_then(Value::as_u64).unwrap_or(0),
                unpacked: v.get("unpacked").and_then(Value::as_bool).unwrap_or(false),
            });
        }
    }
}

/// Whether a file is an asar archive.
pub fn is_asar(path: &Path) -> bool {
    let Ok(mut f) = File::open(path) else { return false };
    let mut h = [0u8; 16];
    if f.read_exact(&mut h).is_err() {
        return false;
    }
    let u = |i: usize| u32::from_le_bytes(h[i..i + 4].try_into().unwrap());
    u(0) == 4 && u(2 * 4) + 4 == u(4) && u(12) < u(8)
}

impl Asar {
    pub fn open(path: &Path) -> Result<Asar> {
        let mut f = File::open(path)?;
        let mut h = [0u8; 16];
        f.read_exact(&mut h).map_err(|_| bad(path, "too short"))?;
        let u = |i: usize| u32::from_le_bytes(h[i..i + 4].try_into().unwrap()) as u64;
        if u(0) != 4 || u(8) + 4 != u(4) || u(12) > u(8) {
            return Err(bad(path, "no asar header"));
        }
        let len = u(12) as usize;
        if len > 256 << 20 {
            return Err(bad(path, "header too large"));
        }
        let mut json = vec![0u8; len];
        f.read_exact(&mut json).map_err(|_| bad(path, "header cut short"))?;
        let header: Value = serde_json::from_slice(&json).map_err(|_| bad(path, "damaged header"))?;
        let mut files = Vec::new();
        walk(&header, "", &mut files);
        Ok(Asar { path: path.to_path_buf(), header, data_start: 8 + u(4), files })
    }

    fn unpacked_dir(&self) -> PathBuf {
        let mut name = self.path.file_name().unwrap_or_default().to_os_string();
        name.push(".unpacked");
        self.path.with_file_name(name)
    }

    pub fn read(&self, file: &AsarFile) -> Result<Vec<u8>> {
        if file.unpacked {
            return Ok(std::fs::read(super::join_rel(&self.unpacked_dir(), &file.name))?);
        }
        let mut f = File::open(&self.path)?;
        f.seek(SeekFrom::Start(self.data_start + file.offset))?;
        let mut out = Vec::with_capacity(file.size as usize);
        f.take(file.size).read_to_end(&mut out)?;
        if out.len() as u64 != file.size {
            return Err(bad(&self.path, &format!("{} is cut short", file.name)));
        }
        Ok(out)
    }

    /// Write a copy with some files replaced (name -> contents) to `out`.
    /// Unpacked files that are replaced are written under
    /// `<out>.unpacked/`. Returns the paths written, relative to out's folder.
    pub fn rewrite(&self, replace: &HashMap<String, Vec<u8>>, out: &Path) -> Result<Vec<String>> {
        let mut header = self.header.clone();
        let mut order: Vec<(AsarFile, Option<&Vec<u8>>)> = Vec::new();
        for f in &self.files {
            order.push((f.clone(), replace.get(&f.name)));
        }
        // New offsets, in the original data order.
        order.sort_by_key(|(f, _)| f.offset);
        let mut at = 0u64;
        let mut written = vec![out.file_name().unwrap_or_default().to_string_lossy().to_string()];
        let unpacked_out = {
            let mut n = out.file_name().unwrap_or_default().to_os_string();
            n.push(".unpacked");
            out.with_file_name(n)
        };
        for (f, new) in &order {
            let node = node_mut(&mut header, &f.name).ok_or_else(|| bad(&self.path, "inconsistent header"))?;
            if f.unpacked {
                if let Some(bytes) = new {
                    let dst = super::safe_join(&unpacked_out, &f.name)?;
                    std::fs::create_dir_all(dst.parent().unwrap())?;
                    std::fs::write(dst, bytes)?;
                    written.push(format!("{}/{}", unpacked_out.file_name().unwrap().to_string_lossy(), f.name));
                    set_size(node, bytes);
                }
                continue;
            }
            let size = new.map(|b| b.len() as u64).unwrap_or(f.size);
            node["offset"] = Value::from(at.to_string());
            if let Some(bytes) = new {
                set_size(node, bytes);
            }
            at += size;
        }
        let json = serde_json::to_vec(&header)?;
        let pad = (4 - json.len() % 4) % 4;
        let payload = 4 + json.len() + pad;
        let mut w = std::io::BufWriter::new(File::create(out)?);
        for v in [4u32, payload as u32 + 4, payload as u32, json.len() as u32] {
            w.write_all(&v.to_le_bytes())?;
        }
        w.write_all(&json)?;
        w.write_all(&vec![0u8; pad])?;
        let mut src = File::open(&self.path)?;
        for (f, new) in &order {
            if f.unpacked {
                continue;
            }
            match new {
                Some(bytes) => w.write_all(bytes)?,
                None => {
                    src.seek(SeekFrom::Start(self.data_start + f.offset))?;
                    let n = std::io::copy(&mut (&mut src).take(f.size), &mut w)?;
                    if n != f.size {
                        return user(format!("{} is cut short in the original archive.", f.name));
                    }
                }
            }
        }
        w.flush()?;
        Ok(written)
    }
}

fn node_mut<'a>(header: &'a mut Value, name: &str) -> Option<&'a mut Value> {
    let mut node = header;
    for part in name.split('/') {
        node = node.get_mut("files")?.get_mut(part)?;
    }
    Some(node)
}

/// New size, and new integrity hashes when the archive records them.
fn set_size(node: &mut Value, bytes: &[u8]) {
    node["size"] = Value::from(bytes.len() as u64);
    if node.get("integrity").is_some() {
        node["integrity"] = integrity(bytes);
    }
}

fn hex(d: &[u8]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

fn integrity(bytes: &[u8]) -> Value {
    let blocks: Vec<Value> = bytes.chunks(BLOCK).map(|c| Value::from(hex(&Sha256::digest(c)))).collect::<Vec<_>>();
    let blocks = if blocks.is_empty() { vec![Value::from(hex(&Sha256::digest(b"")))] } else { blocks };
    json!({"algorithm": "SHA256", "hash": hex(&Sha256::digest(bytes)), "blockSize": BLOCK, "blocks": blocks})
}

/// Build an asar from (name, contents) pairs. Used by tests.
pub fn create(path: &Path, files: &[(&str, &[u8])]) -> Result<()> {
    let mut root = json!({"files": {}});
    let mut data = Vec::new();
    for (name, contents) in files {
        let mut node = &mut root;
        let parts: Vec<&str> = name.split('/').collect();
        for dir in &parts[..parts.len() - 1] {
            let files = node["files"].as_object_mut().unwrap();
            node = files.entry(dir.to_string()).or_insert_with(|| json!({"files": {}}));
        }
        let leaf = json!({"size": contents.len(), "offset": data.len().to_string(), "integrity": integrity(contents)});
        node["files"].as_object_mut().unwrap().insert(parts[parts.len() - 1].to_string(), leaf);
        data.extend_from_slice(contents);
    }
    let json = serde_json::to_vec(&root)?;
    let pad = (4 - json.len() % 4) % 4;
    let payload = (4 + json.len() + pad) as u32;
    let mut out = Vec::new();
    for v in [4u32, payload + 4, payload, json.len() as u32] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&json);
    out.extend(vec![0u8; pad]);
    out.extend(data);
    std::fs::write(path, out)?;
    Ok(())
}
