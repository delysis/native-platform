#![forbid(unsafe_code)]
//! Bind the native port to the reviewed current interface, including its assets.
use sha2::{Digest, Sha256};
use std::path::PathBuf;

const REFERENCE: &str =
    "90349a54061790954cf8a88160e4e29a8a325d4d + Mine a43231626f5deb4ad8f6f08beb36dca40a236270";
const APP_SHA256: &str = "3d45a082bb51a6f11a06b865e5e6c5a198e1c836dbb58fe3ded8562d931a1725";
const CSS_SHA256: &str = "033cb7338db5f570352b7ea29f9406ce6c0482c1758cf34c49395cc949339bea";

fn main() {
    let root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let mut source = String::new();
    for (file, expected) in [("App.svelte", APP_SHA256), ("app.css", CSS_SHA256)] {
        let path = root.join("../../apps/loom/src").join(file);
        println!("cargo:rerun-if-changed={}", path.display());
        let bytes = std::fs::read(&path).expect("Current Loom interface source");
        assert_eq!(
            format!("{:x}", Sha256::digest(&bytes)),
            expected,
            "Loom interface reference changed. Review the current view and its native port together before updating the reference hash."
        );
        if file == "App.svelte" {
            source = String::from_utf8(bytes).expect("UTF-8 Loom interface source");
        }
    }
    let mut icons = Vec::new();
    for class in [
        "outline-toggle",
        "new-document-button",
        "microphone-toggle",
        "suggestions-toggle",
        "loompad-toggle",
    ] {
        let marker = format!("class=\"titlebar-button {class}\"");
        let start = source.find(&marker).expect("Current titlebar control");
        let button = &source[start..];
        let button = &button[..button.find("</button>").expect("Titlebar button end")];
        let svg = &button[button.find("<svg ").expect("Titlebar SVG")..];
        let svg = &svg[..svg.find("</svg>").expect("SVG end") + 6];
        icons.push(svg.to_owned());
    }
    let pane_start = source
        .find("{#each paneSlots.filter")
        .expect("Current pane controls");
    let pane = &source[pane_start..];
    let svg = &pane[pane.find("<svg ").expect("Pane SVG")..];
    let svg = &svg[..svg.find("</svg>").expect("Pane SVG end") + 6];
    let (prefix, conditional) = svg
        .split_once("{#if slot.position === 'right'}")
        .expect("Pane icon orientation");
    let (right, bottom) = conditional.split_once("{:else}").expect("Bottom pane icon");
    let (bottom, suffix) = bottom
        .split_once("{/if}")
        .expect("Pane icon conditional end");
    icons.push(format!("{prefix}{right}{suffix}"));
    icons.push(format!("{prefix}{bottom}{suffix}"));
    let output = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    std::fs::write(
        output.join("loom_titlebar_icons.json"),
        serde_json::to_vec(&icons).unwrap(),
    )
    .unwrap();
    println!("cargo:rustc-env=LOOM_UI_REFERENCE={REFERENCE}");
    println!("cargo:rustc-env=LOOM_UI_REFERENCE_SHA256={APP_SHA256}");
}
