//! RPG Maker MV and MZ (and the many Japanese free games made with them).
//!
//! All text lives in plain JSON under `data/` (MZ) or `www/data/` (MV):
//! event commands in maps, common events and troops, plus the database
//! (actors, items, skills, states, system terms). Messages are written back
//! as event commands; a translation longer than the 4-line window is split
//! over extra "Show Text" commands with the same face and settings.

use super::{
    clean_dir, join_rel, list_files, read_json_messages, safe_join, write_json_messages, Ctx, Engine, Entry, Installer,
    Match,
};
use crate::error::{user, Error, Result};
use crate::protect::JP_RE;
use serde_json::{Map, Value};
use std::fs;
use std::path::{Path, PathBuf};

const SHOW_TEXT: i64 = 101;
const TEXT_LINE: i64 = 401;
const CHOICES: i64 = 102;
const WHEN: i64 = 402;
const SCROLL: i64 = 105;
const SCROLL_LINE: i64 = 405;
const ACTOR_NAME: i64 = 320;
const ACTOR_NICK: i64 = 324;
const ACTOR_PROFILE: i64 = 325;
/// Lines a message window shows at once.
const WINDOW_LINES: usize = 4;

/// Database files and their translatable fields.
const DATABASE: &[(&str, &[&str])] = &[
    ("Actors.json", &["name", "nickname", "profile"]),
    ("Classes.json", &["name"]),
    ("Skills.json", &["name", "description", "message1", "message2"]),
    ("Items.json", &["name", "description"]),
    ("Weapons.json", &["name", "description"]),
    ("Armors.json", &["name", "description"]),
    ("Enemies.json", &["name"]),
    ("States.json", &["name", "message1", "message2", "message3", "message4"]),
];
/// Fields that are names (translated with the name glossary).
const NAME_FIELDS: &[(&str, &str)] = &[("Actors.json", "name")];

fn data_dir(game: &Path) -> Option<(String, &'static str)> {
    if game.join("www/data/System.json").is_file() {
        return Some(("www/data/".into(), "RPG Maker MV"));
    }
    if game.join("data/System.json").is_file() && game.join("js").is_dir() {
        let mz = game.join("js/rmmz_core.js").is_file();
        return Some(("data/".into(), if mz { "RPG Maker MZ" } else { "RPG Maker MV" }));
    }
    None
}

pub fn detect(game_dir: &Path) -> Option<Match> {
    let (dir, name) = data_dir(game_dir)?;
    let mut d = Map::new();
    d.insert("data_dir".into(), Value::from(dir));
    Some(Match::new(
        "rpgmaker",
        name,
        0.95,
        d,
        "auto",
        "Messages, choices, and database text (items, skills, actors, system terms) are translated. \
         Text inside plugins and images stays in Japanese.",
    ))
}

fn is_jp(s: &str) -> bool {
    JP_RE.is_match(s)
}

/// Walks a data file. In export mode it collects entries; in apply mode it
/// takes them in the same order and writes them back.
struct Walk<'a> {
    out: Vec<Entry>,
    input: Option<&'a [Entry]>,
    k: usize,
    mz: bool,
}

