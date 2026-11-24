//! SiglusEngine's `Scene.pck`: one file holding every compiled scene (.ss).
//!
//! Layout (all little-endian i32, offsets from the start of the file, an
//! optional "pack_scn" signature first in newer builds): a 23-field header,
//! then the include/command/name tables, then the scene index
//! `(offset from scn_data_list_ofs, size)` per scene, then the scene data.
//! Each scene is LZSS-packed and XORed with a fixed 256-byte table and, when
//! `scn_data_exe_angou_mod` is set, a 16-byte key unique to each game. Packs
//! whose `original_source_header_size` is 0 ("easy link") store scenes as
//! they are.
//!
//! Format and the 256-byte table as in siglus_rs (xmoezzz, MPL-2.0) and
//! SiglusExtract (GPL-3.0); rewriting follows SiglusExtract's CreateScenePck:
//! everything before the scene data is kept byte for byte.

use super::lzss;
use crate::error::{Error, Result};
use std::collections::HashMap;

/// XORed over every packed scene (`easy angou code`).
pub const EASY_KEY: [u8; 256] = [
    0x70, 0xF8, 0xA6, 0xB0, 0xA1, 0xA5, 0x28, 0x4F, 0xB5, 0x2F, 0x48, 0xFA, 0xE1, 0xE9, 0x4B, 0xDE, 0xB7, 0x4F, 0x62,
    0x95, 0x8B, 0xE0, 0x03, 0x80, 0xE7, 0xCF, 0x0F, 0x6B, 0x92, 0x01, 0xEB, 0xF8, 0xA2, 0x88, 0xCE, 0x63, 0x04, 0x38,
    0xD2, 0x6D, 0x8C, 0xD2, 0x88, 0x76, 0xA7, 0x92, 0x71, 0x8F, 0x4E, 0xB6, 0x8D, 0x01, 0x79, 0x88, 0x83, 0x0A, 0xF9,
    0xE9, 0x2C, 0xDB, 0x67, 0xDB, 0x91, 0x14, 0xD5, 0x9A, 0x4E, 0x79, 0x17, 0x23, 0x08, 0x96, 0x0E, 0x1D, 0x15, 0xF9,
    0xA5, 0xA0, 0x6F, 0x58, 0x17, 0xC8, 0xA9, 0x46, 0xDA, 0x22, 0xFF, 0xFD, 0x87, 0x12, 0x42, 0xFB, 0xA9, 0xB8, 0x67,
    0x6C, 0x91, 0x67, 0x64, 0xF9, 0xD1, 0x1E, 0xE4, 0x50, 0x64, 0x6F, 0xF2, 0x0B, 0xDE, 0x40, 0xE7, 0x47, 0xF1, 0x03,
    0xCC, 0x2A, 0xAD, 0x7F, 0x34, 0x21, 0xA0, 0x64, 0x26, 0x98, 0x6C, 0xED, 0x69, 0xF4, 0xB5, 0x23, 0x08, 0x6E, 0x7D,
    0x92, 0xF6, 0xEB, 0x93, 0xF0, 0x7A, 0x89, 0x5E, 0xF9, 0xF8, 0x7A, 0xAF, 0xE8, 0xA9, 0x48, 0xC2, 0xAC, 0x11, 0x6B,
    0x2B, 0x33, 0xA7, 0x40, 0x0D, 0xDC, 0x7D, 0xA7, 0x5B, 0xCF, 0xC8, 0x31, 0xD1, 0x77, 0x52, 0x8D, 0x82, 0xAC, 0x41,
    0xB8, 0x73, 0xA5, 0x4F, 0x26, 0x7C, 0x0F, 0x39, 0xDA, 0x5B, 0x37, 0x4A, 0xDE, 0xA4, 0x49, 0x0B, 0x7C, 0x17, 0xA3,
    0x43, 0xAE, 0x77, 0x06, 0x64, 0x73, 0xC0, 0x43, 0xA3, 0x18, 0x5A, 0x0F, 0x9F, 0x02, 0x4C, 0x7E, 0x8B, 0x01, 0x9F,
    0x2D, 0xAE, 0x72, 0x54, 0x13, 0xFF, 0x96, 0xAE, 0x0B, 0x34, 0x58, 0xCF, 0xE3, 0x00, 0x78, 0xBE, 0xE3, 0xF5, 0x61,
    0xE4, 0x87, 0x7C, 0xFC, 0x80, 0xAF, 0xC4, 0x8D, 0x46, 0x3A, 0x5D, 0xD0, 0x36, 0xBC, 0xE5, 0x60, 0x77, 0x68, 0x08,
    0x4F, 0xBB, 0xAB, 0xE2, 0x78, 0x07, 0xE8, 0x73, 0xBF,
];

