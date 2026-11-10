//! RPG Maker XP / VX / VX Ace: Ruby Marshal data made by real Ruby
//! (fixtures/rgss/make.rb), archives, and a game end to end. When Ruby is
//! installed, what Yakusu writes is loaded back with Ruby's own Marshal.

mod common;
use common::*;
use std::path::{Path, PathBuf};
use yakusu_core::engines::rgss::{self, archive, marshal};
use yakusu_core::engines::Entry;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rgss").join(name)
}

fn bytes(name: &str) -> Vec<u8> {
    std::fs::read(fixture(name)).unwrap()
}

/// Ruby's view of a Marshal file (None when Ruby isn't installed).
fn ruby_check(data: &[u8]) -> Option<String> {
    let tmp = tempfile::tempdir().ok()?;
    let p = tmp.path().join("x.rvdata2");
    std::fs::write(&p, data).ok()?;
    let out = std::process::Command::new("ruby").arg(fixture("check.rb")).arg(&p).output().ok()?;
    assert!(out.status.success(), "Ruby couldn't load it: {}", String::from_utf8_lossy(&out.stderr));
    Some(String::from_utf8(out.stdout).unwrap())
}

#[test]
fn marshal_round_trips_byte_for_byte() {
    for f in [
        "Map001.rvdata2",
        "CommonEvents.rvdata2",
        "Actors.rvdata2",
        "Items.rvdata2",
        "System.rvdata2",
        "CommonEvents.rxdata",
    ] {
        let b = bytes(f);
        let (v, syms) = marshal::load(&b).unwrap();
        assert_eq!(marshal::dump(&v, &syms), b, "{f}");
    }
    assert!(marshal::load(b"\x04\x09").is_err());
    assert!(marshal::load(b"\x04\x08[\x07").is_err());
}

fn msgs(es: &[Entry]) -> Vec<&str> {
    es.iter().map(|e| e.message.as_str()).collect()
}

#[test]
fn rgss_export() {
    let (v, syms) = marshal::load(&bytes("Map001.rvdata2")).unwrap();
    let es = rgss::export(&v, &syms, false);
    assert_eq!(
        msgs(&es),
        vec![
            "はじまりの村",
            "こんにちは！\n\\C[2]旅人\\C[0]さん、ようこそ。",
            "はい",
            "いいえ",
            "よかった。",
            "ハロルド"
        ]
    );
    assert_eq!(es[5].name.as_deref(), Some("ハロルド"));
    let (s, syms) = marshal::load(&bytes("System.rvdata2")).unwrap();
    // Switch/variable names (editor only) and file names are left alone.
    assert_eq!(
        msgs(&rgss::export(&s, &syms, false)),
        vec!["桜の旅", "炎", "魔法", "レベル", "最大HP", "武器", "戦う", "逃げる"]
    );
    let (x, syms) = marshal::load(&bytes("CommonEvents.rxdata")).unwrap();
    assert_eq!(msgs(&rgss::export(&x, &syms, true)), vec!["一行目です。\n二行目です。"]);
}

