//! Links the trimmed static FFmpeg (see tools/ffmpeg/build-mingw.sh), embeds
//! the icon and manifest, and finds the WebView2Loader.dll that src/gui.rs
//! embeds.

use std::path::{Path, PathBuf};

/// The x64 WebView2Loader.dll shipped inside the locked `webview2-com-sys`
/// crate (or `ANNEX_WEBVIEW2_LOADER`).
fn webview2_loader(root: &Path) -> PathBuf {
    if let Ok(p) = std::env::var("ANNEX_WEBVIEW2_LOADER") {
        return PathBuf::from(p);
    }
    let lock = std::fs::read_to_string(root.join("Cargo.lock")).expect("reading Cargo.lock");
    let version = lock
        .split("[[package]]")
        .find(|p| p.contains("name = \"webview2-com-sys\""))
        .and_then(|p| p.lines().find_map(|l| l.strip_prefix("version = \"")))
        .map(|v| v.trim_end_matches('"').to_string())
        .expect("webview2-com-sys in Cargo.lock");
    let cargo_home = std::env::var_os("CARGO_HOME").map(PathBuf::from).unwrap_or_else(|| {
        let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).expect("home folder");
        PathBuf::from(home).join(".cargo")
    });
    let crate_dir = format!("webview2-com-sys-{version}");
    std::fs::read_dir(cargo_home.join("registry").join("src"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|d| d.path().join(&crate_dir).join("x64").join("WebView2Loader.dll"))
        .find(|p| p.is_file())
        .unwrap_or_else(|| panic!("WebView2Loader.dll from {crate_dir} not found; set ANNEX_WEBVIEW2_LOADER"))
}

fn main() {
    println!("cargo:rerun-if-env-changed=ANNEX_FFMPEG_DIR");
    println!("cargo:rerun-if-env-changed=ANNEX_WEBVIEW2_LOADER");
    println!("cargo:rerun-if-changed=resources/annex.rc");
    println!("cargo:rerun-if-changed=resources/annex.manifest");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("../..");
    let loader = webview2_loader(&root);
    println!("cargo:rerun-if-changed={}", loader.display());
    println!("cargo:rustc-env=ANNEX_WEBVIEW2_LOADER={}", loader.display());
    let ff = std::env::var("ANNEX_FFMPEG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| root.join("third_party/ffmpeg-build"));
    let lib = ff.join("lib");
    if !lib.join("libavcodec.a").exists() {
        panic!(
            "FFmpeg not found at {}. Run tools/ffmpeg/build-mingw.sh or set ANNEX_FFMPEG_DIR.",
            lib.display()
        );
    }
    println!("cargo:rustc-link-search=native={}", lib.display());
    println!("cargo:rustc-link-lib=static=avcodec");
    println!("cargo:rustc-link-lib=static=avutil");
    for l in ["bcrypt", "ole32", "user32", "d3d11", "dxgi"] {
        println!("cargo:rustc-link-lib=dylib={l}");
    }
    embed_resource::compile("resources/annex.rc", embed_resource::NONE)
        .manifest_optional()
        .unwrap();
}
