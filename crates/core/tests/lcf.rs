//! RPG Maker 2000/2003: real files from EasyRPG's TestGame (GPL-3.0, see
//! fixtures/lcf/README.md) round-trip byte for byte, and a map with
//! messages and choices goes through the whole pipeline.

mod common;
use common::*;
use std::path::{Path, PathBuf};
use yakusu_core::engines::lcf;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/lcf").join(name)).unwrap()
}

#[test]
fn real_files_round_trip() {
    for f in ["RPG_RT.ldb", "Map0022.lmu", "Map0015-2003.lmu"] {
        let b = fixture(f);
        let file = lcf::read_file(&b).unwrap();
        assert_eq!(lcf::write_file(&file), b, "{f}");
        // Applying the text unchanged changes nothing.
        let es = lcf::export(&file).unwrap();
        assert_eq!(lcf::write_file(&lcf::apply(&file, &es).unwrap()), b, "{f}");
    }
    let db = lcf::read_file(&fixture("RPG_RT.ldb")).unwrap();
    assert_eq!(db.header, "LcfDataBase");
    let es = lcf::export(&db).unwrap();
    assert_eq!(
        es.iter().map(|e| e.message.as_str()).collect::<Vec<_>>(),
        vec!["%S はテレポートを唱えた！", "%S はエスケープを唱えた！"]
    );
}

#[test]
fn real_database_takes_translations() {
    let db = lcf::read_file(&fixture("RPG_RT.ldb")).unwrap();
    let mut es = lcf::export(&db).unwrap();
    es[0].message = "%S casts Teleport!".into();
    es[1].message = "%S casts Escape!".into();
    let out = lcf::write_file(&lcf::apply(&db, &es).unwrap());
    let again = lcf::read_file(&out).unwrap();
    assert!(lcf::export(&again).unwrap().is_empty());
    assert!(out.windows(18).any(|w| w == b"%S casts Teleport!"));
    // Everything else is where it was: same size apart from the two strings.
    let grew = ("%S casts Teleport!".len() + "%S casts Escape!".len()) as i64;
    let shrank = (encoding_rs::SHIFT_JIS.encode("%S はテレポートを唱えた！").0.len()
        + encoding_rs::SHIFT_JIS.encode("%S はエスケープを唱えた！").0.len()) as i64;
    let diff = out.len() as i64 - fixture("RPG_RT.ldb").len() as i64;
    assert!((diff - (grew - shrank)).abs() <= 8, "{diff}");
}

// ---------------------------------------------------------------- a map built by hand

fn ber(v: u32) -> Vec<u8> {
    let mut g = vec![(v & 0x7F) as u8];
    let mut x = v >> 7;
    while x > 0 {
        g.push((x & 0x7F) as u8 | 0x80);
        x >>= 7;
    }
    g.reverse();
    g
}

fn chunk(id: u32, data: &[u8]) -> Vec<u8> {
    let mut v = ber(id);
    v.extend(ber(data.len() as u32));
    v.extend(data);
    v
}

fn sj(s: &str) -> Vec<u8> {
    encoding_rs::SHIFT_JIS.encode(s).0.into_owned()
}

fn cmd(code: u32, indent: u32, s: &str, params: &[u32]) -> Vec<u8> {
    let mut v = ber(code);
    v.extend(ber(indent));
    let b = sj(s);
    v.extend(ber(b.len() as u32));
    v.extend(b);
    v.extend(ber(params.len() as u32));
    for p in params {
        v.extend(ber(*p));
    }
    v
}