/// Header fields, in file order.
pub const FIELDS: usize = 23;
const SCN_NAME_INDEX_LIST_OFS: usize = 13;
const SCN_NAME_LIST_OFS: usize = 15;
const SCN_NAME_CNT: usize = 16;
const SCN_DATA_INDEX_LIST_OFS: usize = 17;
const SCN_DATA_INDEX_CNT: usize = 18;
const SCN_DATA_LIST_OFS: usize = 19;
const SCN_DATA_CNT: usize = 20;
const SCN_DATA_EXE_ANGOU_MOD: usize = 21;
const ORIGINAL_SOURCE_HEADER_SIZE: usize = 22;

fn bad(msg: impl Into<String>) -> Error {
    Error::User(format!("Scene.pck couldn't be read: {}.", msg.into()))
}

pub fn xor(buf: &mut [u8], key: &[u8]) {
    for (i, b) in buf.iter_mut().enumerate() {
        *b ^= key[i % key.len()];
    }
}

pub struct Pck {
    pub data: Vec<u8>,
    pub h: [i32; FIELDS],
    /// Scene names, by scene number ("" where the table has none).
    pub names: Vec<String>,
    /// (offset from the scene data, size) per scene.
    pub index: Vec<(usize, usize)>,
}

impl Pck {
    pub fn parse(data: Vec<u8>) -> Result<Pck> {
        if data.len() > lzss::MAX_SCENE_BYTES * 16 {
            return Err(bad("it's far larger than any scene pack"));
        }
        let start = if data.starts_with(b"pack_scn") { 8 } else { 0 };
        if data.len() < start + FIELDS * 4 {
            return Err(bad("it's too short"));
        }
        let mut h = [0i32; FIELDS];
        for (k, v) in h.iter_mut().enumerate() {
            let p = start + k * 4;
            *v = i32::from_le_bytes(data[p..p + 4].try_into().unwrap());
        }
        let field = |k: usize, what: &str| -> Result<usize> {
            usize::try_from(h[k]).map_err(|_| bad(format!("its {what} is negative")))
        };
        let data_ofs = field(SCN_DATA_LIST_OFS, "scene data offset")?;
        let idx_ofs = field(SCN_DATA_INDEX_LIST_OFS, "scene index offset")?;
        let count =
            if h[SCN_DATA_CNT] > 0 { field(SCN_DATA_CNT, "")? } else { field(SCN_DATA_INDEX_CNT, "scene count")? };
        if data_ofs > data.len() {
            return Err(bad("its scene data starts past its end"));
        }
        // The index is rewritten in place: it must lie before the scene data.
        let idx_end =
            count.checked_mul(8).and_then(|n| n.checked_add(idx_ofs)).ok_or_else(|| bad("bad scene count"))?;
        if idx_end > data_ofs {
            return Err(bad("its scene index is out of place"));
        }
        let mut index = Vec::with_capacity(count);
        for i in 0..count {
            let p = idx_ofs + i * 8;
            let o = i32::from_le_bytes(data[p..p + 4].try_into().unwrap());
            let s = i32::from_le_bytes(data[p + 4..p + 8].try_into().unwrap());
            let (Ok(o), Ok(s)) = (usize::try_from(o), usize::try_from(s)) else {
                return Err(bad(format!("scene {i} has a negative position")));
            };
            let end = data_ofs.checked_add(o).and_then(|a| a.checked_add(s));
            if end.is_none_or(|e| e > data.len()) {
                return Err(bad(format!("scene {i} runs past the end of the file")));
            }
            index.push((o, s));
        }
        let names = read_names(&data, &h, count);
        Ok(Pck { data, h, names, index })
    }

