//! RPG Maker 2000 / 2003: `RPG_RT.ldb` (database) and `Map####.lmu` (maps).
//!
//! LCF files are a header string, then a "struct": chunks of (BER id, BER
//! size, data) ended by id 0. Arrays are a BER count of (BER index, struct).
//! Event command lists are (code, indent, string, params) tuples ended by
//! four zero bytes, with their byte size stored in a sibling chunk. Only the
//! chunks holding text are opened; everything else is copied as it was.
//! Chunk numbers follow liblcf (EasyRPG, MIT).
//!
//! Text is Shift-JIS (CP932) in Japanese games.

use super::{
    clean_dir, join_rel, list_files, read_json_messages, safe_join, write_json_messages, Ctx, Engine, Entry, Installer,
    Match,
};
use crate::error::{user, Error, Result};
use crate::protect::JP_RE;
use serde_json::Map;
use std::fs;
use std::path::{Path, PathBuf};

type R<T> = std::result::Result<T, String>;

fn ber(b: &[u8], i: &mut usize) -> R<u32> {
    let mut v: u32 = 0;
    for _ in 0..5 {
        let c = *b.get(*i).ok_or("cut short")?;
        *i += 1;
        v = (v << 7) | (c & 0x7F) as u32;
        if c & 0x80 == 0 {
            return Ok(v);
        }
    }
    Err("bad number".into())
}

fn put_ber(out: &mut Vec<u8>, v: u32) {
    let mut groups = vec![(v & 0x7F) as u8];
    let mut x = v >> 7;
    while x > 0 {
        groups.push((x & 0x7F) as u8 | 0x80);
        x >>= 7;
    }
    out.extend(groups.iter().rev());
}

#[derive(Clone, Debug, PartialEq)]
pub struct Chunk {
    pub id: u32,
    pub data: Vec<u8>,
}

/// A struct: its chunks, and whether it ended with the 0 terminator.
#[derive(Clone, Debug, PartialEq)]
pub struct Struct {
    pub chunks: Vec<Chunk>,
    terminated: bool,
}

fn read_struct(b: &[u8], i: &mut usize) -> R<Struct> {
    let mut chunks = Vec::new();
    loop {
        if *i >= b.len() {
            return Ok(Struct { chunks, terminated: false });
        }
        let id = ber(b, i)?;
        if id == 0 {
            return Ok(Struct { chunks, terminated: true });
        }
        let n = ber(b, i)? as usize;
        let data = b.get(*i..*i + n).ok_or("chunk cut short")?.to_vec();
        *i += n;
        chunks.push(Chunk { id, data });
    }
}

fn write_struct(s: &Struct, out: &mut Vec<u8>) {
    for c in &s.chunks {
        put_ber(out, c.id);
        put_ber(out, c.data.len() as u32);
        out.extend_from_slice(&c.data);
    }
    if s.terminated {
        out.push(0);
    }
}

/// An array of structs: (index, struct).
fn read_array(b: &[u8]) -> R<Vec<(u32, Struct)>> {
    let mut i = 0;
    let n = ber(b, &mut i)? as usize;
    let mut out = Vec::with_capacity(n.min(4096));
    for _ in 0..n {
        let id = ber(b, &mut i)?;
        out.push((id, read_struct(b, &mut i)?));
    }
    if i != b.len() {
        return Err("unexpected bytes after an array".into());
    }
    Ok(out)
}

