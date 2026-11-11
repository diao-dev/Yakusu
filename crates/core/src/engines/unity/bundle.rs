//! UnityFS asset bundles (Unity 5.3 and later): read, and write back with
//! some files inside replaced.
//!
//! ```text
//! "UnityFS\0"  u32 format  cstr player_version  cstr engine_version
//! i64 size  u32 compressed_info  u32 uncompressed_info  u32 flags   (big-endian)
//! [format >= 7: align 16]
//! blocks info (here, or at the end when flags & 0x80), compressed by flags & 0x3F:
//!     16-byte hash, u32 n x (u32 uncompressed, u32 compressed, u16 flags [, u64 offset]),
//!     u32 n x (i64 offset, i64 size, u32 flags, cstr path)
//! [flags & 0x200: align 16]  data blocks (each compressed by its flags & 0x3F)
//! ```
//!
//! Compression: 0 none, 1 LZMA, 2 LZ4, 3 LZ4HC. Written bundles use LZ4HC
//! blocks of 128 KiB (Unity's own choice for runtime bundles).
//! Format notes follow UnityPy (MIT) and AssetStudio (MIT).

use crate::error::{Error, Result};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const CHUNK: usize = 0x20000;
const AT_END: u32 = 0x80;
const COMBINED: u32 = 0x40;
const PAD_START: u32 = 0x200;
const ENCRYPTED_OLD: u32 = 0x200;
const ENCRYPTED: u32 = 0x1400;

#[derive(Clone, Debug)]
pub struct Node {
    pub offset: u64,
    pub size: u64,
    pub flags: u32,
    pub path: String,
}

#[derive(Clone, Debug)]
struct Block {
    uncompressed: u32,
    compressed: u32,
    flags: u16,
    offset: Option<u64>,
}

pub struct Bundle {
    pub path: PathBuf,
    format: u32,
    player: String,
    engine: String,
    flags: u32,
    aligned: bool,
    pad_start: bool,
    blocks: Vec<Block>,
    pub nodes: Vec<Node>,
    data_start: u64,
}

fn bad(path: &Path, why: impl std::fmt::Display) -> Error {
    Error::User(format!(
        "{} isn't a Unity asset bundle Yakusu can read ({why}).",
        path.file_name().unwrap_or_default().to_string_lossy()
    ))
}

struct Be<'a> {
    b: &'a [u8],
    i: usize,
}

impl Be<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        let s = self.b.get(self.i..self.i.checked_add(n)?)?;
        self.i += n;
        Some(s)
    }
    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_be_bytes(self.take(2)?.try_into().ok()?))
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_be_bytes(self.take(8)?.try_into().ok()?))
    }
    fn cstr(&mut self) -> Option<String> {
        let end = self.b.get(self.i..)?.iter().position(|&c| c == 0)?;
        let s = String::from_utf8_lossy(&self.b[self.i..self.i + end]).to_string();
        self.i += end + 1;
        Some(s)
    }
}

/// Decompress one block.
pub fn decompress(data: &[u8], size: usize, kind: u32) -> std::result::Result<Vec<u8>, String> {
    match kind & 0x3F {
        0 => Ok(data.to_vec()),
        1 => {
            let opts = lzma_rs::decompress::Options {
                unpacked_size: lzma_rs::decompress::UnpackedSize::UseProvided(Some(size as u64)),
                ..Default::default()
            };
            let mut out = Vec::with_capacity(size.min(64 << 20));
            lzma_rs::lzma_decompress_with_options(&mut std::io::Cursor::new(data), &mut out, &opts)
                .map_err(|e| format!("LZMA: {e}"))?;
            Ok(out)
        }
        // LZ4 can't expand more than ~255 times: a bigger size is damage.
        2 | 3 if size > data.len().saturating_mul(255) + 64 => Err("LZ4: damaged block".into()),
        2 | 3 => lz4_flex::block::decompress(data, size).map_err(|e| format!("LZ4: {e}")),
        k => Err(format!("compression type {k} isn't supported")),
    }
}

/// The first 8 bytes say whether a file is a UnityFS bundle.
pub fn is_bundle(path: &Path) -> bool {
    let mut b = [0u8; 8];
    std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut b)).is_ok() && &b == b"UnityFS\0"
}

