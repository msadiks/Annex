fn main() {
    // The app only exists on macOS; elsewhere the crate is a stub.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        tauri_build::build();
    }
}