fn write_array(a: &[(u32, Struct)]) -> Vec<u8> {
    let mut out = Vec::new();
    put_ber(&mut out, a.len() as u32);
    for (id, s) in a {
        put_ber(&mut out, *id);
        write_struct(s, &mut out);
    }
    out
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cmd {
    pub code: u32,
    pub indent: u32,
    pub string: Vec<u8>,
    /// The parameter count and values, as stored.
    params: Vec<u8>,
}

fn read_commands(b: &[u8]) -> R<Vec<Cmd>> {
    let mut i = 0;
    let mut out = Vec::new();
    loop {
        if b.get(i) == Some(&0) || i >= b.len() {
            break;
        }
        let code = ber(b, &mut i)?;
        let indent = ber(b, &mut i)?;
        let n = ber(b, &mut i)? as usize;
        let string = b.get(i..i + n).ok_or("string cut short")?.to_vec();
        i += n;
        let p0 = i;
        let np = ber(b, &mut i)?;
        for _ in 0..np {
            ber(b, &mut i)?;
        }
        out.push(Cmd { code, indent, string, params: b[p0..i].to_vec() });
    }
    Ok(out)
}

/// A command's first parameter.
fn param0(c: &Cmd) -> Option<u32> {
    let mut i = 0;
    let n = ber(&c.params, &mut i).ok()?;
    (n > 0).then(|| ber(&c.params, &mut i).ok()).flatten()
}

fn write_commands(cmds: &[Cmd]) -> Vec<u8> {
    let mut out = Vec::new();
    for c in cmds {
        put_ber(&mut out, c.code);
        put_ber(&mut out, c.indent);
        put_ber(&mut out, c.string.len() as u32);
        out.extend_from_slice(&c.string);
        out.extend_from_slice(&c.params);
    }
    out.extend([0, 0, 0, 0]);
    out
}

/// A whole LCF file: header string and the top struct.
#[derive(Clone, Debug, PartialEq)]
pub struct File {
    pub header: String,
    pub top: Struct,
}

pub fn read_file(b: &[u8]) -> R<File> {
    let mut i = 0;
    let n = ber(b, &mut i)? as usize;
    let header = String::from_utf8_lossy(b.get(i..i + n).ok_or("cut short")?).to_string();
    i += n;
    if !header.starts_with("Lcf") {
        return Err("not an RPG Maker 2000/2003 file".into());
    }
    let top = read_struct(b, &mut i)?;
    if i != b.len() {
        return Err("unexpected bytes at the end".into());
    }
    Ok(File { header, top })
}

pub fn write_file(f: &File) -> Vec<u8> {
    let mut out = Vec::new();
    put_ber(&mut out, f.header.len() as u32);
    out.extend(f.header.as_bytes());
    write_struct(&f.top, &mut out);
    out
}

// ---------------------------------------------------------------- text

const MESSAGE: u32 = 10110;
const MESSAGE_LINE: u32 = 20110;
const CHOICES: u32 = 10140;
const CHOICE: u32 = 20140;
const HERO_NAME: u32 = 10610;
const HERO_TITLE: u32 = 10620;
const WINDOW_LINES: usize = 4;

/// Database lists: (database chunk, [text field chunks], name field).
const DB: &[(u32, &[u32], Option<u32>)] = &[
    (0x0B, &[0x01, 0x02], Some(0x01)),                   // actors: name, title
    (0x0C, &[0x01, 0x02, 0x03, 0x04, 0x07], None),       // skills
    (0x0D, &[0x01, 0x02], None),                         // items
    (0x0E, &[0x01], None),                               // enemies
    (0x11, &[0x01], None),                               // attributes
    (0x12, &[0x01, 0x33, 0x34, 0x35, 0x36, 0x37], None), // states
    (0x1E, &[0x01], None),                               // classes (2003)
];
const DB_TERMS: u32 = 0x15;
const DB_TROOPS: u32 = 0x0F;
const DB_COMMON: u32 = 0x19;
const MAP_EVENTS: u32 = 0x51;

fn sjis(b: &[u8]) -> String {
    encoding_rs::SHIFT_JIS.decode(b).0.into_owned()
}

fn to_sjis(s: &str) -> Vec<u8> {
    crate::textfit::encode_sjis(s)
}

struct Walk<'a> {
    out: Vec<Entry>,
    input: Option<&'a [Entry]>,
    k: usize,
}