impl Bundle {
    pub fn open(path: &Path) -> Result<Bundle> {
        let mut f = std::fs::File::open(path)?;
        let len = f.metadata()?.len();
        let mut head = vec![0u8; 512.min(len as usize)];
        f.read_exact(&mut head)?;
        let mut r = Be { b: &head, i: 0 };
        let short = || bad(path, "header cut short");
        if r.cstr().ok_or_else(short)? != "UnityFS" {
            return Err(bad(path, "not UnityFS"));
        }
        let format = r.u32().ok_or_else(short)?;
        let player = r.cstr().ok_or_else(short)?;
        let engine = r.cstr().ok_or_else(short)?;
        let _size = r.u64().ok_or_else(short)?;
        let info_c = r.u32().ok_or_else(short)? as usize;
        let info_u = r.u32().ok_or_else(short)? as usize;
        let flags = r.u32().ok_or_else(short)?;
        if info_c as u64 > len || info_u > 256 << 20 {
            return Err(bad(path, "damaged header"));
        }
        let new_flags = new_flag_set(&engine);
        if flags & if new_flags { ENCRYPTED } else { ENCRYPTED_OLD } != 0 {
            return Err(bad(path, "it's encrypted"));
        }
        let aligned = format >= 7 || engine_at_least(&engine, 2019, 4, 15);
        let mut pos = r.i as u64;
        if aligned {
            pos = pos.div_ceil(16) * 16;
        }
        let info_at = if flags & AT_END != 0 { len - info_c as u64 } else { pos };
        f.seek(SeekFrom::Start(info_at))?;
        let mut info = vec![0u8; info_c];
        f.read_exact(&mut info).map_err(|_| short())?;
        let info = decompress(&info, info_u, flags).map_err(|e| bad(path, e))?;
        let mut r = Be { b: &info, i: 16 };
        let short = || bad(path, "block list cut short");
        let nb = r.u32().ok_or_else(short)? as usize;
        let mut blocks = Vec::with_capacity(nb.min(1 << 16));
        for _ in 0..nb {
            blocks.push(Block {
                uncompressed: r.u32().ok_or_else(short)?,
                compressed: r.u32().ok_or_else(short)?,
                flags: r.u16().ok_or_else(short)?,
                offset: if format >= 9 { Some(r.u64().ok_or_else(short)?) } else { None },
            });
        }
        let nn = r.u32().ok_or_else(short)? as usize;
        let mut nodes = Vec::with_capacity(nn.min(1 << 16));
        for _ in 0..nn {
            nodes.push(Node {
                offset: r.u64().ok_or_else(short)?,
                size: r.u64().ok_or_else(short)?,
                flags: r.u32().ok_or_else(short)?,
                path: r.cstr().ok_or_else(short)?,
            });
        }
        let mut data_start = if flags & AT_END != 0 { pos } else { pos + info_c as u64 };
        let pad_start = new_flags && flags & PAD_START != 0;
        if pad_start {
            data_start = data_start.div_ceil(16) * 16;
        }
        Ok(Bundle {
            path: path.to_path_buf(),
            format,
            player,
            engine,
            flags,
            aligned,
            pad_start,
            blocks,
            nodes,
            data_start,
        })
    }

    /// All files' data, decompressed (the bundle's data stream).
    pub fn data(&self) -> Result<Vec<u8>> {
        self.data_until(u64::MAX)
    }

    /// The start of the data stream: at least `limit` bytes (whole blocks).
    pub fn data_until(&self, limit: u64) -> Result<Vec<u8>> {
        let mut f = std::fs::File::open(&self.path)?;
        let total: u64 = self.blocks.iter().map(|b| b.uncompressed as u64).sum();
        if total > 4 << 30 || self.blocks.iter().any(|b| b.compressed as u64 > len_of(&self.path)) {
            return Err(bad(&self.path, "damaged block list"));
        }
        let mut out = Vec::with_capacity(total.min(limit.saturating_add(CHUNK as u64)).min(256 << 20) as usize);
        let mut at = self.data_start;
        for b in &self.blocks {
            if out.len() as u64 >= limit {
                break;
            }
            let pos = b.offset.map(|o| self.data_start + o).unwrap_or(at);
            f.seek(SeekFrom::Start(pos))?;
            let mut c = vec![0u8; b.compressed as usize];
            f.read_exact(&mut c).map_err(|_| bad(&self.path, "data cut short"))?;
            out.extend(decompress(&c, b.uncompressed as usize, b.flags as u32).map_err(|e| bad(&self.path, e))?);
            at = pos + b.compressed as u64;
        }
        Ok(out)
    }

