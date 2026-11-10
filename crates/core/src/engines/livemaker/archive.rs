//! LiveMaker "vf" archives: inside the game's .exe, or a separate .dat
//! (optionally with a .ext index and .001, .002 ... parts).
//!
//! Format notes follow pylivemaker (GPL-3.0, Peter Rowlands and tinfoil),
//! which reverse-engineered it from LiveMaker itself:
//!
//! ```text
//! "vf"  u32 version  u32 count
//! count x  u32-prefixed CP932 name      (version >= 100: XOR'd with a keystream)
//! count+1 x  (u32 offset_low, u32 offset_high)   (XOR'd as well)
//! count x  u8 compression   (0 zlib, 1 stored, 2 scrambled, 3 scrambled + zlib)
//! count x  u32 unknown
//! count x  u32 CRC-32 of the stored bytes
//! count x  u8 encryption flag
//! file data
//! ```
//!
//! An exe ends with `u32 archive_offset` and "lv". Offsets count from the
//! start of the archive ("vf"), except in a .ext index, where they count from
//! the start of the .dat data file. Split data files are cut every 1 GiB.

use crate::error::{user, Error, Result};
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const SEED: u32 = 0x75D6_EE39;
const PART: u64 = 1 << 30;
/// The largest directory Yakusu reads (names and tables of ~1M files).
const MAX_DIR: u64 = 256 * 1024 * 1024;

pub const ZLIB: u8 = 0;
pub const STORED: u8 = 1;
pub const SCRAMBLED: u8 = 2;
pub const SCRAMBLED_ZLIB: u8 = 3;

#[derive(Clone, Debug)]
pub struct Entry {
    /// '/'-separated, decoded from CP932.
    pub name: String,
    raw_name: Vec<u8>,
    offset: u64,
    pub size: u64,
    pub compress: u8,
    unk1: u32,
    checksum: u32,
    encrypt: u8,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Layout {
    /// Appended to the game's .exe, starting at this offset.
    Exe(u64),
    /// A .dat holding the directory and the data.
    Dat,
    /// A .ext holding the directory; the data is in the .dat of the same name.
    Ext,
}

pub struct Archive {
    pub path: PathBuf,
    pub layout: Layout,
    pub version: u32,
    pub entries: Vec<Entry>,
    /// Data files in order, each 1 GiB except the last (one file unless split).
    parts: Vec<PathBuf>,
    /// Bytes after the last file in a .dat (some tools end it like an exe).
    tail: Vec<u8>,
}

fn bad(path: &Path, why: &str) -> Error {
    Error::User(format!(
        "{} isn't a LiveMaker archive Yakusu can read ({why}).",
        path.file_name().unwrap_or_default().to_string_lossy()
    ))
}

/// The XOR keystream that hides names and offsets (LiveMaker's TTpRandom).
struct Keys(u32);

impl Iterator for Keys {
    type Item = u32;
    fn next(&mut self) -> Option<u32> {
        self.0 = self.0.wrapping_mul(5).wrapping_add(SEED);
        Some(self.0)
    }
}

fn keys() -> Keys {
    Keys(0)
}

struct Cur<'a> {
    b: &'a [u8],
    i: usize,
}

impl Cur<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        let s = self.b.get(self.i..self.i.checked_add(n)?)?;
        self.i += n;
        Some(s)
    }
    fn u32(&mut self) -> Option<u32> {
        self.take(4).map(|s| u32::from_le_bytes(s.try_into().unwrap()))
    }
}

pub fn decode_name(raw: &[u8]) -> String {
    let (s, _, _) = encoding_rs::SHIFT_JIS.decode(raw);
    s.replace('\\', "/")
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = flate2::Crc::new();
    c.update(data);
    c.sum()
}

/// Where the archive starts in a file, if it is one.
fn find_start(path: &Path) -> Result<Option<(Layout, u64)>> {
    let mut f = File::open(path)?;
    let len = f.metadata()?.len();
    let mut head = [0u8; 2];
    if len < 10 || f.read_exact(&mut head).is_err() {
        return Ok(None);
    }
    if &head == b"MZ" {
        f.seek(SeekFrom::End(-6))?;
        let mut tail = [0u8; 6];
        f.read_exact(&mut tail)?;
        if &tail[4..] != b"lv" {
            return Ok(None);
        }
        let off = u32::from_le_bytes(tail[..4].try_into().unwrap()) as u64;
        if off + 10 > len {
            return Ok(None);
        }
        f.seek(SeekFrom::Start(off))?;
        f.read_exact(&mut head)?;
        return Ok((&head == b"vf").then_some((Layout::Exe(off), off)));
    }
    if &head != b"vf" {
        return Ok(None);
    }
    let is_ext = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("ext"));
    Ok(Some((if is_ext { Layout::Ext } else { Layout::Dat }, 0)))
}

