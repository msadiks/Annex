//! Links the trimmed static FFmpeg (see tools/ffmpeg/build-mingw.sh) and
//! embeds the icon and manifest.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=ANNEX_FFMPEG_DIR");
    println!("cargo:rerun-if-changed=resources/annex.rc");
    println!("cargo:rerun-if-changed=resources/annex.manifest");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("../..");
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
