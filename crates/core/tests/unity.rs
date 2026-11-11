//! Unity bundles: real sample bundles from UnityPy's tests (MIT, see
//! fixtures/unity/README.md) read, rewritten and read back.

mod common;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use yakusu_core::engines::unity::bundle::{self, Bundle};
use yakusu_core::engines::unity::serialized::{self, SerializedFile};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/unity").join(name)
}

#[test]
fn real_bundles_read_and_rebuild_identically() {
    for f in ["banner_1", "atlas_test"] {
        let b = Bundle::open(&fixture(f)).unwrap();
        assert!(bundle::is_bundle(&fixture(f)));
        let data = b.data().unwrap();
        assert_eq!(b.nodes.len(), 2, "{f}");
        let cab = b.node(&data, &b.nodes[0]).unwrap();
        assert!(serialized::looks_serialized(cab));
        assert!(!serialized::looks_serialized(b.node(&data, &b.nodes[1]).unwrap()));
        let sf = SerializedFile::parse(cab).unwrap();
        assert_eq!(sf.version, 17);
        assert!(sf.unity.starts_with("2018.4"));
        assert!(sf.objects.iter().any(|o| o.class_id == 28)); // a Texture2D
        assert_eq!(sf.rebuild(cab, &HashMap::new()), cab, "{f}");
    }
}

#[test]
fn objects_can_grow_and_shrink() {
    let b = Bundle::open(&fixture("atlas_test")).unwrap();
    let data = b.data().unwrap();
    let cab = b.node(&data, &b.nodes[0]).unwrap().to_vec();
    let sf = SerializedFile::parse(&cab).unwrap();
    let (big, small) = (sf.objects[1].path_id, sf.objects[2].path_id);
    let grown: Vec<u8> = (0..5000u32).map(|i| i as u8).collect();
    let rep = HashMap::from([(big, grown.clone()), (small, vec![1, 2, 3])]);
    let new = sf.rebuild(&cab, &rep);
    let sf2 = SerializedFile::parse(&new).unwrap();
    assert_eq!(sf2.objects.len(), sf.objects.len());
    for (a, b2) in sf.objects.iter().zip(&sf2.objects) {
        assert_eq!(a.path_id, b2.path_id);
        assert_eq!(b2.start % 8, 0, "objects stay 8-byte aligned");
        let want: &[u8] = rep.get(&a.path_id).map(|v| v.as_slice()).unwrap_or(sf.object(&cab, a));
        assert_eq!(sf2.object(&new, b2), want);
    }
}

#[test]
fn bundles_rewrite_with_the_same_crc() {
    let tmp = tempfile::tempdir().unwrap();
    for f in ["banner_1", "atlas_test"] {
        let b = Bundle::open(&fixture(f)).unwrap();
        let data = b.data().unwrap();
        let cab_name = b.nodes[0].path.clone();
        let cab = b.node(&data, &b.nodes[0]).unwrap().to_vec();
        let sf = SerializedFile::parse(&cab).unwrap();
        let new_cab = sf.rebuild(&cab, &HashMap::from([(sf.objects[0].path_id, b"changed!".to_vec())]));
        let out = tmp.path().join(f);
        b.rewrite(&data, &HashMap::from([(cab_name.clone(), new_cab.clone())]), &out, true).unwrap();
        let b2 = Bundle::open(&out).unwrap();
        let data2 = b2.data().unwrap();
        assert_eq!(bundle::crc32(&data2), bundle::crc32(&data), "{f}: CRC kept");
        assert_eq!(b2.node(&data2, &b2.nodes[0]).unwrap(), new_cab.as_slice());
        assert_eq!(b2.nodes[0].path, cab_name);
        // The resource file comes through unchanged.
        assert_eq!(b2.node(&data2, &b2.nodes[1]).unwrap(), b.node(&data, &b.nodes[1]).unwrap());
        assert_eq!(std::fs::metadata(&out).unwrap().len(), {
            let mut h = [0u8; 64];
            use std::io::Read;
            std::fs::File::open(&out).unwrap().read_exact(&mut h).unwrap();
            let s = h.iter().skip(8).position(|&c| c == 0).unwrap() + 9 + 4; // skip "UnityFS\0", format
            let s = s + h[s..].iter().position(|&c| c == 0).unwrap() + 1;
            let s = s + h[s..].iter().position(|&c| c == 0).unwrap() + 1;
            u64::from_be_bytes(h[s..s + 8].try_into().unwrap())
        });
    }
}

