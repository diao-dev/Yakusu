//! LSB: LiveMaker's compiled scripts ("charts").
//!
//! Yakusu only needs two things from a script: the TextIns commands (each
//! holds one TpWord text blob) and, in LiveMaker 3, the message window's
//! "font changeable" flag. The rest is walked over without being
//! interpreted. Script commands refer to each other by line number, never by
//! byte offset, so text blobs of a new length can be spliced in safely.
//!
//! Layout (pylivemaker's notes, GPL-3.0):
//!
//! ```text
//! u32 version (103..=117)  u8 flags  u32 command_count  u32 param_stream_size
//! command_count x param_stream_size bytes: per command type, a bit for each
//!     component property it carries (LSB first)
//! u32 count x command:  u8 type  u32 indent  u8 mute  u8 not_update
//!                       u32 line_no  then the type's fields
//! ```

const MIN_VERSION: u32 = 103;
const MAX_VERSION: u32 = 117;

/// MesNew's PR_FONTCHANGEABLED property (PropertyType 0xA6, bit 0xA5).
const FONT_CHANGEABLE_BIT: usize = 0xA5;
const MES_NEW: u8 = 0x0A;
const TEXT_INS: u8 = 0x14;

pub struct Cur<'a> {
    pub b: &'a [u8],
    pub i: usize,
}

fn short() -> String {
    "cut short".into()
}

impl<'a> Cur<'a> {
    pub fn new(b: &'a [u8]) -> Cur<'a> {
        Cur { b, i: 0 }
    }
    pub fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.i.checked_add(n).ok_or_else(short)?;
        let s = self.b.get(self.i..end).ok_or_else(short)?;
        self.i = end;
        Ok(s)
    }
    pub fn skip(&mut self, n: usize) -> Result<(), String> {
        self.take(n).map(|_| ())
    }
    pub fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    pub fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    /// A u32-prefixed CP932 string.
    pub fn pstr(&mut self) -> Result<String, String> {
        let n = self.u32()? as usize;
        let (s, _, _) = encoding_rs::SHIFT_JIS.decode(self.take(n)?);
        Ok(s.into_owned())
    }
    /// A LiveParser expression: u32 count x (u8 type, name, u32 n,
    /// [u8 func when type is Func], n x operand).
    pub fn parser(&mut self) -> Result<(), String> {
        let count = self.u32()?;
        for _ in 0..count {
            let kind = self.u8()?;
            self.pstr()?;
            let n = self.u32()?;
            if kind == 0x0B {
                self.skip(1)?;
            }
            for _ in 0..n {
                self.operand()?;
            }
        }
        Ok(())
    }
    fn operand(&mut self) -> Result<(), String> {
        match self.u8()? {
            1 => self.skip(4),  // Int
            2 => self.skip(10), // Float (80-bit)
            3 => self.skip(1),  // Flag
            _ => self.pstr().map(|_| ()),
        }
    }
    fn parsers(&mut self, n: usize) -> Result<(), String> {
        (0..n).try_for_each(|_| self.parser())
    }
    fn parser_array(&mut self) -> Result<(), String> {
        let n = self.u32()? as usize;
        self.parsers(n)
    }
    fn label_ref(&mut self) -> Result<(), String> {
        self.pstr()?;
        self.skip(4)
    }
}

#[derive(Clone, Debug)]
pub struct TextIns {
    pub line_no: u32,
    /// The TpWord blob (after its u32 length).
    start: usize,
    end: usize,
}

pub struct Lsb {
    pub version: u32,
    data: Vec<u8>,
    pub texts: Vec<TextIns>,
    /// Byte ranges of MesNew's literal "font changeable = true" values.
    font_flags: Vec<(usize, usize)>,
}

