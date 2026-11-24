//! SiglusEngine games (Key and VisualArt's: Rewrite, Rewrite+, Summer Pockets,
//! Little Busters! Converted Edition, and many others).
//!
//! The story is compiled into one `Scene.pck`. Yakusu reads it natively:
//! unpacks each scene, translates its dialogue, speaker names and choices,
//! and writes a new Scene.pck for the unmodified game to load. Installing
//! swaps it in (the original is backed up); nothing else in the game changes.
//!
//! Format knowledge: siglus_rs and siglus_static_key_tool (xmoezzz, MPL-2.0)
//! and SiglusExtract (xmoezzz, GPL-3.0).

pub mod key;
pub mod lzss;
pub mod pck;
pub mod ss;

use super::{
    clean_dir, list_files, read_json_messages, safe_join, safe_rel, write_json_messages, Ctx, Engine, Installer, Match,
};
use crate::error::{user, Result};
use pck::Pck;
use serde_json::{Map, Value};
use ss::Ss;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Where Scene.pck is, relative to the game folder ("Scene.pck" or
/// "<folder>/Scene.pck" one level down, any letter case).
pub fn find_pck(game_dir: &Path) -> Option<String> {
    let in_dir = |dir: &Path| -> Option<String> {
        fs::read_dir(dir).ok()?.flatten().find_map(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            (n.eq_ignore_ascii_case("Scene.pck") && e.path().is_file()).then_some(n)
        })
    };
    if let Some(n) = in_dir(game_dir) {
        return Some(n);
    }
    let mut subs: Vec<String> = fs::read_dir(game_dir)
        .ok()?
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.eq_ignore_ascii_case("data") || n.eq_ignore_ascii_case("dat"))
        .collect();
    subs.sort();
    subs.into_iter().find_map(|s| in_dir(&game_dir.join(&s)).map(|n| format!("{s}/{n}")))
}

pub fn detect(game_dir: &Path) -> Option<Match> {
    let rel = find_pck(game_dir)?;
    let mut d = Map::new();
    d.insert("pck_rel".into(), Value::from(rel));
    Some(Match::new(
        "siglus",
        "SiglusEngine",
        0.95,
        d,
        "experimental",
        "Dialogue, speaker names and choices are translated; Yakusu writes a new Scene.pck (the original is backed up). \
         Try 150 lines first. If English looks cramped in the game, pick another font in its settings.",
    ))
}

pub struct Siglus {
    pub m: Match,
}

/// The work-file name for scene `i`: its number (so order is kept) and name.
fn scene_rel(i: usize, name: &str) -> String {
    let clean: String =
        name.chars().map(|c| if c.is_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).take(60).collect();
    let rel = if clean.is_empty() { format!("{i:05}.ss") } else { format!("{i:05}_{clean}.ss") };
    safe_rel(&rel).unwrap_or_else(|_| format!("{i:05}.ss"))
}

/// Scene number from a work-file name.
fn scene_no(rel: &str) -> Option<usize> {
    rel.get(..5)?.parse().ok()
}

impl Siglus {
    fn pck_rel(&self, cx: &Ctx) -> Result<String> {
        match find_pck(&cx.game()).or_else(|| self.m.str("pck_rel").map(String::from)) {
            Some(r) => Ok(r),
            None => user("Scene.pck isn't in this game folder any more."),
        }
    }

    /// The original Scene.pck (its backup while a patch is installed).
    fn original(&self, cx: &Ctx) -> Result<(PathBuf, Pck)> {
        let rel = self.pck_rel(cx)?;
        let path = Installer::new(cx.project).original(&cx.game(), &rel)?;
        let pck = Pck::parse(fs::read(&path)?)?;
        Ok((path, pck))
    }
}

impl Engine for Siglus {
    fn m(&self) -> &Match {
        &self.m
    }
    fn m_mut(&mut self) -> &mut Match {
        &mut self.m
    }
    /// SiglusEngine stores text as UTF-16: anything shows if the font has it.
    fn encoding(&self) -> String {
        "utf-16".into()
    }
    fn straight_quotes(&self) -> bool {
        false
    }

