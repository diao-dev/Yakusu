//! RPG Maker XP, VX and VX Ace: Ruby Marshal data files, loose in `Data/`
//! or inside the encrypted `Game.rgssad` / `Game.rgss2a` / `Game.rgss3a`.
//!
//! Text is in event commands (messages, choices, scrolling text, actor
//! name changes) and the database (actors, items, skills, states, system
//! terms), the same way as in MV/MZ. Messages longer than the 4-line window
//! are split over extra "Show Text" commands.

pub mod archive;
pub mod marshal;

use super::{
    clean_dir, join_rel, list_files, read_json_messages, safe_join, write_json_messages, Ctx, Engine, Entry, Installer,
    Match,
};
use crate::error::{user, Error, Result};
use crate::protect::JP_RE;
use marshal::{class, int, ivar, ivar_mut, set_text, text, V};
use serde_json::{Map, Value};
use std::collections::HashMap;
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
const WINDOW_LINES: usize = 4;

const ARCHIVES: &[(&str, &str, &str)] = &[
    ("Game.rgss3a", "rvdata2", "RPG Maker VX Ace"),
    ("Game.rgss2a", "rvdata", "RPG Maker VX"),
    ("Game.rgssad", "rxdata", "RPG Maker XP"),
];

const DB_FIELDS: &[(&str, &[&str])] = &[
    ("RPG::Actor", &["@name", "@nickname", "@description"]),
    ("RPG::Class", &["@name"]),
    ("RPG::Skill", &["@name", "@description", "@message1", "@message2"]),
    ("RPG::Item", &["@name", "@description"]),
    ("RPG::Weapon", &["@name", "@description"]),
    ("RPG::Armor", &["@name", "@description"]),
    ("RPG::Enemy", &["@name"]),
    ("RPG::State", &["@name", "@message1", "@message2", "@message3", "@message4"]),
    ("RPG::Map", &["@display_name"]),
    (
        "RPG::System",
        &[
            "@game_title",
            "@currency_unit",
            "@elements",
            "@skill_types",
            "@weapon_types",
            "@armor_types",
            "@terms",
            "@words",
        ],
    ),
];

/// (archive or "", data extension, engine name)
fn find(game: &Path) -> Option<(String, &'static str, &'static str)> {
    for (arc, ext, name) in ARCHIVES {
        if game.join(arc).is_file() && archive::is_archive(&game.join(arc)) {
            return Some((arc.to_string(), ext, name));
        }
    }
    for (_, ext, name) in ARCHIVES {
        if game.join(format!("Data/System.{ext}")).is_file() {
            return Some((String::new(), ext, name));
        }
    }
    None
}

pub fn detect(game_dir: &Path) -> Option<Match> {
    let (arc, ext, name) = find(game_dir)?;
    let mut d = Map::new();
    d.insert("archive".into(), Value::from(arc));
    d.insert("ext".into(), Value::from(ext));
    Some(Match::new(
        "rgss",
        name,
        0.95,
        d,
        "auto",
        "Messages, choices, and database text (items, skills, actors, system terms) are translated. Text drawn by \
         scripts or images stays in Japanese.",
    ))
}

fn is_jp(s: &str) -> bool {
    JP_RE.is_match(s)
}

