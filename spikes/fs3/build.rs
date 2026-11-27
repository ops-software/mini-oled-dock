//! Embeds the probes' manifest (the repository's build.rs pattern): asInvoker, so a probe never asks for elevation,
//! and longPathAware, so row f measures the shell with long paths enabled for the process (the machine's own setting
//! still decides for plain paths). API checked against the embed-manifest 1.5.1 source (`new_manifest`,
//! `ManifestBuilder::requested_execution_level` / `long_path_aware` / `dpi_awareness`).

use embed_manifest::manifest::{DpiAwareness, ExecutionLevel, Setting};
use embed_manifest::{embed_manifest, new_manifest};

fn main() {
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        let manifest = new_manifest("Files.Spike")
            .requested_execution_level(ExecutionLevel::AsInvoker)
            .long_path_aware(Setting::Enabled)
            .dpi_awareness(DpiAwareness::PerMonitorV2);
        if let Err(e) = embed_manifest(manifest) {
            println!("cargo:warning=cannot embed the manifest: {e}");
        }
    }
    println!("cargo:rerun-if-changed=build.rs");
}
