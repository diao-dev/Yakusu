//! LiveMaker / LiveNovel games (Shigatsu Youka and many free and doujin VNs).
//!
//! The scripts are compiled `.lsb` files inside a "vf" archive: appended to
//! the game's .exe, or in a separate .dat (with an optional .ext index).
//! Yakusu reads them natively: no outside tools. Building writes a copy of
//! that archive with the translated scripts; installing swaps it in, with
//! the original backed up.
//!
//! Format knowledge comes from pylivemaker (GPL-3.0, Peter Rowlands and
//! tinfoil), the community toolkit for LiveMaker translation.

pub mod archive;
pub mod lsb;
pub mod tpword;

use super::{
    clean_dir, list_files, read_json_messages, safe_join, write_json_messages, Ctx, Engine, Entry, Installer, Match,
};
use crate::error::{user, Error, Result};
use archive::Archive;
use lsb::Lsb;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tpword::TpWord;

fn is_lsb(name: &str) -> bool {
    name.to_lowercase().ends_with(".lsb")
}

/// Top-level archives in `dir` that hold scripts, by file name.
fn find_archives(dir: &Path) -> Vec<String> {
    let names: Vec<String> = fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default();
    let lower: Vec<String> = names.iter().map(|n| n.to_lowercase()).collect();
    let mut out = Vec::new();
    for (name, low) in names.iter().zip(&lower) {
        let candidate = if low.ends_with(".exe") || low.ends_with(".ext") {
            true
        } else if let Some(stem) = low.strip_suffix(".dat") {
            // The data half of a .ext pair isn't an archive by itself.
            !lower.contains(&format!("{stem}.ext"))
        } else {
            false
        };
        if !candidate || !archive::is_archive(&dir.join(name)) {
            continue;
        }
        // Save files are vf archives too; only ones with scripts count.
        if Archive::open(&dir.join(name)).is_ok_and(|a| a.entries.iter().any(|e| is_lsb(&e.name))) {
            out.push(name.clone());
        }
    }
    out.sort();
    out
}

pub fn detect(game_dir: &Path) -> Option<Match> {
    let archives = find_archives(game_dir);
    if archives.is_empty() {
        return None;
    }
    let mut d = Map::new();
    d.insert("archives".into(), Value::from(archives));
    Some(Match::new(
        "livemaker",
        "LiveMaker",
        0.95,
        d,
        "auto",
        "Dialogue and narration are translated. Choice menus and name boxes stay in Japanese for now.",
    ))
}

pub struct LiveMaker {
    pub m: Match,
}

/// Where a script's copy lives under work/scripts, and its JSON name.
fn script_rel(arc: &str, name: &str) -> String {
    format!("{arc}/{name}")
}

impl LiveMaker {
    fn archives(&self, cx: &Ctx) -> Vec<String> {
        let found = find_archives(&cx.game());
        if found.is_empty() {
            self.m.strings("archives")
        } else {
            found
        }
    }

    /// The original archive (its backup while a patch is installed).
    fn open_original(cx: &Ctx, arc: &str) -> Result<Archive> {
        let inst = Installer::new(cx.project);
        let path = inst.original(&cx.game(), arc)?;
        let a = Archive::open(&path)?;
        if a.layout == archive::Layout::Ext {
            // The .dat half must be original too.
            let dat = path.with_extension("dat");
            let dat_rel = Path::new(arc).with_extension("dat").to_string_lossy().to_string();
            let want = inst.original(&cx.game(), &dat_rel)?;
            if dat != want {
                return user(format!(
                    "{arc} and its .dat file don't match (one is the original, the other Yakusu's patch). Use Restore, \
                     then read the scripts again."
                ));
            }
        }
        Ok(a)
    }
}

/// The blocks of every TextIns in a script, in order: (text index, blocks).
fn script_blocks(l: &Lsb) -> Vec<(usize, TpWord, Vec<tpword::Block>)> {
    let mut out = Vec::new();
    for i in 0..l.texts.len() {
        // A text blob Yakusu can't read is left alone.
        if let Ok(tw) = TpWord::parse(l.text(i)) {
            let blocks = tw.blocks();
            if !blocks.is_empty() {
                out.push((i, tw, blocks));
            }
        }
    }
    out
}

/// The JSON entries for one script.
pub fn export(l: &Lsb) -> Vec<Entry> {
    script_blocks(l)
        .into_iter()
        .flat_map(|(_, _, blocks)| blocks)
        .map(|b| Entry { name: b.name, names: None, message: b.text })
        .collect()
}

