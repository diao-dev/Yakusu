//! One compiled SiglusEngine scene (.ss): a 33-field header (`S_tnm_scn_header`,
//! 132 bytes), bytecode, and tables. Every string the scene uses (dialogue,
//! names, choices, but also file names, labels and the like) sits in one
//! string table, each UTF-16 unit XORed with `28807 * string number` (some
//! older games store them plain).
//!
//! What's text is read from the bytecode, never from the table alone: the
//! compiler writes a message as `CD_PUSH FM_STR n` directly followed by
//! `CD_TEXT read_flag`, a speaker as `CD_PUSH FM_STR n` + `CD_NAME`, and a
//! choice menu's strings inside `CD_SEL_BLOCK_START` ... `CD_SEL_BLOCK_END`
//! (siglus_rs's script compiler and VM, xmoezzz, MPL-2.0). Only strings used
//! that way are touched. A string also pushed anywhere else (the compiler
//! shares identical strings) keeps its text; the dialogue gets a new string of
//! its own instead, by changing just the 4-byte number in its `CD_PUSH`.
//! The bytecode never changes size and nothing in it moves.
//!
//! Rebuilding writes the new string tables at the end of the scene and points
//! the header's four string-table fields at them; every other field and
//! section stays as it was.

use super::super::Entry;
use crate::error::{Error, Result};
use crate::protect::JP_RE;
use std::collections::HashMap;

pub const HEADER_INTS: usize = 33;
pub const HEADER_SIZE: usize = HEADER_INTS * 4;
const SCN_OFS: usize = 1;
const SCN_SIZE: usize = 2;
const STR_INDEX_LIST_OFS: usize = 3;
const STR_INDEX_CNT: usize = 4;
const STR_LIST_OFS: usize = 5;
const STR_CNT: usize = 6;
const READ_FLAG_CNT: usize = 32;

const CD_NL: u8 = 0x01;
const CD_PUSH: u8 = 0x02;
const CD_EOF: u8 = 0x16;
const CD_TEXT: u8 = 0x31;
const CD_NAME: u8 = 0x32;
const CD_SEL_BLOCK_START: u8 = 0x33;
const CD_SEL_BLOCK_END: u8 = 0x34;
const FM_STR: i32 = 20;
/// How far a choice menu's statement may run before Yakusu stops looking
/// for its end.
const MAX_SELECT_BYTES: usize = 8192;

fn bad(msg: impl Into<String>) -> Error {
    Error::User(format!("A SiglusEngine scene couldn't be read: {}.", msg.into()))
}

fn i32_at(b: &[u8], p: usize) -> Option<i32> {
    Some(i32::from_le_bytes(b.get(p..p.checked_add(4)?)?.try_into().ok()?))
}

/// The XOR applied to every unit of string `n`.
pub fn string_key(n: usize) -> u16 {
    28807u32.wrapping_mul(n as u32) as u16
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Text,
    Name,
    Choice,
}

/// One place in the bytecode that shows a string: the `CD_PUSH` at `at`
/// (from the start of the bytecode) pushing string `idx`.
#[derive(Clone, Copy, Debug)]
pub struct Site {
    pub at: usize,
    pub idx: usize,
    pub kind: Kind,
}

/// One translatable entry: a message (with the speaker shown for it) or a
/// choice. Indexes into [`Ss::sites`].
#[derive(Clone, Copy, Debug)]
struct Slot {
    site: usize,
    name: Option<usize>,
}

pub struct Ss {
    pub data: Vec<u8>,
    pub h: [i32; HEADER_INTS],
    /// Each string's stored units (still XORed if the game XORs them).
    units: Vec<Vec<u16>>,
    /// The strings are XORed (most games) rather than stored plain.
    pub xor: bool,
}

impl Ss {
    /// Parse with the usual XORed strings.
    pub fn parse(data: Vec<u8>) -> Result<Ss> {
        Self::parse_with(data, true)
    }

