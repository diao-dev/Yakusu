//! The desktop window. Everything happens in yakusu-core; this shell only
//! forwards `call(command, args)` from the interface and supplies the folder
//! picker, drag and drop, and the bundled tools.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::Manager;
use yakusu_core::app::App;
use yakusu_core::config::Env;
use yakusu_core::llm;

/// msg-tool, VNTextPatch and version.dll, zipped into the executable by build.rs.
static TOOLS_ZIP: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/tools.zip"));
static TOOLS_HASH: &str = include_str!(concat!(env!("OUT_DIR"), "/tools.hash"));

struct Core(Arc<App>);

#[tauri::command]
async fn call(core: tauri::State<'_, Core>, cmd: String, args: Value) -> Result<Value, String> {
    let app = core.0.clone();
    // Core calls block (file work, the occasional API test): keep them off the UI thread.
    tauri::async_runtime::spawn_blocking(move || app.call(&cmd, &args))
        .await
        .map_err(|e| format!("Unexpected error: {e}"))?
}

/// The unpacked tools in `dir` are all still there: `.complete` names this
/// version and every file it lists exists. Antivirus software sometimes
/// quarantines msg-tool.exe after it was unpacked; then it's unpacked again.
fn tools_intact(dir: &Path) -> bool {
    let Ok(done) = std::fs::read_to_string(dir.join(".complete")) else { return false };
    let mut lines = done.lines();
    if lines.next().map(str::trim) != Some(TOOLS_HASH.trim()) {
        return false;
    }
    let files: Vec<&str> = lines.map(str::trim).filter(|l| !l.is_empty()).collect();
    if files.is_empty() {
        // Written by an older Yakusu (the hash only): check the main tool.
        return dir.join("msg-tool.exe").is_file() || dir.join("msg-tool").is_file();
    }
    files.iter().all(|rel| dir.join(rel).is_file())
}

/// Unpack the embedded tools once per version into the data folder.
fn unpack_tools(home: &Path) -> Option<PathBuf> {
    if TOOLS_ZIP.is_empty() {
        return None;
    }
    let _ = std::fs::create_dir_all(home);
    let dir = home.join(format!("tools-{TOOLS_HASH}"));
    if tools_intact(&dir) {
        return Some(dir);
    }
    let tmp = home.join(format!("tools-{TOOLS_HASH}.part"));
    let _ = std::fs::remove_dir_all(&tmp);
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(TOOLS_ZIP)).ok()?;
    let files: Vec<String> = (0..z.len())
        .filter_map(|i| z.by_index(i).ok().filter(|f| !f.is_dir()).and_then(|f| f.enclosed_name()))
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .collect();
    z.extract(&tmp).ok()?;
    std::fs::write(tmp.join(".complete"), format!("{}\n{}\n", TOOLS_HASH.trim(), files.join("\n"))).ok()?;
    let _ = std::fs::remove_dir_all(&dir);
    // Windows Defender holds a freshly written .exe or .dll for a moment
    // while it scans it, and the folder can't be renamed until it lets go.
    let mut moved = false;
    for _ in 0..10 {
        if std::fs::rename(&tmp, &dir).is_ok() {
            moved = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
    if !moved {
        // Still locked: use the tools where they are. The next start moves them.
        return tools_intact(&tmp).then_some(tmp);
    }
    // Older unpacked versions are no longer needed.
    if let Ok(rd) = std::fs::read_dir(home) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if n.starts_with("tools-") && e.path() != dir {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    Some(dir)
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let home = Env::system(PathBuf::new()).home;
            let tools = std::env::var_os("YAKUSU_TOOLS")
                .map(PathBuf::from)
                .or_else(|| unpack_tools(&home))
                .or_else(|| app.path().resource_dir().ok().map(|d| d.join("tools")))
                .unwrap_or_default();
            let factory = if std::env::var("YAKUSU_FAKE_LLM").as_deref() == Ok("1") {
                llm::fake::factory(Default::default(), None, std::time::Duration::from_millis(300))
            } else {
                llm::http_factory()
            };
            app.manage(Core(App::new(Env::new(home, tools), factory)));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![call])
        .run(tauri::generate_context!())
        .expect("error while running Yakusu");
}
