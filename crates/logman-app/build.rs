//! Embeds the application icon into the Windows executable.
//!
//! The icon goes in under resource ID 1, which is not arbitrary: gpui's
//! Windows backend loads exactly `LoadImageW(module, MAKEINTRESOURCE(1), ...)`
//! for the window class icon (see vendor/gpui/src/platform/windows/
//! platform.rs, `load_icon`). One embedded icon therefore covers Explorer,
//! the taskbar and the running window. Other platforms have no build step:
//! a bare binary carries no icon on macOS (that needs an .app bundle) or
//! Linux (that needs a .desktop entry).

fn main() {
    println!("cargo:rerun-if-changed=../../assets/icon.ico");

    #[cfg(windows)]
    {
        if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
            winresource::WindowsResource::new()
                .set_icon_with_id("../../assets/icon.ico", "1")
                .compile()
                .expect("failed to embed the Windows icon resource");
        }
    }
}
