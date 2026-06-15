//! Build script: embed the application icon as a Win32 resource on Windows, so
//! the icon shows on the `.exe` itself in Explorer and on shortcuts.
//!
//! This is independent of the in-app window/taskbar icon, which `main.rs` sets
//! at runtime via winit. A `.exe` resource icon, by contrast, is baked into the
//! PE binary at link time — which is what this does.

fn main() {
    // The resource format is PE-specific, so only do this when *targeting*
    // Windows (`CARGO_CFG_TARGET_OS` reflects the target, not the host, so this
    // stays correct under cross-compilation).
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    // The icon lives at the repository root, two levels up from this crate.
    // Build an absolute path (joining component-by-component keeps native
    // separators) so it resolves regardless of the resource compiler's cwd.
    let manifest = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let icon = manifest
        .join("..")
        .join("..")
        .join("icon")
        .join("librarian.ico");
    println!("cargo:rerun-if-changed={}", icon.display());

    let mut res = winresource::WindowsResource::new();
    res.set_icon(icon.to_str().expect("icon path is valid UTF-8"));
    if let Err(e) = res.compile() {
        // Don't fail the build if the resource compiler (rc.exe / windres) is
        // unavailable — the app still runs and keeps its runtime window icon.
        // The .exe just won't carry an embedded icon in this build.
        println!("cargo:warning=could not embed exe icon resource: {e}");
    }
}
