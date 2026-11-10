//! LiveMaker: archives, compiled scripts and text blocks, checked against
//! sample files from pylivemaker (GPL-3.0, see fixtures/livemaker/README.md),
//! then a whole game through the pipeline.

mod common;
use common::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use yakusu_core::engines::livemaker::{self, archive, archive::Archive, lsb::Lsb, tpword::TpWord};
use yakusu_core::engines::Entry;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/livemaker").join(name)
}

fn bytes(name: &str) -> Vec<u8> {
    std::fs::read(fixture(name)).unwrap()
}

#[test]
fn archives_read() {
    for f in ["test.exe", "test.dat"] {
        let a = Archive::open(&fixture(f)).unwrap();
        assert_eq!(a.entries.len(), 1, "{f}");
        assert_eq!(a.entries[0].name, "hello.txt");
        assert_eq!(a.read(&a.entries[0]).unwrap(), b"Hello world!\n");
        assert!(a.checksum_ok(&a.entries[0]).unwrap());
    }
    assert!(matches!(Archive::open(&fixture("test.exe")).unwrap().layout, archive::Layout::Exe(2)));
    // A real LiveNovel save file (zlib-compressed entries).
    let a = Archive::open(&fixture("save.dat")).unwrap();
    assert!(a.entries.iter().any(|e| e.name.contains("STATUSSAVEDATA")));
    for e in &a.entries {
        a.read(e).unwrap();
        assert!(a.checksum_ok(e).unwrap(), "{}", e.name);
    }
    assert!(!archive::is_archive(&fixture("00000001.lsb")));
}

#[test]
fn archives_rewrite_identically() {
    let tmp = tempfile::tempdir().unwrap();
    for f in ["test.exe", "test.dat", "save.dat"] {
        let a = Archive::open(&fixture(f)).unwrap();
        let written = a.rewrite(&HashMap::new(), tmp.path()).unwrap();
        assert_eq!(written, vec![f.to_string()]);
        assert_eq!(std::fs::read(tmp.path().join(f)).unwrap(), bytes(f), "{f} changed");
    }
}

#[test]
fn archives_replace_a_file() {
    let tmp = tempfile::tempdir().unwrap();
    for f in ["test.exe", "test.dat"] {
        let a = Archive::open(&fixture(f)).unwrap();
        let new = b"Hello, a much longer world than before!\n".to_vec();
        a.rewrite(&HashMap::from([("hello.txt".to_string(), new.clone())]), tmp.path()).unwrap();
        let b = Archive::open(&tmp.path().join(f)).unwrap();
        assert_eq!(b.read(&b.entries[0]).unwrap(), new);
        assert!(b.checksum_ok(&b.entries[0]).unwrap());
        if f.ends_with(".exe") {
            assert!(std::fs::read(tmp.path().join(f)).unwrap().starts_with(b"MZ"));
        }
    }
    // Archives Yakusu writes from scratch (for tests) read back too.
    let dat = tmp.path().join("game.dat");
    archive::create(&dat, &[("a.lsb", b"one"), ("dir/b.txt", b"two")], None).unwrap();
    let whole = Archive::open(&dat).unwrap();
    assert_eq!(whole.entries[1].name, "dir/b.txt");
    assert_eq!(whole.read(&whole.entries[1]).unwrap(), b"two");
    let exe = tmp.path().join("made.exe");
    archive::create(&exe, &[("x.lsb", b"exe!")], Some(b"MZ-stub")).unwrap();
    let a = Archive::open(&exe).unwrap();
    assert_eq!(a.read(&a.entries[0]).unwrap(), b"exe!");
}

const EXPECTED: &[&str] = &[
    "Hello world!\nPage break",
    "Wait for click",
    "After wait",
    "Text speed fast",
    "Text speed slow",
    "Text speed normal",
];

#[test]
fn lsb_blocks_match_pylivemaker() {
    let l = Lsb::parse(bytes("00000001.lsb")).unwrap();
    let got: Vec<String> = livemaker::export(&l).into_iter().map(|e| e.message).collect();
    assert_eq!(got, EXPECTED);
    assert!(l.texts.iter().all(|t| t.line_no == 8));
    // gamemain.lsb is the game's menu chart: it parses, with no novel text.
    let g = Lsb::parse(bytes("gamemain.lsb")).unwrap();
    assert!(livemaker::export(&g).is_empty());
}

#[test]
fn lsb_and_tpword_round_trip() {
    for f in ["00000001.lsb", "gamemain.lsb"] {
        let orig = bytes(f);
        let l = Lsb::parse(orig.clone()).unwrap();
        assert_eq!(l.rebuild(&HashMap::new(), false), orig, "{f}");
        for i in 0..l.texts.len() {
            let tw = TpWord::parse(l.text(i)).unwrap();
            assert_eq!(tw.to_bytes(), l.text(i), "{f} text {i}");
        }
        // Untouched entries change nothing.
        let (same, lost) = livemaker::apply(&l, &livemaker::export(&l), false).unwrap();
        assert_eq!(same, orig);
        assert!(lost.is_empty());
    }
}

