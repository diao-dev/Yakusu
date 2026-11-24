//! SiglusEngine: Scene.pck and .ss reading and rebuilding, keys, and the
//! whole open -> translate -> install -> restore path on synthetic games.

mod common;
use common::*;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;
use std::sync::Arc;
use yakusu_core::app::App;
use yakusu_core::engines::siglus::{key, lzss, pck, ss, ss::asm};
use yakusu_core::engines::{self, ks::Entry};

const KEY: [u8; 16] = [0x36, 0x0F, 0xC9, 0x6E, 0x2E, 0xD3, 0x8B, 0x0F, 0x66, 0xA5, 0x70, 0xC3, 0x23, 0xA3, 0x6E, 0x94];

const STRINGS: &[&str] =
    &["桜", "おはよう、大雅くん！", "春の朝だった。", "bg_school_01", "背景テスト", "学校へ行く", "家に帰る", "放課後"];

/// A scene the way the compiler writes one: a speaker and a message, a
/// narration line, two commands taking file names (one Japanese), a choice
/// menu, and a message whose string is also an argument elsewhere.
fn scene_code() -> Vec<u8> {
    let mut c = Vec::new();
    c.extend(asm::nl(1));
    c.extend(asm::name(0));
    c.extend(asm::text(1, 0));
    c.extend(asm::nl(2));
    c.extend(asm::text(2, 1));
    c.extend(asm::nl(3));
    c.extend(asm::push_str(3));
    c.extend(asm::command(&[20]));
    c.extend(asm::nl(4));
    c.extend(asm::push_int(5));
    c.extend(asm::push_str(4));
    c.extend(asm::command(&[10, 20]));
    c.extend(asm::nl(5));
    c.push(asm::SEL_START);
    c.extend(asm::push_str(5));
    c.extend(asm::push_str(6));
    c.extend(asm::command(&[20, 20]));
    c.push(asm::SEL_END);
    c.extend(asm::nl(6));
    c.extend(asm::push_str(7));
    c.extend(asm::command(&[20]));
    c.extend(asm::nl(7));
    c.extend(asm::text(7, 2));
    c.push(asm::EOF);
    c
}

fn scene(xor: bool) -> Vec<u8> {
    ss::build(&scene_code(), STRINGS, 3, xor)
}

fn english() -> Vec<Entry> {
    let e = |name: Option<&str>, m: &str| Entry { name: name.map(String::from), names: None, message: m.into() };
    vec![
        e(Some("Sakura"), "Good morning, Taiga! What a lovely day it is today, isn't it?"),
        e(None, "It was a spring morning, and the cherry blossoms were falling."),
        e(None, "Go to school"),
        e(None, "Head back home"),
        e(None, "After school, the classroom was quiet."),
    ]
}