impl Walk<'_> {
    /// One translatable string. Returns the replacement in apply mode.
    fn take(&mut self, text: &str, name: Option<&str>, is_name: bool) -> Option<String> {
        match self.input {
            None => {
                let name = if is_name { Some(text.to_string()) } else { name.map(String::from) };
                self.out.push(Entry { name, names: None, message: text.to_string() });
                None
            }
            Some(es) => {
                let e = es.get(self.k)?;
                self.k += 1;
                Some(e.message.clone()).filter(|m| m != text)
            }
        }
    }

    fn string_field(&mut self, obj: &mut Value, key: &str, is_name: bool) {
        let Some(s) = obj.get(key).and_then(Value::as_str).map(String::from) else { return };
        if !is_jp(&s) {
            return;
        }
        if let Some(new) = self.take(&s, None, is_name) {
            obj[key] = Value::from(new);
        }
    }

    fn string_array(&mut self, arr: &mut Value) {
        let Some(items) = arr.as_array_mut() else { return };
        for it in items.iter_mut() {
            if let Some(s) = it.as_str().map(String::from) {
                if is_jp(&s) {
                    if let Some(new) = self.take(&s, None, false) {
                        *it = Value::from(new);
                    }
                }
            }
        }
    }

    /// An event command list.
    fn list(&mut self, list: &mut Value) {
        let Some(cmds) = list.as_array() else { return };
        let cmds = cmds.clone();
        let mut out: Vec<Value> = Vec::with_capacity(cmds.len());
        // Choices per indent, for the "When ..." branches that repeat them.
        let mut choices: std::collections::HashMap<i64, Vec<Value>> = Default::default();
        let mut i = 0;
        while i < cmds.len() {
            let c = &cmds[i];
            let code = c.get("code").and_then(Value::as_i64).unwrap_or(0);
            let indent = c.get("indent").and_then(Value::as_i64).unwrap_or(0);
            match code {
                SHOW_TEXT | SCROLL => {
                    let line_code = if code == SHOW_TEXT { TEXT_LINE } else { SCROLL_LINE };
                    let mut j = i + 1;
                    let mut lines = Vec::new();
                    while j < cmds.len() && cmds[j].get("code").and_then(Value::as_i64) == Some(line_code) {
                        lines.push(cmds[j]["parameters"][0].as_str().unwrap_or("").to_string());
                        j += 1;
                    }
                    let text = lines.join("\n");
                    let mut header = c.clone();
                    // MZ: the speaker name is the 5th parameter.
                    let speaker = (code == SHOW_TEXT && self.mz)
                        .then(|| header["parameters"].get(4).and_then(Value::as_str).map(String::from))
                        .flatten()
                        .filter(|s| !s.is_empty());
                    if let Some(sp) = &speaker {
                        if is_jp(sp) {
                            // The name box: one name entry (glossary English).
                            if let Some(new) = self.take(sp, None, true) {
                                header["parameters"][4] = Value::from(new);
                            }
                        }
                    }
                    let new = if is_jp(&text) { self.take(&text, speaker.as_deref(), false) } else { None };
                    match new {
                        None => {
                            out.push(header);
                            out.extend(cmds[i + 1..j].iter().cloned());
                        }
                        Some(new) => {
                            let tpl = cmds.get(i + 1).cloned().unwrap_or_else(
                                || serde_json::json!({"code": line_code, "indent": indent, "parameters": [""]}),
                            );
                            let new_lines: Vec<&str> = new.split('\n').collect();
                            let per = if code == SHOW_TEXT { WINDOW_LINES } else { usize::MAX };
                            for (n, chunk) in new_lines.chunks(per).enumerate() {
                                if n == 0 || code == SHOW_TEXT {
                                    out.push(header.clone());
                                }
                                for l in chunk {
                                    let mut t = tpl.clone();
                                    t["code"] = Value::from(line_code);
                                    t["parameters"] = Value::from(vec![Value::from(*l)]);
                                    out.push(t);
                                }
                            }
                        }
                    }
                    i = j;
                    continue;
                }
                CHOICES => {
                    let mut c = c.clone();
                    self.string_array(&mut c["parameters"][0]);
                    choices.insert(indent, c["parameters"][0].as_array().cloned().unwrap_or_default());
                    out.push(c);
                }
                WHEN => {
                    let mut c = c.clone();
                    let idx = c["parameters"][0].as_u64().unwrap_or(u64::MAX) as usize;
                    if let Some(v) = choices.get(&indent).and_then(|ch| ch.get(idx)) {
                        if c["parameters"].get(1).is_some() {
                            c["parameters"][1] = v.clone();
                        }
                    }
                    out.push(c);
                }
                ACTOR_NAME | ACTOR_NICK | ACTOR_PROFILE => {
                    let mut c = c.clone();
                    if let Some(s) = c["parameters"].get(1).and_then(Value::as_str).map(String::from) {
                        if is_jp(&s) {
                            if let Some(new) = self.take(&s, None, code == ACTOR_NAME) {
                                c["parameters"][1] = Value::from(new);
                            }
                        }
                    }
                    out.push(c);
                }
                _ => out.push(c.clone()),
            }
            i += 1;
        }
        if self.input.is_some() {
            *list = Value::from(out);
        }
    }

    fn pages(&mut self, pages: &mut Value) {
        if let Some(ps) = pages.as_array_mut() {
            for p in ps.iter_mut() {
                if let Some(l) = p.get_mut("list") {
                    self.list(l);
                }
            }
        }
    }

    fn file(&mut self, name: &str, v: &mut Value) {
        let base = name.rsplit('/').next().unwrap_or(name);
        if base.starts_with("Map") && base != "MapInfos.json" {
            self.string_field(v, "displayName", false);
            if let Some(events) = v.get_mut("events").and_then(Value::as_array_mut) {
                for e in events.iter_mut().filter(|e| !e.is_null()) {
                    if let Some(p) = e.get_mut("pages") {
                        self.pages(p);
                    }
                }
            }
        } else if base == "CommonEvents.json" {
            if let Some(es) = v.as_array_mut() {
                for e in es.iter_mut().filter(|e| !e.is_null()) {
                    if let Some(l) = e.get_mut("list") {
                        self.list(l);
                    }
                }
            }
        } else if base == "Troops.json" {
            if let Some(ts) = v.as_array_mut() {
                for t in ts.iter_mut().filter(|e| !e.is_null()) {
                    if let Some(p) = t.get_mut("pages") {
                        self.pages(p);
                    }
                }
            }
        } else if base == "System.json" {
            self.string_field(v, "gameTitle", false);
            self.string_field(v, "currencyUnit", false);
            for k in ["elements", "skillTypes", "weaponTypes", "armorTypes", "equipTypes"] {
                if let Some(a) = v.get_mut(k) {
                    self.string_array(a);
                }
            }
            if let Some(terms) = v.get_mut("terms") {
                for k in ["basic", "commands", "params"] {
                    if let Some(a) = terms.get_mut(k) {
                        self.string_array(a);
                    }
                }
                if let Some(msgs) = terms.get_mut("messages") {
                    let keys: Vec<String> = msgs.as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default();
                    for k in keys {
                        self.string_field(msgs, &k, false);
                    }
                }
            }
        } else if let Some((_, fields)) = DATABASE.iter().find(|(f, _)| *f == base) {
            if let Some(items) = v.as_array_mut() {
                for it in items.iter_mut().filter(|e| !e.is_null()) {
                    for f in fields.iter() {
                        let is_name = NAME_FIELDS.contains(&(base, f));
                        self.string_field(it, f, is_name);
                    }
                }
            }
        }
    }
}

