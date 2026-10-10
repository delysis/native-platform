#![forbid(unsafe_code)]
//! Extract the reviewed titlebar's static SVG alternatives by semantic key.
//!
//! This is deliberately not a general Svelte interpreter. Unknown, ambiguous or
//! newly nested conditionals fail closed. The native application's independent
//! source-hash gate remains in build.rs. This file can also be tested with
//! `rustc --edition=2024 --test build_assets.rs` without building an application.
#[path = "src/icon.rs"]
mod icon;
use std::collections::BTreeMap;

fn unique<'a>(source: &'a str, marker: &str) -> Result<(&'a str, &'a str), String> {
    let mut matches = source.match_indices(marker);
    let (index, _) = matches
        .next()
        .ok_or_else(|| format!("Missing asset anchor: {marker}"))?;
    if matches.next().is_some() {
        return Err(format!("Ambiguous asset anchor: {marker}"));
    }
    Ok((&source[..index], &source[index + marker.len()..]))
}

fn button<'a>(source: &'a str, marker: &str) -> Result<&'a str, String> {
    let (before, after) = unique(source, marker)?;
    let start = before
        .rfind("<button")
        .ok_or("Asset anchor is outside a button")?;
    if before[start..].contains("</button>") {
        return Err("Asset anchor follows a closed button".into());
    }
    let end = after.find("</button>").ok_or("Unclosed titlebar button")?;
    if after[..end].contains("<button") {
        return Err("Nested titlebar button".into());
    }
    let marker_end = before.len() + marker.len();
    Ok(&source[start..marker_end + end + "</button>".len()])
}

fn svg(source: &str) -> Result<String, String> {
    let (before, after) = unique(source, "<svg ")?;
    let (body, _) = unique(after, "</svg>")?;
    let end = before.len() + "<svg ".len() + body.len() + "</svg>".len();
    let svg = &source[before.len()..end];
    if svg.contains(['{', '}']) {
        return Err("An unresolved Svelte expression reached an SVG asset".into());
    }
    Ok(svg.into())
}

/// The nine named assets consumed by the native scene's stable icon ABI.
pub fn extract(source: &str) -> Result<BTreeMap<&'static str, String>, String> {
    // Exclude script strings from marker lookup. The exact reviewed component
    // has one script; a changed component shape requires an explicit review.
    let (_, markup) = unique(source, "</script>")?;
    let mut assets = BTreeMap::new();
    for (key, marker) in [
        ("outline", "class=\"titlebar-button outline-toggle\""),
        ("add", "class=\"titlebar-button new-document-button\""),
        ("record", "class=\"titlebar-button microphone-toggle\""),
    ] {
        assets.insert(key, svg(button(markup, marker)?)?);
    }

    let suggestions = button(
        markup,
        "class:suggestions-toggle={suggestionInteraction === 'ghost'}",
    )?;
    let (_, conditional) = unique(suggestions, "{#if suggestionInteraction === 'ghost'}")?;
    let (ghost, rest) = unique(conditional, "{:else}")?;
    let (loompad, _) = unique(rest, "{/if}")?;
    assets.insert("ghost", svg(ghost)?);
    assets.insert("loompad", svg(loompad)?);

    let (_, panes) = unique(
        markup,
        "{#each paneSlots.filter(slot => slot.selected) as slot (slot.position)}",
    )?;
    let (panes, _) = panes
        .split_once("{/each}")
        .ok_or("Unclosed pane control loop")?;
    let (prefix, conditional) = unique(panes, "{#if slot.position === 'right'}")?;
    let (right, rest) = unique(conditional, "{:else if slot.position === 'bottom'}")?;
    let (bottom, rest) = unique(rest, "{:else}")?;
    let (main, suffix) = unique(rest, "{/if}")?;
    for (key, path) in [
        ("pane-right", right),
        ("pane-bottom", bottom),
        ("pane-main", main),
    ] {
        assets.insert(key, svg(&format!("{prefix}{path}{suffix}"))?);
    }
    // Main has both a configured and an unconfigured affordance. They must
    // resolve to exactly the same asset, not two independently maintained icons.
    let fallback = svg(button(
        markup,
        "aria-label={mainPaneOpen ? 'Collapse main pane' : 'Show main pane'}",
    )?)?;
    if assets.get("pane-main") != Some(&fallback) {
        return Err("Configured and fallback main-pane icons disagree".into());
    }
    // App.svelte delegates its selector arrow to the platform select widget.
    assets.insert(
        "selector",
        r#"<svg viewBox="0 0 24 24"><path d="M7 10L12 15L17 10"/></svg>"#.into(),
    );
    if assets.len() != icon::KEYS.len() || icon::KEYS.iter().any(|key| !assets.contains_key(key)) {
        return Err("The semantic titlebar asset set is incomplete".into());
    }
    Ok(assets)
}