fn sibling(path: &Path, ext: &str) -> PathBuf {
    path.with_extension(ext)
}

/// The data parts of a .dat/.ext archive: `name.dat`, `name.001`, ...
fn data_parts(path: &Path, layout: &Layout) -> Vec<PathBuf> {
    match layout {
        Layout::Exe(_) => vec![path.to_path_buf()],
        Layout::Dat | Layout::Ext => {
            let mut v = vec![sibling(path, "dat")];
            for i in 1..100 {
                let p = sibling(path, &format!("{i:03}"));
                if !p.is_file() {
                    break;
                }
                v.push(p);
            }
            v
        }
    }
}

/// Just tells whether a file is a LiveMaker archive.
pub fn is_archive(path: &Path) -> bool {
    matches!(find_start(path), Ok(Some(_)))
}

impl Archive {
    pub fn open(path: &Path) -> Result<Archive> {
        let Some((layout, start)) = find_start(path)? else {
            return Err(bad(path, "no LiveMaker archive inside"));
        };
        let mut f = File::open(path)?;
        let len = f.metadata()?.len();
        let avail = MAX_DIR.min(len - start);
        // Read just enough for the file list: big games keep gigabytes of
        // data after it.
        let mut window = avail.min(1 << 20);
        let (version, entries) = loop {
            f.seek(SeekFrom::Start(start))?;
            let mut dir = Vec::new();
            (&mut f).take(window).read_to_end(&mut dir)?;
            match parse_dir(&dir) {
                Ok(v) => break v,
                Err(None) if window < avail => window = avail.min(window * 8),
                Err(None) => return Err(bad(path, "its file list is cut short")),
                Err(Some(why)) => return Err(bad(path, &why)),
            }
        };
        let parts = data_parts(path, &layout);
        if matches!(layout, Layout::Ext) && !parts[0].is_file() {
            return Err(bad(path, "the .dat file that goes with it is missing"));
        }
        // Every file must lie inside the data.
        let avail: u64 = match layout {
            Layout::Exe(start) => len - start,
            Layout::Dat => parts.iter().map(|p| fs::metadata(p).map(|m| m.len()).unwrap_or(0)).sum(),
            Layout::Ext => parts.iter().map(|p| fs::metadata(p).map(|m| m.len()).unwrap_or(0)).sum(),
        };
        if entries.iter().any(|e| e.offset.checked_add(e.size).is_none_or(|end| end > avail)) {
            return Err(bad(path, "its file list points past the end of the data"));
        }
        let mut tail = Vec::new();
        if layout == Layout::Dat && parts.len() == 1 {
            let end = entries.last().map(|e| e.offset + e.size).unwrap_or(0);
            if end < len && len - end <= 64 {
                f.seek(SeekFrom::Start(end))?;
                f.read_to_end(&mut tail)?;
            }
        }
        Ok(Archive { path: path.to_path_buf(), layout, version, entries, parts, tail })
    }

    /// Byte position of an entry in the data stream (across parts).
    fn data_pos(&self, e: &Entry) -> u64 {
        match self.layout {
            Layout::Exe(start) => start + e.offset,
            Layout::Dat | Layout::Ext => e.offset,
        }
    }

