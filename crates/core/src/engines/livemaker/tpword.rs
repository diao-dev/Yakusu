//! TpWord: the compiled form of a LiveNovel text block (one TextIns command).
//!
//! Layout (pylivemaker's notes, GPL-3.0):
//!
//! ```text
//! "TpWord" + 3 ASCII digits (version)
//! u32 count x decorator     (style runs, including ruby text)
//! u32 count x condition     (version >= 104)
//! u32 count x link          (version >= 105)
//! u32 count x glyph
//! ```
//!
//! Every glyph starts with a type byte, then an i32 condition from version
//! 104. Text is a run of Char glyphs (one CP932 character each) with LINE
//! returns between lines; anything else (page breaks, events, images) ends
//! a block. Each glyph is kept as its raw bytes, so a script that isn't
//! changed is written back byte for byte.

use super::lsb::Cur;

pub const CHAR: u8 = 1;
pub const RETURN: u8 = 3;
pub const EVENT: u8 = 6;
pub const VAR: u8 = 7;
pub const HIST_CHAR: u8 = 0x0A;

pub const BREAK_LINE: u8 = 0;

#[derive(Clone, Debug)]
pub struct Glyph {
    pub kind: u8,
    pub raw: Vec<u8>,
}

impl Glyph {
    /// The character of a Char glyph.
    pub fn ch(&self) -> Option<String> {
        if self.kind != CHAR || self.raw.len() < 7 {
            return None;
        }
        let n = self.raw.len();
        let v = u16::from_le_bytes([self.raw[n - 6], self.raw[n - 5]]);
        let bytes: Vec<u8> = if v < 0x100 { vec![v as u8] } else { vec![(v >> 8) as u8, v as u8] };
        let (s, _, _) = encoding_rs::SHIFT_JIS.decode(&bytes);
        Some(s.into_owned())
    }
    pub fn decorator(&self) -> Option<i32> {
        if self.kind != CHAR || self.raw.len() < 7 {
            return None;
        }
        let n = self.raw.len();
        Some(i32::from_le_bytes(self.raw[n - 4..].try_into().unwrap()))
    }
    fn break_type(&self) -> Option<u8> {
        (self.kind == RETURN).then(|| *self.raw.last().unwrap())
    }
    /// The parts of an Event glyph ("\r\n"-separated, a leading \x01 marks
    /// a system event).
    fn event(&self, version: u32) -> Option<Vec<String>> {
        if self.kind != EVENT {
            return None;
        }
        let at = if version >= 104 { 5 } else { 1 };
        let mut c = Cur::new(&self.raw);
        c.i = at;
        let s = c.pstr().ok()?;
        Some(s.split("\r\n").map(String::from).collect())
    }
}

#[derive(Clone, Debug)]
pub struct TpWord {
    pub version: u32,
    /// Everything before the glyph count.
    head: Vec<u8>,
    pub glyphs: Vec<Glyph>,
    /// For each decorator: does it carry ruby (furigana)?
    ruby: Vec<bool>,
}

/// One translatable run of text.
#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    /// Glyph range [start, end).
    pub start: usize,
    pub end: usize,
    /// Lines joined with '\n'; variables shown as `<v1>`, `<v2>` ...
    pub text: String,
    /// The speaker set by a NAMELABEL event, if any.
    pub name: Option<String>,
    /// Glyph indices of the variables, in order.
    vars: Vec<usize>,
}

fn bad(why: &str) -> String {
    format!("text block: {why}")
}

impl TpWord {
    pub fn parse(data: &[u8]) -> Result<TpWord, String> {
        if data.len() < 9 || &data[..6] != b"TpWord" {
            return Err(bad("missing TpWord header"));
        }
        let version: u32 =
            std::str::from_utf8(&data[6..9]).ok().and_then(|s| s.parse().ok()).ok_or_else(|| bad("bad version"))?;
        let mut c = Cur::new(data);
        c.i = 9;
        let n = c.u32()?;
        let mut ruby = Vec::new();
        for _ in 0..n {
            c.skip(4 * 4 + 2)?;
            c.skip(if version < 100 { 1 } else { 4 })?;
            c.pstr()?;
            ruby.push(!c.pstr()?.is_empty());
            if version >= 100 {
                c.skip(8)?;
            }
        }
        if version >= 104 {
            for _ in 0..c.u32()? {
                c.skip(4)?;
                c.pstr()?;
            }
        }
        if version >= 105 {
            for _ in 0..c.u32()? {
                c.skip(4)?;
                c.pstr()?;
                c.pstr()?;
            }
        }
        let head = data[..c.i].to_vec();
        let count = c.u32()?;
        let mut glyphs = Vec::with_capacity(count.min(1 << 20) as usize);
        for _ in 0..count {
            let start = c.i;
            let kind = c.u8()?;
            Self::skip_glyph(&mut c, kind, version)?;
            glyphs.push(Glyph { kind, raw: data[start..c.i].to_vec() });
        }
        if c.i != data.len() {
            return Err(bad("unexpected bytes after the text"));
        }
        Ok(TpWord { version, head, glyphs, ruby })
    }