    fn prepare(&mut self, cx: &Ctx) -> Result<PathBuf> {
        let scripts = cx.project.work("scripts");
        let json = cx.project.work("json");
        clean_dir(&scripts)?;
        clean_dir(&json)?;
        let (path, pck) = self.original(cx)?;
        cx.log(format!("Reading Scene.pck ({} scenes)", pck.index.len()));
        let key = if pck.needs_key() {
            let k = key::resolve(cx, &pck, &path, self.m.str("siglus_key"))?;
            self.m.set("siglus_key", key::hex(&k));
            Some(k)
        } else {
            self.m.set("siglus_key", Value::Null);
            None
        };
        // Unpack every scene first; which way strings are stored is decided
        // over all of them (most games XOR them, a few older ones don't).
        let mut scenes: Vec<(usize, Vec<u8>)> = Vec::new();
        let mut unreadable = Vec::new();
        let (mut plain, mut xored) = (0i64, 0i64);
        for i in 0..pck.index.len() {
            if pck.index[i].1 == 0 {
                continue;
            }
            match pck.scene(i, key.as_ref()).and_then(Ss::parse) {
                Ok(s) => {
                    let (p, x) = s.codec_scores();
                    plain += p;
                    xored += x;
                    scenes.push((i, s.data));
                }
                Err(e) => unreadable.push(format!("scene {i} {} ({e})", pck.names[i])),
            }
        }
        let xor = xored >= plain;
        let mut lines = 0usize;
        for (i, data) in scenes {
            let rel = scene_rel(i, &pck.names[i]);
            fs::write(safe_join(&scripts, &rel)?, &data)?;
            let s = Ss::parse_with(data, xor)?;
            let entries = s.export();
            if !entries.is_empty() {
                lines += entries.len();
                write_json_messages(&safe_join(&json, &format!("{rel}.json"))?, &entries)?;
            }
        }
        for u in unreadable.iter().take(10) {
            cx.log(format!("Couldn't read {u}; its text stays in Japanese"));
        }
        if lines == 0 {
            return user("Yakusu read Scene.pck but found no dialogue in it.");
        }
        self.m.set("pck_rel", self.pck_rel(cx)?);
        self.m.set("plain_strings", !xor);
        cx.log(format!("Found {lines} lines in {} scenes", pck.index.len() - unreadable.len()));
        Ok(json)
    }

    fn build(&mut self, cx: &Ctx) -> Result<PathBuf> {
        let scripts = cx.project.work("scripts");
        let tl = cx.project.work("json_tl");
        let out = cx.project.work("out");
        clean_dir(&out)?;
        let xor = !self.m.flag("plain_strings");
        let (_, pck) = self.original(cx)?;
        let key = match self.m.str("siglus_key") {
            Some(s) => Some(key::parse_hex(s).filter(|k| pck.check_key(k)).ok_or_else(|| {
                crate::error::Error::User("The saved Scene.pck key no longer fits. Read the scripts again.".into())
            })?),
            None if pck.needs_key() => return user("Scene.pck needs its key: read the scripts again."),
            None => None,
        };
        let mut replace: HashMap<usize, Vec<u8>> = HashMap::new();
        for rel in list_files(&scripts, Some(&[".ss"])) {
            let jpath = safe_join(&tl, &format!("{rel}.json"))?;
            if !jpath.is_file() {
                continue;
            }
            let Some(i) = scene_no(&rel).filter(|&i| i < pck.index.len()) else { continue };
            let s = Ss::parse_with(fs::read(safe_join(&scripts, &rel)?)?, xor)?;
            let new =
                s.apply(&read_json_messages(&jpath)?).map_err(|e| crate::error::Error::User(format!("{rel}: {e}")))?;
            if new != s.data {
                replace.insert(i, new);
            }
        }
        if replace.is_empty() {
            return user("No translated lines to pack yet.");
        }
        let rebuilt = pck.rewrite(&replace, key.as_ref())?;
        // Read the new pack back before it can go near the game.
        let check = Pck::parse(rebuilt.clone())?;
        for (&i, want) in &replace {
            if check.scene(i, key.as_ref())? != *want {
                return user(format!("The rebuilt Scene.pck didn't read back correctly (scene {i})."));
            }
        }
        let rel = self.pck_rel(cx)?;
        let dst = safe_join(&out, &rel)?;
        fs::create_dir_all(dst.parent().unwrap())?;
        fs::write(&dst, rebuilt)?;
        cx.log(format!("Built {rel} with {} translated scenes", replace.len()));
        Ok(out)
    }
}
