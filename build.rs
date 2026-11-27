//! Embeds the Windows manifest in the exe: asInvoker (the image asks for no elevation; the roles that need it, the
//! dock itself among them since it acts on windows of elevated apps, get it from the "Dock" task's highest-privileges
//! principal or from a "runas" relaunch, `roles::gate`, so the agent and --undo-user can run as the signed-in user),
//! per-monitor v2 DPI and the segment heap (less fragmentation).
//! Also delay-loads rarely used system DLLs so the watchdog and CLI modes never map them.
//! API checked against the embed-manifest 1.5.1 source (`embed_manifest`, `new_manifest`,
//! `ManifestBuilder::requested_execution_level` / `dpi_awareness` / `heap_type`).

use std::path::{Path, PathBuf};

use embed_manifest::manifest::{DpiAwareness, ExecutionLevel, HeapType};
use embed_manifest::{embed_manifest, new_manifest};

/// DLLs loaded lazily (only the dock proper uses them; the watchdog and CLI modes do not).
const DELAY_LOAD: &[&str] = &[
    "shell32.dll",
    "ole32.dll",
    "oleaut32.dll",
    "propsys.dll",
    "shlwapi.dll",
    "dwmapi.dll",
    "powrprof.dll",
    "psapi.dll",
    "bcrypt.dll",
    "winhttp.dll",
    "api-ms-win-shcore-scaling-l1-1-1.dll",
    "api-ms-win-core-winrt-l1-1-0.dll",
];

fn main() {
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        // Every build is asInvoker: elevation belongs to a role (`roles::gate`), not to the image. `DOCK_NO_MANIFEST`
        // (set by the test and preview builds) is still accepted and has no effect.
        let manifest = new_manifest("Dock.Shell")
            .requested_execution_level(ExecutionLevel::AsInvoker)
            .dpi_awareness(DpiAwareness::PerMonitorV2)
            .heap_type(HeapType::SegmentHeap);
        if let Err(e) = embed_manifest(manifest) {
            // A missing manifest must not fail the build, but it must be reported.
            println!("cargo:warning=cannot embed the manifest: {e}");
        }
        // Version resource (Task Manager name, file properties) and the application icon (id 1: Explorer,
        // Installed apps, the Settings window; regenerate it with tools/make-icon.ps1). No manifest
        // here: embed-manifest owns it.
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/dock.ico");
        res.set("ProductName", "mini-oled-dock")
            .set("FileDescription", "mini-oled-dock")
            .set("LegalCopyright", "MIT")
            .set("OriginalFilename", "dock.exe")
            .set("InternalName", "dock");
        if let Err(e) = res.compile() {
            println!("cargo:warning=cannot embed the version resource: {e}");
        }
        if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
            for dll in DELAY_LOAD {
                println!("cargo:rustc-link-arg-bins=/DELAYLOAD:{dll}");
            }
            println!("cargo:rustc-link-arg-bins=delayimp.lib");
            // The static imports are looked up in System32 only (LOAD_LIBRARY_SEARCH_SYSTEM32), before main runs.
            println!("cargo:rustc-link-arg-bins=/DEPENDENTLOADFLAG:0x800");
        }
    }
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=assets/dock.ico");
    // The keyed string tables (strings/en.toml, strings/it.toml): a bad strings file fails the build.
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR")).join("strings.rs");
    if let Err(e) = oled_i18n_build::generate(Path::new("strings"), &out) {
        panic!("strings/: {e}")
    }
}
