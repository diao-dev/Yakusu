//! Opening games that used to fail (odd archive layouts, wrapper folders,
//! media archives) and the Lines workbench (reference text, offline checks,
//! subtext suggestions).

mod common;
use common::*;
use serde_json::{json, Value};
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use yakusu_core::app::App;
use yakusu_core::engines::{self, generic, kirikiri::Kirikiri, ks, xp3};

fn app_with(tmp: &Path, factory: yakusu_core::llm::ClientFactory) -> Arc<App> {
    App::with_opener(env(tmp), factory, Box::new(|_| Ok(())))
}

fn call(a: &Arc<App>, cmd: &str, args: Value) -> Value {
    a.call(cmd, &args).unwrap_or_else(|e| panic!("{cmd}: {e}"))
}

fn wait_job(a: &Arc<App>) -> Value {
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(20) {
        let st = call(a, "state", json!({}));
        if !st["job"]["running"].as_bool().unwrap_or(false) {
            return st;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("job did not finish");
}

/// Set the "encrypted" flag on every file in an archive written by
/// `xp3::write_archive` (its index is stored uncompressed).
fn flag_encrypted(path: &Path) {
    let mut b = fs::read(path).unwrap();
    let mut i = 0;
    while i + 16 <= b.len() {
        if &b[i..i + 4] == b"info" {
            b[i + 12..i + 16].copy_from_slice(&0x8000_0000u32.to_le_bytes());
            i += 16;
        } else {
            i += 1;
        }
    }
    fs::write(path, b).unwrap();
}

fn logged(pipe: &mut yakusu_core::pipeline::Pipeline) -> Arc<Mutex<Vec<String>>> {
    let log: Arc<Mutex<Vec<String>>> = Default::default();
    let l = log.clone();
    pipe.log = Arc::new(move |m: &str| l.lock().unwrap().push(m.to_string()));
    log
}

// ------------------------------------------------------------------ opening games

#[test]
fn kirikiri_handles_leading_dot_slash_and_plain_flagged_and_scn_magic() {
    assert!(xp3::looks_like_scn(b"PSB\0\x02\x00\x00\x00"));
    assert!(xp3::looks_like_scn(b"MDF\0\x10\x00\x00\x00"));
    assert!(!xp3::looks_like_scn(b"\x9f\x13\xa2\x07encrypted"));
    assert!(!xp3::looks_like_scn(b"PS"));
    // "./" is dropped only when a real name follows: the rest is still checked.
    assert_eq!(Kirikiri::clean_entry_name("./scenario/a.ks"), "scenario/a.ks");
    assert_eq!(Kirikiri::clean_entry_name(".\\scenario\\a.ks"), "scenario/a.ks");
    assert!(engines::safe_rel(&Kirikiri::clean_entry_name("./../evil.ks")).is_err());
    assert!(engines::safe_rel(&Kirikiri::clean_entry_name("./")).is_err());
    assert!(engines::safe_rel(&Kirikiri::clean_entry_name(".")).is_err());

    // An archive whose names start with "./", whose first script is empty,
    // and whose packer set the "encrypted" flag on plain scripts.
    let tmp = tempfile::tempdir().unwrap();
    let e = env(tmp.path());
    let game = tmp.path().join("game");
    let src = tmp.path().join("src");
    fs::create_dir_all(src.join("scenario")).unwrap();
    fs::create_dir_all(&game).unwrap();
    fs::write(src.join("scenario/aaa_empty.ks"), b"").unwrap();
    fs::write(src.join("scenario/a.ks"), ks::encode("*start\n朝だ。[p]\n桜が散っていた。[p]\n", "utf-16")).unwrap();
    xp3::write_archive(&src, &game.join("data.xp3"), &["./scenario/aaa_empty.ks".into(), "./scenario/a.ks".into()])
        .unwrap();
    flag_encrypted(&game.join("data.xp3"));
    let pipe = pipeline(&e, &game, settings("quick", 10, 1), fake().0);
    let st = pipe.extract().expect("plain scripts behind an encrypted flag are read directly");
    assert_eq!(st.lines, 2);
    assert!(pipe.live_lines().iter().all(|l| l.file.starts_with("ks/scenario/")), "{:?}", pipe.live_lines());
    assert!(pipe.p.meta().engine.as_ref().unwrap().details.get("xp3_title").is_none());

    // A .scn that doesn't start like a scene file is encrypted, whatever its
    // name says: Yakusu asks which game it is instead of passing noise on.
    let game2 = tmp.path().join("game2");
    let src2 = tmp.path().join("src2");
    fs::create_dir_all(&game2).unwrap();
    fs::create_dir_all(&src2).unwrap();
    fs::write(src2.join("start.scn"), b"\x9f\x13\xa2\x07 this is not a scene file at all").unwrap();
    xp3::write_archive(&src2, &game2.join("data.xp3"), &["start.scn".into()]).unwrap();
    let pipe2 = pipeline(&e, &game2, settings("quick", 10, 1), fake().0);
    let err = pipe2.extract().unwrap_err();
    assert_eq!(err.need(), Some("title"), "{err}");
    assert!(err.to_string().contains("encrypted"), "{err}");
}

#[test]
fn generic_skips_media_archives_and_survives_empty_sibling() {
    use std::io::Write;
    for media in ["voice.pfs", "data/bgm01.ypf", "cg_a.pack", "se.arc", "vo2.pfs", "voice.pfs.000", "graphics.pack"] {
        assert!(generic::looks_like_media_archive(media), "{media}");
    }
    for script in
        ["data.pfs", "scene.int", "script.pfs", "ysbin.ypf", "data01000.arc", "bgi.arc", "evdata.pack", "root.pfs"]
    {
        assert!(!generic::looks_like_media_archive(script), "{script}");
    }

    // Every folder matching a pattern part is searched (case differs on Linux).
    let tmp = tempfile::tempdir().unwrap();
    let g = tmp.path().join("globbed");
    fs::create_dir_all(g.join("GameData")).unwrap();
    fs::create_dir_all(g.join("gamedata")).unwrap();
    fs::write(g.join("GameData/a.pack"), b"x").unwrap();
    fs::write(g.join("gamedata/b.pack"), b"x").unwrap();
    let mut hits = generic::glob(&g, &["GameData/*.pack"]);
    hits.sort();
    if cfg!(windows) {
        assert_eq!(hits.len(), 1);
    } else {
        assert_eq!(hits, vec!["GameData/a.pack".to_string(), "gamedata/b.pack".to_string()]);
    }

    if fake_msgtool(tmp.path()).is_none() {
        eprintln!("python3 not available; skipping the msg-tool part");
        return;
    }
    let zip = |path: &Path, files: &[(&str, &[u8])]| {
        let mut z = zip::ZipWriter::new(fs::File::create(path).unwrap());
        for (name, data) in files {
            z.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            z.write_all(data).unwrap();
        }
        z.finish().unwrap();
    };
    // Artemis with its archives in data/: the script, a voice pack, and an
    // archive with no text in it.
    let game = tmp.path().join("Artemis");
    fs::create_dir_all(game.join("data")).unwrap();
    zip(&game.join("data/script.pfs"), &[("a.scr", "桜|「おはよう」\n|春の朝だった。\n".as_bytes())]);
    zip(&game.join("data/voice.pfs"), &[("v001.scr", "|声。\n".as_bytes())]);
    zip(&game.join("data/system.pfs"), &[("sys.bin", b"\x00\x01".as_slice())]);
    let e = env(tmp.path());
    let m = engines::detect(&game).unwrap();
    assert_eq!(m[0].engine_id, "artemis");
    assert_eq!(m[0].strings("files").len(), 3, "data/*.pfs is found: {:?}", m[0].strings("files"));
    let mut pipe = pipeline(&e, &game, settings("quick", 10, 1), fake().0);
    let log = logged(&mut pipe);
    let st = pipe.extract().expect("one empty archive doesn't stop the others");
    assert_eq!(st.lines, 2, "only the script archive's lines, not the voice pack's");
    let log = log.lock().unwrap().join("\n");
    assert!(log.contains("Skipping data/voice.pfs (media archive)"), "{log}");
    assert!(log.contains("couldn't read data/system.pfs"), "{log}");

    // When no archive has text, the reason is still reported.
    let empty = tmp.path().join("Empty");
    fs::create_dir_all(&empty).unwrap();
    zip(&empty.join("a.pfs"), &[("x.bin", b"\x00".as_slice())]);
    zip(&empty.join("b.pfs"), &[("y.bin", b"\x00".as_slice())]);
    let pipe = pipeline(&e, &empty, settings("quick", 10, 1), fake().0);
    assert!(pipe.extract().is_err());
}

#[test]
fn open_game_auto_descends_single_wrapper_subfolder() {
    let tmp = tempfile::tempdir().unwrap();
    let a = app_with(tmp.path(), fake().0);
    // Unzipped: Download/GameName/<the game>.
    let wrapper = tmp.path().join("Download");
    make_game(&wrapper.join("GameName"), SECOND_KS);
    call(&a, "open", json!({"game_dir": wrapper}));
    let st = wait_job(&a);
    assert_eq!(st["project"]["name"], "GameName");
    assert_eq!(st["project"]["total"], 8, "{}", st["job"]);
    assert!(st["logs"].to_string().contains("inside the one you picked"));

    // A folder with an .exe of its own is the game's folder: no looking down.
    let launcher = tmp.path().join("Launcher");
    make_game(&launcher.join("inner"), SECOND_KS);
    fs::write(launcher.join("setup.exe"), b"MZ").unwrap();
    call(&a, "open", json!({"game_dir": launcher}));
    let st = call(&a, "state", json!({}));
    assert_eq!(st["project"]["name"], "Launcher");
    assert!(st["folder_note"].is_string());

    // Two games inside, or a game only inside a saves folder: not guessed.
    let two = tmp.path().join("Two");
    make_game(&two.join("A"), SECOND_KS);
    make_game(&two.join("B"), SECOND_KS);
    call(&a, "open", json!({"game_dir": two}));
    assert_eq!(call(&a, "state", json!({}))["project"]["name"], "Two");
    let saves = tmp.path().join("Saved");
    make_game(&saves.join("saves"), SECOND_KS);
    call(&a, "open", json!({"game_dir": saves}));
    assert_eq!(call(&a, "state", json!({}))["project"]["name"], "Saved");
}

#[test]
fn reopening_an_unread_game_drops_an_engine_that_no_longer_applies() {
    let tmp = tempfile::tempdir().unwrap();
    let a = app_with(tmp.path(), fake().0);
    // First look: an Artemis archive, which can't be read here (no msg-tool).
    let game = tmp.path().join("g");
    fs::create_dir_all(&game).unwrap();
    fs::write(game.join("game.exe"), b"MZ").unwrap();
    fs::write(game.join("data.pfs"), b"not really").unwrap();
    call(&a, "open", json!({"game_dir": game}));
    let st = wait_job(&a);
    assert_eq!(st["project"]["engine"]["engine_id"], "artemis");
    assert_eq!(st["project"]["extracted"], false);
    // The folder really holds a Kirikiri game: opening it again reads that.
    fs::remove_file(game.join("data.pfs")).unwrap();
    make_game(&game, SECOND_KS);
    call(&a, "open", json!({"game_dir": game}));
    let st = wait_job(&a);
    assert_eq!(st["project"]["engine"]["engine_id"], "kirikiri", "{}", st["job"]);
    assert_eq!(st["project"]["total"], 8);
}

#[test]
fn locked_file_helpers_are_harmless_where_nothing_is_locked() {
    let tmp = tempfile::tempdir().unwrap();
    // A folder that's already gone counts as removed.
    engines::remove_dir_force(&tmp.path().join("missing")).unwrap();
    let d = tmp.path().join("d/e");
    fs::create_dir_all(&d).unwrap();
    fs::write(d.join("f.txt"), b"x").unwrap();
    engines::remove_dir_force(&tmp.path().join("d")).unwrap();
    assert!(!tmp.path().join("d").exists());
    // rename_retry replaces an existing (read-only) file.
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    fs::write(&a, b"new").unwrap();
    fs::write(&b, b"old").unwrap();
    let mut perm = fs::metadata(&b).unwrap().permissions();
    perm.set_readonly(true);
    fs::set_permissions(&b, perm).unwrap();
    engines::rename_retry(&a, &b).unwrap();
    assert_eq!(fs::read(&b).unwrap(), b"new");
}

// ------------------------------------------------------------------ workbench

fn translated_app(tmp: &Path, factory: yakusu_core::llm::ClientFactory) -> Arc<App> {
    let a = app_with(tmp, factory);
    let game = make_game(&tmp.join("g"), SECOND_KS);
    call(&a, "open", json!({"game_dir": game}));
    wait_job(&a);
    call(&a, "settings", json!({"provider": "anthropic", "api_key": "sk-test-1234567890abcdef", "preset": "quick"}));
    a
}

fn rows(a: &Arc<App>, filter: &str, q: &str) -> Vec<Value> {
    call(a, "lines", json!({"filter": filter, "q": q, "per": 500}))["rows"].as_array().unwrap().clone()
}

#[test]
fn reference_import_csv_and_json_and_prompt_injection() {
    let tmp = tempfile::tempdir().unwrap();
    let seen: Arc<Mutex<Vec<(String, String)>>> = Default::default();
    let s2 = seen.clone();
    let a = translated_app(
        tmp.path(),
        fake_with(Box::new(move |system: &str, user: &str| {
            s2.lock().unwrap().push((system.to_string(), user.to_string()));
            None
        })),
    );
    let live: Vec<Value> = rows(&a, "all", "").into_iter().filter(|r| r["skip"].as_str() == Some("")).collect();
    assert_eq!(live.len(), 8);
    let (id0, id1) = (live[0]["id"].as_str().unwrap(), live[1]["id"].as_str().unwrap());

    // CSV by id (a Korean fan patch, say), with a BOM and a row for no line.
    let csv = format!("\u{feff}id,korean\n{id0},봄날 아침.\n{id1},오늘부터 새 학교.\nnope#1,없음\n");
    let r = call(&a, "import_reference", json!({"text": csv, "label": "kr.csv"}));
    assert_eq!((r["matched"].as_u64(), r["unmatched"].as_u64()), (Some(2), Some(1)), "{r}");
    let st = call(&a, "state", json!({}));
    assert_eq!(st["project"]["reference_count"], 2);
    assert_eq!(st["project"]["reference_label"], "kr.csv");
    let all = rows(&a, "all", "");
    assert_eq!(all.iter().find(|r| r["id"] == id0).unwrap()["ref"], "봄날 아침.");
    // The reference is searchable.
    let hit = rows(&a, "all", "새 학교");
    assert_eq!(hit.len(), 1);
    assert_eq!(hit[0]["id"], id1);

    // JSON matched by the Japanese (codes and spaces don't matter), then by position.
    let jp1 = live[1]["jp"].as_str().unwrap().replace(|c: char| c == '⟦' || c == '⟧' || c.is_ascii_digit(), "");
    let r = call(&a, "import_reference", json!({"text": json!([{"jp": jp1, "kr": "참조"}]).to_string()}));
    assert_eq!(r["matched"], 1, "{r}");
    let r = call(&a, "import_reference", json!({"text": json!(["첫째", "둘째"]).to_string(), "label": "list"}));
    assert_eq!(r["matched"], 2);
    assert_eq!(rows(&a, "all", "").iter().find(|r| r["id"] == id1).unwrap()["ref"], "둘째");
    // An object of {id: text}.
    let r = call(&a, "import_reference", json!({"text": json!({id0: "하나"}).to_string()}));
    assert_eq!(r["matched"], 1);
    // Nothing matching: an error, and the loaded reference stays.
    assert!(a.call("import_reference", &json!({"text": "id,ref\nnope,x\n"})).is_err());
    assert!(a.call("import_reference", &json!({"text": "speaker,note\na,b\n"})).is_err());
    assert_eq!(call(&a, "state", json!({}))["project"]["reference_count"], 1);

    // The model sees the reference next to its line, with the rule for it.
    call(&a, "run", json!({"action": "translate"}));
    let st = wait_job(&a);
    assert!(st["job"]["error"].is_null(), "{}", st["job"]);
    let tr: Vec<(String, String)> =
        seen.lock().unwrap().iter().filter(|(_, u)| u.contains("TRANSLATE")).cloned().collect();
    assert!(!tr.is_empty());
    assert!(tr.iter().any(|(_, u)| u.contains("\"ref\":\"하나\"")), "{tr:?}");
    assert!(tr.iter().all(|(s, _)| s.contains("existing translation of that line")));

    // Turned off: neither the rule nor the text is sent.
    call(&a, "box", json!({"use_reference_in_prompt": false}));
    seen.lock().unwrap().clear();
    let ids: Vec<String> = live.iter().map(|r| r["id"].as_str().unwrap().to_string()).collect();
    call(&a, "retranslate", json!({"ids": ids}));
    wait_job(&a);
    let tr: Vec<(String, String)> =
        seen.lock().unwrap().iter().filter(|(_, u)| u.contains("TRANSLATE")).cloned().collect();
    assert!(!tr.is_empty());
    assert!(tr.iter().all(|(s, u)| !u.contains("\"ref\"") && !s.contains("existing translation of that line")));

    call(&a, "clear_reference", json!({}));
    let st = call(&a, "state", json!({}));
    assert_eq!(st["project"]["reference_count"], 0);
    assert!(st["project"]["reference_label"].is_null());
    assert!(rows(&a, "all", "").iter().all(|r| r["ref"].is_null()));
}

#[test]
fn deterministic_lint_catches_code_encoding_overflow_and_glossary() {
    let tmp = tempfile::tempdir().unwrap();
    let (factory, calls) = fake();
    let e = env(tmp.path());
    let game = make_game(&tmp.path().join("g"), SECOND_KS);
    let pipe = pipeline(&e, &game, settings("quick", 10, 1), factory);
    pipe.extract().unwrap();
    pipe.translate(None, false).unwrap();
    // The Shift-JIS script makes this a cp932 game.
    let eng = pipe.engine().unwrap();
    assert_eq!(eng.encoding(), "cp932");
    let live = pipe.live_lines();
    let prot = |l: &yakusu_core::project::Line| yakusu_core::protect::protect(&l.jp, eng.line_break_for(&l.file));
    let with_code = live.iter().find(|l| prot(l).text.contains('⟦')).unwrap().clone();
    let plain: Vec<_> = live.iter().filter(|l| !prot(l).text.contains('⟦') && l.id != with_code.id).cloned().collect();
    // Glossary checks ignore one-character names: use a two-character one.
    let with_name = live.iter().find(|l| l.jp.contains("大雅") && l.id != with_code.id).unwrap().clone();
    let row = |l: &yakusu_core::project::Line, en: &str| yakusu_core::project::ResultRow {
        id: l.id.clone(),
        en_ph: en.into(),
        stage: "manual".into(),
        model: "you".into(),
        src: Some(l.jp.clone()),
        ..Default::default()
    };
    let plain: Vec<_> = plain.into_iter().filter(|l| l.id != with_name.id).collect();
    let (emoji, long, japanese) = (&plain[0], &plain[1], &plain[2]);
    pipe.p
        .add_results(vec![
            row(&with_code, "The code is gone."),
            row(emoji, "Good morning 🌸 café"),
            row(long, &"word ".repeat(200)),
            row(japanese, "Still 日本語 here"),
        ])
        .unwrap();
    let mut g = pipe.p.glossary();
    g.names.insert("大雅".into(), "Zelda".into());
    pipe.p.set_glossary(g).unwrap();
    let before = calls.lock().unwrap().len();

    let r = pipe.lint().unwrap();
    assert_eq!(calls.lock().unwrap().len(), before, "lint never asks a model");
    let kinds = |id: &str| -> Vec<String> {
        r["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|i| i["id"] == id)
            .map(|i| i["kind"].as_str().unwrap().into())
            .collect()
    };
    assert!(kinds(&with_code.id).contains(&"code_errors".to_string()), "{r}");
    assert!(kinds(&emoji.id).contains(&"encoding_loss".to_string()), "{r}");
    assert!(!kinds(&emoji.id).contains(&"code_errors".to_string()));
    assert!(kinds(&long.id).contains(&"overflow".to_string()), "{r}");
    assert!(kinds(&japanese.id).contains(&"japanese_left".to_string()), "{r}");
    assert!(kinds(&with_name.id).contains(&"glossary_issues".to_string()), "{r}");
    let detail =
        r["items"].as_array().unwrap().iter().find(|i| i["kind"] == "encoding_loss").unwrap()["detail"].clone();
    assert!(detail.as_str().unwrap().contains('🌸') && !detail.as_str().unwrap().contains('é'), "{detail}");
    for k in ["code_errors", "encoding_loss", "overflow", "japanese_left", "glossary_issues"] {
        assert!(r[k].as_u64().unwrap() >= 1, "{k}: {r}");
    }
    assert_eq!(r["total"], live.len());
    assert_eq!(r["untranslated"], 0);
    assert!(r["clean"].as_u64().unwrap() < live.len() as u64);
    assert_eq!(r["encoding"], "cp932");
}

#[test]
fn subtext_qa_flags_and_accept_dismiss_flow() {
    let tmp = tempfile::tempdir().unwrap();
    let a = translated_app(tmp.path(), fake().0);
    call(&a, "run", json!({"action": "translate"}));
    wait_job(&a);
    let before: Vec<Value> = rows(&a, "translated", "");
    call(&a, "run", json!({"action": "subtext"}));
    let st = wait_job(&a);
    assert!(st["job"]["error"].is_null(), "{}", st["job"]);
    let flagged = rows(&a, "subtext", "");
    assert!(!flagged.is_empty());
    assert_eq!(st["project"]["subtext_flags"], flagged.len());
    // Only a suggestion: the English itself hasn't changed.
    for f in &flagged {
        let was = before.iter().find(|b| b["id"] == f["id"]).unwrap();
        assert_eq!(f["en"], was["en"]);
        assert_eq!(f["subtext_note"], "Reads too literally.");
        assert_ne!(f["subtext_suggestion"], f["en"]);
    }
    // A second run doesn't pay for the same lines again.
    call(&a, "run", json!({"action": "subtext"}));
    let st = wait_job(&a);
    assert_eq!(st["job"]["result"], 0);

    let first = flagged[0].clone();
    let id = first["id"].as_str().unwrap();
    call(&a, "subtext_action", json!({"id": id, "accept": true}));
    let now = rows(&a, "all", "").into_iter().find(|r| r["id"] == id).unwrap();
    assert_eq!(now["en"], first["subtext_suggestion"]);
    assert_eq!(now["before"], first["en"]);
    assert!(now["subtext_note"].is_null());
    assert!(a.call("subtext_action", &json!({"id": id, "accept": true})).is_err(), "nothing left to accept");

    if let Some(second) = flagged.get(1) {
        let id2 = second["id"].as_str().unwrap();
        call(&a, "subtext_action", json!({"id": id2, "accept": false}));
        let now = rows(&a, "all", "").into_iter().find(|r| r["id"] == id2).unwrap();
        assert_eq!(now["en"], second["en"]);
        assert!(now["subtext_note"].is_null() && now["subtext_suggestion"].is_null());
    }
    assert_eq!(call(&a, "state", json!({}))["project"]["subtext_flags"], flagged.len().saturating_sub(2));
}
