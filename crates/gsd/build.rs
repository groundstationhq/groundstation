//! Embeds the built UI (`ui/dist`) into the daemon. Without a UI build the
//! daemon still compiles and serves a placeholder page, so `cargo build` never
//! needs Node. Release builds run `npm run build` in `ui/` first.

use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let src = manifest.join("../../ui/dist");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("ui");

    println!("cargo:rerun-if-changed={}", src.display());
    println!(
        "cargo:rerun-if-changed={}",
        src.join("index.html").display()
    );

    let _ = fs::remove_dir_all(&out);
    fs::create_dir_all(&out).unwrap();

    if src.join("index.html").is_file() {
        copy_dir(&src, &out);
        println!("cargo:rustc-env=GSD_UI_EMBEDDED=1");
    } else {
        fs::write(out.join("index.html"), PLACEHOLDER).unwrap();
        println!("cargo:rustc-env=GSD_UI_EMBEDDED=0");
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
<body><main><h1>Ground Station is running.</h1>
<p>This build of <code>gsd</code> was compiled without the UI. The API is up at <code>/v1/health</code>.</p>
<p>To include the UI, run <code>npm run build</code> in <code>ui/</code> and rebuild <code>gsd</code>, or install a release build from <a href="https://groundstation.sh/install" style="color:#3ee0c0">groundstation.sh</a>.</p></main></body></html>
"#;