    /// Scenes are LZSS-packed and XORed (not an "easy link" pack).
    pub fn packed(&self) -> bool {
        self.h[ORIGINAL_SOURCE_HEADER_SIZE] > 0
    }

    /// Scenes are also XORed with the game's own 16-byte key.
    pub fn needs_key(&self) -> bool {
        self.packed() && self.h[SCN_DATA_EXE_ANGOU_MOD] != 0
    }

    fn data_ofs(&self) -> usize {
        self.h[SCN_DATA_LIST_OFS] as usize
    }

    pub fn raw(&self, i: usize) -> &[u8] {
        let (o, s) = self.index[i];
        let at = self.data_ofs() + o;
        &self.data[at..at + s]
    }

    /// Scene `i` as a plain .ss file.
    pub fn scene(&self, i: usize, key: Option<&[u8; 16]>) -> Result<Vec<u8>> {
        let mut buf = self.raw(i).to_vec();
        if !self.packed() {
            return Ok(buf);
        }
        if self.needs_key() {
            let Some(k) = key else {
                return Err(Error::User("Scene.pck needs this game's 16-byte key.".into()));
            };
            xor(&mut buf, k);
        }
        xor(&mut buf, &EASY_KEY);
        lzss::unpack(&buf)
    }

    /// Scene data as stored in the pack.
    pub fn encode(&self, ss: &[u8], key: Option<&[u8; 16]>) -> Vec<u8> {
        if !self.packed() {
            return ss.to_vec();
        }
        let mut buf = lzss::pack(ss);
        xor(&mut buf, &EASY_KEY);
        if let (true, Some(k)) = (self.needs_key(), key) {
            xor(&mut buf, k);
        }
        buf
    }

    /// `key` opens this pack: the first scene with data unpacks into a
    /// well-formed .ss (its header says 132 bytes and fits the scene).
    pub fn check_key(&self, key: &[u8; 16]) -> bool {
        let Some(i) = (0..self.index.len()).find(|&i| self.index[i].1 > 0) else { return true };
        // The unpacked size in the LZSS header comes first: a wrong key gives
        // an absurd one, refused before any work.
        let mut head = self.raw(i)[..self.raw(i).len().min(8)].to_vec();
        xor(&mut head, key);
        xor(&mut head, &EASY_KEY);
        // (Packs rebuilt by SiglusExtract count the size without the 8-byte
        // header: allow a few bytes either way.)
        let size = self.index[i].1;
        let packed = head.get(0..4).map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize);
        if packed.is_none_or(|p| p + 16 < size || p > size + 16)
            || lzss::unpacked_size(&head).is_none_or(|n| n > lzss::MAX_SCENE_BYTES)
        {
            return false;
        }
        self.scene(i, Some(key)).is_ok_and(|ss| super::ss::Ss::parse(ss).is_ok())
    }

    /// The pack with scenes replaced (`replace`: scene number -> new .ss).
    /// Untouched scenes are copied as stored; everything before the scene
    /// data, apart from the index, and anything after it is kept.
    pub fn rewrite(&self, replace: &HashMap<usize, Vec<u8>>, key: Option<&[u8; 16]>) -> Result<Vec<u8>> {
        let data_ofs = self.data_ofs();
        let idx_ofs = self.h[SCN_DATA_INDEX_LIST_OFS] as usize;
        let tail_from = self.index.iter().map(|(o, s)| data_ofs + o + s).max().unwrap_or(data_ofs).max(data_ofs);
        let mut out = self.data[..data_ofs].to_vec();
        let mut at = 0usize;
        for i in 0..self.index.len() {
            let chunk = match replace.get(&i) {
                Some(ss) => std::borrow::Cow::Owned(self.encode(ss, key)),
                None => std::borrow::Cow::Borrowed(self.raw(i)),
            };
            let p = idx_ofs + i * 8;
            let (Ok(o), Ok(s)) = (i32::try_from(at), i32::try_from(chunk.len())) else {
                return Err(bad("the rebuilt pack would be too big"));
            };
            out[p..p + 4].copy_from_slice(&o.to_le_bytes());
            out[p + 4..p + 8].copy_from_slice(&s.to_le_bytes());
            out.extend_from_slice(&chunk);
            at += chunk.len();
        }
        out.extend_from_slice(&self.data[tail_from..]);
        Ok(out)
    }
}

