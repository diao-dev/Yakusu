//! TyranoScript / TyranoBuilder games (a huge share of Japanese free and
//! doujin VNs).
//!
//! Scripts are KAG-style `.ks` files under `data/scenario/`, either loose in
//! the game folder, or packed: in `resources/app.asar` (Electron builds), in
//! `package.nw`, or in a zip appended to the game's .exe (NW.js builds).
//! Text goes through the same `.ks` reader as Kirikiri, with TyranoScript's
//! `#name` speaker lines. Choice buttons (`[glink text=...]`), on-screen
//! text (`[ptext]`, `[mtext]`) and character display names (`[chara_new
//! jname=...]`) are translated too.

use super::ks::{self, Dialect};
use super::{
    asar, clean_dir, join_rel, read_json_messages, safe_join, write_json_messages, zipped, Ctx, Engine, Entry,
    Installer, Match,
};
use crate::error::{user, Result};
use regex::Regex;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

const SCENARIO: &str = "data/scenario/";

static CHARA_ID_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"[\[@]chara_\w+\b[^\]\n]*?\bname\s*=\s*"?([^"\s\]]+)"#).unwrap());
/// Attributes shown on screen: (tag, attribute).
static ATTR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)[\[@](glink|ptext|mtext|chara_new|button)\b[^\]\n]*?\b(text|jname)\s*=\s*"([^"\n]*)""#).unwrap()
});
static SCRIPT_START: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^\s*(@iscript|\[iscript\])").unwrap());
static SCRIPT_END: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^\s*(@endscript|\[endscript\])").unwrap());

#[derive(Clone, Debug, PartialEq)]
enum Pack {
    /// Loose files under the game folder.
    Folder,
    Asar,
    Zip,
}

#[derive(Clone, Debug)]
struct Loc {
    pack: Pack,
    /// The container, relative to the game folder ("" for loose files).
    rel: String,
    /// Where the game's root is inside it ("" or e.g. "resources/app/").
    root: String,
}

fn is_dir(p: &Path) -> bool {
    p.is_dir()
}

fn has_ks(names: &[String], root: &str) -> bool {
    let pre = format!("{root}{SCENARIO}");
    names.iter().any(|n| n.starts_with(&pre) && n.to_lowercase().ends_with(".ks"))
}

/// Where this game's TyranoScript scenario is, if it is one.
fn locate(game: &Path) -> Option<Loc> {
    // Loose: the game folder itself, an unpacked Electron app, or a
    // package.nw folder. TyranoScript ships its engine in tyrano/.
    for root in ["", "resources/app/", "package.nw/", "www/"] {
        let base = join_rel(game, root);
        if is_dir(&base.join("data/scenario"))
            && (is_dir(&base.join("tyrano")) || base.join("data/system/Config.tjs").is_file())
        {
            return Some(Loc { pack: Pack::Folder, rel: String::new(), root: root.into() });
        }
    }
    let asar = game.join("resources/app.asar");
    if asar.is_file() && asar::is_asar(&asar) {
        if let Ok(a) = asar::Asar::open(&asar) {
            let names: Vec<String> = a.files.iter().map(|f| f.name.clone()).collect();
            if has_ks(&names, "") {
                return Some(Loc { pack: Pack::Asar, rel: "resources/app.asar".into(), root: String::new() });
            }
        }
    }
    let mut zips: Vec<String> = vec!["package.nw".into()];
    if let Ok(rd) = fs::read_dir(game) {
        let mut exes: Vec<String> = rd
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.to_lowercase().ends_with(".exe"))
            .collect();
        exes.sort();
        zips.extend(exes);
    }
    for z in zips {
        let p = game.join(&z);
        if p.is_file() && zipped::is_zip(&p) && zipped::names(&p).is_ok_and(|n| has_ks(&n, "")) {
            return Some(Loc { pack: Pack::Zip, rel: z, root: String::new() });
        }
    }
    None
}

pub fn detect(game_dir: &Path) -> Option<Match> {
    let loc = locate(game_dir)?;
    let mut d = Map::new();
    d.insert("pack".into(), Value::from(format!("{:?}", loc.pack)));
    d.insert("container".into(), Value::from(loc.rel.clone()));
    d.insert("root".into(), Value::from(loc.root.clone()));
    Some(Match::new("tyrano", "TyranoScript", 0.95, d, "auto", ""))
}