impl Lsb {
    pub fn parse(data: Vec<u8>) -> Result<Lsb, String> {
        let mut c = Cur::new(&data);
        let version = c.u32()?;
        if !(MIN_VERSION..=MAX_VERSION).contains(&version) {
            return Err(format!("script version {version} isn't one Yakusu knows (103 to 117)"));
        }
        c.skip(1)?;
        let types = c.u32()? as usize;
        let row = c.u32()? as usize;
        if types > 256 || row > 64 {
            return Err("damaged header".into());
        }
        let params = c.take(types * row)?;
        let flags = |t: usize| -> Vec<bool> {
            if t >= types {
                return vec![];
            }
            params[t * row..(t + 1) * row].iter().flat_map(|b| (0..8).map(move |s| b & (1 << s) != 0)).collect()
        };
        let count = c.u32()?;
        let mut texts = Vec::new();
        let mut font_flags = Vec::new();
        for _ in 0..count {
            let kind = c.u8()?;
            c.skip(4 + 1 + 1)?;
            let line_no = c.u32()?;
            let v = version;
            let opt = |c: &mut Cur, cond: bool| if cond { c.parser() } else { Ok(()) };
            match kind {
                0 | 1 | 6 | 0x0E | 0x13 | 0x1A | 0x1C | 0x20 | 0x27 | 0x37 => c.parser()?,
                2 | 0x16 | 0x2D | 0x2E | 0x2F => {}
                3 | 0x10 | 0x1B => {
                    c.pstr()?;
                }
                4 => {
                    c.label_ref()?;
                    c.parser()?;
                }
                5 => {
                    c.label_ref()?;
                    c.pstr()?;
                    c.parser()?;
                    c.parser_array()?;
                }
                7 => {
                    c.parsers(2)?;
                    opt(&mut c, v > 0x6A)?;
                }
                0x08
                | 0x09
                | 0x0A
                | 0x0B
                | 0x0C
                | 0x17
                | 0x18
                | 0x19
                | 0x1E
                | 0x24
                | 0x25
                | 0x2A
                | 0x2B
                | 0x2C
                | 0x30..=0x36
                | 0x38
                | 0x3B
                | 0x3C => {
                    let f = flags(kind as usize);
                    let n = f.iter().filter(|x| **x).count();
                    let font_idx = (kind == MES_NEW && f.get(FONT_CHANGEABLE_BIT) == Some(&true))
                        .then(|| f[..FONT_CHANGEABLE_BIT].iter().filter(|x| **x).count());
                    for i in 0..n {
                        let at = c.i;
                        c.parser()?;
                        if Some(i) == font_idx {
                            if let Some(r) = literal_true(&data[at..c.i]) {
                                font_flags.push((at + r.0, at + r.1));
                            }
                        }
                    }
                    if kind == 0x3B || kind == 0x3C {
                        c.parser()?;
                        c.parser_array()?;
                    }
                }
                0x0D => {
                    c.parsers(4)?;
                    c.parser_array()?;
                    c.parsers(3)?;
                    opt(&mut c, v > 0x64)?;
                    opt(&mut c, v > 0x6A)?;
                    opt(&mut c, v > 0x74)?;
                }
                0x0F => {
                    c.pstr()?;
                    c.skip(1)?;
                    c.parser()?;
                    c.skip(1)?;
                }
                0x11 => {
                    c.parsers(2)?;
                    c.pstr()?;
                }
                0x12 => c.parsers(3)?,
                TEXT_INS => {
                    let n = c.u32()? as usize;
                    let start = c.i;
                    c.skip(n)?;
                    texts.push(TextIns { line_no, start, end: c.i });
                    c.parsers(3)?;
                    opt(&mut c, v > 0x6A)?;
                }
                0x15 => {
                    c.parsers(3)?;
                    opt(&mut c, v > 0x6A)?;
                }
                0x1D => {
                    c.parsers(4)?;
                    opt(&mut c, v > 0x6E)?;
                }
                0x1F | 0x21 | 0x22 | 0x23 => {
                    c.parser()?;
                    c.skip(4)?;
                }
                0x26 => {
                    c.parser()?;
                    c.pstr()?;
                    if v > 0x68 {
                        c.skip(4)?;
                    }
                    c.parser()?;
                }
                0x28 | 0x29 => {
                    c.label_ref()?;
                    c.skip(1)?;
                }
                0x39 => {
                    c.parsers(6)?;
                    opt(&mut c, v > 0x6B)?;
                }
                0x3A => {
                    c.parser()?;
                    opt(&mut c, v > 0x6E)?;
                }
                other => return Err(format!("unknown command type 0x{other:02X} at line {line_no}")),
            }
        }
        if c.i != data.len() {
            return Err("unexpected bytes after the last command".into());
        }
        Ok(Lsb { version, data, texts, font_flags })
    }

    /// LiveMaker 3 (script version 117) shows ASCII full-width unless the
    /// message window's "font changeable" flag is off.
    pub fn is_lm3(&self) -> bool {
        self.version >= 117
    }

    pub fn has_font_flag(&self) -> bool {
        !self.font_flags.is_empty()
    }

    pub fn text(&self, i: usize) -> &[u8] {
        let t = &self.texts[i];
        &self.data[t.start..t.end]
    }

    /// The script with some text blobs replaced (index into `texts` -> new
    /// TpWord bytes) and, if `half_width`, the font flag turned off.
    pub fn rebuild(&self, replace: &std::collections::HashMap<usize, Vec<u8>>, half_width: bool) -> Vec<u8> {
        let mut data = self.data.clone();
        if half_width {
            for (a, b) in &self.font_flags {
                data[*a..*b].fill(0);
            }
        }
        let mut out = Vec::with_capacity(data.len());
        let mut pos = 0;
        for (i, t) in self.texts.iter().enumerate() {
            if let Some(new) = replace.get(&i) {
                out.extend_from_slice(&data[pos..t.start - 4]);
                out.extend_from_slice(&(new.len() as u32).to_le_bytes());
                out.extend_from_slice(new);
                pos = t.end;
            }
        }
        out.extend_from_slice(&data[pos..]);
        out
    }
}

/// If `lp` is the expression "= 1" (one To operation with one Int or Flag
/// operand of 1), the byte range of that 1 within `lp`.
fn literal_true(lp: &[u8]) -> Option<(usize, usize)> {
    let mut c = Cur::new(lp);
    if c.u32().ok()? != 1 || c.u8().ok()? != 1 {
        return None;
    }
    c.pstr().ok()?;
    if c.u32().ok()? != 1 {
        return None;
    }
    let (at, len) = match c.u8().ok()? {
        1 => (c.i, 4),
        3 => (c.i, 1),
        _ => return None,
    };
    let v = c.take(len).ok()?;
    let one = if len == 4 { v == [1, 0, 0, 0] } else { v == [1] };
    (one && c.i == lp.len()).then_some((at, at + len))
}