#[test]
fn replaced_text_reads_back() {
    let l = Lsb::parse(bytes("00000001.lsb")).unwrap();
    let mut entries = livemaker::export(&l);
    entries[0].message = "First line, rather longer now.\nSecond line".into();
    entries[2].message = "Short".into();
    entries[5].message = "日本語もOK、それにéは？".into();
    let (new, lost) = livemaker::apply(&l, &entries, false).unwrap();
    assert_eq!(lost, vec!['é']); // not in CP932
    let l2 = Lsb::parse(new).unwrap();
    let got: Vec<String> = livemaker::export(&l2).into_iter().map(|e| e.message).collect();
    assert_eq!(got[0], "First line, rather longer now.\nSecond line");
    assert_eq!(got[1], EXPECTED[1]);
    assert_eq!(got[2], "Short");
    assert_eq!(got[5], "日本語もOK、それに?は？");
    assert_eq!(got.len(), EXPECTED.len());
    // Wrong line counts are refused rather than misapplied.
    assert!(livemaker::apply(&l, &entries[1..], false).is_err());
}

#[test]
fn damaged_scripts_are_refused() {
    let orig = bytes("00000001.lsb");
    assert!(Lsb::parse(orig[..orig.len() - 3].to_vec()).is_err());
    let mut v = orig.clone();
    v[0] = 50; // version
    assert!(Lsb::parse(v).is_err());
    assert!(TpWord::parse(b"TpWord10").is_err());
    let mut extra = orig;
    extra.push(0);
    assert!(Lsb::parse(extra).is_err());
}

/// A LiveMaker game: an exe with the sample script, its text made Japanese.
fn make_lm_game(root: &Path) -> PathBuf {
    std::fs::create_dir_all(root).unwrap();
    let l = Lsb::parse(bytes("00000001.lsb")).unwrap();
    let jp = [
        "春の朝。窓の外では桜が散っていた。\n今日から新しい学校だ。",
        "「おはよう、大雅くん！」",
        "僕は眠い目をこすった。",
        "駅までの坂道は長い。",
        "二人で歩く道は短く感じた。",
        "またね。",
    ];
    let entries: Vec<Entry> = jp.iter().map(|m| Entry { name: None, names: None, message: m.to_string() }).collect();
    let (script, lost) = livemaker::apply(&l, &entries, false).unwrap();
    assert!(lost.is_empty());
    let mut stub = b"MZ".to_vec();
    stub.extend(vec![0u8; 1000]);
    archive::create(
        &root.join("game.exe"),
        &[("00000001.lsb", &script), ("gamemain.lsb", &bytes("gamemain.lsb")), ("bg.png", b"\x89PNG fake")],
        Some(&stub),
    )
    .unwrap();
    // A save file next to it must not be taken for the game's scripts.
    std::fs::copy(fixture("save.dat"), root.join("save.dat")).unwrap();
    root.to_path_buf()
}

#[test]
fn full_flow_livemaker() {
    let tmp = tempfile::tempdir().unwrap();
    let env = env(tmp.path());
    let game = make_lm_game(&tmp.path().join("LM Game"));
    let exe_before = sha(&game.join("game.exe"));
    let (factory, _) = fake();
    let pipe = pipeline(&env, &game, settings("recommended", 3, 2), factory);
    let m = pipe.p.meta().engine.clone().unwrap();
    assert_eq!(m.engine_id, "livemaker");
    assert_eq!(m.strings("archives"), vec!["game.exe".to_string()]);

    let stats = pipe.extract().unwrap();
    assert_eq!(stats.lines, 6);
    pipe.draft_style().unwrap();
    pipe.translate(None, false).unwrap();
    let out = pipe.build().unwrap();
    assert_eq!(out.applied, 6);
    let built = pipe.p.work("out/game.exe");
    let a = Archive::open(&built).unwrap();
    let e = a.entries.iter().find(|e| e.name == "00000001.lsb").unwrap();
    let texts: Vec<String> =
        livemaker::export(&Lsb::parse(a.read(e).unwrap()).unwrap()).into_iter().map(|e| e.message).collect();
    assert_eq!(texts.len(), 6);
    assert!(texts.iter().all(|t| !yakusu_core::protect::has_japanese(t)), "{texts:?}");
    // Everything else is carried over untouched.
    let png = a.entries.iter().find(|e| e.name == "bg.png").unwrap();
    assert_eq!(a.read(png).unwrap(), b"\x89PNG fake");
    assert!(std::fs::read(&built).unwrap().starts_with(b"MZ"));

    pipe.install().unwrap();
    assert_ne!(sha(&game.join("game.exe")), exe_before);
    // Reading again while installed reads the original, not our English.
    let again = pipe.extract().unwrap();
    assert_eq!(again.lines, 6);
    assert!(pipe.live_lines().iter().all(|l| yakusu_core::protect::has_japanese(&l.jp)));
    pipe.restore().unwrap();
    assert_eq!(sha(&game.join("game.exe")), exe_before);
}

