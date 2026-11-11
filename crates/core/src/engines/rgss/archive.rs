//! RGSS encrypted archives: `Game.rgssad` (XP), `Game.rgss2a` (VX),
//! `Game.rgss3a` (VX Ace). Read, and written back with some files replaced.
//!
//! Version 1 (XP/VX): "RGSSAD\0\x01", then entries one after another. A key
//! starting at 0xDEADCAFE (advanced as key*7+3) hides each name length,
//! name byte and size; the data is XORed in 4-byte blocks with its own copy
//! of the key at that point.
//!
//! Version 3 (VX Ace): "RGSSAD\0\x03", u32 seed, key = seed*9+3, then a table
//! of (offset, size, file key, name length) XORed with the key and ended by
//! offset 0; names are XORed with the key's bytes; data with its file key.

use crate::error::{Error, Result};
use std::path::Path;

#[derive(Clone, Debug)]
pub struct Entry {
    /// '/'-separated.
    pub name: String,
    raw_name: Vec<u8>,
    offset: usize,
    pub size: usize,
    key: u32,
}

pub struct Archive {
    pub version: u8,
    seed: u32,
    data: Vec<u8>,
    pub entries: Vec<Entry>,
}

fn bad(why: &str) -> Error {
    Error::User(format!("The game's RGSS archive can't be read ({why})."))
}

fn next(k: u32) -> u32 {
    k.wrapping_mul(7).wrapping_add(3)
}

/// XOR data in 4-byte blocks, advancing the key per block.
fn crypt(data: &[u8], mut key: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    for chunk in data.chunks(4) {
        let k = key.to_le_bytes();
        out.extend(chunk.iter().zip(k).map(|(b, k)| b ^ k));
        key = next(key);
    }
    out
}