    /// The object classes in the bundle's first serialized file, read from
    /// as little data as possible (to skip bundles of textures or sounds).
    pub fn classes(&self) -> Result<Vec<i32>> {
        let Some(n) = self.nodes.iter().min_by_key(|n| n.offset) else { return Ok(vec![]) };
        let mut want = n.offset + 64 * 1024;
        loop {
            let data = self.data_until(want)?;
            let end = (n.offset + n.size).min(data.len() as u64) as usize;
            let head = data.get(n.offset as usize..end).unwrap_or(&[]);
            if let Ok(sf) = super::serialized::SerializedFile::parse_head(head, n.size) {
                return Ok(sf.objects.iter().map(|o| o.class_id).collect());
            }
            if data.len() as u64 >= n.offset + n.size || want >= n.offset + n.size {
                return Ok(vec![]);
            }
            want = (want * 4).min(n.offset + n.size);
        }
    }

    /// One file's bytes out of [`Bundle::data`].
    pub fn node<'a>(&self, data: &'a [u8], n: &Node) -> Result<&'a [u8]> {
        data.get(n.offset as usize..(n.offset + n.size) as usize)
            .ok_or_else(|| bad(&self.path, "a file runs past the data"))
    }

    /// Write the bundle to `out` with some files replaced (path -> bytes).
    /// When `keep_crc` is set, 4 bytes after the last file make the data's
    /// CRC-32 match the original's, so a game that checks it still accepts
    /// the bundle.
    pub fn rewrite(
        &self,
        data: &[u8],
        replace: &std::collections::HashMap<String, Vec<u8>>,
        out: &Path,
        keep_crc: bool,
    ) -> Result<()> {
        let align = if self.nodes.iter().all(|n| n.offset % 16 == 0) { 16 } else { 1 };
        let mut stream: Vec<u8> = Vec::with_capacity(data.len() + 4096);
        let mut nodes = Vec::with_capacity(self.nodes.len());
        let mut order: Vec<&Node> = self.nodes.iter().collect();
        order.sort_by_key(|n| n.offset);
        let mut new_nodes: std::collections::HashMap<String, Node> = Default::default();
        for n in order {
            while !stream.len().is_multiple_of(align) {
                stream.push(0);
            }
            let bytes: &[u8] = match replace.get(&n.path) {
                Some(b) => b,
                None => self.node(data, n)?,
            };
            new_nodes.insert(
                n.path.clone(),
                Node { offset: stream.len() as u64, size: bytes.len() as u64, flags: n.flags, path: n.path.clone() },
            );
            stream.extend_from_slice(bytes);
        }
        for n in &self.nodes {
            nodes.push(new_nodes[&n.path].clone());
        }
        if keep_crc {
            let target = crc32(data);
            stream.extend_from_slice(&[0; 4]);
            let at = stream.len() - 4;
            let state = crc32_state(&stream[..at]);
            stream[at..].copy_from_slice(&forge(state, target));
            debug_assert_eq!(crc32(&stream), target);
        }
        // Blocks: LZ4HC unless the original stored its data uncompressed.
        let kind: u16 = match self.blocks.first().map(|b| b.flags & 0x3F) {
            Some(0) => 0,
            _ => 3,
        };
        let mut blocks = Vec::new();
        let mut packed = Vec::with_capacity(stream.len() / 2);
        let chunk = if kind == 0 { stream.len().max(1) } else { CHUNK };
        for c in stream.chunks(chunk) {
            let (bytes, flags) = if kind == 0 {
                (c.to_vec(), 0)
            } else {
                let z = lz4_flex::block::compress(c);
                if z.len() < c.len() {
                    (z, kind)
                } else {
                    (c.to_vec(), 0)
                }
            };
            blocks.push(Block {
                uncompressed: c.len() as u32,
                compressed: bytes.len() as u32,
                flags,
                offset: if self.format >= 9 { Some(packed.len() as u64) } else { None },
            });
            packed.extend(bytes);
        }
        // Blocks info.
        let mut info = vec![0u8; 16];
        info.extend((blocks.len() as u32).to_be_bytes());
        for b in &blocks {
            info.extend(b.uncompressed.to_be_bytes());
            info.extend(b.compressed.to_be_bytes());
            info.extend(b.flags.to_be_bytes());
            if let Some(o) = b.offset {
                info.extend(o.to_be_bytes());
            }
        }
        info.extend((nodes.len() as u32).to_be_bytes());
        for n in &nodes {
            info.extend(n.offset.to_be_bytes());
            info.extend(n.size.to_be_bytes());
            info.extend(n.flags.to_be_bytes());
            info.extend(n.path.as_bytes());
            info.push(0);
        }
        let info_c = lz4_flex::block::compress(&info);
        // Keep the layout flags; the info is LZ4HC now.
        let flags = (self.flags & !0x3F & !AT_END) | COMBINED | 3;
        let mut head = Vec::new();
        head.extend(b"UnityFS\0");
        head.extend(self.format.to_be_bytes());
        head.extend(self.player.as_bytes());
        head.push(0);
        head.extend(self.engine.as_bytes());
        head.push(0);
        let size_at = head.len();
        head.extend(0u64.to_be_bytes());
        head.extend((info_c.len() as u32).to_be_bytes());
        head.extend((info.len() as u32).to_be_bytes());
        head.extend(flags.to_be_bytes());
        if self.aligned {
            while !head.len().is_multiple_of(16) {
                head.push(0);
            }
        }
        head.extend(&info_c);
        if self.pad_start {
            while !head.len().is_multiple_of(16) {
                head.push(0);
            }
        }
        let total = (head.len() + packed.len()) as u64;
        head[size_at..size_at + 8].copy_from_slice(&total.to_be_bytes());
        let mut w = std::io::BufWriter::new(std::fs::File::create(out)?);
        w.write_all(&head)?;
        w.write_all(&packed)?;
        w.flush()?;
        Ok(())
    }
}

