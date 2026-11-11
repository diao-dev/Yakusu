//! Ruby Marshal 4.8 (RPG Maker XP / VX / VX Ace data files).
//!
//! Parsed into a tree that writes back byte for byte. Object links (`@`)
//! keep pointing at the right object when objects are added or strings
//! change: each parsed object remembers its original index, and the writer
//! maps old indices to new ones.

#[derive(Clone, Debug, PartialEq)]
pub enum V {
    Nil,
    True,
    False,
    Int(i64),
    Sym(String),
    /// A symbol written as a back-reference (`;`) to the n-th symbol.
    SymLink(usize),
    /// A back-reference (`@`) to the object with this original index.
    Link(usize),
    /// `I`: a value with instance variables (strings with an encoding).
    Ivar(Box<V>, Vec<(V, V)>),
    Str(Vec<u8>),
    Float(Vec<u8>),
    Big(u8, Vec<u8>),
    Regexp(Vec<u8>, u8),
    Array(Vec<V>),
    Hash(Vec<(V, V)>, Option<Box<V>>),
    /// `o`: class name (symbol) and instance variables.
    Object(Box<V>, Vec<(V, V)>),
    /// `u`: class name and its `_dump` bytes (RPG::Table, Color, Tone).
    UserDef(Box<V>, Vec<u8>),
    /// `U`: class name and its `marshal_dump` value.
    UserMarshal(Box<V>, Box<V>),
    Struct(Box<V>, Vec<(V, V)>),
    Extended(Box<V>, Box<V>),
    UserClass(Box<V>, Box<V>),
    Class(Vec<u8>),
    Module(Vec<u8>),
    /// A value added by Yakusu: its objects take no original index.
    Fresh(Box<V>),
}

pub struct Reader<'a> {
    b: &'a [u8],
    i: usize,
    syms: Vec<String>,
    objs: usize,
}

type R<T> = std::result::Result<T, String>;

impl Reader<'_> {
    fn byte(&mut self) -> R<u8> {
        let c = *self.b.get(self.i).ok_or("cut short")?;
        self.i += 1;
        Ok(c)
    }
    fn bytes(&mut self, n: usize) -> R<Vec<u8>> {
        let s = self.b.get(self.i..self.i + n).ok_or("cut short")?.to_vec();
        self.i += n;
        Ok(s)
    }
    fn long(&mut self) -> R<i64> {
        let c = self.byte()? as i8;
        Ok(match c {
            0 => 0,
            5..=127 => c as i64 - 5,
            -128..=-5 => c as i64 + 5,
            1..=4 => {
                let mut x: i64 = 0;
                for k in 0..c {
                    x |= (self.byte()? as i64) << (8 * k);
                }
                x
            }
            _ => {
                let n = -(c as i64);
                let mut x: i64 = -1;
                for k in 0..n {
                    x &= !(0xFF << (8 * k));
                    x |= (self.byte()? as i64) << (8 * k);
                }
                x
            }
        })
    }
    fn len(&mut self) -> R<usize> {
        let n = self.long()?;
        if n < 0 || n as usize > self.b.len() {
            return Err("bad length".into());
        }
        Ok(n as usize)
    }
    fn blob(&mut self) -> R<Vec<u8>> {
        let n = self.len()?;
        self.bytes(n)
    }
    fn pairs(&mut self, depth: usize) -> R<Vec<(V, V)>> {
        let n = self.len()?;
        let mut v = Vec::with_capacity(n.min(1 << 16));
        for _ in 0..n {
            let k = self.value(depth)?;
            let val = self.value(depth)?;
            v.push((k, val));
        }
        Ok(v)
    }
    fn entry(&mut self) {
        self.objs += 1;
    }
    fn value(&mut self, depth: usize) -> R<V> {
        if depth > 512 {
            return Err("nested too deep".into());
        }
        let d = depth + 1;
        let t = self.byte()?;
        Ok(match t {
            b'0' => V::Nil,
            b'T' => V::True,
            b'F' => V::False,
            b'i' => V::Int(self.long()?),
            b':' => {
                let s = String::from_utf8_lossy(&self.blob()?).to_string();
                self.syms.push(s.clone());
                V::Sym(s)
            }
            b';' => {
                let n = self.len()?;
                if n >= self.syms.len() {
                    return Err("bad symbol link".into());
                }
                V::SymLink(n)
            }
            b'@' => {
                let n = self.len()?;
                if n >= self.objs {
                    return Err("bad object link".into());
                }
                V::Link(n)
            }
            b'I' => {
                let inner = self.value(d)?;
                let iv = self.pairs(d)?;
                V::Ivar(Box::new(inner), iv)
            }
            b'"' => {
                self.entry();
                V::Str(self.blob()?)
            }
            b'f' => {
                self.entry();
                V::Float(self.blob()?)
            }
            b'l' => {
                self.entry();
                let sign = self.byte()?;
                let n = self.len()?;
                V::Big(sign, self.bytes(n * 2)?)
            }
            b'/' => {
                self.entry();
                let s = self.blob()?;
                V::Regexp(s, self.byte()?)
            }
            b'[' => {
                self.entry();
                let n = self.len()?;
                let mut v = Vec::with_capacity(n.min(1 << 16));
                for _ in 0..n {
                    v.push(self.value(d)?);
                }
                V::Array(v)
            }
            b'{' | b'}' => {
                self.entry();
                let p = self.pairs(d)?;
                let def = if t == b'}' { Some(Box::new(self.value(d)?)) } else { None };
                V::Hash(p, def)
            }
            b'o' => {
                self.entry();
                let cls = self.value(d)?;
                V::Object(Box::new(cls), self.pairs(d)?)
            }
            b'S' => {
                self.entry();
                let cls = self.value(d)?;
                V::Struct(Box::new(cls), self.pairs(d)?)
            }
            b'u' => {
                let cls = self.value(d)?;
                let data = self.blob()?;
                self.entry();
                V::UserDef(Box::new(cls), data)
            }
            b'U' => {
                self.entry();
                let cls = self.value(d)?;
                V::UserMarshal(Box::new(cls), Box::new(self.value(d)?))
            }
            b'e' => {
                let m = self.value(d)?;
                V::Extended(Box::new(m), Box::new(self.value(d)?))
            }
            b'C' => {
                let c = self.value(d)?;
                V::UserClass(Box::new(c), Box::new(self.value(d)?))
            }
            b'c' => {
                self.entry();
                V::Class(self.blob()?)
            }
            b'm' => {
                self.entry();
                V::Module(self.blob()?)
            }
            other => return Err(format!("unknown type {:?}", other as char)),
        })
    }
}