    pub fn parse_with(data: Vec<u8>, xor: bool) -> Result<Ss> {
        if data.len() < HEADER_SIZE {
            return Err(bad("it's too short"));
        }
        let mut h = [0i32; HEADER_INTS];
        for (k, v) in h.iter_mut().enumerate() {
            *v = i32_at(&data, k * 4).unwrap();
        }
        if h[0] as usize != HEADER_SIZE {
            return Err(bad(format!("its header says {} bytes, not {HEADER_SIZE}", h[0])));
        }
        let field = |k: usize| usize::try_from(h[k]).map_err(|_| bad("a negative offset or count"));
        let (code_ofs, code_len) = (field(SCN_OFS)?, field(SCN_SIZE)?);
        if code_ofs.checked_add(code_len).is_none_or(|e| e > data.len()) {
            return Err(bad("its bytecode runs past the end"));
        }
        let (idx_ofs, cnt, list_ofs) = (field(STR_INDEX_LIST_OFS)?, field(STR_INDEX_CNT)?, field(STR_LIST_OFS)?);
        if h[STR_CNT] != h[STR_INDEX_CNT] {
            return Err(bad("its string counts don't match"));
        }
        if cnt.checked_mul(8).and_then(|n| n.checked_add(idx_ofs)).is_none_or(|e| e > data.len()) {
            return Err(bad("its string index runs past the end"));
        }
        let mut units = Vec::with_capacity(cnt);
        for i in 0..cnt {
            let (o, n) = (i32_at(&data, idx_ofs + i * 8).unwrap(), i32_at(&data, idx_ofs + i * 8 + 4).unwrap());
            let (Ok(o), Ok(n)) = (usize::try_from(o), usize::try_from(n)) else {
                return Err(bad(format!("string {i} has a negative position")));
            };
            let from = o.checked_mul(2).and_then(|b| b.checked_add(list_ofs));
            let to = from.and_then(|f| f.checked_add(n.checked_mul(2)?));
            let (Some(from), Some(to)) = (from, to) else { return Err(bad("a string position overflows")) };
            let Some(bytes) = data.get(from..to) else {
                return Err(bad(format!("string {i} runs past the end")));
            };
            units.push(bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect());
        }
        Ok(Ss { data, h, units, xor })
    }

    pub fn code(&self) -> &[u8] {
        let (o, n) = (self.h[SCN_OFS] as usize, self.h[SCN_SIZE] as usize);
        &self.data[o..o + n]
    }

    pub fn string_count(&self) -> usize {
        self.units.len()
    }

    /// String `i` as text.
    pub fn string(&self, i: usize) -> String {
        decode(&self.units[i], i, self.xor)
    }

    /// How text-like the strings look decoded plain and decoded XORed:
    /// (plain, xor). For telling which way a game stores them.
    pub fn codec_scores(&self) -> (i64, i64) {
        let score = |s: &str| -> i64 { s.chars().map(|c| if texty(c) { 1 } else { -3 }).sum() };
        self.units
            .iter()
            .enumerate()
            .fold((0, 0), |(p, x), (i, u)| (p + score(&decode(u, i, false)), x + score(&decode(u, i, true))))
    }

    /// Every `CD_PUSH FM_STR n` in the bytecode: (position, n).
    fn pushes(&self) -> Vec<(usize, usize)> {
        let code = self.code();
        let n = self.units.len();
        let mut out = Vec::new();
        for i in 0..code.len().saturating_sub(8) {
            if code[i] == CD_PUSH && i32_at(code, i + 1) == Some(FM_STR) {
                if let Some(idx) = i32_at(code, i + 5).and_then(|v| usize::try_from(v).ok()).filter(|&v| v < n) {
                    out.push((i, idx));
                }
            }
        }
        out
    }