fn u32le(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

pub fn is_archive(path: &Path) -> bool {
    let mut h = [0u8; 8];
    std::fs::File::open(path).and_then(|mut f| std::io::Read::read_exact(&mut f, &mut h)).is_ok()
        && &h[..7] == b"RGSSAD\0"
}

impl Archive {
    pub fn open(path: &Path) -> Result<Archive> {
        let data = std::fs::read(path)?;
        Self::parse(data)
    }

    pub fn parse(data: Vec<u8>) -> Result<Archive> {
        if data.len() < 8 || &data[..7] != b"RGSSAD\0" {
            return Err(bad("not an RGSS archive"));
        }
        let version = data[7];
        let mut entries = Vec::new();
        let mut seed = 0;
        match version {
            1 => {
                let mut key: u32 = 0xDEAD_CAFE;
                let mut i = 8;
                while i < data.len() {
                    let n = (u32le(&data, i).ok_or_else(|| bad("cut short"))? ^ key) as usize;
                    key = next(key);
                    i += 4;
                    let raw = data.get(i..i + n).ok_or_else(|| bad("cut short"))?;
                    let mut name = Vec::with_capacity(n);
                    for &c in raw {
                        name.push(c ^ (key & 0xFF) as u8);
                        key = next(key);
                    }
                    i += n;
                    let size = (u32le(&data, i).ok_or_else(|| bad("cut short"))? ^ key) as usize;
                    key = next(key);
                    i += 4;
                    if i + size > data.len() {
                        return Err(bad("a file runs past the end"));
                    }
                    entries.push(Entry { name: decode(&name), raw_name: name, offset: i, size, key });
                    i += size;
                }
            }
            3 => {
                seed = u32le(&data, 8).ok_or_else(|| bad("cut short"))?;
                let key = seed.wrapping_mul(9).wrapping_add(3);
                let mut i = 12;
                loop {
                    let rd = |at: usize| u32le(&data, at).map(|v| v ^ key).ok_or_else(|| bad("cut short"));
                    let offset = rd(i)? as usize;
                    if offset == 0 {
                        break;
                    }
                    let size = rd(i + 4)? as usize;
                    let fkey = rd(i + 8)?;
                    let n = rd(i + 12)? as usize;
                    i += 16;
                    let raw = data.get(i..i + n).ok_or_else(|| bad("cut short"))?;
                    let kb = key.to_le_bytes();
                    let name: Vec<u8> = raw.iter().enumerate().map(|(j, c)| c ^ kb[j % 4]).collect();
                    i += n;
                    if offset + size > data.len() {
                        return Err(bad("a file runs past the end"));
                    }
                    entries.push(Entry { name: decode(&name), raw_name: name, offset, size, key: fkey });
                }
            }
            v => return Err(bad(&format!("version {v} isn't supported"))),
        }
        Ok(Archive { version, seed, data, entries })
    }

    pub fn read(&self, e: &Entry) -> Vec<u8> {
        crypt(&self.data[e.offset..e.offset + e.size], e.key)
    }

    /// The archive with some files replaced (name -> plain bytes).
    pub fn rebuild(&self, replace: &std::collections::HashMap<String, Vec<u8>>) -> Vec<u8> {
        let plain = |e: &Entry| replace.get(&e.name).cloned();
        let mut out = self.data[..8].to_vec();
        match self.version {
            1 => {
                let mut key: u32 = 0xDEAD_CAFE;
                for e in &self.entries {
                    out.extend((e.raw_name.len() as u32 ^ key).to_le_bytes());
                    key = next(key);
                    for &c in &e.raw_name {
                        out.push(c ^ (key & 0xFF) as u8);
                        key = next(key);
                    }
                    // The data is encrypted with the key as it is after the size.
                    let body = match plain(e) {
                        Some(p) => crypt(&p, next(key)),
                        None => self.data[e.offset..e.offset + e.size].to_vec(),
                    };
                    out.extend((body.len() as u32 ^ key).to_le_bytes());
                    key = next(key);
                    out.extend(body);
                }
            }
            _ => {
                let key = self.seed.wrapping_mul(9).wrapping_add(3);
                out.extend(self.seed.to_le_bytes());
                let table_len: usize = self.entries.iter().map(|e| 16 + e.raw_name.len()).sum::<usize>() + 16;
                let mut at = 12 + table_len;
                let mut bodies = Vec::new();
                for e in &self.entries {
                    let body = match plain(e) {
                        Some(p) => crypt(&p, e.key),
                        None => self.data[e.offset..e.offset + e.size].to_vec(),
                    };
                    for v in [at as u32, body.len() as u32, e.key, e.raw_name.len() as u32] {
                        out.extend((v ^ key).to_le_bytes());
                    }
                    let kb = key.to_le_bytes();
                    out.extend(e.raw_name.iter().enumerate().map(|(j, c)| c ^ kb[j % 4]));
                    at += body.len();
                    bodies.push(body);
                }
                // End of table: offset 0, then padding the original also has.
                for _ in 0..4 {
                    out.extend(key.to_le_bytes());
                }
                for b in bodies {
                    out.extend(b);
                }
            }
        }
        out
    }
}

fn decode(name: &[u8]) -> String {
    String::from_utf8_lossy(name).replace('\\', "/")
}

/// Build an archive from (name, plain bytes). Used by tests.
pub fn create(version: u8, files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut a = Archive { version, seed: 0x1234_5678, data: b"RGSSAD\0".to_vec(), entries: vec![] };
    a.data.push(version);
    let mut map = std::collections::HashMap::new();
    for (i, (name, bytes)) in files.iter().enumerate() {
        let raw = name.replace('/', "\\").into_bytes();
        a.entries.push(Entry {
            name: name.to_string(),
            raw_name: raw,
            offset: 8,
            size: 0,
            key: 0x0BAD_F00D ^ i as u32,
        });
        map.insert(name.to_string(), bytes.to_vec());
    }
    a.rebuild(&map)
}