/// Parse a Marshal 4.8 stream. Returns the root and the symbol table.
pub fn load(b: &[u8]) -> R<(V, Vec<String>)> {
    if b.len() < 2 || b[0] != 4 || b[1] != 8 {
        return Err("not Ruby Marshal 4.8 data".into());
    }
    let mut r = Reader { b, i: 2, syms: vec![], objs: 0 };
    let v = r.value(0)?;
    if r.i != b.len() {
        return Err("unexpected bytes after the data".into());
    }
    Ok((v, r.syms))
}

pub struct Writer {
    out: Vec<u8>,
    syms: Vec<String>,
    /// Original object index -> new index.
    map: Vec<usize>,
    objs: usize,
    /// Symbols of the original stream, to resolve SymLinks.
    old_syms: Vec<String>,
}

impl Writer {
    fn long(&mut self, x: i64) {
        if x == 0 {
            self.out.push(0);
        } else if (1..123).contains(&x) {
            self.out.push((x + 5) as u8);
        } else if (-123..0).contains(&x) {
            self.out.push(((x - 5) as i8) as u8);
        } else {
            let mut bytes = Vec::new();
            let mut y = x;
            for _ in 0..8 {
                bytes.push((y & 0xFF) as u8);
                y >>= 8;
                if (x >= 0 && y == 0) || (x < 0 && y == -1) {
                    break;
                }
            }
            let n = bytes.len() as i8;
            self.out.push(if x >= 0 { n as u8 } else { (-n) as u8 });
            self.out.extend(bytes);
        }
    }
    fn blob(&mut self, b: &[u8]) {
        self.long(b.len() as i64);
        self.out.extend_from_slice(b);
    }
    /// An object that existed in the original takes the next old index too;
    /// `fresh` objects (added by Yakusu) only take a new one.
    fn entry(&mut self, fresh: bool) {
        if !fresh {
            self.map.push(self.objs);
        }
        self.objs += 1;
    }
    fn sym(&mut self, s: &str) {
        match self.syms.iter().position(|x| x == s) {
            Some(i) => {
                self.out.push(b';');
                self.long(i as i64);
            }
            None => {
                self.out.push(b':');
                self.blob(s.as_bytes());
                self.syms.push(s.to_string());
            }
        }
    }
    fn pairs(&mut self, p: &[(V, V)], fresh: bool) {
        self.long(p.len() as i64);
        for (k, v) in p {
            self.value(k, fresh);
            self.value(v, fresh);
        }
    }
    fn value(&mut self, v: &V, fresh: bool) {
        match v {
            V::Nil => self.out.push(b'0'),
            V::True => self.out.push(b'T'),
            V::False => self.out.push(b'F'),
            V::Int(x) => {
                self.out.push(b'i');
                self.long(*x);
            }
            V::Sym(s) => self.sym(s),
            V::SymLink(n) => {
                let s = self.old_syms.get(*n).cloned().unwrap_or_default();
                self.sym(&s)
            }
            V::Link(n) => {
                self.out.push(b'@');
                let new = self.map.get(*n).copied().unwrap_or(*n);
                self.long(new as i64);
            }
            V::Ivar(inner, iv) => {
                self.out.push(b'I');
                self.value(inner, fresh);
                self.pairs(iv, fresh);
            }
            V::Str(b) => {
                self.out.push(b'"');
                self.entry(fresh);
                self.blob(b);
            }
            V::Float(b) => {
                self.out.push(b'f');
                self.entry(fresh);
                self.blob(b);
            }
            V::Big(sign, b) => {
                self.out.push(b'l');
                self.entry(fresh);
                self.out.push(*sign);
                self.long((b.len() / 2) as i64);
                self.out.extend_from_slice(b);
            }
            V::Regexp(s, opt) => {
                self.out.push(b'/');
                self.entry(fresh);
                self.blob(s);
                self.out.push(*opt);
            }
            V::Array(items) => {
                self.out.push(b'[');
                self.entry(fresh);
                self.long(items.len() as i64);
                for it in items {
                    self.value(it, fresh);
                }
            }
            V::Hash(p, def) => {
                self.out.push(if def.is_some() { b'}' } else { b'{' });
                self.entry(fresh);
                self.pairs(p, fresh);
                if let Some(d) = def {
                    self.value(d, fresh);
                }
            }
            V::Object(cls, iv) => {
                self.out.push(b'o');
                self.entry(fresh);
                self.value(cls, fresh);
                self.pairs(iv, fresh);
            }
            V::Struct(cls, iv) => {
                self.out.push(b'S');
                self.entry(fresh);
                self.value(cls, fresh);
                self.pairs(iv, fresh);
            }
            V::UserDef(cls, data) => {
                self.out.push(b'u');
                self.value(cls, fresh);
                self.blob(data);
                self.entry(fresh);
            }
            V::UserMarshal(cls, inner) => {
                self.out.push(b'U');
                self.entry(fresh);
                self.value(cls, fresh);
                self.value(inner, fresh);
            }
            V::Extended(m, inner) => {
                self.out.push(b'e');
                self.value(m, fresh);
                self.value(inner, fresh);
            }
            V::UserClass(c, inner) => {
                self.out.push(b'C');
                self.value(c, fresh);
                self.value(inner, fresh);
            }
            V::Class(b) => {
                self.out.push(b'c');
                self.entry(fresh);
                self.blob(b);
            }
            V::Module(b) => {
                self.out.push(b'm');
                self.entry(fresh);
                self.blob(b);
            }
            V::Fresh(inner) => self.value(inner, true),
        }
    }
}