#[test]
fn text_assets_round_trip() {
    // A TextAsset object as Unity writes it: name, text, both 4-aligned.
    let b = Bundle::open(&fixture("banner_1")).unwrap();
    let data = b.data().unwrap();
    let sf = SerializedFile::parse(b.node(&data, &b.nodes[0]).unwrap()).unwrap();
    let mut obj = Vec::new();
    obj.extend(5u32.to_le_bytes());
    obj.extend(b"story\0\0\0");
    obj.extend(6u32.to_le_bytes());
    obj.extend("こんに".as_bytes()[..6].to_vec());
    obj.extend([0, 0]);
    let (name, text, rest) = sf.text_asset(&obj).unwrap();
    assert_eq!((name.as_str(), text.as_slice(), rest), ("story", "こん".as_bytes(), obj.len()));
    let new = sf.with_text(&obj, "Hello there".as_bytes()).unwrap();
    let (name2, text2, rest2) = sf.text_asset(&new).unwrap();
    assert_eq!((name2.as_str(), text2.as_slice(), rest2), ("story", b"Hello there".as_slice(), new.len()));
    assert_eq!(new.len() % 4, 0);
}

#[test]
fn type_trees_walk_real_objects_exactly() {
    for f in ["banner_1", "atlas_test"] {
        let b = Bundle::open(&fixture(f)).unwrap();
        let data = b.data().unwrap();
        let cab = b.node(&data, &b.nodes[0]).unwrap();
        let sf = SerializedFile::parse(cab).unwrap();
        for o in &sf.objects {
            let obj = sf.object(cab, o);
            assert_eq!(sf.walked_len(obj, o), Some(obj.len()), "{f}: class {} path {}", o.class_id, o.path_id);
        }
        // Rename a sprite: the object still walks exactly, with the new name.
        let o = sf.objects.iter().find(|o| o.class_id == 213).unwrap();
        let obj = sf.object(cab, o);
        let strs = sf.strings(obj, o).unwrap();
        let name = strs.iter().find(|s| s.path == "Base.m_Name").unwrap_or_else(|| panic!("{strs:?}")).clone();
        let new = sf.with_strings(obj, &[(name, "スプライトの新しい名前".into())]);
        assert_eq!(sf.walked_len(&new, o), Some(new.len()));
        assert!(sf.strings(&new, o).unwrap().iter().any(|s| s.text == "スプライトの新しい名前"));
    }
}

/// A minimal SerializedFile (format 17, no type trees) holding TextAssets.
fn serialized_with_text(assets: &[(i64, &str, &str)]) -> Vec<u8> {
    let mut meta = Vec::new();
    meta.extend(b"2021.3.8f1\0");
    meta.extend(19i32.to_le_bytes()); // StandaloneWindows64
    meta.push(0); // no type trees
    meta.extend(1i32.to_le_bytes());
    meta.extend(49i32.to_le_bytes()); // TextAsset
    meta.push(0);
    meta.extend((-1i16).to_le_bytes());
    meta.extend([0u8; 16]);
    meta.extend((assets.len() as i32).to_le_bytes());
    let mut data = Vec::new();
    let mut table = Vec::new();
    for (pid, name, text) in assets {
        while data.len() % 8 != 0 {
            data.push(0);
        }
        let mut obj = Vec::new();
        obj.extend((name.len() as u32).to_le_bytes());
        obj.extend(name.as_bytes());
        while obj.len() % 4 != 0 {
            obj.push(0);
        }
        obj.extend((text.len() as u32).to_le_bytes());
        obj.extend(text.as_bytes());
        while obj.len() % 4 != 0 {
            obj.push(0);
        }
        table.push((*pid, data.len() as u32, obj.len() as u32));
        data.extend(obj);
    }
    // Object entries are 4-aligned relative to the file start (header is 20 bytes).
    for (pid, start, size) in table {
        while (20 + meta.len()) % 4 != 0 {
            meta.push(0);
        }
        meta.extend(pid.to_le_bytes());
        meta.extend(start.to_le_bytes());
        meta.extend(size.to_le_bytes());
        meta.extend(0i32.to_le_bytes());
    }
    meta.extend(0i32.to_le_bytes()); // script types
    meta.extend(0i32.to_le_bytes()); // externals
    meta.push(0); // user info
    let mut data_offset = 20 + meta.len();
    data_offset = data_offset.div_ceil(16) * 16;
    let total = data_offset + data.len();
    let mut out = Vec::new();
    out.extend((meta.len() as u32).to_be_bytes());
    out.extend((total as u32).to_be_bytes());
    out.extend(17u32.to_be_bytes());
    out.extend((data_offset as u32).to_be_bytes());
    out.extend([0, 0, 0, 0]); // little-endian
    out.extend(meta);
    out.resize(data_offset, 0);
    out.extend(data);
    out
}