#[test]
fn lzss_roundtrip_and_malformed_guards() {
    let data: Vec<u8> = "放課後の教室は静かだった。".repeat(300).into_bytes();
    let p = lzss::pack(&data);
    assert!(p.len() < data.len() / 4, "repetitive text packs small");
    assert_eq!(lzss::unpack(&p).unwrap(), data);
    // Cut short, absurd sizes, back-references before the start: errors.
    assert!(lzss::unpack(&p[..p.len() / 2]).is_err());
    assert!(lzss::unpack(&[1, 2, 3]).is_err());
    let mut huge = p.clone();
    huge[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(lzss::unpack(&huge).is_err());
    let bad_ref = [12u8, 0, 0, 0, 4, 0, 0, 0, 0x00, 0xF0, 0x00, 0x00];
    assert!(lzss::unpack(&bad_ref).is_err());
}

#[test]
fn ss_bytecode_extracts_only_dialogue_names_and_choices() {
    let s = ss::Ss::parse(scene(true)).unwrap();
    let got = s.export();
    let msgs: Vec<&str> = got.iter().map(|e| e.message.as_str()).collect();
    assert_eq!(msgs, vec!["おはよう、大雅くん！", "春の朝だった。", "学校へ行く", "家に帰る", "放課後"]);
    assert_eq!(got[0].name.as_deref(), Some("桜"));
    assert!(got[1].name.is_none(), "a speaker belongs to one message");
    // Asset names, Japanese or not, are never offered for translation.
    assert!(!msgs.contains(&"bg_school_01") && !msgs.contains(&"背景テスト"));

    let out = s.apply(&english()).unwrap();
    let new = ss::Ss::parse(out.clone()).unwrap();
    let back = new.export();
    // (Choices are found by their Japanese, so a translated scene lists
    // only its messages; Yakusu always reads the original scene.)
    let msgs: Vec<String> = back.iter().map(|e| e.message.clone()).collect();
    assert_eq!(msgs, [0, 1, 4].map(|k| english()[k].message.clone()).to_vec());
    assert_eq!(back[0].name.as_deref(), Some("Sakura"));
    assert_eq!((new.string(5), new.string(6)), ("Go to school".into(), "Head back home".into()));
    // Bytecode keeps its size; the file names are untouched.
    assert_eq!(new.code().len(), s.code().len());
    assert_eq!(new.string(3), "bg_school_01");
    assert_eq!(new.string(4), "背景テスト");
    // 放課後 is also a command's argument: it keeps its text there, and the
    // message got a string of its own (only that push's number changed).
    assert_eq!(new.string(7), "放課後");
    assert_eq!(new.string_count(), STRINGS.len() + 1);
    let diff: Vec<usize> = (0..s.code().len()).filter(|&i| s.code()[i] != new.code()[i]).collect();
    assert!(!diff.is_empty() && diff.len() <= 4, "only the one push operand changed: {diff:?}");
    // Nothing changed: the scene comes back byte for byte.
    assert_eq!(s.apply(&got).unwrap(), s.data);
    // Wrong number of lines is refused.
    assert!(s.apply(&english()[..3]).is_err());

    // Strings stored plain (older games) read and write the same way.
    let plain = ss::Ss::parse_with(scene(false), false).unwrap();
    assert_eq!(plain.export()[0].message, "おはよう、大雅くん！");
    let (p, x) = plain.codec_scores();
    assert!(p > x, "plain strings score as plain");
    let (p, x) = s.codec_scores();
    assert!(x > p, "XORed strings score as XORed");
    let rebuilt = ss::Ss::parse_with(plain.apply(&english()).unwrap(), false).unwrap();
    assert_eq!(rebuilt.export()[2].message, "After school, the classroom was quiet.");
}

#[test]
fn pck_reads_rewrites_and_checks_keys() {
    let scenes =
        vec![("start".to_string(), scene(true)), ("empty".to_string(), vec![]), ("next".to_string(), scene(true))];
    let mut scenes = scenes;
    scenes[1].1 = ss::build(&[asm::EOF], &[], 0, true);
    let raw = pck::build(&scenes, Some(&KEY), true);
    let p = pck::Pck::parse(raw.clone()).unwrap();
    assert!(p.packed() && p.needs_key());
    assert_eq!(p.names, vec!["start", "empty", "next"]);
    assert!(p.check_key(&KEY));
    let mut wrong = KEY;
    wrong[3] ^= 1;
    assert!(!p.check_key(&wrong));
    assert!(p.scene(0, None).is_err());
    assert_eq!(p.scene(2, Some(&KEY)).unwrap(), scenes[2].1);

    // Only scene 2 changes: scenes 0 and 1 are copied as stored.
    let new2 = ss::Ss::parse(scenes[2].1.clone()).unwrap().apply(&english()).unwrap();
    let out = p.rewrite(&[(2usize, new2.clone())].into_iter().collect(), Some(&KEY)).unwrap();
    let q = pck::Pck::parse(out.clone()).unwrap();
    assert_eq!(q.raw(0), p.raw(0));
    assert_eq!(q.raw(1), p.raw(1));
    assert_eq!(q.scene(2, Some(&KEY)).unwrap(), new2);
    let data_ofs = (out.len() - q.index.iter().map(|x| x.1).sum::<usize>()) as usize;
    let idx_ofs = data_ofs - 3 * 8;
    assert_eq!(out[..idx_ofs], raw[..idx_ofs], "everything before the index is kept");

    // "Easy link" packs store scenes as they are.
    let easy = pck::Pck::parse(pck::build(&scenes, None, false)).unwrap();
    assert!(!easy.packed() && !easy.needs_key());
    assert_eq!(easy.scene(0, None).unwrap(), scenes[0].1);

    // A "pack_scn" signature in front is read too.
    let mut signed = b"pack_scn".to_vec();
    let shifted = pck::build(&scenes, None, false);
    // Offsets are from the file start: shift every non-zero field by 8.
    let mut h: Vec<u8> = Vec::new();
    for k in 0..pck::FIELDS {
        let v = i32::from_le_bytes(shifted[k * 4..k * 4 + 4].try_into().unwrap());
        let v = if [1, 3, 5, 7, 9, 11, 13, 15, 17, 19].contains(&k) { v + 8 } else { v };
        h.extend_from_slice(&v.to_le_bytes());
    }
    signed.extend(h);
    signed.extend_from_slice(&shifted[pck::FIELDS * 4..]);
    let sp = pck::Pck::parse(signed).unwrap();
    assert_eq!(sp.names, vec!["start", "empty", "next"]);
    assert_eq!(sp.scene(2, None).unwrap(), scenes[2].1);
}

#[test]
fn keys_parse_in_every_common_spelling() {
    for s in [
        "36 0F C9 6E 2E D3 8B 0F 66 A5 70 C3 23 A3 6E 94",
        "360FC96E2ED38B0F66A570C323A36E94",
        "0x36, 0x0F, 0xC9, 0x6E, 0x2E, 0xD3, 0x8B, 0x0F, 0x66, 0xA5, 0x70, 0xC3, 0x23, 0xA3, 0x6E, 0x94",
        "[36:0F:C9:6E:2E:D3:8B:0F:66:A5:70:C3:23:A3:6E:94]",
    ] {
        assert_eq!(key::parse_hex(s), Some(KEY), "{s}");
    }
    assert_eq!(key::parse_hex("36 0F C9"), None);
    assert_eq!(key::parse_hex("zz0FC96E2ED38B0F66A570C323A36E94"), None);
    let toml = "# siglus_rs key\nkey = [\n  0x36, 0x0F, 0xC9, 0x6E, 0x2E, 0xD3, 0x8B, 0x0F,\n  0x66, 0xA5, 0x70, 0xC3, 0x23, 0xA3, 0x6E, 0x94,\n]\n";
    assert_eq!(key::parse_key_toml(toml), Some(KEY));
    assert_eq!(key::parse_key_toml("key = \"36 0F C9 6E 2E D3 8B 0F 66 A5 70 C3 23 A3 6E 94\""), Some(KEY));
    assert_eq!(
        key::parse_key_toml("key = [54, 15, 201, 110, 46, 211, 139, 15, 102, 165, 112, 195, 35, 163, 110, 148]"),
        Some(KEY)
    );
    let tool = "mode           : resource-crack\nstatus         : key recovered and hard-validated\nkey            : 36 0F C9 6E 2E D3 8B 0F 66 A5 70 C3 23 A3 6E 94\n";
    assert_eq!(key::keys_in_tool_output(tool), vec![KEY]);
    let exe = "status         : candidate(s) found\ncandidate 01   : 00 11 22 33 44 55 66 77 88 99 AA BB CC DD EE FF\n  source        : .rdata\ncandidate 02   : 36 0F C9 6E 2E D3 8B 0F 66 A5 70 C3 23 A3 6E 94\n";
    assert_eq!(key::keys_in_tool_output(exe).len(), 2);
}

fn siglus_game(dir: &Path, with_key_file: bool) -> std::path::PathBuf {
    fs::create_dir_all(dir).unwrap();
    let scenes = vec![("start".to_string(), scene(true)), ("ending".to_string(), scene(true))];
    fs::write(dir.join("Scene.pck"), pck::build(&scenes, Some(&KEY), true)).unwrap();
    fs::write(dir.join("Gameexe.dat"), b"not used by Yakusu").unwrap();
    fs::write(dir.join("SiglusEngine.exe"), b"MZ").unwrap();
    if with_key_file {
        fs::write(dir.join("key.toml"), format!("key = \"{}\"\n", key::hex(&KEY))).unwrap();
    }
    dir.to_path_buf()
}

fn sha(p: &Path) -> Vec<u8> {
    Sha256::digest(fs::read(p).unwrap()).to_vec()
}

#[test]
fn encrypted_scene_pck_full_pipeline_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let e = env(tmp.path());
    let game = siglus_game(&tmp.path().join("Rewrite"), true);
    let original = sha(&game.join("Scene.pck"));
    let m = engines::detect(&game).unwrap();
    assert_eq!((m[0].engine_id.as_str(), m[0].name.as_str()), ("siglus", "SiglusEngine"));
    assert!(m.iter().all(|m| m.engine_id != "circus"), "Scene.pck isn't offered as a Circus pack");
    let pipe = pipeline(&e, &game, settings("quick", 10, 1), fake().0);
    let st = pipe.extract().unwrap();
    assert_eq!(st.lines, 10, "5 lines in each of 2 scenes");
    assert_eq!(pipe.engine().unwrap().encoding(), "utf-16");
    assert_eq!(pipe.p.meta().engine.as_ref().unwrap().str("siglus_key"), Some(key::hex(&KEY).as_str()));
    pipe.translate(None, false).unwrap();
    pipe.finish().unwrap();
    assert_ne!(sha(&game.join("Scene.pck")), original);
    // The installed pack opens with the same key and has the English in.
    let p = pck::Pck::parse(fs::read(game.join("Scene.pck")).unwrap()).unwrap();
    for i in 0..2 {
        let s = ss::Ss::parse(p.scene(i, Some(&KEY)).unwrap()).unwrap();
        let x = s.export();
        assert!(x.iter().all(|e| !yakusu_core::protect::has_japanese(&e.message)), "{x:?}");
        assert_eq!(x[0].name.as_deref(), Some("Sakura"));
        assert_eq!(s.string(3), "bg_school_01");
        assert_eq!(s.string(4), "背景テスト");
    }
    pipe.restore().unwrap();
    assert_eq!(sha(&game.join("Scene.pck")), original, "restored byte for byte");
}