fn len_of(p: &Path) -> u64 {
    std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
}

/// Unity 2020.3.34 / 2021.3.2 / 2022.1.1 and later use 0x200 for padding
/// (earlier, and Unity China builds, used it for encryption).
fn new_flag_set(engine: &str) -> bool {
    let v = version(engine);
    match v.0 {
        0..=2019 => false,
        2020 => v >= (2020, 3, 34),
        2021 => v >= (2021, 3, 2),
        2022 => v >= (2022, 1, 1),
        _ => true,
    }
}

fn version(s: &str) -> (u32, u32, u32) {
    let mut it =
        s.split(|c: char| !c.is_ascii_digit()).filter(|p| !p.is_empty()).map(|p| p.parse::<u32>().unwrap_or(0));
    (it.next().unwrap_or(0), it.next().unwrap_or(0), it.next().unwrap_or(0))
}

fn engine_at_least(s: &str, a: u32, b: u32, c: u32) -> bool {
    let v = version(s);
    v.0 == a && v >= (a, b, c)
}

// ---------------------------------------------------------------- CRC-32

fn table() -> &'static [u32; 256] {
    static T: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, e) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
            *e = c;
        }
        t
    })
}

/// The CRC register after `data` (before the final inversion).
fn crc32_state(data: &[u8]) -> u32 {
    let t = table();
    data.iter().fold(0xFFFF_FFFFu32, |c, &b| t[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8))
}

pub fn crc32(data: &[u8]) -> u32 {
    !crc32_state(data)
}

/// 4 bytes that take the CRC register from `state` to the final CRC `target`.
fn forge(state: u32, target: u32) -> [u8; 4] {
    let t = table();
    let mut inv = [0u8; 256];
    for (i, v) in t.iter().enumerate() {
        inv[(v >> 24) as usize] = i as u8;
    }
    let mut r = !target;
    for _ in 0..4 {
        let j = inv[(r >> 24) as usize];
        r = ((r ^ t[j as usize]) << 8) | j as u32;
    }
    (r ^ state).to_le_bytes()
}
