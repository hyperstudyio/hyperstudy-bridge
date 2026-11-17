use std::path::PathBuf;

fn main() {
    let mut attributes = tauri_build::Attributes::new();
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        // Tauri embeds its Windows manifest (Common Controls 6) into the app binary only. Test binaries link the
        // same code without it, load Common Controls 5, and exit at start with STATUS_ENTRYPOINT_NOT_FOUND. So
        // the same manifest is embedded by the linker into every binary of this package, tests included, and
        // Tauri is told not to add its own copy to the app (two manifests fail the link).
        let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("windows-app-manifest.xml");
        println!("cargo:rerun-if-changed={}", manifest.display());
        println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg=/MANIFESTINPUT:{}", manifest.display());
        attributes = attributes
            .windows_attributes(tauri_build::WindowsAttributes::new_without_app_manifest());
    }
    tauri_build::try_build(attributes).expect("failed to run the Tauri build script");
}