#[test]
fn wrong_or_missing_siglus_key_returns_setup_error() {
    let tmp = tempfile::tempdir().unwrap();
    let e = env(tmp.path());
    let game = siglus_game(&tmp.path().join("NoKey"), false);
    let pipe = pipeline(&e, &game, settings("quick", 10, 1), fake().0);
    let err = pipe.extract().unwrap_err();
    assert_eq!(err.need(), Some("siglus_key"), "{err}");

    // Through the app: a malformed key is refused, a wrong one asks again,
    // the right one reads the game.
    let a: Arc<App> = App::with_opener(env(tmp.path()), fake().0, Box::new(|_| Ok(())));
    a.call("open", &json!({"game_dir": game})).unwrap();
    wait(&a);
    assert_eq!(a.call("state", &json!({})).unwrap()["job"]["need"], "siglus_key");
    assert!(a.call("engine", &json!({"siglus_key": "36 0F"})).is_err());
    a.call("engine", &json!({"siglus_key": "00112233445566778899AABBCCDDEEFF"})).unwrap();
    assert_eq!(wait(&a)["job"]["need"], "siglus_key");
    a.call(
        "engine",
        &json!({"siglus_key": "0x36,0x0F,0xC9,0x6E,0x2E,0xD3,0x8B,0x0F,0x66,0xA5,0x70,0xC3,0x23,0xA3,0x6E,0x94"}),
    )
    .unwrap();
    let st = wait(&a);
    assert!(st["job"]["error"].is_null(), "{}", st["job"]);
    assert_eq!(st["project"]["total"], 10);
    assert_eq!(st["project"]["engine"]["details"]["siglus_key"], key::hex(&KEY));
}

