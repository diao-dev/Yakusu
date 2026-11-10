//! Unity games (experimental): Naninovel and text kept in Unity data.
//!
//! Yakusu looks in the game's asset bundles (`StreamingAssets`, including
//! Addressables' `aa/` folder) and in `resources.assets` /
//! `sharedassets*.assets` for:
//!
//! - TextAssets holding Naninovel scripts (`.nani` text): generic text
//!   lines, `@print` and `@choice` text;
//! - TextAssets holding Naninovel localization documents (`# id`, `; source`,
//!   translation lines): the Japanese source is translated and written as
//!   that locale's text, so picking that language in the game shows English;
//! - Naninovel script assets (MonoBehaviours with a `textMap`): the
//!   localizable text, found through the file's type tree.
//!
//! Bundles are rebuilt with the same CRC-32 over their data, so games that
//! check bundle CRCs still load them.

pub mod bundle;
pub mod serialized;

use super::{
    clean_dir, join_rel, list_files, read_json_messages, safe_join, write_json_messages, Ctx, Engine, Entry, Installer,
    Match,
};
use crate::error::{user, Result};
use crate::protect::JP_RE;
use bundle::Bundle;
use regex::Regex;
use serde_json::{Map, Value};
use serialized::{SerializedFile, StrField, TEXT_ASSET};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

const MONO_BEHAVIOUR: i32 = 114;
const MAX_BUNDLE: u64 = 512 << 20;
const MAX_ASSETS: u64 = 1536 << 20;

static AUTHOR_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"^([^\s:\[\]"@#;{}]{1,32}): (.*)$"#).unwrap());
static PRINT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^(\s*@(?:print|choice|title)\s+)(?:"((?:[^"\\]|\\.)*)"|([^\s"]+))(.*)$"#).unwrap());

/// One file holding Unity data: a bundle or a .assets file.
#[derive(Clone, Debug, PartialEq)]
enum Kind {
    Bundle,
    Assets,
}

fn data_dirs(game: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(game)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .filter(|e| e.file_name().to_string_lossy().ends_with("_Data"))
                .map(|e| e.path())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// Unity data files under the game folder, as (relative path, kind).
fn containers(game: &Path) -> Vec<(String, Kind)> {
    let mut out = Vec::new();
    for d in data_dirs(game) {
        let dname = d.file_name().unwrap().to_string_lossy().to_string();
        for rel in list_files(&d.join("StreamingAssets"), None) {
            let p = join_rel(&d.join("StreamingAssets"), &rel);
            if fs::metadata(&p).is_ok_and(|m| m.len() <= MAX_BUNDLE) && bundle::is_bundle(&p) {
                out.push((format!("{dname}/StreamingAssets/{rel}"), Kind::Bundle));
            }
        }
        for rel in list_files(&d, Some(&[".assets"])) {
            if rel.contains('/') {
                continue;
            }
            let low = rel.to_lowercase();
            if low == "resources.assets" || low.starts_with("sharedassets") {
                out.push((format!("{dname}/{rel}"), Kind::Assets));
            }
        }
    }
    out
}

pub fn detect(game_dir: &Path) -> Option<Match> {
    if !game_dir.join("UnityPlayer.dll").is_file() || data_dirs(game_dir).is_empty() {
        return None;
    }
    Some(Match::new(
        "unity",
        "Unity",
        0.9,
        Map::new(),
        "experimental",
        "Yakusu reads Naninovel scripts and localization text inside Unity data. Other Unity games keep their text in \
         ways that differ per game, so it may find nothing. Try 150 lines first to check it in the game.",
    ))
}

// ---------------------------------------------------------------- text formats

fn decode(b: &[u8]) -> Option<String> {
    let b = b.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(b);
    std::str::from_utf8(b).ok().map(String::from)
}

/// A Naninovel localization document: "# id" headers followed by "; source".
fn is_l10n(t: &str) -> bool {
    let lines: Vec<&str> = t.lines().collect();
    let pairs = lines.windows(2).filter(|w| w[0].starts_with("# ") && w[1].starts_with(';')).count();
    pairs >= 3 && pairs * 4 >= lines.iter().filter(|l| l.starts_with("# ")).count() * 3
}

/// A Naninovel script: commands (@), labels (#) and text lines.
fn is_nani(t: &str) -> bool {
    let lines: Vec<&str> = t.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let cmds = lines.iter().filter(|l| l.starts_with('@')).count();
    cmds >= 2 && cmds * 20 >= lines.len() && JP_RE.is_match(t) && !is_l10n(t)
}

#[derive(Clone, Debug, PartialEq)]
enum Format {
    Nani,
    L10n,
    TextMap,
}

impl Format {
    fn tag(&self) -> &'static str {
        match self {
            Format::Nani => "nani",
            Format::L10n => "l10n",
            Format::TextMap => "textmap",
        }
    }
    fn from_tag(s: &str) -> Option<Format> {
        Some(match s {
            "nani" => Format::Nani,
            "l10n" => Format::L10n,
            "textmap" => Format::TextMap,
            _ => return None,
        })
    }
}