/// A LiveMaker 3 script with one MesNew whose PR_FONTCHANGEABLED is `font`
/// (the other properties empty), using the sample's property table.
fn mes_new_script(font: &[u8]) -> Vec<u8> {
    let g = bytes("gamemain.lsb");
    let (types, row) = (
        u32::from_le_bytes(g[5..9].try_into().unwrap()) as usize,
        u32::from_le_bytes(g[9..13].try_into().unwrap()) as usize,
    );
    let table = &g[13..13 + types * row];
    let mes = &table[0x0A * row..0x0B * row];
    let bit = |i: usize| mes[i / 8] & (1 << (i % 8)) != 0;
    let mut out = g[..13 + types * row].to_vec();
    out.extend_from_slice(&1u32.to_le_bytes()); // one command
    out.push(0x0A); // MesNew
    out.extend_from_slice(&[0, 0, 0, 0, 0, 0, 7, 0, 0, 0]); // indent, mute, not_update, line 7
    for i in 0..row * 8 {
        if bit(i) {
            if i == 0xA5 {
                out.extend_from_slice(font);
            } else {
                out.extend_from_slice(&0u32.to_le_bytes());
            }
        }
    }
    out
}

/// The expression "= <operand>".
fn assign(operand: &[u8]) -> Vec<u8> {
    let mut v = 1u32.to_le_bytes().to_vec();
    v.push(1); // To
    v.extend_from_slice(&4u32.to_le_bytes());
    v.extend_from_slice(b"____");
    v.extend_from_slice(&1u32.to_le_bytes());
    v.extend_from_slice(operand);
    v
}

#[test]
fn half_width_fix_flips_only_the_flag() {
    for (operand, want) in
        [(&[3u8, 1][..], true), (&[1, 1, 0, 0, 0][..], true), (&[3, 0][..], false), (&[1, 2, 0, 0, 0][..], false)]
    {
        let orig = mes_new_script(&assign(operand));
        let l = Lsb::parse(orig.clone()).unwrap();
        assert!(l.is_lm3());
        assert_eq!(l.has_font_flag(), want, "{operand:?}");
        let fixed = l.rebuild(&HashMap::new(), true);
        assert_eq!(fixed.len(), orig.len());
        let diff: Vec<usize> = (0..orig.len()).filter(|&i| orig[i] != fixed[i]).collect();
        assert_eq!(diff.len(), want as usize, "{operand:?}");
        assert!(diff.iter().all(|&i| orig[i] == 1 && fixed[i] == 0));
        assert!(!Lsb::parse(fixed).unwrap().has_font_flag());
    }
    // An expression that isn't a plain literal is left alone.
    let mut not_literal = assign(&[3, 1]);
    not_literal[0] = 2;
    not_literal.extend(assign(&[3, 1])[4..].to_vec());
    let l = Lsb::parse(mes_new_script(&not_literal)).unwrap();
    assert!(!l.has_font_flag());
}

#[test]
fn variables_inside_text_are_kept() {
    use yakusu_core::engines::livemaker::tpword::{Glyph, VAR};
    let l = Lsb::parse(bytes("00000001.lsb")).unwrap();
    let mut tw = TpWord::parse(l.text(0)).unwrap();
    assert!(tw.version >= 105);
    // "Hello <name>world!" with a variable glyph after "Hello ".
    let mut raw = vec![VAR];
    raw.extend_from_slice(&[0; 4 + 4 + 4 + 4]); // condition, decorator, unk3, link
    raw.extend_from_slice(&4u32.to_le_bytes());
    raw.extend_from_slice(b"Name");
    let at = tw.blocks()[0].start + 6;
    tw.glyphs.insert(at, Glyph { kind: VAR, raw: raw.clone() });
    let blocks = tw.blocks();
    assert_eq!(blocks[0].text, "Hello <v1>world!\nPage break");
    tw.replace(&blocks[0], "Hi <v1>, how are you?").unwrap();
    let again = TpWord::parse(&tw.to_bytes()).unwrap();
    assert_eq!(again.blocks()[0].text, "Hi <v1>, how are you?");
    assert!(again.glyphs.iter().any(|g| g.raw == raw));
    // An unknown code is refused, not written as text.
    let b = again.blocks();
    let mut tw2 = again.clone();
    assert!(tw2.replace(&b[0], "Bad <v9> code").is_err());
}