/// Scenario files: inner name -> bytes, from the original game files.
fn read_scenario(cx: &Ctx, loc: &Loc) -> Result<BTreeMap<String, Vec<u8>>> {
    let game = cx.game();
    let inst = Installer::new(cx.project);
    let mut out = BTreeMap::new();
    let wanted = |n: &str| n.starts_with(&format!("{}{SCENARIO}", loc.root)) && n.to_lowercase().ends_with(".ks");
    match loc.pack {
        Pack::Folder => {
            let dir = join_rel(&game, &format!("{}{SCENARIO}", loc.root));
            for rel in super::list_files(&dir, Some(&[".ks"])) {
                let name = format!("{}{SCENARIO}{rel}", loc.root);
                out.insert(name.clone(), fs::read(inst.original(&game, &name)?)?);
            }
        }
        Pack::Asar => {
            let a = asar::Asar::open(&inst.original(&game, &loc.rel)?)?;
            for f in a.files.iter().filter(|f| wanted(&f.name)) {
                out.insert(f.name.clone(), a.read(f)?);
            }
        }
        Pack::Zip => {
            let path = inst.original(&game, &loc.rel)?;
            for n in zipped::names(&path)?.into_iter().filter(|n| wanted(n)) {
                out.insert(n.clone(), zipped::read(&path, &n)?);
            }
        }
    }
    Ok(out)
}

fn in_script_mask(text: &str) -> Vec<bool> {
    let mut inside = false;
    text.split('\n')
        .map(|l| {
            if inside {
                if SCRIPT_END.is_match(l) {
                    inside = false;
                }
                true
            } else if SCRIPT_START.is_match(l) {
                inside = true;
                true
            } else {
                false
            }
        })
        .collect()
}

/// On-screen attribute values, in order: (line, byte range of the value, is a name).
fn attrs(text: &str) -> Vec<(usize, std::ops::Range<usize>, bool)> {
    let mask = in_script_mask(text);
    let mut out = Vec::new();
    for (i, line) in text.split('\n').enumerate() {
        if mask[i] {
            continue;
        }
        for c in ATTR_RE.captures_iter(line) {
            let tag = c[1].to_lowercase();
            let attr = c[2].to_lowercase();
            if (tag == "chara_new") != (attr == "jname") {
                continue;
            }
            let v = c.get(3).unwrap();
            if crate::protect::JP_RE.is_match(v.as_str()) {
                out.push((i, v.range(), tag == "chara_new"));
            }
        }
    }
    out
}

pub fn export(text: &str, d: &Dialect) -> Vec<Entry> {
    let mut entries = ks::export_entries_with(text, d);
    let lines: Vec<&str> = text.split('\n').collect();
    for (i, r, is_name) in attrs(text) {
        let v = lines[i][r].to_string();
        entries.push(Entry { name: is_name.then(|| v.clone()), names: None, message: v });
    }
    entries
}

/// Text that can sit inside a "quoted" attribute.
fn attr_safe(s: &str) -> String {
    s.replace('"', "'").replace(['[', ']'], "").replace('\n', " ")
}

pub fn apply(text: &str, entries: &[Entry], d: &Dialect) -> Result<String> {
    let n_units = ks::parse_with(text, d).len();
    let found = attrs(text);
    if entries.len() != n_units + found.len() {
        return user(format!(
            "The script changed since it was read ({} vs {} lines). Read the scripts again.",
            n_units + found.len(),
            entries.len()
        ));
    }
    let new = ks::apply_entries_with(text, &entries[..n_units], d)?;
    // Command lines aren't touched by the text pass, so the attributes are
    // where they were.
    let mut lines: Vec<String> = new.split('\n').map(String::from).collect();
    let mut by_line: BTreeMap<usize, Vec<(std::ops::Range<usize>, String)>> = BTreeMap::new();
    for ((i, r, _), e) in found.into_iter().zip(&entries[n_units..]) {
        by_line.entry(i).or_default().push((r, attr_safe(&e.message)));
    }
    for (i, mut reps) in by_line {
        reps.sort_by_key(|(r, _)| std::cmp::Reverse(r.start));
        for (r, v) in reps {
            lines[i].replace_range(r, &v);
        }
    }
    Ok(lines.join("\n"))
}

pub struct Tyrano {
    pub m: Match,
}

impl Tyrano {
    fn loc(&self, cx: &Ctx) -> Result<Loc> {
        if let Some(l) = locate(&cx.game()) {
            return Ok(l);
        }
        let pack = match self.m.str("pack") {
            Some("Asar") => Pack::Asar,
            Some("Zip") => Pack::Zip,
            _ => Pack::Folder,
        };
        Ok(Loc {
            pack,
            rel: self.m.str("container").unwrap_or("").into(),
            root: self.m.str("root").unwrap_or("").into(),
        })
    }