/// Naninovel script: (line index, prefix kept, text, author).
fn nani_units(t: &str) -> Vec<(usize, String, String, String, Option<String>)> {
    let mut out = Vec::new();
    for (i, line) in t.split('\n').enumerate() {
        let (body, cr) = match line.strip_suffix('\r') {
            Some(b) => (b, "\r"),
            None => (line, ""),
        };
        let s = body.trim_start();
        if s.is_empty() || s.starts_with(';') || s.starts_with('#') {
            continue;
        }
        if s.starts_with('@') {
            if let Some(c) = PRINT_RE.captures(body) {
                let text = c.get(2).or(c.get(3)).map(|m| m.as_str()).unwrap_or("");
                if JP_RE.is_match(text) {
                    let quoted = c.get(2).is_some();
                    let pre = format!("{}{}", &c[1], if quoted { "\"" } else { "" });
                    let post = format!("{}{}{cr}", if quoted { "\"" } else { "" }, &c[4]);
                    out.push((i, pre, text.replace("\\\"", "\""), post, None));
                }
            }
            continue;
        }
        let indent = &body[..body.len() - s.len()];
        let (author, text) = match AUTHOR_RE.captures(s) {
            Some(c) => (Some(c[1].to_string()), c[2].to_string()),
            None => (None, s.to_string()),
        };
        if JP_RE.is_match(&text) {
            let pre = match &author {
                Some(a) => format!("{indent}{a}: "),
                None => indent.to_string(),
            };
            let name = author.map(|a| a.split('.').next().unwrap_or(&a).to_string());
            out.push((i, pre, text, cr.to_string(), name));
        }
    }
    out
}

/// Localization document blocks: (source, translation line range, author prefix to keep).
fn l10n_units(t: &str) -> Vec<(String, std::ops::Range<usize>, String)> {
    let lines: Vec<&str> = t.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if !lines[i].starts_with("# ") {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        let mut src = Vec::new();
        while j < lines.len() && lines[j].starts_with(';') {
            src.push(lines[j].trim_start_matches(';').trim_start());
            j += 1;
        }
        let start = j;
        while j < lines.len() && !lines[j].starts_with("# ") {
            j += 1;
        }
        // Trailing blank lines belong to the gap, not the translation.
        let mut end = j;
        while end > start && lines[end - 1].trim().is_empty() {
            end -= 1;
        }
        let source = src.join("\n");
        if JP_RE.is_match(&source) {
            // Keep an "Author: " prefix when the existing translation has it too.
            let keep = AUTHOR_RE
                .captures(&source)
                .map(|c| format!("{}: ", &c[1]))
                .filter(|p| lines.get(start).is_some_and(|l| l.starts_with(p.as_str())))
                .unwrap_or_default();
            let text = if keep.is_empty() { source } else { source[keep.len()..].to_string() };
            out.push((text, start..end, keep));
        }
        i = j;
    }
    out
}

fn is_textmap(f: &StrField) -> bool {
    f.path.to_lowercase().contains("textmap") && JP_RE.is_match(&f.text)
}

fn export_text(t: &str, f: &Format) -> Vec<Entry> {
    match f {
        Format::Nani => {
            nani_units(t).into_iter().map(|(_, _, text, _, name)| Entry { name, names: None, message: text }).collect()
        }
        Format::L10n => {
            l10n_units(t).into_iter().map(|(text, _, _)| Entry { name: None, names: None, message: text }).collect()
        }
        Format::TextMap => vec![],
    }
}

