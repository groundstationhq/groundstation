//! Embeds the built UI (`ui/dist`). Without a UI build everything still
//! compiles and serves a placeholder page, so `cargo build` never needs Node.
//! Release builds run `npm run build` in `ui/` first.

use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let ui = manifest.join("../../ui");
    let src = ui.join("dist");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("ui");
    let built = src.join("index.html").is_file();

    // Cargo reruns this whenever a watched path is missing, and otherwise when
    // anything in a watched directory is newer than the last run. So watch the
    // UI build, made empty while there's none yet: `npm run build` writes into
    // it, and nothing else under ui/ (node_modules, an edited source file)
    // touches it. A checkout that can't be written to watches ui/ itself.
    println!("cargo:rerun-if-env-changed=GS_UI_REQUIRED");
    if built {
        println!("cargo:rerun-if-changed={}", src.display());
        println!(
            "cargo:rerun-if-changed={}",
            src.join("index.html").display()
        );
    } else if ui.is_dir() {
        let watched = match fs::create_dir_all(&src) {
            Ok(()) => &src,
            Err(_) => &ui,
        };
        println!("cargo:rerun-if-changed={}", watched.display());
    }

    let _ = fs::remove_dir_all(&out);
    fs::create_dir_all(&out).unwrap();

    if built {
        copy_dir(&src, &out);
        println!("cargo:rustc-env=GS_UI_EMBEDDED=1");
    } else {
        // Release builds set this, so they can't ship the placeholder by accident.
        assert!(
            std::env::var_os("GS_UI_REQUIRED").is_none_or(|v| v != "1"),
            "GS_UI_REQUIRED=1, but {} has no index.html: run `npm run build` in ui/ first",
            src.display()
        );
        fs::write(out.join("index.html"), PLACEHOLDER).unwrap();
        println!("cargo:rustc-env=GS_UI_EMBEDDED=0");
    }
}

fn copy_dir(from: &Path, to: &Path) {
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            fs::create_dir_all(&target).unwrap();
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap();
        }
    }
}

const PLACEHOLDER: &str = r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>Ground Station</title>
<meta name="color-scheme" content="dark">
<style>body{margin:0;min-height:100vh;display:grid;place-items:center;background:#0a0a0b;color:#a4a5ab;font:15px/1.6 system-ui,sans-serif}
main{max-width:520px;padding:32px}h1{color:#ededef;font-size:18px;margin:0 0 8px}code{font-family:ui-monospace,Menlo,monospace;color:#ededef}</style></head>
<body><main><h1>Ground Station is running without its UI.</h1>
<p>This build was compiled without <code>ui/dist</code>, so there is nothing to show here.</p>
<p>To include the UI, run <code>npm run build</code> in <code>ui/</code>, then build again. Release builds include it.</p></main></body></html>
"#;