#[test]
fn plain_and_easy_link_packs_need_no_key() {
    let tmp = tempfile::tempdir().unwrap();
    let e = env(tmp.path());
    let game = tmp.path().join("Old");
    fs::create_dir_all(game.join("Data")).unwrap();
    let scenes = vec![("s".to_string(), scene(false))];
    fs::write(game.join("Data/SCENE.PCK"), pck::build(&scenes, None, false)).unwrap();
    let m = engines::detect(&game).unwrap();
    assert_eq!(m[0].engine_id, "siglus");
    assert_eq!(m[0].str("pck_rel"), Some("Data/SCENE.PCK"));
    let pipe = pipeline(&e, &game, settings("quick", 10, 1), fake().0);
    assert_eq!(pipe.extract().unwrap().lines, 5);
    assert!(pipe.p.meta().engine.as_ref().unwrap().flag("plain_strings"));
    pipe.translate(None, false).unwrap();
    pipe.finish().unwrap();
    let p = pck::Pck::parse(fs::read(game.join("Data/SCENE.PCK")).unwrap()).unwrap();
    let s = ss::Ss::parse_with(p.scene(0, None).unwrap(), false).unwrap();
    assert!(s.export().iter().all(|e| !yakusu_core::protect::has_japanese(&e.message)));
}

fn wait(a: &Arc<App>) -> serde_json::Value {
    let t0 = std::time::Instant::now();
    loop {
        let st = a.call("state", &json!({})).unwrap();
        if !st["job"]["running"].as_bool().unwrap_or(false) || t0.elapsed().as_secs() > 20 {
            return st;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}
