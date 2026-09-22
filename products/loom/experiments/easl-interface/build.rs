#![forbid(unsafe_code)]
//! Bind the native port to the reviewed current interface, including its assets.
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::PathBuf};

mod build_assets;

const REFERENCE: &str =
    "90349a54061790954cf8a88160e4e29a8a325d4d + Mine a43231626f5deb4ad8f6f08beb36dca40a236270";
const APP_SHA256: &str = "3d45a082bb51a6f11a06b865e5e6c5a198e1c836dbb58fe3ded8562d931a1725";
const CSS_SHA256: &str = "033cb7338db5f570352b7ea29f9406ce6c0482c1758cf34c49395cc949339bea";

fn main() {
    let review = std::env::var_os("CARGO_FEATURE_REVIEW_ONLY").is_some();
    let native = std::env::var_os("CARGO_FEATURE_NATIVE_APP").is_some();
    assert!(
        review != native,
        "Select exactly one of native-app or review-only; review-only requires --no-default-features"
    );
    let root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let mut source = String::new();
    let mut observed = BTreeMap::new();
    for (file, expected) in [("App.svelte", APP_SHA256), ("app.css", CSS_SHA256)] {
        let path = root.join("../../apps/loom/src").join(file);
        println!("cargo:rerun-if-changed={}", path.display());
        let bytes = std::fs::read(&path).expect("Current Loom interface source");
        let actual = format!("{:x}", Sha256::digest(&bytes));
        if native {
            assert_eq!(
                actual, expected,
                "Loom interface reference changed. Review the current view and its native port together before updating the reference hash."
            );
        }
        observed.insert(file, actual);
        if file == "App.svelte" {
            source = String::from_utf8(bytes).expect("UTF-8 Loom interface source");
        }
    }
    println!("cargo:rerun-if-changed=build_assets.rs");
    println!("cargo:rerun-if-changed=src/icon.rs");
    println!("cargo:rerun-if-changed=ui/loom.easl");
    let icons = build_assets::extract(&source).expect("Reviewed semantic titlebar assets");
    let icon_hashes: BTreeMap<_, _> = icons
        .iter()
        .map(|(key, svg)| (*key, format!("{:x}", Sha256::digest(svg.as_bytes()))))
        .collect();
    let easl = std::fs::read(root.join("ui/loom.easl")).expect("Embedded EASL interface");
    let observed_app_sha = observed["App.svelte"].clone();
    let output = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    std::fs::write(
        output.join("loom_titlebar_icons.json"),
        serde_json::to_vec(&icons).unwrap(),
    )
    .unwrap();
    std::fs::write(output.join("loom_ui_reference.json"), serde_json::to_vec_pretty(
        &serde_json::json!({
            "schema_version": 1,
            "qualification": if review { "unqualified-review-only" } else { "source-gated-not-live-qualified" },
            "expected_app_sha256": APP_SHA256,
            "expected_css_sha256": CSS_SHA256,
            "observed_sources": observed,
            "embedded_easl_sha256": format!("{:x}", Sha256::digest(&easl)),
            "icons": icon_hashes,
            "selectors": {
                "outline": "class=titlebar-button outline-toggle",
                "add": "class=titlebar-button new-document-button",
                "record": "class=titlebar-button microphone-toggle",
                "suggestions": "suggestionInteraction === 'ghost'",
                "panes": "paneSlots.filter(slot => slot.selected)",
                "selector": "native-authored platform-select arrow, not an App.svelte SVG",
                "fallback_main": "mainPaneOpen ? 'Collapse main pane' : 'Show main pane'"
            },
            "add_menu_icons": "none in the current App.svelte; text-only menu rows"
        })
    ).unwrap()).unwrap();
    println!("cargo:rustc-env=LOOM_UI_EXPECTED_APP_SHA256={APP_SHA256}");
    if review {
        println!("cargo:rustc-env=LOOM_UI_REFERENCE=UNQUALIFIED REVIEW ONLY");
        println!("cargo:rustc-env=LOOM_UI_REFERENCE_SHA256={observed_app_sha}");
    } else {
        println!("cargo:rustc-env=LOOM_UI_REFERENCE={REFERENCE}");
        println!("cargo:rustc-env=LOOM_UI_REFERENCE_SHA256={APP_SHA256}");
    }
}
