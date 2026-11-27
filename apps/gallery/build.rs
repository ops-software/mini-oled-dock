//! Embeds gallery.exe's manifest: asInvoker (the gallery never needs elevation, so there is no DOCK_NO_MANIFEST switch)
//! and per-monitor v2 DPI. API checked against the embed-manifest 1.5.1 source (`new_manifest`,
//! `requested_execution_level`, `dpi_awareness`).

use embed_manifest::manifest::{DpiAwareness, ExecutionLevel};
use embed_manifest::{embed_manifest, new_manifest};

fn main() {
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        let manifest = new_manifest("Kit.Gallery")
            .requested_execution_level(ExecutionLevel::AsInvoker)
            .dpi_awareness(DpiAwareness::PerMonitorV2);
        if let Err(e) = embed_manifest(manifest) {
            // A missing manifest must not fail the build, but it must be reported.
            println!("cargo:warning=cannot embed the manifest: {e}");
        }
    }
    println!("cargo:rerun-if-changed=build.rs");
}
