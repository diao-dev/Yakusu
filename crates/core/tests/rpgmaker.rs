//! RPG Maker MV / MZ: event messages, choices, database, and a game end to end.

mod common;
use common::*;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use yakusu_core::engines::{rpgmaker, Entry};

fn cmd(code: i64, indent: i64, params: Value) -> Value {
    json!({"code": code, "indent": indent, "parameters": params})
}

fn map(mz: bool) -> Value {
    let header = if mz { json!(["Actor1", 0, 0, 2, "ハロルド"]) } else { json!(["Actor1", 0, 0, 2]) };
    json!({
        "displayName": "はじまりの村",
        "note": "",
        "events": [null, {"id": 1, "name": "EV001", "pages": [{"list": [
            cmd(101, 0, header),
            cmd(401, 0, json!(["こんにちは！"])),
            cmd(401, 0, json!(["\\C[2]旅人\\C[0]さん、ようこそ。"])),
            cmd(102, 0, json!([["はい", "いいえ"], 1, 0, 2, 0])),
            cmd(402, 0, json!([0, "はい"])),
            cmd(101, 1, json!(["", 0, 0, 2])),
            cmd(401, 1, json!(["よかった。"])),
            cmd(0, 1, json!([])),
            cmd(402, 0, json!([1, "いいえ"])),
            cmd(0, 1, json!([])),
            cmd(404, 0, json!([])),
            cmd(356, 0, json!(["ShowMessage 秘密"])),
            cmd(320, 0, json!([1, "ハロルド"])),
            cmd(0, 0, json!([]))
        ]}]}]
    })
}

fn system() -> Value {
    json!({
        "gameTitle": "桜の旅",
        "currencyUnit": "G",
        "elements": ["", "炎"],
        "skillTypes": ["", "魔法"],
        "weaponTypes": [""],
        "armorTypes": [""],
        "equipTypes": ["", "武器"],
        "terms": {
            "basic": ["レベル", "Lv"],
            "commands": ["戦う", null],
            "params": ["最大HP"],
            "messages": {"actionFailure": "%1には効かなかった！", "alwaysDash": "常時ダッシュ", "levelUp": "%1は%2 %3 に上がった！"}
        },
        "switches": ["", "イベント済み"]
    })
}

fn msgs(es: &[Entry]) -> Vec<(Option<&str>, &str)> {
    es.iter().map(|e| (e.name.as_deref(), e.message.as_str())).collect()
}

#[test]
fn rpgmaker_export() {
    let es = rpgmaker::export("Map001.json", &map(false), false);
    assert_eq!(
        msgs(&es),
        vec![
            (None, "はじまりの村"),
            (None, "こんにちは！\n\\C[2]旅人\\C[0]さん、ようこそ。"),
            (None, "はい"),
            (None, "いいえ"),
            (None, "よかった。"),
            (Some("ハロルド"), "ハロルド"),
        ]
    );
    // MZ: the speaker name box is its own (name) entry and the line's speaker.
    let es = rpgmaker::export("Map001.json", &map(true), true);
    assert_eq!(es[1].name.as_deref(), Some("ハロルド"));
    assert_eq!(es[2].name.as_deref(), Some("ハロルド"));
    assert!(es[2].message.starts_with("こんにちは"));
    // System terms; switch names (editor only) are left alone.
    let s = rpgmaker::export("System.json", &system(), false);
    let texts: Vec<&str> = s.iter().map(|e| e.message.as_str()).collect();
    assert_eq!(
        texts,
        vec![
            "桜の旅",
            "炎",
            "魔法",
            "武器",
            "レベル",
            "戦う",
            "最大HP",
            "%1には効かなかった！",
            "常時ダッシュ",
            "%1は%2 %3 に上がった！"
        ]
    );
    let db = json!([null, {"id": 1, "name": "ハロルド", "nickname": "", "profile": "旅の剣士。\n腕は確か。"}]);
    let a = rpgmaker::export("Actors.json", &db, false);
    assert_eq!(msgs(&a), vec![(Some("ハロルド"), "ハロルド"), (None, "旅の剣士。\n腕は確か。")]);
}