/// Scene names from the name tables (UTF-16, `(offset, length)` in units).
fn read_names(data: &[u8], h: &[i32; FIELDS], count: usize) -> Vec<String> {
    let mut names = vec![String::new(); count];
    let (Ok(idx), Ok(list), Ok(n)) = (
        usize::try_from(h[SCN_NAME_INDEX_LIST_OFS]),
        usize::try_from(h[SCN_NAME_LIST_OFS]),
        usize::try_from(h[SCN_NAME_CNT]),
    ) else {
        return names;
    };
    for (i, name) in names.iter_mut().enumerate().take(n) {
        let p = idx + i * 8;
        let Some(e) = data.get(p..p + 8) else { break };
        let o = u32::from_le_bytes(e[0..4].try_into().unwrap()) as usize;
        let l = u32::from_le_bytes(e[4..8].try_into().unwrap()) as usize;
        let Some(bytes) = o
            .checked_mul(2)
            .and_then(|a| Some((list.checked_add(a)?, l.checked_mul(2)?)))
            .and_then(|(from, len)| data.get(from..from.checked_add(len)?))
        else {
            continue;
        };
        let units: Vec<u16> =
            bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|u| *u != 0).collect();
        *name = String::from_utf16_lossy(&units);
    }
    names
}

/// A Scene.pck from scenes (`(name, .ss bytes)`), for tests and tools:
/// the tables a real pack has, empty, then the names, index and data.
pub fn build(scenes: &[(String, Vec<u8>)], key: Option<&[u8; 16]>, packed: bool) -> Vec<u8> {
    let mut h = [0i32; FIELDS];
    h[0] = (FIELDS * 4) as i32;
    let mut body: Vec<u8> = Vec::new();
    let base = FIELDS * 4;
    // Empty include/command tables all point just past the header.
    for k in [1, 3, 5, 7, 9, 11] {
        h[k] = base as i32;
    }
    let mut name_units: Vec<u16> = Vec::new();
    let mut name_index: Vec<u8> = Vec::new();
    for (name, _) in scenes {
        let u: Vec<u16> = name.encode_utf16().collect();
        name_index.extend_from_slice(&(name_units.len() as i32).to_le_bytes());
        name_index.extend_from_slice(&(u.len() as i32).to_le_bytes());
        name_units.extend(u);
    }
    h[SCN_NAME_INDEX_LIST_OFS] = (base + body.len()) as i32;
    h[SCN_NAME_INDEX_LIST_OFS + 1] = scenes.len() as i32;
    body.extend_from_slice(&name_index);
    h[SCN_NAME_LIST_OFS] = (base + body.len()) as i32;
    h[SCN_NAME_CNT] = scenes.len() as i32;
    body.extend(name_units.iter().flat_map(|u| u.to_le_bytes()));
    h[SCN_DATA_INDEX_LIST_OFS] = (base + body.len()) as i32;
    h[SCN_DATA_INDEX_CNT] = scenes.len() as i32;
    let idx_at = body.len();
    body.resize(body.len() + scenes.len() * 8, 0);
    h[SCN_DATA_LIST_OFS] = (base + body.len()) as i32;
    h[SCN_DATA_CNT] = scenes.len() as i32;
    h[SCN_DATA_EXE_ANGOU_MOD] = key.is_some() as i32;
    h[ORIGINAL_SOURCE_HEADER_SIZE] = if packed { 4 } else { 0 };
    let mut out: Vec<u8> = h.iter().flat_map(|v| v.to_le_bytes()).collect();
    out.extend_from_slice(&body);
    let shell = Pck { data: out.clone(), h, names: vec![], index: vec![] };
    let mut at = 0usize;
    for (i, (_, ss)) in scenes.iter().enumerate() {
        let chunk = shell.encode(ss, key);
        let p = base + idx_at + i * 8;
        out[p..p + 4].copy_from_slice(&(at as i32).to_le_bytes());
        out[p + 4..p + 8].copy_from_slice(&(chunk.len() as i32).to_le_bytes());
        out.extend_from_slice(&chunk);
        at += chunk.len();
    }
    out
}