fn wanted(name: &str) -> bool {
    let base = name.rsplit('/').next().unwrap_or(name);
    (base.starts_with("Map") && base != "MapInfos.json")
        || ["CommonEvents.json", "Troops.json", "System.json"].contains(&base)
        || DATABASE.iter().any(|(f, _)| *f == base)
}

fn parse(bytes: &[u8], name: &str) -> Result<Value> {
    let b = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    serde_json::from_slice(b).map_err(|e| Error::User(format!("{name} isn't valid JSON ({e}).")))
}

/// The entries of one data file.
pub fn export(name: &str, data: &Value, mz: bool) -> Vec<Entry> {
    let mut w = Walk { out: vec![], input: None, k: 0, mz };
    let mut v = data.clone();
    w.file(name, &mut v);
    w.out
}

/// Apply entries (same order as [`export`]) to a data file.
pub fn apply(name: &str, data: &Value, entries: &[Entry], mz: bool) -> Result<Value> {
    let n = export(name, data, mz).len();
    if n != entries.len() {
        return user(format!(
            "{name} changed since it was read ({n} vs {} lines). Read the scripts again.",
            entries.len()
        ));
    }
    let mut w = Walk { out: vec![], input: Some(entries), k: 0, mz };
    let mut v = data.clone();
    w.file(name, &mut v);
    Ok(v)
}

pub struct RpgMaker {
    pub m: Match,
}

impl RpgMaker {
    fn dir(&self, cx: &Ctx) -> String {
        data_dir(&cx.game()).map(|d| d.0).or_else(|| self.m.str("data_dir").map(String::from)).unwrap_or("data/".into())
    }
    fn mz(&self) -> bool {
        self.m.name.contains("MZ")
    }
}

impl Engine for RpgMaker {
    fn m(&self) -> &Match {
        &self.m
    }
    fn m_mut(&mut self) -> &mut Match {
        &mut self.m
    }
    fn encoding(&self) -> String {
        "utf-8".into()
    }
    fn straight_quotes(&self) -> bool {
        false
    }

    fn prepare(&mut self, cx: &Ctx) -> Result<PathBuf> {
        let game = cx.game();
        let dir = self.dir(cx);
        let inst = Installer::new(cx.project);
        let scripts = cx.project.work("scripts");
        let json = cx.project.work("json");
        clean_dir(&scripts)?;
        clean_dir(&json)?;
        let mut lines = 0;
        let mut files = 0;
        for rel in list_files(&join_rel(&game, &dir), Some(&[".json"])) {
            if rel.contains('/') || !wanted(&rel) {
                continue;
            }
            let full = format!("{dir}{rel}");
            let bytes = fs::read(inst.original(&game, &full)?)?;
            let v = parse(&bytes, &rel)?;
            fs::write(safe_join(&scripts, &rel)?, &bytes)?;
            files += 1;
            let entries = export(&rel, &v, self.mz());
            if !entries.is_empty() {
                lines += entries.len();
                write_json_messages(&safe_join(&json, &format!("{rel}.json"))?, &entries)?;
            }
        }
        if files == 0 {
            return user("This RPG Maker game's data folder has no data files. If the game is a single .exe, its data is packed and Yakusu can't read it.");
        }
        self.m.set("data_dir", dir);
        cx.log(format!("Found {lines} lines in {files} data files"));
        Ok(json)
    }

    fn build(&mut self, cx: &Ctx) -> Result<PathBuf> {
        let dir = self.dir(cx);
        let scripts = cx.project.work("scripts");
        let tl = cx.project.work("json_tl");
        let out = cx.project.work("out");
        clean_dir(&out)?;
        let mut changed = 0;
        for jrel in list_files(&tl, Some(&[".json"])) {
            let rel = &jrel[..jrel.len() - 5];
            let bytes = fs::read(safe_join(&scripts, rel)?)?;
            let v = parse(&bytes, rel)?;
            let new = apply(rel, &v, &read_json_messages(&join_rel(&tl, &jrel))?, self.mz())?;
            if new != v {
                let dst = safe_join(&out, &format!("{dir}{rel}"))?;
                fs::create_dir_all(dst.parent().unwrap())?;
                fs::write(dst, serde_json::to_vec(&new)?)?;
                changed += 1;
            }
        }
        if changed == 0 {
            return user("No translated lines to pack yet.");
        }
        cx.log(format!("Wrote {changed} translated data files"));
        Ok(out)
    }
}
