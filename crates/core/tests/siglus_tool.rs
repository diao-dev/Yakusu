//! The automatic SiglusEngine key search through siglus-static-key-tool,
//! with a stand-in tool (its own test binary: the tool override is an
//! environment variable).

#![cfg(unix)]

mod common;
use common::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use yakusu_core::engines::siglus::{key, pck, ss, ss::asm};

#[test]
fn key_tool_output_is_checked_and_used() {
    let tmp = tempfile::tempdir().unwrap();
    let real: [u8; 16] = *b"0123456789abcdef";
    let mut code = asm::nl(1);
    code.extend(asm::text(0, 0));
    code.push(asm::EOF);
    let scene = ss::build(&code, &["こんにちは。"], 1, true);
    let game = tmp.path().join("g");
    fs::create_dir_all(&game).unwrap();
    fs::write(game.join("Scene.pck"), pck::build(&[("s".into(), scene)], Some(&real), true)).unwrap();
    fs::write(game.join("Gameexe.dat"), b"x").unwrap();

    // The stand-in prints a wrong candidate first, then the real key, and
    // records the arguments it was given.
    let args_log = tmp.path().join("args.txt");
    let tool = tmp.path().join("siglus-static-key-tool");
    fs::write(
        &tool,
        format!(
            "#!/bin/sh\necho \"$@\" >> '{}'\necho 'status         : candidate(s) found'\n\
             echo 'candidate 01   : 00 11 22 33 44 55 66 77 88 99 AA BB CC DD EE FF'\n\
             echo 'key            : {}'\n",
            args_log.display(),
            key::hex(&real)
        ),
    )
    .unwrap();
    fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
    std::env::set_var("YAKUSU_SIGLUS_STATIC_KEY_TOOL", &tool);

    let e = env(tmp.path());
    let pipe = pipeline(&e, &game, settings("quick", 10, 1), fake().0);
    assert_eq!(pipe.extract().unwrap().lines, 1);
    assert_eq!(pipe.p.meta().engine.as_ref().unwrap().str("siglus_key"), Some(key::hex(&real).as_str()));
    let args = fs::read_to_string(&args_log).unwrap();
    assert!(args.contains("--scene") && args.contains("--game") && args.contains("Gameexe.dat"), "{args}");
    // Read again: the saved key is used, the tool isn't run a second time.
    pipe.extract().unwrap();
    assert_eq!(fs::read_to_string(&args_log).unwrap().lines().count(), 1);
}