impl Walk<'_> {
    fn take(&mut self, s: &str, is_name: bool) -> Option<String> {
        match self.input {
            None => {
                self.out.push(Entry { name: is_name.then(|| s.to_string()), names: None, message: s.to_string() });
                None
            }
            Some(es) => {
                let e = es.get(self.k)?;
                self.k += 1;
                Some(e.message.clone()).filter(|m| m != s)
            }
        }
    }

    fn text(&mut self, data: &mut Vec<u8>, is_name: bool) {
        let s = sjis(data);
        if JP_RE.is_match(&s) {
            if let Some(new) = self.take(&s, is_name) {
                *data = to_sjis(&new);
            }
        }
    }

    fn commands(&mut self, cmds: Vec<Cmd>) -> Vec<Cmd> {
        let mut out = Vec::with_capacity(cmds.len());
        // Choice texts per indent, for the branches that repeat them.
        let mut choices: std::collections::HashMap<u32, Vec<String>> = Default::default();
        let mut i = 0;
        while i < cmds.len() {
            let c = &cmds[i];
            match c.code {
                MESSAGE => {
                    let mut j = i + 1;
                    let mut lines = vec![sjis(&c.string)];
                    while j < cmds.len() && cmds[j].code == MESSAGE_LINE {
                        lines.push(sjis(&cmds[j].string));
                        j += 1;
                    }
                    let joined = lines.join("\n");
                    match JP_RE.is_match(&joined).then(|| self.take(&joined, false)).flatten() {
                        None => out.extend(cmds[i..j].iter().cloned()),
                        Some(new) => {
                            // Each window starts with a "Show Message" holding its
                            // first line; the rest are continuation lines.
                            let line_tpl = cmds.get(i + 1).filter(|x| x.code == MESSAGE_LINE).cloned().unwrap_or(Cmd {
                                code: MESSAGE_LINE,
                                indent: c.indent,
                                string: vec![],
                                params: vec![0],
                            });
                            let new_lines: Vec<&str> = new.split('\n').collect();
                            for chunk in new_lines.chunks(WINDOW_LINES) {
                                for (n, l) in chunk.iter().enumerate() {
                                    let mut cmd = if n == 0 { c.clone() } else { line_tpl.clone() };
                                    cmd.string = to_sjis(l);
                                    out.push(cmd);
                                }
                            }
                        }
                    }
                    i = j;
                    continue;
                }
                CHOICES => {
                    // The choices are "a/b/c" in one string.
                    let mut c = c.clone();
                    let parts: Vec<String> = sjis(&c.string).split('/').map(String::from).collect();
                    let new_parts: Vec<String> = parts
                        .iter()
                        .map(|p| {
                            if JP_RE.is_match(p) {
                                self.take(p, false).map(|t| t.replace(['/', '\n'], " ")).unwrap_or_else(|| p.clone())
                            } else {
                                p.clone()
                            }
                        })
                        .collect();
                    if new_parts != parts {
                        c.string = to_sjis(&new_parts.join("/"));
                        choices.insert(c.indent, new_parts);
                    } else {
                        choices.remove(&c.indent);
                    }
                    out.push(c);
                }
                CHOICE => {
                    // A branch shows its choice's text (parameter 0 is its index).
                    let mut c = c.clone();
                    let idx = param0(&c).unwrap_or(u32::MAX) as usize;
                    if let Some(t) = choices.get(&c.indent).and_then(|p| p.get(idx)) {
                        let t = to_sjis(t);
                        if !c.string.is_empty() && t != c.string {
                            c.string = t;
                        }
                    }
                    out.push(c);
                }
                HERO_NAME | HERO_TITLE => {
                    let mut c = c.clone();
                    self.text(&mut c.string, c.code == HERO_NAME);
                    out.push(c);
                }
                _ => out.push(c.clone()),
            }
            i += 1;
        }
        out
    }

    /// An event command list chunk and its size chunk, inside a struct.
    fn list_in(&mut self, s: &mut Struct, size_id: u32, list_id: u32) -> R<()> {
        let Some(pos) = s.chunks.iter().position(|c| c.id == list_id) else { return Ok(()) };
        let cmds = read_commands(&s.chunks[pos].data)?;
        let walked = self.commands(cmds.clone());
        if self.input.is_some() && walked != cmds {
            let data = write_commands(&walked);
            let len = data.len() as u32;
            s.chunks[pos].data = data;
            if let Some(sz) = s.chunks.iter_mut().find(|c| c.id == size_id) {
                let mut b = Vec::new();
                put_ber(&mut b, len);
                sz.data = b;
            }
        }
        Ok(())
    }

    /// Run `f` on each struct of the array in chunk `id` of `s`.
    fn array_in(&mut self, s: &mut Struct, id: u32, f: &mut dyn FnMut(&mut Self, &mut Struct) -> R<()>) -> R<()> {
        let Some(pos) = s.chunks.iter().position(|c| c.id == id) else { return Ok(()) };
        let mut arr = read_array(&s.chunks[pos].data)?;
        for (_, item) in arr.iter_mut() {
            f(self, item)?;
        }
        if self.input.is_some() {
            s.chunks[pos].data = write_array(&arr);
        }
        Ok(())
    }

    fn file(&mut self, f: &mut File) -> R<()> {
        let top = &mut f.top;
        if f.header == "LcfMapUnit" {
            self.array_in(top, MAP_EVENTS, &mut |w, ev| {
                w.array_in(ev, 0x05, &mut |w, page| w.list_in(page, 0x33, 0x34))
            })?;
        } else if f.header == "LcfDataBase" {
            for (db, fields, name) in DB {
                self.array_in(top, *db, &mut |w, item| {
                    for c in item.chunks.iter_mut().filter(|c| fields.contains(&c.id)) {
                        w.text(&mut c.data, Some(c.id) == *name);
                    }
                    Ok(())
                })?;
            }
            if let Some(pos) = top.chunks.iter().position(|c| c.id == DB_TERMS) {
                let mut i = 0;
                let mut terms = read_struct(&top.chunks[pos].data, &mut i)?;
                for c in terms.chunks.iter_mut() {
                    self.text(&mut c.data, false);
                }
                if self.input.is_some() {
                    let mut b = Vec::new();
                    write_struct(&terms, &mut b);
                    top.chunks[pos].data = b;
                }
            }
            self.array_in(top, DB_TROOPS, &mut |w, troop| {
                w.array_in(troop, 0x0B, &mut |w, page| w.list_in(page, 0x0B, 0x0C))
            })?;
            self.array_in(top, DB_COMMON, &mut |w, ce| w.list_in(ce, 0x15, 0x16))?;
        }
        Ok(())
    }
}