    /// The places that show text, in bytecode order.
    pub fn sites(&self) -> Vec<Site> {
        let code = self.code();
        let flags = self.h[READ_FLAG_CNT];
        let pushes = self.pushes();
        let mut out: Vec<Site> = Vec::new();
        for &(at, idx) in &pushes {
            match code.get(at + 9) {
                Some(&CD_TEXT) => {
                    let rf = i32_at(code, at + 10);
                    if rf.is_some_and(|rf| rf >= 0 && (flags <= 0 || rf < flags)) {
                        out.push(Site { at, idx, kind: Kind::Text });
                    }
                }
                Some(&CD_NAME) => out.push(Site { at, idx, kind: Kind::Name }),
                _ => {}
            }
        }
        // Choice menus: a statement (CD_NL line) that opens a select block.
        let taken: std::collections::HashSet<usize> = out.iter().map(|s| s.at).collect();
        let mut p = 0;
        while p + 6 <= code.len() {
            if code[p] == CD_NL && code[p + 5] == CD_SEL_BLOCK_START {
                let start = p + 6;
                let limit = code.len().min(start + MAX_SELECT_BYTES);
                let end = (start..limit).find(|&q| {
                    code[q] == CD_SEL_BLOCK_END && (q + 1 == code.len() || matches!(code[q + 1], CD_NL | CD_EOF))
                });
                if let Some(end) = end {
                    for &(at, idx) in pushes.iter().filter(|(at, _)| *at >= start && at + 9 <= end) {
                        if !taken.contains(&at) && JP_RE.is_match(&self.string(idx)) {
                            out.push(Site { at, idx, kind: Kind::Choice });
                        }
                    }
                    p = end;
                    continue;
                }
            }
            p += 1;
        }
        out.sort_by_key(|s| s.at);
        out
    }

    fn slots(&self, sites: &[Site]) -> Vec<Slot> {
        let mut out = Vec::new();
        let mut name: Option<usize> = None;
        for (k, s) in sites.iter().enumerate() {
            match s.kind {
                Kind::Name => name = Some(k),
                Kind::Text => out.push(Slot { site: k, name: name.take() }),
                Kind::Choice => out.push(Slot { site: k, name: None }),
            }
        }
        out
    }

    /// The scene's messages (with their speakers) and choices, in order.
    pub fn export(&self) -> Vec<Entry> {
        let sites = self.sites();
        self.slots(&sites)
            .into_iter()
            .map(|sl| Entry {
                name: sl.name.map(|n| self.string(sites[n].idx)).filter(|n| !n.is_empty()),
                names: None,
                message: self.string(sites[sl.site].idx),
            })
            .collect()
    }

    /// The scene with `entries` (same order as [`Ss::export`]) written in.
    /// Unchanged entries leave the scene byte for byte as it was.
    pub fn apply(&self, entries: &[Entry]) -> Result<Vec<u8>> {
        let sites = self.sites();
        let slots = self.slots(&sites);
        if entries.len() != slots.len() {
            return Err(Error::User(format!(
                "The translated file has {} lines but the scene has {}. Read the scripts again.",
                entries.len(),
                slots.len()
            )));
        }
        // What each site should show.
        let mut want: HashMap<usize, String> = HashMap::new();
        for (sl, e) in slots.iter().zip(entries) {
            want.insert(sites[sl.site].at, e.message.clone());
            if let (Some(n), Some(name)) = (sl.name, e.name.as_ref().filter(|n| !n.trim().is_empty())) {
                want.insert(sites[n].at, name.clone());
            }
        }
        let mut strings: Vec<String> = (0..self.units.len()).map(|i| self.string(i)).collect();
        let original = strings.clone();
        let mut uses: HashMap<usize, Vec<usize>> = HashMap::new();
        for (at, idx) in self.pushes() {
            uses.entry(idx).or_default().push(at);
        }
        let mut repoint: Vec<(usize, usize)> = Vec::new();
        let mut added: HashMap<String, usize> = HashMap::new();
        let mut by_idx: HashMap<usize, Vec<(usize, String)>> = HashMap::new();
        for s in &sites {
            if let Some(w) = want.get(&s.at).filter(|w| **w != original[s.idx]) {
                by_idx.entry(s.idx).or_default().push((s.at, w.clone()));
            }
        }
        let mut idxs: Vec<usize> = by_idx.keys().copied().collect();
        idxs.sort_unstable();
        for idx in idxs {
            let mine = &by_idx[&idx];
            // Every push of this string is one of ours being changed: the
            // string itself can change (to the first one's text).
            let exclusive = uses.get(&idx).is_some_and(|u| u.len() == mine.len());
            let in_place = exclusive.then(|| mine[0].1.clone());
            if let Some(t) = &in_place {
                strings[idx] = t.clone();
            }
            for (at, text) in mine {
                if in_place.as_ref() == Some(text) {
                    continue;
                }
                let n = *added.entry(text.clone()).or_insert_with(|| {
                    strings.push(text.clone());
                    strings.len() - 1
                });
                repoint.push((*at, n));
            }
        }
        if strings == original && repoint.is_empty() {
            return Ok(self.data.clone());
        }
        let out = self.rebuild(&strings, &repoint)?;
        // Read it back. The bytecode is the same size, so every string push
        // is where it was: each must now show what was asked for, and every
        // other push exactly what it showed before.
        let check = Ss::parse_with(out.clone(), self.xor)?;
        if check.code().len() != self.code().len() {
            return Err(Error::User("Scene check failed: the bytecode changed size.".into()));
        }
        let code = check.code();
        for (at, idx) in self.pushes() {
            let now = i32_at(code, at + 5).and_then(|v| usize::try_from(v).ok()).filter(|&v| v < check.string_count());
            let shown = now.map(|n| check.string(n));
            let expected = want.get(&at).unwrap_or(&original[idx]);
            if shown.as_ref() != Some(expected) {
                return Err(Error::User(format!(
                    "Scene check failed: a string didn't read back as written ({:?}).",
                    expected.chars().take(30).collect::<String>()
                )));
            }
        }
        Ok(out)
    }