    /// The bytes as stored (compressed / scrambled).
    pub fn read_raw(&self, e: &Entry) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(e.size as usize);
        let mut pos = self.data_pos(e);
        while (out.len() as u64) < e.size {
            let (idx, at) = match self.layout {
                Layout::Exe(_) => (0, pos),
                _ => ((pos / PART) as usize, pos % PART),
            };
            let part = self.parts.get(idx).ok_or_else(|| bad(&self.path, "a data part is missing"))?;
            let mut f = File::open(part)?;
            f.seek(SeekFrom::Start(at))?;
            let want = e.size - out.len() as u64;
            let before = out.len();
            f.take(want).read_to_end(&mut out)?;
            let got = (out.len() - before) as u64;
            if got == 0 {
                return Err(bad(&self.path, &format!("{} is cut short", e.name)));
            }
            pos += got;
        }
        Ok(out)
    }

    /// Whether the stored bytes match the entry's checksum.
    pub fn checksum_ok(&self, e: &Entry) -> Result<bool> {
        Ok(crc32(&self.read_raw(e)?) == e.checksum)
    }

    /// The file's contents.
    pub fn read(&self, e: &Entry) -> Result<Vec<u8>> {
        let mut data = self.read_raw(e)?;
        if matches!(e.compress, SCRAMBLED | SCRAMBLED_ZLIB) {
            data = unscramble(&data).ok_or_else(|| bad(&self.path, &format!("{} won't unscramble", e.name)))?;
        }
        if matches!(e.compress, ZLIB | SCRAMBLED_ZLIB) {
            let mut out = Vec::new();
            ZlibDecoder::new(&data[..])
                .read_to_end(&mut out)
                .map_err(|_| bad(&self.path, &format!("{} won't decompress", e.name)))?;
            data = out;
        }
        Ok(data)
    }

    /// The files this archive is made of, as names next to `path`.
    pub fn files(&self) -> Vec<String> {
        let name = |p: &Path| p.file_name().unwrap_or_default().to_string_lossy().to_string();
        match self.layout {
            Layout::Exe(_) | Layout::Dat => self.parts.iter().map(|p| name(p)).collect(),
            Layout::Ext => std::iter::once(name(&self.path)).chain(self.parts.iter().map(|p| name(p))).collect(),
        }
    }

    /// Write a copy of this archive into `out_dir` with some files replaced
    /// (`name -> new contents`). Unchanged files are copied byte for byte.
    /// Returns the file names written (the same names as the original's).
    pub fn rewrite(&self, replace: &std::collections::HashMap<String, Vec<u8>>, out_dir: &Path) -> Result<Vec<String>> {
        if self.parts.len() > 1 {
            return user(
                "This game's data is split over several files over 1 GB each. Yakusu can read it but can't rebuild it yet.",
            );
        }
        // New entries are packed in memory; everything else is streamed
        // from the original, in the original order.
        let mut fresh: Vec<Option<Vec<u8>>> = Vec::with_capacity(self.entries.len());
        let mut table: Vec<(Entry, u64)> = Vec::with_capacity(self.entries.len());
        let mut at = 0u64;
        for e in &self.entries {
            let mut ne = e.clone();
            let stored = match replace.get(&e.name) {
                Some(new) => {
                    // Scrambling isn't written back: the game reads plain entries too.
                    ne.compress = if matches!(e.compress, ZLIB | SCRAMBLED_ZLIB) { ZLIB } else { STORED };
                    ne.encrypt = 0;
                    let bytes = if ne.compress == ZLIB {
                        let mut z = ZlibEncoder::new(Vec::new(), Compression::default());
                        z.write_all(new)?;
                        z.finish()?
                    } else {
                        new.clone()
                    };
                    ne.checksum = crc32(&bytes);
                    ne.size = bytes.len() as u64;
                    Some(bytes)
                }
                None => None,
            };
            table.push((ne.clone(), at));
            at += ne.size;
            fresh.push(stored);
        }
        let data_len = at;
        let dir_len = directory(self.version, &table, 0).len() as u64;
        // Offsets count from the archive start (so past the directory),
        // except for a .ext index, where they count from the .dat start.
        let base = if self.layout == Layout::Ext { 0 } else { dir_len };
        let dir = directory(self.version, &table, base);
        if !matches!(self.layout, Layout::Exe(_)) && base + data_len >= PART {
            return user("The rebuilt archive would be over 1 GB, which Yakusu can't write yet.");
        }
        if base + data_len > u32::MAX as u64 {
            return user("The rebuilt archive would be over 4 GB, which Yakusu can't write.");
        }
        fs::create_dir_all(out_dir)?;
        let fname = |p: &Path| p.file_name().unwrap_or_default().to_string_lossy().to_string();
        let write_data = |w: &mut dyn Write| -> Result<()> {
            for (e, new) in self.entries.iter().zip(&fresh) {
                match new {
                    Some(bytes) => w.write_all(bytes)?,
                    None => self.copy_raw(e, w)?,
                }
            }
            Ok(())
        };
        let mut written = Vec::new();
        match self.layout {
            Layout::Exe(start) => {
                let out = out_dir.join(fname(&self.path));
                let mut w = std::io::BufWriter::new(File::create(&out)?);
                std::io::copy(&mut File::open(&self.path)?.take(start), &mut w)?;
                w.write_all(&dir)?;
                write_data(&mut w)?;
                w.write_all(&(start as u32).to_le_bytes())?;
                w.write_all(b"lv")?;
                w.flush()?;
                written.push(fname(&self.path));
            }
            Layout::Dat => {
                let out = out_dir.join(fname(&self.path));
                let mut w = std::io::BufWriter::new(File::create(&out)?);
                w.write_all(&dir)?;
                write_data(&mut w)?;
                w.write_all(&self.tail)?;
                w.flush()?;
                written.push(fname(&self.path));
            }
            Layout::Ext => {
                fs::write(out_dir.join(fname(&self.path)), &dir)?;
                let mut w = std::io::BufWriter::new(File::create(out_dir.join(fname(&self.parts[0])))?);
                write_data(&mut w)?;
                w.flush()?;
                written.push(fname(&self.path));
                written.push(fname(&self.parts[0]));
            }
        }
        Ok(written)
    }

    /// Stream an entry's stored bytes (single-part archives).
    fn copy_raw(&self, e: &Entry, w: &mut dyn Write) -> Result<()> {
        let mut f = File::open(&self.parts[0])?;
        f.seek(SeekFrom::Start(self.data_pos(e)))?;
        let n = std::io::copy(&mut f.take(e.size), w)?;
        if n != e.size {
            return Err(bad(&self.path, &format!("{} is cut short", e.name)));
        }
        Ok(())
    }
}