/// Apply translated entries (same order as [`export`]) to a script.
/// Returns the new script bytes and characters that couldn't be shown.
pub fn apply(l: &Lsb, entries: &[Entry], half_width: bool) -> Result<(Vec<u8>, Vec<char>)> {
    let mut replace: HashMap<usize, Vec<u8>> = HashMap::new();
    let mut lost = Vec::new();
    let mut k = 0usize;
    let total: usize = script_blocks(l).iter().map(|(_, _, b)| b.len()).sum();
    if entries.len() != total {
        return user(format!(
            "The translated file has {} lines but the script has {total}. Read the scripts again.",
            entries.len()
        ));
    }
    for (i, mut tw, blocks) in script_blocks(l) {
        let mut changed = false;
        let mine = &entries[k..k + blocks.len()];
        k += blocks.len();
        for (b, e) in blocks.iter().zip(mine).rev() {
            if e.message != b.text {
                lost.extend(tw.replace(b, &e.message).map_err(Error::User)?);
                changed = true;
            }
        }
        if changed {
            replace.insert(i, tw.to_bytes());
        }
    }
    Ok((l.rebuild(&replace, half_width), lost))
}

impl Engine for LiveMaker {
    fn m(&self) -> &Match {
        &self.m
    }
    fn m_mut(&mut self) -> &mut Match {
        &mut self.m
    }

    fn prepare(&mut self, cx: &Ctx) -> Result<PathBuf> {
        let scripts = cx.project.work("scripts");
        let json = cx.project.work("json");
        clean_dir(&scripts)?;
        clean_dir(&json)?;
        let archives = self.archives(cx);
        if archives.is_empty() {
            return user("No LiveMaker archive with scripts was found in this folder.");
        }
        let (mut found, mut unreadable, mut lines) = (0usize, Vec::new(), 0usize);
        let mut fixable = false;
        for arc in &archives {
            let a = Self::open_original(cx, arc)?;
            cx.log(format!("Reading {arc} ({} files)", a.entries.len()));
            for e in a.entries.iter().filter(|e| is_lsb(&e.name)) {
                let rel = script_rel(arc, &e.name);
                let data = a.read(e)?;
                let dst = safe_join(&scripts, &rel)?;
                fs::create_dir_all(dst.parent().unwrap())?;
                fs::write(&dst, &data)?;
                found += 1;
                let l = match Lsb::parse(data) {
                    Ok(l) => l,
                    Err(why) => {
                        unreadable.push(format!("{} ({why})", e.name));
                        continue;
                    }
                };
                fixable |= l.is_lm3() && l.has_font_flag();
                let entries = export(&l);
                if !entries.is_empty() {
                    lines += entries.len();
                    write_json_messages(&safe_join(&json, &format!("{rel}.json"))?, &entries)?;
                }
            }
        }
        for u in unreadable.iter().take(10) {
            cx.log(format!("Couldn't read {u}; its text stays in Japanese"));
        }
        if lines == 0 {
            return user(if found == 0 {
                "This LiveMaker game's archive has no scripts in it.".to_string()
            } else {
                format!("Yakusu read {found} scripts but found no text in them.")
            });
        }
        self.m.set("archives", archives.clone());
        self.m.set("half_width_fix", fixable);
        cx.log(format!("Found {lines} text blocks in {} scripts", found - unreadable.len()));
        Ok(json)
    }

    fn build(&mut self, cx: &Ctx) -> Result<PathBuf> {
        let scripts = cx.project.work("scripts");
        let tl = cx.project.work("json_tl");
        let out = cx.project.work("out");
        clean_dir(&out)?;
        let half_width = self.m.flag("half_width_fix");
        let mut all_lost: Vec<char> = Vec::new();
        let mut total = 0usize;
        for arc in self.m.strings("archives") {
            let mut replace: HashMap<String, Vec<u8>> = HashMap::new();
            for rel in list_files(&scripts.join(&arc), Some(&[".lsb"])) {
                let orig = fs::read(safe_join(&scripts.join(&arc), &rel)?)?;
                let Ok(l) = Lsb::parse(orig.clone()) else { continue };
                let jpath = safe_join(&tl, &format!("{arc}/{rel}.json"))?;
                let (new, lost) = if jpath.is_file() {
                    apply(&l, &read_json_messages(&jpath)?, half_width && l.is_lm3())?
                } else {
                    (l.rebuild(&HashMap::new(), half_width && l.is_lm3()), vec![])
                };
                all_lost.extend(lost);
                if new != orig {
                    replace.insert(rel, new);
                }
            }
            if replace.is_empty() {
                continue;
            }
            let a = Self::open_original(cx, &arc)?;
            let files = a.rewrite(&replace, &out)?;
            total += replace.len();
            cx.log(format!("Built {} with {} translated scripts", files.join(" + "), replace.len()));
        }
        if total == 0 {
            return user("No translated lines to pack yet.");
        }
        all_lost.sort_unstable();
        all_lost.dedup();
        if !all_lost.is_empty() {
            let shown: String = all_lost.iter().take(20).collect();
            cx.log(format!("These characters can't be shown by the game and became '?': {shown}"));
        }
        if half_width {
            cx.log("Turned off the message window's font switching so English shows half-width (LiveMaker 3).");
        }
        Ok(out)
    }
}