    /// The scene with new strings (tables appended at the end) and the
    /// given pushes pointed at other string numbers.
    fn rebuild(&self, strings: &[String], repoint: &[(usize, usize)]) -> Result<Vec<u8>> {
        let mut out = self.data.clone();
        while !out.len().is_multiple_of(4) {
            out.push(0);
        }
        let idx_ofs = out.len();
        let mut list: Vec<u8> = Vec::new();
        let mut index: Vec<u8> = Vec::with_capacity(strings.len() * 8);
        let mut units_at = 0usize;
        for (i, s) in strings.iter().enumerate() {
            let u: Vec<u16> = s.encode_utf16().collect();
            index.extend_from_slice(&(units_at as i32).to_le_bytes());
            index.extend_from_slice(&(u.len() as i32).to_le_bytes());
            let key = if self.xor { string_key(i) } else { 0 };
            list.extend(u.iter().flat_map(|c| (c ^ key).to_le_bytes()));
            units_at += u.len();
        }
        out.extend_from_slice(&index);
        let list_ofs = out.len();
        out.extend_from_slice(&list);
        if out.len() > super::lzss::MAX_SCENE_BYTES {
            return Err(Error::User("A translated scene would be too big for the game.".into()));
        }
        let mut h = self.h;
        h[STR_INDEX_LIST_OFS] = idx_ofs as i32;
        h[STR_INDEX_CNT] = strings.len() as i32;
        h[STR_LIST_OFS] = list_ofs as i32;
        h[STR_CNT] = strings.len() as i32;
        for (k, v) in h.iter().enumerate() {
            out[k * 4..k * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
        let code_ofs = self.h[SCN_OFS] as usize;
        for &(at, n) in repoint {
            let p = code_ofs + at + 5;
            out[p..p + 4].copy_from_slice(&(n as i32).to_le_bytes());
        }
        Ok(out)
    }
}

fn decode(units: &[u16], i: usize, xor: bool) -> String {
    let key = if xor { string_key(i) } else { 0 };
    String::from_utf16_lossy(&units.iter().map(|u| u ^ key).collect::<Vec<_>>())
}

/// Characters real scene strings are made of.
fn texty(c: char) -> bool {
    matches!(c as u32,
        0x20..=0x7E | 0x0A | 0x3000..=0x30FF | 0x4E00..=0x9FFF | 0xFF01..=0xFF9F | 0x2010..=0x2026 | 0x25A0..=0x266F)
}

/// A .ss scene from parts, for tests and tools. `strings` are stored XORed
/// when `xor`; `code` is the bytecode.
pub fn build(code: &[u8], strings: &[&str], read_flags: usize, xor: bool) -> Vec<u8> {
    let mut h = [0i32; HEADER_INTS];
    h[0] = HEADER_SIZE as i32;
    let mut body: Vec<u8> = Vec::new();
    h[SCN_OFS] = HEADER_SIZE as i32;
    h[SCN_SIZE] = code.len() as i32;
    body.extend_from_slice(code);
    while !body.len().is_multiple_of(4) {
        body.push(0);
    }
    h[STR_INDEX_LIST_OFS] = (HEADER_SIZE + body.len()) as i32;
    h[STR_INDEX_CNT] = strings.len() as i32;
    let mut list = Vec::new();
    let mut at = 0usize;
    for (i, s) in strings.iter().enumerate() {
        let u: Vec<u16> = s.encode_utf16().collect();
        body.extend_from_slice(&(at as i32).to_le_bytes());
        body.extend_from_slice(&(u.len() as i32).to_le_bytes());
        let key = if xor { string_key(i) } else { 0 };
        list.extend(u.iter().flat_map(|c| (c ^ key).to_le_bytes()));
        at += u.len();
    }
    h[STR_LIST_OFS] = (HEADER_SIZE + body.len()) as i32;
    h[STR_CNT] = strings.len() as i32;
    body.extend_from_slice(&list);
    // Empty label/command/name tables point at the end.
    let end = (HEADER_SIZE + body.len()) as i32;
    for k in (7..31).step_by(2) {
        h[k] = end;
    }
    h[31] = end;
    body.extend((0..read_flags).flat_map(|n| (n as i32).to_le_bytes()));
    h[READ_FLAG_CNT] = read_flags as i32;
    let mut out: Vec<u8> = h.iter().flat_map(|v| v.to_le_bytes()).collect();
    out.extend_from_slice(&body);
    out
}

/// Bytecode helpers for tests and tools: the instructions the compiler
/// writes for a message, a speaker and a statement.
pub mod asm {
    use super::*;
    pub fn nl(line: i32) -> Vec<u8> {
        let mut v = vec![CD_NL];
        v.extend_from_slice(&line.to_le_bytes());
        v
    }
    pub fn push_str(n: i32) -> Vec<u8> {
        let mut v = vec![CD_PUSH];
        v.extend_from_slice(&FM_STR.to_le_bytes());
        v.extend_from_slice(&n.to_le_bytes());
        v
    }
    pub fn push_int(n: i32) -> Vec<u8> {
        let mut v = vec![CD_PUSH];
        v.extend_from_slice(&10i32.to_le_bytes());
        v.extend_from_slice(&n.to_le_bytes());
        v
    }
    pub fn text(n: i32, read_flag: i32) -> Vec<u8> {
        let mut v = push_str(n);
        v.push(CD_TEXT);
        v.extend_from_slice(&read_flag.to_le_bytes());
        v
    }
    pub fn name(n: i32) -> Vec<u8> {
        let mut v = push_str(n);
        v.push(CD_NAME);
        v
    }
    /// A command taking `args` string/int forms (CD_COMMAND with its form list).
    pub fn command(arg_forms: &[i32]) -> Vec<u8> {
        let mut v = vec![0x30];
        v.extend_from_slice(&0i32.to_le_bytes());
        v.extend_from_slice(&(arg_forms.len() as i32).to_le_bytes());
        for f in arg_forms {
            v.extend_from_slice(&f.to_le_bytes());
        }
        v.extend_from_slice(&0i32.to_le_bytes());
        v.extend_from_slice(&0i32.to_le_bytes());
        v
    }
    pub const SEL_START: u8 = CD_SEL_BLOCK_START;
    pub const SEL_END: u8 = CD_SEL_BLOCK_END;
    pub const EOF: u8 = CD_EOF;
}