    fn dialect(&self) -> Dialect {
        Dialect { hash_names: true, fixed_names: self.m.strings("chara_ids").into_iter().collect() }
    }
}

impl Engine for Tyrano {
    fn m(&self) -> &Match {
        &self.m
    }
    fn m_mut(&mut self) -> &mut Match {
        &mut self.m
    }
    fn encoding(&self) -> String {
        "utf-8".into()
    }
    fn line_break_for(&self, _rel: &str) -> &'static str {
        "[r]"
    }
    fn straight_quotes(&self) -> bool {
        false
    }

    fn prepare(&mut self, cx: &Ctx) -> Result<PathBuf> {
        let loc = self.loc(cx)?;
        let files = read_scenario(cx, &loc)?;
        if files.is_empty() {
            return user("No TyranoScript scenario files (data/scenario/*.ks) were found.");
        }
        let scripts = cx.project.work("scripts");
        let json = cx.project.work("json");
        clean_dir(&scripts)?;
        clean_dir(&json)?;
        let mut ids: HashSet<String> = HashSet::new();
        let mut texts = Vec::new();
        for (name, data) in &files {
            let dst = safe_join(&scripts, name)?;
            fs::create_dir_all(dst.parent().unwrap())?;
            fs::write(&dst, data)?;
            let (text, _) = ks::decode(data);
            ids.extend(CHARA_ID_RE.captures_iter(&text).map(|c| c[1].to_string()));
            texts.push((name, text));
        }
        let mut ids: Vec<String> = ids.into_iter().collect();
        ids.sort();
        self.m.set("chara_ids", ids);
        self.m.set("pack", format!("{:?}", loc.pack));
        self.m.set("container", loc.rel.clone());
        self.m.set("root", loc.root.clone());
        let d = self.dialect();
        let mut lines = 0;
        for (name, text) in texts {
            let entries = export(&text, &d);
            if !entries.is_empty() {
                lines += entries.len();
                write_json_messages(&safe_join(&json, &format!("{name}.json"))?, &entries)?;
            }
        }
        cx.log(format!("Found {lines} lines in {} scenario files", files.len()));
        Ok(json)
    }

    fn build(&mut self, cx: &Ctx) -> Result<PathBuf> {
        let loc = self.loc(cx)?;
        let scripts = cx.project.work("scripts");
        let tl = cx.project.work("json_tl");
        let out = cx.project.work("out");
        clean_dir(&out)?;
        let d = self.dialect();
        let mut replace: HashMap<String, Vec<u8>> = HashMap::new();
        for jrel in super::list_files(&tl, Some(&[".json"])) {
            let name = &jrel[..jrel.len() - 5];
            let orig = fs::read(safe_join(&scripts, name)?)?;
            let (text, enc) = ks::decode(&orig);
            let new = ks::encode(&apply(&text, &read_json_messages(&join_rel(&tl, &jrel))?, &d)?, enc);
            if new != orig {
                replace.insert(name.to_string(), new);
            }
        }
        if replace.is_empty() {
            return user("No translated lines to pack yet.");
        }
        let game_orig = |rel: &str| Installer::new(cx.project).original(&cx.game(), rel);
        match loc.pack {
            Pack::Folder => {
                for (name, bytes) in &replace {
                    let dst = safe_join(&out, name)?;
                    fs::create_dir_all(dst.parent().unwrap())?;
                    fs::write(dst, bytes)?;
                }
                cx.log(format!("Wrote {} translated scenario files", replace.len()));
            }
            Pack::Asar => {
                let dst = safe_join(&out, &loc.rel)?;
                fs::create_dir_all(dst.parent().unwrap())?;
                asar::Asar::open(&game_orig(&loc.rel)?)?.rewrite(&replace, &dst)?;
                cx.log(format!("Built {} with {} translated scenario files", loc.rel, replace.len()));
            }
            Pack::Zip => {
                let dst = safe_join(&out, &loc.rel)?;
                fs::create_dir_all(dst.parent().unwrap())?;
                zipped::rewrite(&game_orig(&loc.rel)?, &replace, &dst)?;
                cx.log(format!("Built {} with {} translated scenario files", loc.rel, replace.len()));
            }
        }
        Ok(out)
    }
}
