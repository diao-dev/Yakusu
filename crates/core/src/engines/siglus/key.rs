//! The 16-byte key a SiglusEngine game XORs its Scene.pck with (when
//! `scn_data_exe_angou_mod` is set). Tried in order: the key saved for the
//! project, a key.toml / key.txt in the game folder (as siglus_rs and
//! SiglusExtract write them), then siglus-static-key-tool (xmoezzz, MPL-2.0),
//! bundled with Yakusu, which recovers it from Scene.pck and Gameexe.dat or
//! from the game's .exe. Every candidate is checked by opening a scene.

use super::pck::Pck;
use crate::engines::{run_tool, Ctx};
use crate::error::{Error, Result};
use std::path::{Path, PathBuf};

pub fn hex(key: &[u8; 16]) -> String {
    key.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" ")
}

/// A key typed or pasted in any common way: "36 0F C9 ...", "360FC9...",
/// "0x36, 0x0F, ...", "36:0F:...", with or without brackets.
pub fn parse_hex(s: &str) -> Option<[u8; 16]> {
    let s = s.trim().trim_matches(|c| matches!(c, '[' | ']' | '"' | '\''));
    let tokens: Vec<&str> =
        s.split(|c: char| c.is_whitespace() || matches!(c, ',' | ':' | '-')).filter(|t| !t.is_empty()).collect();
    let digits: String = if tokens.len() == 16 {
        // One token per byte: "0x6" and "6" are allowed.
        tokens
            .iter()
            .map(|t| {
                let t = t.trim_start_matches("0x").trim_start_matches("0X");
                if t.len() == 1 {
                    format!("0{t}")
                } else {
                    t.to_string()
                }
            })
            .collect()
    } else {
        tokens.iter().map(|t| t.trim_start_matches("0x").trim_start_matches("0X")).collect()
    };
    if digits.len() != 32 || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&digits[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// `key = [0x36, 0x0F, ...]` (hex or decimal bytes, may span lines),
/// `key = "36 0F ..."` or `key_hex = "..."`, as in siglus_rs's key.toml.
pub fn parse_key_toml(text: &str) -> Option<[u8; 16]> {
    let mut collecting: Option<String> = None;
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if let Some(acc) = collecting.as_mut() {
            acc.push(' ');
            acc.push_str(line);
            if line.contains(']') {
                break;
            }
            continue;
        }
        let Some((lhs, rhs)) = line.split_once('=') else { continue };
        if !matches!(lhs.trim(), "key" | "key_hex" | "exe_key") {
            continue;
        }
        let rhs = rhs.trim();
        if rhs.starts_with('[') && !rhs.contains(']') {
            collecting = Some(rhs.to_string());
            continue;
        }
        if let Some(k) = parse_value(rhs) {
            return Some(k);
        }
    }
    collecting.and_then(|v| parse_value(&v))
}

fn parse_value(v: &str) -> Option<[u8; 16]> {
    let v = v.trim();
    if let Some(inner) = v.strip_prefix('[').and_then(|r| r.split(']').next()) {
        let bytes: Option<Vec<u8>> = inner
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(|t| match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
                Some(h) => u8::from_str_radix(h, 16).ok(),
                None => t.parse::<u8>().ok(),
            })
            .collect();
        return bytes.and_then(|b| b.try_into().ok());
    }
    parse_hex(v)
}

/// Every 16-byte key printed by siglus-static-key-tool ("key : 36 0F ..."
/// or "candidate 01 : 36 0F ..."), best first.
pub fn keys_in_tool_output(out: &str) -> Vec<[u8; 16]> {
    out.lines()
        .filter_map(|l| {
            let (label, rest) = l.split_once(':')?;
            let label = label.trim().to_lowercase();
            (label == "key" || label.starts_with("candidate")).then(|| parse_hex(rest)).flatten()
        })
        .collect()
}

fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    let exact = dir.join(name);
    if exact.is_file() {
        return Some(exact);
    }
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(name) && e.path().is_file())
        .map(|e| e.path())
}

/// The game's .exe for the tool's EXE mode: the biggest one that isn't an
/// installer, uninstaller or config tool.
fn game_exe(dir: &Path) -> Option<PathBuf> {
    let mut exes: Vec<(u64, PathBuf)> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| {
            let n = e.file_name().to_string_lossy().to_lowercase();
            n.ends_with(".exe")
                && !["unins", "setup", "config", "install", "vcredist", "dxsetup"].iter().any(|x| n.contains(x))
        })
        .filter_map(|e| Some((e.metadata().ok()?.len(), e.path())))
        .collect();
    exes.sort();
    exes.pop().map(|(_, p)| p)
}

/// The key for `pck` (read from `pck_path`, the original file), or a setup
/// request the interface answers with a key field.
pub fn resolve(cx: &Ctx, pck: &Pck, pck_path: &Path, saved: Option<&str>) -> Result<[u8; 16]> {
    if let Some(s) = saved {
        match parse_hex(s) {
            Some(k) if pck.check_key(&k) => return Ok(k),
            Some(_) => cx.log("The saved key doesn't open this Scene.pck; looking for the right one."),
            None => cx.log("The saved key isn't 16 hex bytes; looking for the right one."),
        }
    }
    let game = cx.game();
    let pck_dir = pck_path.parent().map(Path::to_path_buf).unwrap_or_else(|| game.clone());
    for dir in [game.clone(), pck_dir] {
        for name in ["key.toml", "key.txt"] {
            let Some(f) = find_file(&dir, name) else { continue };
            let text = std::fs::read_to_string(&f).unwrap_or_default();
            let k = parse_key_toml(&text).or_else(|| parse_hex(&text));
            match k {
                Some(k) if pck.check_key(&k) => {
                    cx.log(format!("Using the key from {}", f.display()));
                    return Ok(k);
                }
                Some(_) => cx.log(format!("The key in {} doesn't open this Scene.pck.", f.display())),
                None => {}
            }
        }
    }
    if let Some(tool) = cx.env.tool_path("siglus-static-key-tool") {
        let tool = tool.to_string_lossy().to_string();
        let gameexe = find_file(&game, "Gameexe.dat");
        let pck_arg = pck_path.to_string_lossy().to_string();
        let mut runs: Vec<Vec<String>> = Vec::new();
        if let Some(g) = &gameexe {
            runs.push(vec![
                tool.clone(),
                "--scene".into(),
                pck_arg.clone(),
                "--game".into(),
                g.to_string_lossy().into(),
            ]);
        }
        if let Some(exe) = game_exe(&game) {
            let mut r =
                vec![tool.clone(), "--exe".into(), exe.to_string_lossy().into(), "--scene".into(), pck_arg.clone()];
            if let Some(g) = &gameexe {
                r.extend(["--gameexe".into(), g.to_string_lossy().into()]);
            }
            runs.push(r);
        }
        for args in runs {
            cx.log("Looking for this game's Scene.pck key (this can take a minute)…");
            let Ok((_, out)) = run_tool(&args, cx) else { continue };
            if let Some(k) = keys_in_tool_output(&out).into_iter().find(|k| pck.check_key(k)) {
                cx.log(format!("Found the key: {}", hex(&k)));
                return Ok(k);
            }
        }
    } else {
        cx.log("siglus-static-key-tool isn't available, so the key can't be found automatically.");
    }
    Err(Error::Setup {
        need: "siglus_key",
        msg:
            "This SiglusEngine game's Scene.pck is encrypted, and Yakusu couldn't find its 16-byte key automatically. \
              Paste the key (32 hex digits), or put a key.toml with it in the game folder, then read the scripts again."
                .into(),
    })
}