    fn skip_glyph(c: &mut Cur, kind: u8, version: u32) -> Result<(), String> {
        let cond = |c: &mut Cur| if version >= 104 { c.skip(4) } else { Ok(()) };
        // Char and Img share a "real glyph" prefix.
        let real = |c: &mut Cur| -> Result<(), String> {
            cond(c)?;
            if version < 105 {
                c.pstr()?;
            } else {
                c.skip(4)?;
            }
            c.skip(4)
        };
        match kind {
            CHAR => {
                real(c)?;
                c.skip(2 + 4)
            }
            2 => {
                cond(c)?;
                c.skip(1)?;
                if version >= 105 {
                    c.skip(9)?;
                }
                Ok(())
            }
            RETURN => {
                cond(c)?;
                c.skip(1)
            }
            4 | 5 => cond(c),
            EVENT => {
                cond(c)?;
                c.pstr().map(|_| ())
            }
            VAR | HIST_CHAR => {
                cond(c)?;
                c.skip(4)?;
                if version > 100 {
                    c.skip(4)?;
                }
                if version > 100 && version < 105 {
                    c.pstr()?;
                }
                if version >= 105 {
                    c.skip(4)?;
                }
                if version < 102 {
                    c.parser()
                } else {
                    c.pstr().map(|_| ())
                }
            }
            9 => {
                real(c)?;
                c.pstr()?;
                c.skip(1)?;
                if version >= 103 {
                    c.pstr()?;
                }
                if version >= 105 {
                    c.skip(16)?;
                    c.pstr()?;
                }
                Ok(())
            }
            // An unknown glyph is a lone type byte (as LiveMaker reads it).
            _ => Ok(()),
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = self.head.clone();
        out.extend_from_slice(&(self.glyphs.len() as u32).to_le_bytes());
        for g in &self.glyphs {
            out.extend_from_slice(&g.raw);
        }
        out
    }

    /// The text blocks, in order.
    pub fn blocks(&self) -> Vec<Block> {
        let mut out = Vec::new();
        let mut name: Option<String> = None;
        let mut cur: Option<Block> = None;
        let flush = |cur: &mut Option<Block>, out: &mut Vec<Block>| {
            if let Some(mut b) = cur.take() {
                b.text = b.text.trim_end_matches('\n').to_string();
                if !b.text.is_empty() {
                    out.push(b);
                }
            }
        };
        for (i, g) in self.glyphs.iter().enumerate() {
            match g.kind {
                CHAR => {
                    let b = cur.get_or_insert_with(|| Block {
                        start: i,
                        end: i,
                        text: String::new(),
                        name: name.clone(),
                        vars: vec![],
                    });
                    b.text.push_str(&g.ch().unwrap_or_default());
                    b.end = i + 1;
                }
                VAR | HIST_CHAR => {
                    let b = cur.get_or_insert_with(|| Block {
                        start: i,
                        end: i,
                        text: String::new(),
                        name: name.clone(),
                        vars: vec![],
                    });
                    b.vars.push(i);
                    b.text.push_str(&format!("<v{}>", b.vars.len()));
                    b.end = i + 1;
                }
                RETURN if g.break_type() == Some(BREAK_LINE) && cur.is_some() => {
                    cur.as_mut().unwrap().text.push('\n');
                }
                _ => {
                    flush(&mut cur, &mut out);
                    if let Some(parts) = g.event(self.version) {
                        let ev = parts[0].trim_start_matches('\x01');
                        if ev == "NAMELABEL" {
                            name = if parts[0].starts_with('\x01') {
                                None
                            } else {
                                parts.get(1).cloned().filter(|s| !s.is_empty())
                            };
                        }
                    }
                }
            }
        }
        flush(&mut cur, &mut out);
        // A block of nothing but variables isn't text.
        out.retain(|b| !only_vars(&b.text));
        out
    }

    /// The glyph to copy for new characters: the block's first character
    /// without ruby, so furigana isn't stretched over the English.
    fn template(&self, b: &Block) -> Option<&Glyph> {
        let chars = || self.glyphs[b.start..b.end].iter().filter(|g| g.kind == CHAR);
        chars()
            .find(|g| g.decorator().is_none_or(|d| d < 0 || !self.ruby.get(d as usize).copied().unwrap_or(false)))
            .or_else(|| chars().next())
    }

    /// Replace block `b` (from [`TpWord::blocks`] of this same TpWord) with
    /// `text`. Returns characters that had to be replaced with '?'.
    /// Replace several blocks last-first: earlier glyph indices stay valid.
    pub fn replace(&mut self, b: &Block, text: &str) -> Result<Vec<char>, String> {
        let Some(tpl) = self.template(b).cloned() else {
            return Err(bad("a block made only of variables can't be replaced"));
        };
        let cond: Vec<u8> = if self.version >= 104 { tpl.raw[1..5].to_vec() } else { vec![] };
        let n = tpl.raw.len();
        let mut new: Vec<Glyph> = Vec::new();
        let mut lost = Vec::new();
        let mut rest = text;
        while let Some(ch) = rest.chars().next() {
            if ch == '<' {
                if let Some(k) = var_code(rest) {
                    let (num, len) = k;
                    let idx = b.vars.get(num.wrapping_sub(1)).ok_or_else(|| bad("unknown variable code"))?;
                    new.push(self.glyphs[*idx].clone());
                    rest = &rest[len..];
                    continue;
                }
            }
            rest = &rest[ch.len_utf8()..];
            match ch {
                '\r' => {}
                '\n' => {
                    let mut raw = vec![RETURN];
                    raw.extend_from_slice(&cond);
                    raw.push(BREAK_LINE);
                    new.push(Glyph { kind: RETURN, raw });
                }
                _ => {
                    let mut buf = [0u8; 4];
                    let (enc, _, err) = encoding_rs::SHIFT_JIS.encode(ch.encode_utf8(&mut buf));
                    let bytes: Vec<u8> = if err || enc.is_empty() || enc.len() > 2 {
                        lost.push(ch);
                        vec![b'?']
                    } else {
                        enc.into_owned()
                    };
                    let v: u16 =
                        if bytes.len() == 1 { bytes[0] as u16 } else { (bytes[0] as u16) << 8 | bytes[1] as u16 };
                    let mut raw = tpl.raw.clone();
                    raw[n - 6..n - 4].copy_from_slice(&v.to_le_bytes());
                    new.push(Glyph { kind: CHAR, raw });
                }
            }
        }
        self.glyphs.splice(b.start..b.end, new);
        Ok(lost)
    }
}

impl TpWord {
    /// Change the speaker shown by NAMELABEL events from `old` to `new`.
    /// Returns characters that had to be replaced with '?'.
    pub fn rename(&mut self, old: &str, new: &str) -> Vec<char> {
        let mut lost = Vec::new();
        let at = if self.version >= 104 { 5 } else { 1 };
        for g in self.glyphs.iter_mut().filter(|g| g.kind == EVENT) {
            let Some(parts) = g.event(self.version) else { continue };
            if parts.len() < 2 || parts[0] != "NAMELABEL" || parts[1] != old {
                continue;
            }
            let mut text = format!("NAMELABEL\r\n{new}");
            for p in &parts[2..] {
                text.push_str("\r\n");
                text.push_str(p);
            }
            let mut bytes = Vec::new();
            for ch in text.chars() {
                let mut buf = [0u8; 4];
                let (enc, _, err) = encoding_rs::SHIFT_JIS.encode(ch.encode_utf8(&mut buf));
                if err {
                    lost.push(ch);
                    bytes.push(b'?');
                } else {
                    bytes.extend_from_slice(&enc);
                }
            }
            let mut raw = g.raw[..at].to_vec();
            raw.extend((bytes.len() as u32).to_le_bytes());
            raw.extend(bytes);
            g.raw = raw;
        }
        lost
    }
}

/// `<v12>` at the start of `s`: (12, byte length).
fn var_code(s: &str) -> Option<(usize, usize)> {
    let inner = s.strip_prefix("<v")?;
    let digits: String = inner.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() || !inner[digits.len()..].starts_with('>') {
        return None;
    }
    Some((digits.parse().ok()?, 3 + digits.len()))
}

fn only_vars(text: &str) -> bool {
    let mut rest = text;
    while let Some(ch) = rest.chars().next() {
        if let Some((_, len)) = var_code(rest) {
            rest = &rest[len..];
        } else if ch.is_whitespace() {
            rest = &rest[ch.len_utf8()..];
        } else {
            return false;
        }
    }
    true
}