/// Write a tree from [`load`] (with `syms`, its symbol table) back out.
pub fn dump(v: &V, syms: &[String]) -> Vec<u8> {
    let mut w = Writer { out: vec![4, 8], syms: vec![], map: vec![], objs: 0, old_syms: syms.to_vec() };
    w.value(v, false);
    w.out
}

// ---------------------------------------------------------------- helpers

/// The symbol a node names (resolving symbol links).
pub fn sym<'a>(v: &'a V, syms: &'a [String]) -> Option<&'a str> {
    match v {
        V::Sym(s) => Some(s),
        V::SymLink(n) => syms.get(*n).map(String::as_str),
        _ => None,
    }
}

/// An object's class name.
pub fn class<'a>(v: &'a V, syms: &'a [String]) -> Option<&'a str> {
    match v {
        V::Object(c, _) | V::Struct(c, _) | V::UserDef(c, _) | V::UserMarshal(c, _) => sym(c, syms),
        _ => None,
    }
}

/// An instance variable of an object (`@name`).
pub fn ivar<'a>(v: &'a V, name: &str, syms: &[String]) -> Option<&'a V> {
    match v {
        V::Object(_, iv) => iv.iter().find(|(k, _)| sym(k, syms) == Some(name)).map(|(_, v)| v),
        _ => None,
    }
}

pub fn ivar_mut<'a>(v: &'a mut V, name: &str, syms: &[String]) -> Option<&'a mut V> {
    match v {
        V::Object(_, iv) => iv.iter_mut().find(|(k, _)| sym(k, syms) == Some(name)).map(|(_, v)| v),
        _ => None,
    }
}

/// A string's text (a `"` with or without an encoding ivar).
pub fn text(v: &V) -> Option<String> {
    match v {
        V::Str(b) => std::str::from_utf8(b).ok().map(String::from),
        V::Ivar(inner, _) => text(inner),
        _ => None,
    }
}

/// Replace a string's bytes, keeping its encoding ivars.
pub fn set_text(v: &mut V, s: &str) -> bool {
    match v {
        V::Str(b) => {
            *b = s.as_bytes().to_vec();
            true
        }
        V::Ivar(inner, _) => set_text(inner, s),
        _ => false,
    }
}

pub fn int(v: &V) -> Option<i64> {
    match v {
        V::Int(x) => Some(*x),
        _ => None,
    }
}