#[test]
fn rpgmaker_apply_splits_long_messages() {
    let v = map(false);
    let mut es = rpgmaker::export("Map001.json", &v, false);
    es[0].message = "Starting Village".into();
    es[1].message = "Hello!\nWelcome,\n\\C[2]traveler\\C[0].\nIt's been a while\nsince anyone came by.".into();
    es[2].message = "Yes".into();
    es[3].message = "No".into();
    es[5].message = "Harold".into();
    let out = rpgmaker::apply("Map001.json", &v, &es, false).unwrap();
    assert_eq!(out["displayName"], "Starting Village");
    let list = out["events"][1]["pages"][0]["list"].as_array().unwrap();
    let codes: Vec<i64> = list.iter().map(|c| c["code"].as_i64().unwrap()).collect();
    // 5 lines: a second "Show Text" with the same face carries the 5th.
    assert_eq!(codes[..8], [101, 401, 401, 401, 401, 101, 401, 102]);
    assert_eq!(list[5]["parameters"], json!(["Actor1", 0, 0, 2]));
    assert_eq!(list[6]["parameters"][0], "since anyone came by.");
    assert_eq!(list[7]["parameters"][0], json!(["Yes", "No"]));
    // "When" branches follow the new choice text; untranslated lines stay.
    assert_eq!(list[8]["parameters"], json!([0, "Yes"]));
    assert_eq!(list[10]["parameters"][0], "よかった。");
    assert_eq!(list[12]["parameters"], json!([1, "No"]));
    // Plugin commands are never touched.
    assert!(list.iter().any(|c| c["parameters"][0] == "ShowMessage 秘密"));
    assert!(list.iter().any(|c| c["code"] == 320 && c["parameters"][1] == "Harold"));
    // Same counts or it refuses.
    assert!(rpgmaker::apply("Map001.json", &v, &es[1..], false).is_err());
    // Untouched entries give the same JSON back.
    let same = rpgmaker::apply("Map001.json", &v, &rpgmaker::export("Map001.json", &v, false), false).unwrap();
    assert_eq!(same, v);
}

fn make_rm_game(root: &Path) -> PathBuf {
    let data = root.join("www/data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(root.join("www/js")).unwrap();
    std::fs::write(root.join("www/js/rpg_core.js"), "// core").unwrap();
    std::fs::write(root.join("Game.exe"), b"MZ nw").unwrap();
    let w = |n: &str, v: Value| std::fs::write(data.join(n), serde_json::to_vec(&v).unwrap()).unwrap();
    w("Map001.json", map(false));
    w("System.json", system());
    w("MapInfos.json", json!([null, {"id": 1, "name": "村マップ"}]));
    w(
        "CommonEvents.json",
        json!([null, {"id": 1, "name": "共通", "list": [cmd(101, 0, json!(["", 0, 0, 2])), cmd(401, 0, json!(["共通イベントのメッセージ。"])), cmd(0, 0, json!([]))]}]),
    );
    w("Items.json", json!([null, {"id": 1, "name": "ポーション", "description": "HPを500回復する。"}]));
    w("Actors.json", json!([null, {"id": 1, "name": "ハロルド", "nickname": "", "profile": ""}]));
    root.to_path_buf()
}

#[test]
fn full_flow_rpgmaker() {
    let tmp = tempfile::tempdir().unwrap();
    let env = env(tmp.path());
    let game = make_rm_game(&tmp.path().join("RM Game"));
    let before = sha(&game.join("www/data/Map001.json"));
    let (factory, _) = fake();
    let pipe = pipeline(&env, &game, settings("recommended", 4, 2), factory);
    let m = pipe.p.meta().engine.clone().unwrap();
    assert_eq!(m.engine_id, "rpgmaker");
    assert_eq!(m.name, "RPG Maker MV");
    let stats = pipe.extract().unwrap();
    assert!(stats.lines >= 15, "{}", stats.lines);
    pipe.draft_style().unwrap();
    pipe.translate(None, false).unwrap();
    let out = pipe.build().unwrap();
    assert!(out.applied >= 10, "{}", out.applied);
    pipe.install().unwrap();
    let read =
        |n: &str| -> Value { serde_json::from_slice(&std::fs::read(game.join("www/data").join(n)).unwrap()).unwrap() };
    let m1 = read("Map001.json");
    let list = m1["events"][1]["pages"][0]["list"].as_array().unwrap();
    assert!(list
        .iter()
        .filter(|c| c["code"] == 401)
        .all(|c| !yakusu_core::protect::has_japanese(c["parameters"][0].as_str().unwrap())));
    // The color code survives translation.
    assert!(list.iter().any(|c| c["code"] == 401 && c["parameters"][0].as_str().unwrap().contains("\\C[2]")));
    assert!(!yakusu_core::protect::has_japanese(read("Items.json")[1]["name"].as_str().unwrap()));
    // Editor-only names stay.
    assert_eq!(read("MapInfos.json")[1]["name"], "村マップ");
    pipe.restore().unwrap();
    assert_eq!(sha(&game.join("www/data/Map001.json")), before);
}

#[test]
fn rpgmaker_wait_codes_are_protected() {
    let p = yakusu_core::protect::protect("待って\\.\\.\\.ください\\!\\|", "\n");
    assert!(!p.text.contains('\\'), "{}", p.text);
}