pub fn export(f: &File) -> Result<Vec<Entry>> {
    let mut w = Walk { out: vec![], input: None, k: 0 };
    let mut c = f.clone();
    w.file(&mut c).map_err(Error::User)?;
    Ok(w.out)
}

pub fn apply(f: &File, entries: &[Entry]) -> Result<File> {
    let n = export(f)?.len();
    if n != entries.len() {
        return user(format!(
            "A data file changed since it was read ({n} vs {} lines). Read the scripts again.",
            entries.len()
        ));
    }
    let mut w = Walk { out: vec![], input: Some(entries), k: 0 };
    let mut c = f.clone();
    w.file(&mut c).map_err(Error::User)?;
    Ok(c)
}

// ---------------------------------------------------------------- engine

fn is_data(name: &str) -> bool {
    let n = name.to_lowercase();
    n == "rpg_rt.ldb" || (n.starts_with("map") && n.ends_with(".lmu"))
}

pub fn detect(game_dir: &Path) -> Option<Match> {
    if !game_dir.join("RPG_RT.ldb").is_file() {
        return None;
    }
    Some(Match::new(
        "lcf",
        "RPG Maker 2000/2003",
        0.95,
        Map::new(),
        "auto",
        "Messages, choices and database text are translated. Text drawn in pictures stays in Japanese.",
    ))
}

pub struct Lcf {
    pub m: Match,
}

impl Engine for Lcf {
    fn m(&self) -> &Match {
        &self.m
    }
    fn m_mut(&mut self) -> &mut Match {
        &mut self.m
    }

    fn prepare(&mut self, cx: &Ctx) -> Result<PathBuf> {
        let game = cx.game();
        let inst = Installer::new(cx.project);
        let json = cx.project.work("json");
        let scripts = cx.project.work("scripts");
        clean_dir(&json)?;
        clean_dir(&scripts)?;
        let mut lines = 0;
        let mut files = 0;
        for name in list_files(&game, None).into_iter().filter(|n| !n.contains('/') && is_data(n)) {
            let bytes = fs::read(inst.original(&game, &name)?)?;
            let f = read_file(&bytes).map_err(|e| Error::User(format!("{name} can't be read ({e}).")))?;
            fs::write(safe_join(&scripts, &name)?, &bytes)?;
            files += 1;
            let es = export(&f)?;
            if !es.is_empty() {
                lines += es.len();
                write_json_messages(&safe_join(&json, &format!("{name}.json"))?, &es)?;
            }
        }
        cx.log(format!("Found {lines} lines in {files} data files"));
        Ok(json)
    }

    fn build(&mut self, cx: &Ctx) -> Result<PathBuf> {
        let scripts = cx.project.work("scripts");
        let tl = cx.project.work("json_tl");
        let out = cx.project.work("out");
        clean_dir(&out)?;
        let mut n = 0;
        for jrel in list_files(&tl, Some(&[".json"])) {
            let name = &jrel[..jrel.len() - 5];
            let bytes = fs::read(safe_join(&scripts, name)?)?;
            let f = read_file(&bytes).map_err(Error::User)?;
            let new = write_file(&apply(&f, &read_json_messages(&join_rel(&tl, &jrel))?)?);
            if new != bytes {
                fs::write(safe_join(&out, name)?, new)?;
                n += 1;
            }
        }
        if n == 0 {
            return user("No translated lines to pack yet.");
        }
        cx.log(format!("Wrote {n} translated data files"));
        Ok(out)
    }
}