#[test]
fn synthetic_serialized_file_parses() {
    let b = serialized_with_text(&[(1, "a", "x"), (7, "story", "テキスト")]);
    assert!(serialized::looks_serialized(&b));
    let sf = SerializedFile::parse(&b).unwrap();
    assert_eq!(sf.objects.len(), 2);
    let o = &sf.objects[1];
    assert_eq!((o.path_id, o.class_id), (7, 49));
    let (name, text, _) = sf.text_asset(sf.object(&b, o)).unwrap();
    assert_eq!((name.as_str(), text.as_slice()), ("story", "テキスト".as_bytes()));
}

const SCRIPT: &str = "; 第一章\n# start\n@back 教室\nKohaku: おはよう、今日もいい天気だね。\n僕は眠い目をこすった。\n@choice \"一緒に行く\" goto:.together\n@choice \"先に行く\" goto:.alone\n@stop\n";

#[test]
fn full_flow_unity_naninovel() {
    use common::*;
    let script = SCRIPT;
    let tmp = tempfile::tempdir().unwrap();
    let env = env(tmp.path());
    let game = tmp.path().join("Manosaba-like");
    let aa = game.join("Game_Data/StreamingAssets/aa/StandaloneWindows64");
    std::fs::create_dir_all(&aa).unwrap();
    std::fs::write(game.join("UnityPlayer.dll"), b"MZ").unwrap();
    std::fs::write(game.join("Game.exe"), b"MZ").unwrap();
    // A bundle with the script as a TextAsset (built from a real bundle).
    let b = Bundle::open(&fixture("banner_1")).unwrap();
    let data = b.data().unwrap();
    let cab = serialized_with_text(&[(3, "chapter1", script), (9, "notes", "not a script")]);
    let bundle_path = aa.join("naninovel-scripts_assets_all.bundle");
    b.rewrite(&data, &HashMap::from([(b.nodes[0].path.clone(), cab)]), &bundle_path, true).unwrap();
    let before = common::sha(&bundle_path);

    let (factory, _) = fake();
    let pipe = pipeline(&env, &game, settings("recommended", 4, 2), factory);
    assert_eq!(pipe.p.meta().engine.as_ref().unwrap().engine_id, "unity");
    let stats = pipe.extract().unwrap();
    assert_eq!(stats.lines, 4);
    pipe.draft_style().unwrap();
    pipe.translate(None, false).unwrap();
    let out = pipe.build().unwrap();
    assert_eq!(out.applied, 4);
    pipe.install().unwrap();
    let nb = Bundle::open(&bundle_path).unwrap();
    let nd = nb.data().unwrap();
    assert_eq!(
        bundle::crc32(&nd),
        bundle::crc32(
            &Bundle::open(
                &pipe.p.path(
                    "backup/Game_Data/StreamingAssets/aa/StandaloneWindows64/naninovel-scripts_assets_all.bundle"
                )
            )
            .unwrap()
            .data()
            .unwrap()
        )
    );
    let cab = nb.node(&nd, &nb.nodes[0]).unwrap();
    let sf = SerializedFile::parse(cab).unwrap();
    let o = sf.objects.iter().find(|o| o.path_id == 3).unwrap();
    let (_, text, _) = sf.text_asset(sf.object(cab, o)).unwrap();
    let text = String::from_utf8(text).unwrap();
    assert!(text.contains("@back 教室") && text.contains("goto:.together") && text.starts_with("; 第一章"));
    assert!(
        text.lines().filter(|l| l.starts_with("Kohaku: ")).all(|l| !yakusu_core::protect::has_japanese(l)),
        "{text}"
    );
    let other = sf.objects.iter().find(|o| o.path_id == 9).unwrap();
    assert_eq!(sf.text_asset(sf.object(cab, other)).unwrap().1, b"not a script");
    pipe.restore().unwrap();
    assert_eq!(common::sha(&bundle_path), before);
}