struct Walk<'a> {
    syms: &'a [String],
    out: Vec<Entry>,
    input: Option<&'a [Entry]>,
    k: usize,
    xp: bool,
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

    /// A string node, translated in place.
    fn string(&mut self, v: &mut V, is_name: bool) {
        if let Some(s) = text(v) {
            if is_jp(&s) {
                if let Some(new) = self.take(&s, is_name) {
                    set_text(v, &new);
                }
            }
        }
    }

    /// Strings inside a value (arrays and objects, recursively).
    fn strings_in(&mut self, v: &mut V) {
        match v {
            V::Str(_) | V::Ivar(..) => self.string(v, false),
            V::Array(items) => items.iter_mut().for_each(|i| self.strings_in(i)),
            V::Object(_, iv) => iv.iter_mut().for_each(|(_, x)| self.strings_in(x)),
            _ => {}
        }
    }

    fn code(&self, c: &V) -> i64 {
        ivar(c, "@code", self.syms).and_then(int).unwrap_or(0)
    }

    fn params<'b>(&self, c: &'b mut V) -> Option<&'b mut Vec<V>> {
        match ivar_mut(c, "@parameters", self.syms)? {
            V::Array(p) => Some(p),
            _ => None,
        }
    }

    fn param_text(&self, c: &V, i: usize) -> Option<String> {
        match ivar(c, "@parameters", self.syms)? {
            V::Array(p) => p.get(i).and_then(text),
            _ => None,
        }
    }

    /// Set the text of a one-line command (its first parameter).
    fn set_line(&self, c: &mut V, line: &str) {
        if let Some(p) = self.params(c) {
            if let Some(s) = p.first_mut() {
                set_text(s, line);
            }
        }
    }

    /// A new event command like `tpl` with code `code` and text `line`.
    fn line_cmd(&self, tpl: &V, code: i64, line: &str) -> V {
        let mut c = tpl.clone();
        if let Some(x) = ivar_mut(&mut c, "@code", self.syms) {
            *x = V::Int(code);
        }
        if let Some(p) = self.params(&mut c) {
            let mut s = p.first().cloned().filter(|s| text(s).is_some()).unwrap_or(V::Str(vec![]));
            set_text(&mut s, line);
            *p = vec![s];
        }
        V::Fresh(Box::new(c))
    }

    fn list(&mut self, cmds: &mut Vec<V>) {
        let orig = std::mem::take(cmds);
        let mut out: Vec<V> = Vec::with_capacity(orig.len());
        let mut choices: HashMap<i64, Vec<String>> = HashMap::new();
        let mut i = 0;
        while i < orig.len() {
            let code = self.code(&orig[i]);
            let indent = ivar(&orig[i], "@indent", self.syms).and_then(int).unwrap_or(0);
            if code == SHOW_TEXT || code == SCROLL {
                let line_code = if code == SHOW_TEXT { TEXT_LINE } else { SCROLL_LINE };
                // XP: "Show Text" carries the first line itself.
                let header_has_text = self.xp && code == SHOW_TEXT;
                let mut j = i + 1;
                let mut lines = Vec::new();
                if header_has_text {
                    lines.push(self.param_text(&orig[i], 0).unwrap_or_default());
                }
                while j < orig.len() && self.code(&orig[j]) == line_code {
                    lines.push(self.param_text(&orig[j], 0).unwrap_or_default());
                    j += 1;
                }
                let joined = lines.join("\n");
                let new = if is_jp(&joined) { self.take(&joined, false) } else { None };
                match new {
                    None => out.extend(orig[i..j].iter().cloned()),
                    Some(new) => {
                        // Every original command is kept, in order (so Ruby's
                        // object references stay valid): lines are reused first,
                        // extra lines and windows are new commands, unused lines
                        // are emptied.
                        let header = orig[i].clone();
                        let mut spare: std::collections::VecDeque<V> = orig[i + 1..j].iter().cloned().collect();
                        let line_tpl = spare.front().cloned().unwrap_or_else(|| header.clone());
                        let new_lines: Vec<&str> = new.split('\n').collect();
                        let per = if code == SHOW_TEXT { WINDOW_LINES } else { usize::MAX };
                        for (n, chunk) in new_lines.chunks(per).enumerate() {
                            let mut rest = chunk;
                            let mut h = header.clone();
                            if header_has_text {
                                self.set_line(&mut h, chunk[0]);
                                rest = &chunk[1..];
                            }
                            out.push(if n == 0 { h } else { V::Fresh(Box::new(h)) });
                            for l in rest {
                                match spare.pop_front() {
                                    Some(mut c) => {
                                        self.set_line(&mut c, l);
                                        out.push(c);
                                    }
                                    None => out.push(self.line_cmd(&line_tpl, line_code, l)),
                                }
                            }
                        }
                        for mut c in spare {
                            self.set_line(&mut c, "");
                            out.push(c);
                        }
                    }
                }
                i = j;
                continue;
            }
            let mut c = orig[i].clone();
            match code {
                CHOICES => {
                    if let Some(p) = self.params(&mut c) {
                        if let Some(V::Array(items)) = p.get_mut(0) {
                            let mut texts = Vec::new();
                            for it in items.iter_mut() {
                                self.string(it, false);
                                texts.push(text(it).unwrap_or_default());
                            }
                            choices.insert(indent, texts);
                        }
                    }
                }
                WHEN => {
                    let idx = self.params(&mut c).and_then(|p| p.first().and_then(int)).unwrap_or(-1);
                    if let Some(t) = choices.get(&indent).and_then(|ch| ch.get(idx as usize)).cloned() {
                        if let Some(p) = self.params(&mut c) {
                            if let Some(s) = p.get_mut(1) {
                                set_text(s, &t);
                            }
                        }
                    }
                }
                ACTOR_NAME | ACTOR_NICK | ACTOR_PROFILE => {
                    if let Some(p) = self.params(&mut c) {
                        if let Some(s) = p.get_mut(1) {
                            self.string(s, code == ACTOR_NAME);
                        }
                    }
                }
                _ => {}
            }
            out.push(c);
            i += 1;
        }
        *cmds = out;
    }

    fn is_command_list(&self, items: &[V]) -> bool {
        !items.is_empty() && items.iter().all(|c| class(c, self.syms) == Some("RPG::EventCommand"))
    }

    /// Walk a whole data file.
    fn value(&mut self, v: &mut V) {
        match v {
            V::Array(items) => {
                if self.is_command_list(items) {
                    self.list(items);
                } else {
                    items.iter_mut().for_each(|i| self.value(i));
                }
            }
            V::Hash(pairs, _) => pairs.iter_mut().for_each(|(_, x)| self.value(x)),
            V::Object(..) => {
                let cls = class(v, self.syms).unwrap_or("").to_string();
                if let Some((_, fields)) = DB_FIELDS.iter().find(|(c, _)| *c == cls) {
                    for f in fields.iter() {
                        let is_name = cls == "RPG::Actor" && *f == "@name";
                        if let Some(x) = ivar_mut(v, f, self.syms) {
                            if is_name {
                                self.string(x, true);
                            } else {
                                self.strings_in(x);
                            }
                        }
                    }
                }
                // Events, pages and lists live in other fields; RPG::System
                // and the database have nothing else to translate.
                if cls != "RPG::System" {
                    if let V::Object(_, iv) = v {
                        for (k, x) in iv.iter_mut() {
                            let name = marshal::sym(k, self.syms).unwrap_or("");
                            if ["@events", "@pages", "@list"].contains(&name) || matches!(x, V::Hash(..)) {
                                self.value(x);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

pub fn parse(bytes: &[u8], name: &str) -> Result<(V, Vec<String>)> {
    marshal::load(bytes).map_err(|e| Error::User(format!("{name} can't be read ({e}).")))
}

pub fn export(v: &V, syms: &[String], xp: bool) -> Vec<Entry> {
    let mut w = Walk { syms, out: vec![], input: None, k: 0, xp };
    let mut c = v.clone();
    w.value(&mut c);
    w.out
}

pub fn apply(v: &V, syms: &[String], entries: &[Entry], xp: bool) -> Result<V> {
    let n = export(v, syms, xp).len();
    if n != entries.len() {
        return user(format!(
            "A data file changed since it was read ({n} vs {} lines). Read the scripts again.",
            entries.len()
        ));
    }
    let mut w = Walk { syms, out: vec![], input: Some(entries), k: 0, xp };
    let mut c = v.clone();
    w.value(&mut c);
    Ok(c)
}

fn wanted(name: &str, ext: &str) -> bool {
    let base = name.rsplit('/').next().unwrap_or(name);
    let Some(stem) = base.strip_suffix(&format!(".{ext}")) else { return false };
    (stem.starts_with("Map") && stem != "MapInfos")
        || [
            "CommonEvents",
            "Troops",
            "System",
            "Actors",
            "Classes",
            "Skills",
            "Items",
            "Weapons",
            "Armors",
            "Enemies",
            "States",
        ]
        .contains(&stem)
}

pub struct Rgss {
    pub m: Match,
}

impl Rgss {
    fn ext(&self) -> &str {
        self.m.str("ext").unwrap_or("rvdata2")
    }
    fn xp(&self) -> bool {
        self.ext() == "rxdata"
    }
    /// Data files (name -> bytes) from the original game.
    fn sources(&self, cx: &Ctx) -> Result<Vec<(String, Vec<u8>)>> {
        let game = cx.game();
        let inst = Installer::new(cx.project);
        let ext = self.ext().to_string();
        let mut out = Vec::new();
        match self.m.str("archive") {
            Some(arc) => {
                let a = archive::Archive::open(&inst.original(&game, arc)?)?;
                for e in a.entries.iter().filter(|e| wanted(&e.name, &ext)) {
                    out.push((e.name.clone(), a.read(e)));
                }
            }
            None => {
                for rel in list_files(&game.join("Data"), None) {
                    let name = format!("Data/{rel}");
                    if !rel.contains('/') && wanted(&name, &ext) {
                        out.push((name.clone(), fs::read(inst.original(&game, &name)?)?));
                    }
                }
            }
        }
        Ok(out)
    }
}

impl Engine for Rgss {
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
        let json = cx.project.work("json");
        let scripts = cx.project.work("scripts");
        clean_dir(&json)?;
        clean_dir(&scripts)?;
        let files = self.sources(cx)?;
        if files.is_empty() {
            return user("No RPG Maker data files were found.");
        }
        let mut lines = 0;
        for (name, bytes) in &files {
            fs::write(safe_join(&scripts, &name.replace('/', "_"))?, bytes)?;
            let (v, syms) = parse(bytes, name)?;
            let es = export(&v, &syms, self.xp());
            if !es.is_empty() {
                lines += es.len();
                write_json_messages(&safe_join(&json, &format!("{name}.json"))?, &es)?;
            }
        }
        cx.log(format!("Found {lines} lines in {} data files", files.len()));
        Ok(json)
    }

    fn build(&mut self, cx: &Ctx) -> Result<PathBuf> {
        let scripts = cx.project.work("scripts");
        let tl = cx.project.work("json_tl");
        let out = cx.project.work("out");
        clean_dir(&out)?;
        let mut replace: HashMap<String, Vec<u8>> = HashMap::new();
        for jrel in list_files(&tl, Some(&[".json"])) {
            let name = &jrel[..jrel.len() - 5];
            let bytes = fs::read(safe_join(&scripts, &name.replace('/', "_"))?)?;
            let (v, syms) = parse(&bytes, name)?;
            let new = apply(&v, &syms, &read_json_messages(&join_rel(&tl, &jrel))?, self.xp())?;
            if new != v {
                replace.insert(name.to_string(), marshal::dump(&new, &syms));
            }
        }
        if replace.is_empty() {
            return user("No translated lines to pack yet.");
        }
        match self.m.str("archive") {
            Some(arc) => {
                let a = archive::Archive::open(&Installer::new(cx.project).original(&cx.game(), arc)?)?;
                fs::write(safe_join(&out, arc)?, a.rebuild(&replace))?;
                cx.log(format!("Rebuilt {arc} with {} translated data files", replace.len()));
            }
            None => {
                for (name, bytes) in &replace {
                    let dst = safe_join(&out, name)?;
                    fs::create_dir_all(dst.parent().unwrap())?;
                    fs::write(dst, bytes)?;
                }
                cx.log(format!("Wrote {} translated data files", replace.len()));
            }
        }
        Ok(out)
    }
}
