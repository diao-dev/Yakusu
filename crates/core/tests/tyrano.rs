//! TyranoScript: speaker lines, choices and names, and the three ways the
//! scenario is shipped (loose, app.asar, zip in the exe), end to end.

mod common;
use common::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use yakusu_core::engines::ks::Dialect;
use yakusu_core::engines::{asar, tyrano, zipped, Entry};

const FIRST: &str = ";ティラノスクリプトのサンプル
[title name=\"桜の約束\"]
[chara_new name=\"akane\" storage=\"chara/akane/normal.png\" jname=\"あかね\"]
[chara_new name=\"やまと\" storage=\"chara/yamato/normal.png\" jname=\"やまと\"]
*start
[cm]
[bg storage=\"room.jpg\" time=\"100\"]
春の朝。窓の外では、桜が静かに散っていた。[l][r]
今日から新しい学校に通う。[p]
#akane:happy
おはよう！今日もいい天気だね。[p]
#やまと
……ああ、おはよう。[p]
#ゆい
わたしも一緒に行っていい？[p]
#
二人で駅まで歩いた。[p]
[glink color=\"blue\" storage=\"scene1.ks\" size=\"20\" text=\"一緒に行く\" target=\"*a\"]
[glink color=\"blue\" storage=\"scene1.ks\" size=\"20\" text=\"先に行く\" target=\"*b\"]
[s]
[iscript]
f.name = \"テスト\";
tf.text = \"[glink text='内部']\";
[endscript]
[ptext layer=0 x=10 y=10 text=\"第一章\" size=30]
";

const SECOND: &str = "*a
#あかね
じゃあ、一緒に行こう！[p]
[jump storage=\"first.ks\" target=\"*start\"]
";

fn dialect() -> Dialect {
    Dialect { hash_names: true, fixed_names: ["akane", "やまと"].iter().map(|s| s.to_string()).collect() }
}

fn msgs(es: &[Entry]) -> Vec<(Option<&str>, &str)> {
    es.iter().map(|e| (e.name.as_deref(), e.message.as_str())).collect()
}

#[test]
fn tyrano_export() {
    let es = tyrano::export(FIRST, &dialect());
    assert_eq!(
        msgs(&es),
        vec![
            (None, "春の朝。窓の外では、桜が静かに散っていた。[l][r]今日から新しい学校に通う。[p]"),
            (Some("akane"), "おはよう！今日もいい天気だね。[p]"),
            (Some("やまと"), "……ああ、おはよう。[p]"),
            (Some("ゆい"), "わたしも一緒に行っていい？[p]"),
            (None, "二人で駅まで歩いた。[p]"),
            (Some("あかね"), "あかね"),
            (Some("やまと"), "やまと"),
            (None, "一緒に行く"),
            (None, "先に行く"),
            (None, "第一章"),
        ]
    );
}

#[test]
fn tyrano_apply() {
    let d = dialect();
    let mut es = tyrano::export(FIRST, &d);
    let en = [
        "A spring morning. Outside, the cherry blossoms fell.[l][r]Today I start at a new school.[p]",
        "Morning! Lovely weather again.[p]",
        "...Yeah, morning.[p]",
        "Can I come too?[p]",
        "We walked to the station.[p]",
        "Akane",
        "Yamato",
        "Go \"together\"",
        "Go [ahead]",
        "Chapter One",
    ];
    for (e, t) in es.iter_mut().zip(en) {
        e.message = t.into();
    }
    es[1].name = Some("Akane".into());
    es[2].name = Some("Yamato".into());
    es[3].name = Some("Yui".into());
    let out = tyrano::apply(FIRST, &es, &d).unwrap();
    // Character ids are kept so sprites still work; plain names are translated.
    assert!(out.contains("\n#akane:happy\n") && out.contains("\n#やまと\n") && out.contains("\n#Yui\n"));
    assert!(out.contains("jname=\"Akane\"") && out.contains("name=\"akane\""));
    assert!(out.contains("text=\"Go 'together'\"") && out.contains("text=\"Go ahead\""));
    assert!(out.contains("text=\"Chapter One\" size=30"));
    // Script blocks and commands are untouched.
    assert!(out.contains("f.name = \"テスト\";") && out.contains("tf.text = \"[glink text='内部']\";"));
    assert!(out.contains("[bg storage=\"room.jpg\" time=\"100\"]"));
    assert_eq!(out.split('\n').count(), FIRST.split('\n').count());
    // Reading our output gives the English back, in the same shape.
    let again = tyrano::export(&out, &d);
    assert!(again.iter().all(|e| !yakusu_core::protect::has_japanese(&e.message)), "{again:?}");
    assert!(tyrano::apply(FIRST, &es[1..], &d).is_err());
}

#[derive(Clone, Copy, Debug)]
enum Ship {
    Loose,
    Asar,
    ExeZip,
}

fn make_tyrano(root: &Path, ship: Ship) -> PathBuf {
    std::fs::create_dir_all(root).unwrap();
    let files: Vec<(&str, &[u8])> = vec![
        ("data/scenario/first.ks", FIRST.as_bytes()),
        ("data/scenario/scene1.ks", SECOND.as_bytes()),
        ("data/system/Config.tjs", b";System.title = \"test\";"),
        ("tyrano/libs.js", b"// engine"),
        ("data/bgimage/room.jpg", b"\xFF\xD8 fake jpg"),
        ("package.json", b"{\"main\":\"index.html\"}"),
    ];
    match ship {
        Ship::Loose => {
            for (n, b) in &files {
                let p = root.join(n);
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(p, b).unwrap();
            }
            std::fs::write(root.join("Game.exe"), b"MZ nw").unwrap();
        }
        Ship::Asar => {
            std::fs::create_dir_all(root.join("resources")).unwrap();
            asar::create(&root.join("resources/app.asar"), &files).unwrap();
            std::fs::write(root.join("Game.exe"), b"MZ electron").unwrap();
        }
        Ship::ExeZip => {
            let mut stub = b"MZ".to_vec();
            stub.extend(vec![0x90; 4000]);
            zipped::create(&root.join("Game.exe"), &files, Some(&stub)).unwrap();
        }
    }
    root.to_path_buf()
}

fn read_back(game_or_out: &Path, ship: Ship, name: &str) -> String {
    let b = match ship {
        Ship::Loose => std::fs::read(game_or_out.join(name)).unwrap(),
        Ship::Asar => {
            let a = asar::Asar::open(&game_or_out.join("resources/app.asar")).unwrap();
            a.read(a.files.iter().find(|f| f.name == name).unwrap()).unwrap()
        }
        Ship::ExeZip => zipped::read(&game_or_out.join("Game.exe"), name).unwrap(),
    };
    String::from_utf8(b).unwrap()
}

#[test]
fn full_flow_tyrano_every_packaging() {
    for ship in [Ship::Loose, Ship::Asar, Ship::ExeZip] {
        let tmp = tempfile::tempdir().unwrap();
        let env = env(tmp.path());
        let game = make_tyrano(&tmp.path().join("Tyrano Game"), ship);
        let before: HashMap<String, String> = yakusu_core::engines::list_files(&game, None)
            .into_iter()
            .map(|r| (r.clone(), sha(&game.join(&r))))
            .collect();
        let (factory, _) = fake();
        let pipe = pipeline(&env, &game, settings("recommended", 4, 2), factory);
        let m = pipe.p.meta().engine.clone().unwrap();
        assert_eq!(m.engine_id, "tyrano", "{ship:?}");
        let stats = pipe.extract().unwrap();
        assert!(stats.lines >= 8, "{ship:?}: {}", stats.lines);
        pipe.draft_style().unwrap();
        pipe.translate(None, false).unwrap();
        let out = pipe.build().unwrap();
        assert!(out.applied >= 7, "{ship:?}: {}", out.applied);
        pipe.install().unwrap();
        let first = read_back(&game, ship, "data/scenario/first.ks");
        assert!(
            !yakusu_core::protect::has_japanese(
                &first
                    .replace("やまと", "")
                    .replace("テスト", "")
                    .replace("内部", "")
                    .replace("ティラノスクリプトのサンプル", "")
                    .replace("桜の約束", "")
            ),
            "{ship:?}: {first}"
        );
        assert!(first.contains("#akane:happy") && first.contains("[s]"));
        let second = read_back(&game, ship, "data/scenario/scene1.ks");
        assert!(!second.contains("一緒に行こう"), "{ship:?}");
        // Other files come through unchanged.
        if !matches!(ship, Ship::Loose) {
            let img = match ship {
                Ship::Asar => {
                    let a = asar::Asar::open(&game.join("resources/app.asar")).unwrap();
                    a.read(a.files.iter().find(|f| f.name == "data/bgimage/room.jpg").unwrap()).unwrap()
                }
                _ => zipped::read(&game.join("Game.exe"), "data/bgimage/room.jpg").unwrap(),
            };
            assert_eq!(img, b"\xFF\xD8 fake jpg");
        }
        if matches!(ship, Ship::ExeZip) {
            assert!(std::fs::read(game.join("Game.exe")).unwrap().starts_with(b"MZ\x90\x90"));
        }
        // Reading again while installed reads the originals.
        pipe.extract().unwrap();
        assert!(pipe.live_lines().iter().all(|l| yakusu_core::protect::has_japanese(&l.jp)));
        pipe.restore().unwrap();
        for (r, h) in &before {
            assert_eq!(&sha(&game.join(r)), h, "{ship:?}: {r} not restored");
        }
    }
}