/// Write a new archive holding `files` (name, contents), zlib-compressed.
/// With `exe`, the archive is appended to those bytes (a game .exe);
/// otherwise it's a standalone .dat. Used by tests and tools.
pub fn create(path: &Path, files: &[(&str, &[u8])], exe: Option<&[u8]>) -> Result<()> {
    let mut table = Vec::new();
    let mut data = Vec::new();
    for (name, contents) in files {
        let mut z = ZlibEncoder::new(Vec::new(), Compression::default());
        z.write_all(contents)?;
        let stored = z.finish()?;
        let win_name = name.replace('/', "\\");
        let (raw, _, _) = encoding_rs::SHIFT_JIS.encode(&win_name);
        let e = Entry {
            name: name.to_string(),
            raw_name: raw.into_owned(),
            offset: 0,
            size: stored.len() as u64,
            compress: ZLIB,
            unk1: 0,
            checksum: crc32(&stored),
            encrypt: 0,
        };
        table.push((e, data.len() as u64));
        data.extend_from_slice(&stored);
    }
    let version = 102;
    let base = directory(version, &table, 0).len() as u64;
    let mut out = exe.map(|e| e.to_vec()).unwrap_or_default();
    let start = out.len() as u32;
    out.extend(directory(version, &table, base));
    out.extend(data);
    if exe.is_some() {
        out.extend_from_slice(&start.to_le_bytes());
        out.extend_from_slice(b"lv");
    }
    fs::write(path, out)?;
    Ok(())
}

/// Parse a file list. `Err(None)`: `dir` is too short; `Err(Some(why))`: damaged.
fn parse_dir(dir: &[u8]) -> std::result::Result<(u32, Vec<Entry>), Option<String>> {
    let mut c = Cur { b: dir, i: 2 };
    let version = c.u32().ok_or(None)?;
    let count = c.u32().ok_or(None)? as usize;
    if version < 100 {
        return Err(Some(format!("archive version {version} is too old")));
    }
    if count > 1_000_000 {
        return Err(Some("its file list is damaged".into()));
    }
    let mut k = keys();
    let mut names = Vec::with_capacity(count);
    for _ in 0..count {
        let n = c.u32().ok_or(None)? as usize;
        let raw: Vec<u8> = c.take(n).ok_or(None)?.iter().map(|b| b ^ (k.next().unwrap() as u8)).collect();
        names.push(raw);
    }
    let (mut kl, mut kh) = (keys(), keys());
    let mut offsets = Vec::with_capacity(count + 1);
    for _ in 0..=count {
        let low = c.u32().ok_or(None)? ^ kl.next().unwrap();
        let high = c.u32().ok_or(None)?;
        let high = if version >= 101 { high ^ kh.next().unwrap() } else { high };
        offsets.push((((high & 0x8000_0000) as u64) << 32) | low as u64);
    }
    let compress: Vec<u8> = c.take(count).ok_or(None)?.to_vec();
    let unk1: Vec<u32> = (0..count).map(|_| c.u32()).collect::<Option<_>>().ok_or(None)?;
    let checks: Vec<u32> = (0..count).map(|_| c.u32()).collect::<Option<_>>().ok_or(None)?;
    let encrypt: Vec<u8> = c.take(count).ok_or(None)?.to_vec();
    let mut entries = Vec::with_capacity(count);
    for i in 0..count {
        let (a, b) = (offsets[i], offsets[i + 1]);
        if b < a {
            return Err(Some("its file list is damaged".into()));
        }
        entries.push(Entry {
            name: decode_name(&names[i]),
            raw_name: names[i].clone(),
            offset: a,
            size: b - a,
            compress: compress[i],
            unk1: unk1[i],
            checksum: checks[i],
            encrypt: encrypt[i],
        });
    }
    Ok((version, entries))
}

