#![forbid(unsafe_code)]
//! Give `generate_context!` an `OUT_DIR` and target identity, without source-tree
//! schema generation. This probe has no IPC commands or capability manifests.
fn main() {
    println!("cargo:rerun-if-changed=tauri.conf.json");
    println!("cargo:rerun-if-changed=icons/icon.png");
    let target = std::env::var("TARGET").expect("Cargo target triple");
    println!("cargo:rustc-env=TAURI_ENV_TARGET_TRIPLE={target}");
}