fn apply_text(t: &str, f: &Format, entries: &[Entry]) -> Result<String> {
    let mut lines: Vec<String> = t.split('\n').map(String::from).collect();
    match f {
        Format::Nani => {
            let units = nani_units(t);
            if units.len() != entries.len() {
                return user("A Naninovel script changed since it was read. Read the scripts again.");
            }
            for ((i, pre, text, post, _), e) in units.into_iter().zip(entries) {
                if e.message != text {
                    let msg = e.message.replace('\n', " ");
                    let msg = if pre.ends_with('"') { msg.replace('"', "\\\"") } else { msg };
                    lines[i] = format!("{pre}{msg}{post}");
                }
            }
        }
        Format::L10n => {
            let units = l10n_units(t);
            if units.len() != entries.len() {
                return user("A localization document changed since it was read. Read the scripts again.");
            }
            let cr = if t.contains("\r\n") { "\r" } else { "" };
            // Last first, so earlier line numbers stay valid.
            for ((text, range, keep), e) in units.into_iter().zip(entries).rev() {
                if e.message != text {
                    let new = format!("{keep}{}{cr}", e.message.replace('\n', " "));
                    lines.splice(range, std::iter::once(new));
                }
            }
        }
        Format::TextMap => {}
    }
    Ok(lines.join("\n"))
}

// ---------------------------------------------------------------- reading

/// One serialized file's bytes inside a container.
struct Loaded {
    /// Bundle data (all nodes), or the .assets file.
    data: Vec<u8>,
    bundle: Option<Bundle>,
}

fn load(path: &Path, kind: &Kind) -> Result<Loaded> {
    Ok(match kind {
        Kind::Bundle => {
            let b = Bundle::open(path)?;
            Loaded { data: b.data()?, bundle: Some(b) }
        }
        Kind::Assets => {
            if fs::metadata(path)?.len() > MAX_ASSETS {
                return user(format!("{} is too large for Yakusu to rebuild.", path.display()));
            }
            Loaded { data: fs::read(path)?, bundle: None }
        }
    })
}

/// Whether a container may hold text (bundles of only textures, sounds or
/// meshes are skipped without unpacking them).
fn may_have_text(path: &Path, kind: &Kind) -> bool {
    match kind {
        Kind::Assets => true,
        Kind::Bundle => Bundle::open(path)
            .and_then(|b| b.classes())
            .map(|c| c.is_empty() || c.iter().any(|&k| k == TEXT_ASSET || k == MONO_BEHAVIOUR))
            .unwrap_or(false),
    }
}

/// (node name, serialized file bytes) inside a loaded container.
fn serialized_files(l: &Loaded) -> Vec<(String, &[u8])> {
    match &l.bundle {
        Some(b) => b
            .nodes
            .iter()
            .filter_map(|n| b.node(&l.data, n).ok().map(|d| (n.path.clone(), d)))
            .filter(|(_, d)| serialized::looks_serialized(d))
            .collect(),
        None => vec![("-".to_string(), l.data.as_slice())],
    }
}

/// Translatable objects in one serialized file: (path id, format, entries).
fn scan(sf: &SerializedFile, b: &[u8]) -> Vec<(i64, Format, Vec<Entry>)> {
    let mut out = Vec::new();
    for o in &sf.objects {
        let obj = sf.object(b, o);
        if o.class_id == TEXT_ASSET {
            let Some((_, bytes, _)) = sf.text_asset(obj) else { continue };
            let Some(t) = decode(&bytes) else { continue };
            let f = if is_l10n(&t) {
                Format::L10n
            } else if is_nani(&t) {
                Format::Nani
            } else {
                continue;
            };
            let es = export_text(&t, &f);
            if !es.is_empty() {
                out.push((o.path_id, f, es));
            }
        } else if o.class_id == MONO_BEHAVIOUR {
            let Some(strs) = sf.strings(obj, o) else { continue };
            let es: Vec<Entry> = strs
                .iter()
                .filter(|s| is_textmap(s))
                .map(|s| Entry { name: None, names: None, message: s.text.clone() })
                .collect();
            if !es.is_empty() {
                out.push((o.path_id, Format::TextMap, es));
            }
        }
    }
    out
}

