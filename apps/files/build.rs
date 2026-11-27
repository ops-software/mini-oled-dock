//! Embeds files.exe's manifest and version resource. Always asInvoker: files never runs elevated (an elevated caller
//! starts it de-elevated), per-monitor v2 DPI, the segment heap and long paths. The display name of the app lives in
//! one string table only, so the version resource names the suite. API checked against the embed-manifest 1.5.1
//! source (`ManifestBuilder::long_path_aware`). Also generates the window's string table from strings/en.toml and
//! it.toml (FZ-I18N) and embeds assets/files.ico as icon 1 once that file exists.

use std::path::Path;

use embed_manifest::manifest::{DpiAwareness, ExecutionLevel, HeapType, Setting};
use embed_manifest::{embed_manifest, new_manifest};

/// The application icon (F1a.14 adds it); until then files.exe carries no icon.
const ICON: &str = "assets/files.ico";

fn main() {
    // The window's keyed strings: a broken or incomplete table fails the build with the generator's message.
    let out_dir = std::env::var_os("OUT_DIR").map(std::path::PathBuf::from).unwrap_or_default();
    if let Err(e) = oled_i18n_build::generate(Path::new("strings"), &out_dir.join("strings.rs")) {
        panic!("strings: {e}");
    }

    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        let manifest = new_manifest("Files.Engine")
            .requested_execution_level(ExecutionLevel::AsInvoker)
            .dpi_awareness(DpiAwareness::PerMonitorV2)
            .heap_type(HeapType::SegmentHeap)
            .long_path_aware(Setting::Enabled);
        if let Err(e) = embed_manifest(manifest) {
            // A missing manifest must not fail the build, but it must be reported.
            println!("cargo:warning=cannot embed the manifest: {e}");
        }
        // Version resource (Task Manager name, file properties) and, once it exists, the icon as resource 1 (the
        // window's WindowSpec.icon_resource). No manifest here: embed-manifest owns it.
        let mut res = winresource::WindowsResource::new();
        res.set("ProductName", "mini-oled-dock")
            .set("FileDescription", "mini-oled-dock files")
            .set("LegalCopyright", "MIT")
            .set("OriginalFilename", "files.exe")
            .set("InternalName", "files");
        if Path::new(ICON).exists() {
            res.set_icon_with_id(ICON, "1");
            println!("cargo:rerun-if-changed={ICON}");
        }
        // For the files binary only: the fault DLL example and the test executables carry no version resource.
        if let Err(e) = res.compile_for("files") {
            println!("cargo:warning=cannot embed the version resource: {e}");
        }
    }
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");
}
