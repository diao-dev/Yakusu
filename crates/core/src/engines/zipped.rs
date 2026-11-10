//! Zip packages: NW.js `package.nw`, or a zip appended to the game's .exe
//! (how NW.js games are usually shipped).

use crate::error::{Error, Result};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::Path;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

fn bad(path: &Path, e: impl std::fmt::Display) -> Error {
    Error::User(format!(
        "{} can't be read as a zip package ({e}).",
        path.file_name().unwrap_or_default().to_string_lossy()
    ))
}

fn open(path: &Path) -> Result<ZipArchive<File>> {
    ZipArchive::new(File::open(path)?).map_err(|e| bad(path, e))
}

/// Whether `path` holds a zip (at its start or appended to an exe).
pub fn is_zip(path: &Path) -> bool {
    open(path).is_ok()
}

/// File names inside, '/'-separated (folders left out).
pub fn names(path: &Path) -> Result<Vec<String>> {
    let z = open(path)?;
    Ok(z.file_names().filter(|n| !n.ends_with('/')).map(|n| n.replace('\\', "/")).collect())
}

pub fn read(path: &Path, name: &str) -> Result<Vec<u8>> {
    let mut z = open(path)?;
    let idx = (0..z.len())
        .find(|&i| z.name_for_index(i).is_some_and(|n| n.replace('\\', "/") == name))
        .ok_or_else(|| bad(path, format!("{name} is missing")))?;
    let mut f = z.by_index(idx).map_err(|e| bad(path, e))?;
    let mut out = Vec::new();
    f.read_to_end(&mut out).map_err(|e| bad(path, e))?;
    Ok(out)
}

/// Write a copy of the package to `out` with some files replaced. Anything
/// before the zip (the exe) is kept; untouched files are copied as stored.
pub fn rewrite(path: &Path, replace: &HashMap<String, Vec<u8>>, out: &Path) -> Result<()> {
    let mut z = open(path)?;
    // Two ways a zip ends up after an exe: plain concatenation (offsets count
    // from the zip; offset() > 0), or written in place (offsets count from
    // the file start; the first entry shows where the zip begins). Write the
    // copy the same way the original was made.
    let first = (0..z.len()).filter_map(|i| z.by_index_raw(i).ok().map(|f| f.header_start())).min().unwrap_or(0);
    let concat = z.offset() > 0;
    let prefix = if concat { z.offset() } else { first };
    let mut w = BufWriter::new(File::create(out)?);
    std::io::copy(&mut File::open(path)?.take(prefix), &mut w)?;
    if concat {
        let tmp = out.with_extension("zip-part");
        write_entries(&mut z, path, replace, BufWriter::new(File::create(&tmp)?))?;
        std::io::copy(&mut File::open(&tmp)?, &mut w)?;
        w.flush()?;
        std::fs::remove_file(&tmp)?;
        return Ok(());
    }
    write_entries(&mut z, path, replace, w)
}

fn write_entries<W: Write + std::io::Seek>(
    z: &mut ZipArchive<File>,
    path: &Path,
    replace: &HashMap<String, Vec<u8>>,
    w: W,
) -> Result<()> {
    let mut zw = ZipWriter::new(w);
    for i in 0..z.len() {
        let name = z.name_for_index(i).unwrap_or_default().to_string();
        match replace.get(&name.replace('\\', "/")) {
            Some(bytes) => {
                let f = z.by_index_raw(i).map_err(|e| bad(path, e))?;
                let method = if f.compression() == CompressionMethod::Stored {
                    CompressionMethod::Stored
                } else {
                    CompressionMethod::Deflated
                };
                let mut opts = SimpleFileOptions::default().compression_method(method);
                if let Some(t) = f.last_modified() {
                    opts = opts.last_modified_time(t);
                }
                if let Some(m) = f.unix_mode() {
                    opts = opts.unix_permissions(m);
                }
                drop(f);
                zw.start_file(name, opts).map_err(|e| bad(path, e))?;
                zw.write_all(bytes)?;
            }
            None => {
                let f = z.by_index_raw(i).map_err(|e| bad(path, e))?;
                zw.raw_copy_file(f).map_err(|e| bad(path, e))?;
            }
        }
    }
    zw.finish().map_err(|e| bad(path, e))?.flush()?;
    Ok(())
}

/// Build a zip from (name, contents), optionally after `prefix` bytes. Tests.
pub fn create(path: &Path, files: &[(&str, &[u8])], prefix: Option<&[u8]>) -> Result<()> {
    let mut w = BufWriter::new(File::create(path)?);
    if let Some(p) = prefix {
        w.write_all(p)?;
    }
    let mut zw = ZipWriter::new(w);
    for (name, contents) in files {
        zw.start_file(*name, SimpleFileOptions::default().compression_method(CompressionMethod::Deflated))
            .map_err(|e| bad(path, e))?;
        zw.write_all(contents)?;
    }
    zw.finish().map_err(|e| bad(path, e))?.flush()?;
    Ok(())
}