#[cfg(test)]
mod tests {
    use super::*;
    const CURRENT: &str = include_str!("../../apps/loom/src/App.svelte");

    #[test]
    fn current_component_resolves_every_named_asset_and_all_three_pane_orientations() {
        let assets = extract(CURRENT).unwrap();
        assert_eq!(assets.len(), icon::KEYS.len());
        for key in icon::KEYS {
            assert!(assets.contains_key(key), "{key}");
            assert!(!assets[key].contains(['{', '}']), "{key}");
        }
        assert!(assets["add"].contains("M8 3v10M3 8h10"));
        assert!(assets["pane-main"].contains("M5 2.5v11M11 2.5v11"));
        assert!(assets["pane-right"].contains("M10 2.5v11"));
        assert!(assets["pane-bottom"].contains("M2 10h12"));
        assert!(assets["ghost"].contains("M3.5 15V8"));
        assert!(assets["loompad"].contains("viewBox=\"0 0 20 18\""));
    }

    #[test]
    fn missing_conditional_alternatives_and_unknown_svg_expressions_are_errors() {
        for (from, to) in [
            (
                "{:else if slot.position === 'bottom'}",
                "{:else if slot.position === 'side'}",
            ),
            (
                "{#if suggestionInteraction === 'ghost'}",
                "{#if suggestionInteraction === 'other'}",
            ),
            ("M8 3v10M3 8h10", "{unreviewedPath}"),
        ] {
            let changed = CURRENT.replacen(from, to, 1);
            assert_ne!(changed, CURRENT, "fixture anchor {from}");
            assert!(extract(&changed).is_err(), "{from}");
        }
    }

    #[test]
    fn duplicate_controls_or_different_fallback_main_icon_cannot_silently_win() {
        let duplicate = button(CURRENT, "class=\"titlebar-button outline-toggle\"").unwrap();
        assert!(extract(&format!("{CURRENT}\n{duplicate}")).is_err());
        let fallback = button(
            CURRENT,
            "aria-label={mainPaneOpen ? 'Collapse main pane' : 'Show main pane'}",
        )
        .unwrap();
        let changed = CURRENT.replace(
            fallback,
            &fallback.replace("M5 2.5v11M11 2.5v11", "M2 10h12"),
        );
        assert!(extract(&changed).is_err());
    }

    #[test]
    fn physical_order_does_not_choose_asset_identity() {
        let marker = "class=\"titlebar-button microphone-toggle\"";
        let record = button(CURRENT, marker).unwrap();
        let moved = format!("{}\n{record}", CURRENT.replacen(record, "", 1));
        assert_eq!(extract(CURRENT).unwrap(), extract(&moved).unwrap());
    }

    #[test]
    fn malformed_or_multiple_svg_bodies_fail_instead_of_selecting_the_first() {
        for malformed in [
            "<svg viewBox=\"0 0 16 16\">",
            "<svg a></svg><svg b></svg>",
            "<svg a>{#if x}<path/>{/if}</svg>",
        ] {
            assert!(svg(malformed).is_err());
        }
        assert!(button("x marker </button>", "marker").is_err());
        assert!(button("<button>x</button>marker</button>", "marker").is_err());
        assert!(button("<button>marker<button>x</button>", "marker").is_err());
    }
}