/// The new bytes for one object.
fn rebuild_object(
    sf: &SerializedFile,
    obj: &[u8],
    o: &serialized::Object,
    f: &Format,
    entries: &[Entry],
) -> Result<Option<Vec<u8>>> {
    match f {
        Format::TextMap => {
            let strs: Vec<StrField> = sf.strings(obj, o).unwrap_or_default().into_iter().filter(is_textmap).collect();
            if strs.len() != entries.len() {
                return user("A Naninovel script asset changed since it was read. Read the scripts again.");
            }
            let changes: Vec<(StrField, String)> = strs
                .into_iter()
                .zip(entries)
                .filter(|(s, e)| s.text != e.message)
                .map(|(s, e)| (s, e.message.clone()))
                .collect();
            Ok((!changes.is_empty()).then(|| sf.with_strings(obj, &changes)))
        }
        _ => {
            let Some((_, bytes, _)) = sf.text_asset(obj) else { return Ok(None) };
            let Some(t) = decode(&bytes) else { return Ok(None) };
            let new = apply_text(&t, f, entries)?;
            if new == t {
                return Ok(None);
            }
            let bom = bytes.starts_with(b"\xEF\xBB\xBF");
            let mut nb = if bom { b"\xEF\xBB\xBF".to_vec() } else { vec![] };
            nb.extend(new.as_bytes());
            Ok(sf.with_text(obj, &nb))
        }
    }
}

pub struct Unity {
    pub m: Match,
}

fn json_rel(container: &str, node: &str, path_id: i64, f: &Format) -> String {
    format!("{container}/{node}/{path_id}.{}.json", f.tag())
}