#[test]
fn texture_bundles_are_recognised_without_unpacking() {
    let mut c = Bundle::open(&fixture("atlas_test")).unwrap().classes().unwrap();
    c.sort();
    c.dedup();
    assert_eq!(c, vec![28, 142, 213, 687078895]);
}

#[test]
fn localization_documents_win_over_scripts() {
    use common::*;
    let tmp = tempfile::tempdir().unwrap();
    let env = env(tmp.path());
    let game = tmp.path().join("L10n Game");
    let aa = game.join("Game_Data/StreamingAssets/aa/StandaloneWindows64");
    std::fs::create_dir_all(&aa).unwrap();
    std::fs::write(game.join("UnityPlayer.dll"), b"MZ").unwrap();
    let b = Bundle::open(&fixture("banner_1")).unwrap();
    let data = b.data().unwrap();
    let put = |name: &str, assets: &[(i64, &str, &str)]| {
        let cab = serialized_with_text(assets);
        b.rewrite(&data, &HashMap::from([(b.nodes[0].path.clone(), cab)]), &aa.join(name), true).unwrap();
    };
    let doc = "# aa1\n; おはよう！\n早上好！\n\n# aa2|aa3\n; > @print \"#aa2\"\n; 前半|後半\n前|后\n\n# aa4\n; 三つ目\n第三\n";
    put("naninovel-scripts_assets_all.bundle", &[(3, "chapter1", SCRIPT)]);
    put("localization-zhhans_assets_all.bundle", &[(5, "chapter1", doc)]);
    put("localization-zhhant_assets_all.bundle", &[(5, "chapter1", doc)]);
    let (factory, _) = fake();
    let pipe = pipeline(&env, &game, settings("recommended", 4, 2), factory);
    let stats = pipe.extract().unwrap();
    assert_eq!(stats.lines, 3, "only the Simplified Chinese documents");
    assert_eq!(pipe.p.meta().engine.clone().unwrap().str("locale"), Some("Simplified Chinese"));
    pipe.draft_style().unwrap();
    pipe.translate(None, false).unwrap();
    pipe.build().unwrap();
    let out = pipe.p.work("out/Game_Data/StreamingAssets/aa/StandaloneWindows64");
    let names: Vec<String> =
        std::fs::read_dir(&out).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect();
    assert_eq!(names, vec!["localization-zhhans_assets_all.bundle".to_string()]);
    let nb = Bundle::open(&out.join(&names[0])).unwrap();
    let nd = nb.data().unwrap();
    let cab = nb.node(&nd, &nb.nodes[0]).unwrap();
    let sf = SerializedFile::parse(cab).unwrap();
    let text = String::from_utf8(sf.text_asset(sf.object(cab, &sf.objects[0])).unwrap().1).unwrap();
    assert!(text.contains("; おはよう！\n") && text.contains("# aa2|aa3\n"), "{text}");
    assert!(!text.contains("早上好") && !text.contains("前|后"), "{text}");
    let frag = text.lines().skip_while(|l| !l.starts_with("; 前半")).nth(1).unwrap();
    assert_eq!(frag.matches('|').count(), 1, "{text}");
}