#[test]
fn rgss_apply_long_message_keeps_references() {
    let (v, syms) = marshal::load(&bytes("Map001.rvdata2")).unwrap();
    let mut es = rgss::export(&v, &syms, false);
    es[1].message = "Hello!\nWelcome,\n\\C[2]traveler\\C[0].\nIt's been a while\nsince anyone came by.".into();
    es[2].message = "Yes".into();
    es[3].message = "No".into();
    es[4].message = "Good.".into();
    let new = rgss::apply(&v, &syms, &es, false).unwrap();
    let out = marshal::dump(&new, &syms);
    let (again, syms2) = marshal::load(&out).unwrap();
    let es2 = rgss::export(&again, &syms2, false);
    assert_eq!(msgs(&es2), vec!["はじまりの村", "ハロルド"]);
    if let Some(r) = ruby_check(&out) {
        let lines: Vec<&str> = r.lines().collect();
        assert_eq!(lines[0], r#"101|0|["Actor1",0,0,2]"#);
        assert_eq!(lines[1], r#"401|0|["Hello!"]"#);
        assert_eq!(lines[4], r#"401|0|["It's been a while"]"#);
        assert_eq!(lines[5], r#"101|0|["Actor1",0,0,2]"#);
        assert_eq!(lines[6], r#"401|0|["since anyone came by."]"#);
        assert_eq!(lines[7], r#"102|0|[["Yes","No"],2]"#);
        assert_eq!(lines[8], r#"402|0|[0,"Yes"]"#);
        assert!(r.contains(r#"402|0|[1,"No"]"#));
        // The shared comment string is still one object after the new commands.
        assert!(r.contains("shared=true"), "{r}");
    }
    // XP: the first line lives in "Show Text" itself.
    let (x, syms) = marshal::load(&bytes("CommonEvents.rxdata")).unwrap();
    let mut es = rgss::export(&x, &syms, true);
    es[0].message = "Line one.".into();
    let out = marshal::dump(&rgss::apply(&x, &syms, &es, true).unwrap(), &syms);
    if let Some(r) = ruby_check(&out) {
        assert!(r.starts_with("101|0|[\"Line one.\"]\n401|0|[\"\"]\n"), "{r}");
    }
}

#[test]
fn archives_v1_and_v3() {
    for v in [1u8, 3] {
        let files: Vec<(&str, &[u8])> = vec![
            ("Data/Map001.rvdata2", b"map data!"),
            ("Data/System.rvdata2", b"sys"),
            ("Graphics/x.png", b"\x89PNG"),
        ];
        let a = archive::Archive::parse(archive::create(v, &files)).unwrap();
        assert_eq!(a.version, v);
        let names: Vec<&str> = a.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["Data/Map001.rvdata2", "Data/System.rvdata2", "Graphics/x.png"]);
        assert_eq!(a.read(&a.entries[0]), b"map data!");
        let rebuilt = a.rebuild(&[("Data/System.rvdata2".to_string(), b"new system data".to_vec())].into());
        let b = archive::Archive::parse(rebuilt).unwrap();
        assert_eq!(b.read(&b.entries[1]), b"new system data");
        assert_eq!(b.read(&b.entries[2]), b"\x89PNG");
    }
}

fn make_ace_game(root: &Path, packed: bool) -> PathBuf {
    std::fs::create_dir_all(root).unwrap();
    std::fs::write(root.join("Game.exe"), b"MZ").unwrap();
    std::fs::write(root.join("Game.ini"), "[Game]\r\nLibrary=System\\RGSS301.dll\r\n").unwrap();
    let names = ["Map001", "CommonEvents", "Actors", "Items", "System"];
    let datas: Vec<(String, Vec<u8>)> =
        names.iter().map(|n| (format!("Data/{n}.rvdata2"), bytes(&format!("{n}.rvdata2")))).collect();
    if packed {
        let files: Vec<(&str, &[u8])> = datas.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect();
        std::fs::write(root.join("Game.rgss3a"), archive::create(3, &files)).unwrap();
    } else {
        std::fs::create_dir_all(root.join("Data")).unwrap();
        for (n, b) in &datas {
            std::fs::write(root.join(n), b).unwrap();
        }
    }
    root.to_path_buf()
}

#[test]
fn full_flow_vx_ace() {
    for packed in [true, false] {
        let tmp = tempfile::tempdir().unwrap();
        let env = env(tmp.path());
        let game = make_ace_game(&tmp.path().join("Ace Game"), packed);
        let (factory, _) = fake();
        let pipe = pipeline(&env, &game, settings("recommended", 4, 2), factory);
        let m = pipe.p.meta().engine.clone().unwrap();
        assert_eq!((m.engine_id.as_str(), m.name.as_str()), ("rgss", "RPG Maker VX Ace"));
        let stats = pipe.extract().unwrap();
        assert!(stats.lines >= 15, "{}", stats.lines);
        pipe.draft_style().unwrap();
        pipe.translate(None, false).unwrap();
        pipe.build().unwrap();
        pipe.install().unwrap();
        let map = if packed {
            let a = archive::Archive::open(&game.join("Game.rgss3a")).unwrap();
            a.read(a.entries.iter().find(|e| e.name == "Data/Map001.rvdata2").unwrap())
        } else {
            std::fs::read(game.join("Data/Map001.rvdata2")).unwrap()
        };
        let (v, syms) = marshal::load(&map).unwrap();
        let left = rgss::export(&v, &syms, false);
        // Only the actor name change (a name: glossary English) may remain if unnamed.
        assert!(left.len() <= 1, "{packed}: {:?}", msgs(&left));
        if let Some(r) = ruby_check(&map) {
            assert!(r.contains("shared=true"));
            assert!(r.contains("\\\\C[2]"), "color code kept: {r}");
        }
        pipe.restore().unwrap();
    }
}