impl Engine for Unity {
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
        let inst = Installer::new(cx.project);
        let json = cx.project.work("json");
        clean_dir(&json)?;
        let list = containers(&game);
        if list.is_empty() {
            return user("No Unity data files were found next to the game.");
        }
        let (mut lines, mut objects) = (0usize, 0usize);
        let mut found: Vec<Value> = Vec::new();
        for (k, (rel, kind)) in list.iter().enumerate() {
            let before = lines;
            let path = inst.original(&game, rel)?;
            if k > 0 && k % 50 == 0 {
                cx.log(format!("Looked through {k} of {} data files", list.len()));
            }
            if !may_have_text(&path, kind) {
                continue;
            }
            let loaded = match load(&path, kind) {
                Ok(l) => l,
                Err(e) => {
                    cx.log(format!("Skipping {rel}: {e}"));
                    continue;
                }
            };
            for (node, b) in serialized_files(&loaded) {
                let Ok(sf) = SerializedFile::parse(b) else { continue };
                for (pid, f, es) in scan(&sf, b) {
                    lines += es.len();
                    objects += 1;
                    write_json_messages(&safe_join(&json, &json_rel(rel, &node, pid, &f))?, &es)?;
                }
            }
            if lines > before {
                found.push(Value::from(rel.clone()));
            }
        }
        if lines == 0 {
            return user(
                "Yakusu looked through this Unity game's data and found no text it knows how to translate \
                 (it reads Naninovel scripts and localization text). This game keeps its text another way.",
            );
        }
        self.m.set("containers", Value::from(found));
        cx.log(format!("Found {lines} lines in {objects} scripts"));
        Ok(json)
    }

    fn build(&mut self, cx: &Ctx) -> Result<PathBuf> {
        let game = cx.game();
        let inst = Installer::new(cx.project);
        let tl = cx.project.work("json_tl");
        let out = cx.project.work("out");
        clean_dir(&out)?;
        // container -> node -> path id -> (format, json path)
        let mut plan: BTreeMap<String, BTreeMap<String, Vec<(i64, Format, PathBuf)>>> = BTreeMap::new();
        let names: Vec<(String, Kind)> = containers(&game);
        for jrel in list_files(&tl, Some(&[".json"])) {
            let Some((cont, _)) = names.iter().find(|(c, _)| jrel.starts_with(&format!("{c}/"))) else { continue };
            let rest = &jrel[cont.len() + 1..];
            let Some((node, file)) = rest.rsplit_once('/') else { continue };
            let mut parts = file.trim_end_matches(".json").rsplitn(2, '.');
            let (Some(tag), Some(pid)) = (parts.next(), parts.next()) else { continue };
            let (Some(f), Ok(pid)) = (Format::from_tag(tag), pid.parse::<i64>()) else { continue };
            plan.entry(cont.clone()).or_default().entry(node.to_string()).or_default().push((
                pid,
                f,
                join_rel(&tl, &jrel),
            ));
        }
        let mut written = 0;
        for (cont, nodes) in plan {
            let kind = names.iter().find(|(c, _)| *c == cont).map(|(_, k)| k.clone()).unwrap();
            let src = inst.original(&game, &cont)?;
            let loaded = load(&src, &kind)?;
            let mut new_nodes: HashMap<String, Vec<u8>> = HashMap::new();
            for (node, b) in serialized_files(&loaded) {
                let Some(objs) = nodes.get(&node) else { continue };
                let sf = SerializedFile::parse(b)?;
                let mut rep: HashMap<i64, Vec<u8>> = HashMap::new();
                for (pid, f, jpath) in objs {
                    let Some(o) = sf.objects.iter().find(|o| o.path_id == *pid) else { continue };
                    if let Some(nb) = rebuild_object(&sf, sf.object(b, o), o, f, &read_json_messages(jpath)?)? {
                        rep.insert(*pid, nb);
                    }
                }
                if !rep.is_empty() {
                    new_nodes.insert(node.clone(), sf.rebuild(b, &rep));
                }
            }
            if new_nodes.is_empty() {
                continue;
            }
            let dst = safe_join(&out, &cont)?;
            fs::create_dir_all(dst.parent().unwrap())?;
            match &loaded.bundle {
                Some(bd) => bd.rewrite(&loaded.data, &new_nodes, &dst, true)?,
                None => fs::write(&dst, new_nodes.into_values().next().unwrap())?,
            }
            written += 1;
            cx.log(format!("Rebuilt {cont}"));
        }
        if written == 0 {
            return user("No translated lines to pack yet.");
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NANI: &str = "; 第一章\n# start\n@back 教室\n@char Kohaku.Happy\nKohaku: おはよう、[i]今日もいい天気だね。\n地の文はここ。\n@choice \"一緒に行く\" goto:.together\n@choice 先に行く goto:.alone\n@stop\n";

    #[test]
    fn nani_round_trip() {
        assert!(is_nani(NANI));
        let es = export_text(NANI, &Format::Nani);
        let m: Vec<(&str, Option<&str>)> = es.iter().map(|e| (e.message.as_str(), e.name.as_deref())).collect();
        assert_eq!(
            m,
            vec![
                ("おはよう、[i]今日もいい天気だね。", Some("Kohaku")),
                ("地の文はここ。", None),
                ("一緒に行く", None),
                ("先に行く", None)
            ]
        );
        let en: Vec<Entry> = ["Morning, [i]lovely weather again.", "Narration here.", "Go \"together\"", "Go ahead"]
            .iter()
            .map(|s| Entry { name: None, names: None, message: s.to_string() })
            .collect();
        let out = apply_text(NANI, &Format::Nani, &en).unwrap();
        assert!(out.contains("Kohaku: Morning, [i]lovely weather again.\n"));
        assert!(out.contains("@choice \"Go \\\"together\\\"\" goto:.together"));
        assert!(out.contains("@choice Go ahead goto:.alone"));
        assert!(out.contains("@back 教室") && out.contains("; 第一章"));
        assert_eq!(out.lines().count(), NANI.lines().count());
    }

    const L10N: &str =
        "# ~a1\n; Kohaku: おはよう！\nKohaku: 早上好！\n\n# ~a2\n; 地の文。\n叙述。\n\n# ~a3\n; Hello\nHello\n";

    #[test]
    fn l10n_round_trip() {
        assert!(is_l10n(L10N));
        assert!(!is_nani(L10N) && !is_l10n(NANI));
        let doc = format!("{L10N}# ~a4\n; 四つ目\n第四\n");
        assert!(is_l10n(&doc));
        let es = export_text(&doc, &Format::L10n);
        let m: Vec<&str> = es.iter().map(|e| e.message.as_str()).collect();
        assert_eq!(m, vec!["おはよう！", "地の文。", "四つ目"]);
        let en: Vec<Entry> = ["Morning!", "Narration.", "Fourth"]
            .iter()
            .map(|s| Entry { name: None, names: None, message: s.to_string() })
            .collect();
        let out = apply_text(&doc, &Format::L10n, &en).unwrap();
        assert_eq!(out, "# ~a1\n; Kohaku: おはよう！\nKohaku: Morning!\n\n# ~a2\n; 地の文。\nNarration.\n\n# ~a3\n; Hello\nHello\n# ~a4\n; 四つ目\nFourth\n");
    }
}