/// Map0001.lmu with one event whose page shows a message and a choice.
fn map() -> Vec<u8> {
    let mut cmds = Vec::new();
    cmds.extend(cmd(10110, 0, "こんにちは！", &[]));
    cmds.extend(cmd(20110, 0, "今日はいい天気ですね。", &[]));
    cmds.extend(cmd(10140, 0, "はい/いいえ", &[2]));
    cmds.extend(cmd(20140, 0, "はい", &[0]));
    cmds.extend(cmd(10110, 1, "よかった。", &[]));
    cmds.extend(cmd(10, 1, "", &[])); // end of branch
    cmds.extend(cmd(20140, 0, "いいえ", &[1]));
    cmds.extend(cmd(10, 1, "", &[])); // end of branch
    cmds.extend(cmd(20141, 0, "", &[]));
    cmds.extend(cmd(10610, 0, "ハロルド", &[1]));
    cmds.extend([0, 0, 0, 0]);
    let mut page = chunk(0x33, &ber(cmds.len() as u32));
    page.extend(chunk(0x34, &cmds));
    page.push(0);
    let mut pages = ber(1);
    pages.extend(ber(1));
    pages.extend(page);
    let mut event = chunk(0x01, b"EV0001");
    event.extend(chunk(0x05, &pages));
    event.push(0);
    let mut events = ber(1);
    events.extend(ber(1));
    events.extend(event);
    let mut out = ber(10);
    out.extend(b"LcfMapUnit");
    out.extend(chunk(0x01, &ber(1))); // chipset
    out.extend(chunk(0x51, &events));
    out.push(0);
    out
}

fn make_game(root: &Path) -> PathBuf {
    std::fs::create_dir_all(root).unwrap();
    std::fs::write(root.join("RPG_RT.exe"), b"MZ").unwrap();
    std::fs::write(root.join("RPG_RT.ldb"), fixture("RPG_RT.ldb")).unwrap();
    std::fs::write(root.join("Map0001.lmu"), map()).unwrap();
    root.to_path_buf()
}

#[test]
fn messages_split_and_choices_follow() {
    let f = lcf::read_file(&map()).unwrap();
    assert_eq!(lcf::write_file(&f), map());
    let mut es = lcf::export(&f).unwrap();
    let got: Vec<&str> = es.iter().map(|e| e.message.as_str()).collect();
    assert_eq!(got, vec!["こんにちは！\n今日はいい天気ですね。", "はい", "いいえ", "よかった。", "ハロルド"]);
    es[0].message = "Hello!\nLovely\nweather\ntoday,\nisn't it?".into();
    es[1].message = "Yes".into();
    es[2].message = "No".into();
    let out = lcf::write_file(&lcf::apply(&f, &es).unwrap());
    let f2 = lcf::read_file(&out).unwrap();
    let left: Vec<String> = lcf::export(&f2).unwrap().into_iter().map(|e| e.message).collect();
    assert_eq!(left, vec!["よかった。", "ハロルド"]);
    // Two windows now: the 5th line starts a new "Show Message".
    let text = String::from_utf8_lossy(&out).to_string();
    for s in ["Hello!", "isn't it?", "Yes/No"] {
        assert!(text.contains(s), "{s}");
    }
    assert_eq!(out.windows(3).filter(|w| w == b"Yes").count(), 2, "choice and its branch");
}

#[test]
fn full_flow_rm2000() {
    let tmp = tempfile::tempdir().unwrap();
    let env = env(tmp.path());
    let game = make_game(&tmp.path().join("RM2k Game"));
    let before = sha(&game.join("Map0001.lmu"));
    let (factory, _) = fake();
    let pipe = pipeline(&env, &game, settings("recommended", 4, 2), factory);
    assert_eq!(pipe.p.meta().engine.as_ref().unwrap().engine_id, "lcf");
    let stats = pipe.extract().unwrap();
    assert_eq!(stats.lines, 6); // the hero name goes through the name glossary
    pipe.draft_style().unwrap();
    pipe.translate(None, false).unwrap();
    pipe.build().unwrap();
    pipe.install().unwrap();
    let m = lcf::read_file(&std::fs::read(game.join("Map0001.lmu")).unwrap()).unwrap();
    assert!(lcf::export(&m).unwrap().len() <= 1); // maybe the hero name, if unnamed
    pipe.restore().unwrap();
    assert_eq!(sha(&game.join("Map0001.lmu")), before);
}