/// The directory for `table` (entry, data position), offsets shifted by `base`.
fn directory(version: u32, table: &[(Entry, u64)], base: u64) -> Vec<u8> {
    let mut d = Vec::new();
    d.extend_from_slice(b"vf");
    d.extend_from_slice(&version.to_le_bytes());
    d.extend_from_slice(&(table.len() as u32).to_le_bytes());
    let mut k = keys();
    for (e, _) in table {
        d.extend_from_slice(&(e.raw_name.len() as u32).to_le_bytes());
        d.extend(e.raw_name.iter().map(|b| b ^ (k.next().unwrap() as u8)));
    }
    let (mut kl, mut kh) = (keys(), keys());
    let end = table.last().map(|(e, at)| at + e.size).unwrap_or(0);
    for off in table.iter().map(|(_, at)| *at).chain(std::iter::once(end)) {
        let off = off + base;
        let low = (off & 0xFFFF_FFFF) as u32;
        d.extend_from_slice(&(low ^ kl.next().unwrap()).to_le_bytes());
        // LiveMaker only ever writes 0 or 0xFFFFFFFF here; reading takes the top bit.
        let high_bit = ((off >> 32) as u32) << 31;
        let high = if version >= 101 {
            if (high_bit ^ kh.next().unwrap()) & 0x8000_0000 != 0 {
                0xFFFF_FFFF
            } else {
                0
            }
        } else {
            high_bit
        };
        d.extend_from_slice(&high.to_le_bytes());
    }
    d.extend(table.iter().map(|(e, _)| e.compress));
    for (e, _) in table {
        d.extend_from_slice(&e.unk1.to_le_bytes());
    }
    for (e, _) in table {
        d.extend_from_slice(&e.checksum.to_le_bytes());
    }
    d.extend(table.iter().map(|(e, _)| e.encrypt));
    d
}

// ------------------------------------------------------------------ scrambling

/// LiveMaker Pro's chunk shuffle (TScramble), undone.
fn unscramble(data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < 8 {
        return Some(data.to_vec());
    }
    let chunk = i32::from_le_bytes(data[..4].try_into().ok()?);
    let seed = u32::from_le_bytes(data[4..8].try_into().ok()?);
    if chunk <= 0 {
        return None;
    }
    let chunk = chunk as usize;
    let body = &data[8..];
    let total = body.len().div_ceil(chunk);
    let mut out = Vec::with_capacity(body.len());
    for i in randseq(total, seed ^ 0xF8EA) {
        let a = i * chunk;
        out.extend_from_slice(body.get(a..(a + chunk).min(body.len()))?);
    }
    (out.len() == body.len()).then_some(out)
}

struct Scramble([u32; 5]);

impl Scramble {
    fn new(seed: u32) -> Scramble {
        let mut s = if seed == 0 { 0xFFFF_FFFF } else { seed };
        let mut st = [0u32; 5];
        for x in &mut st {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            *x = s;
        }
        let mut r = Scramble(st);
        for _ in 0..19 {
            r.rand();
        }
        r
    }
    fn rand(&mut self) -> u32 {
        let st = &mut self.0;
        let x = (st[3] as u64)
            .wrapping_mul(0x7DD4_FFC7)
            .wrapping_add((st[2] as u64).wrapping_mul(0x5D4))
            .wrapping_add((st[1] as u64).wrapping_mul(0x6F0))
            .wrapping_add((st[0] as u64).wrapping_mul(0x13FB))
            .wrapping_add(st[4] as u64);
        st[4] = (x >> 32) as u32;
        st[3] = st[2];
        st[2] = st[1];
        st[1] = st[0];
        st[0] = x as u32;
        st[0]
    }
    fn randint(&mut self, low: usize, high: usize) -> usize {
        let r = self.rand() as f64 / 4_294_967_296.0;
        low + (r * (high - low + 1) as f64) as usize
    }
}

fn randseq(count: usize, seed: u32) -> Vec<usize> {
    let mut s = Scramble::new(seed);
    let mut values: Vec<usize> = (0..count).collect();
    let mut seq = vec![0; count];
    let mut i = 0;
    while !values.is_empty() {
        let n = if values.len() == 1 { 0 } else { s.randint(0, values.len() - 2) };
        seq[values.remove(n)] = i;
        i += 1;
    }
    seq
}
