use information_native_catalog::parse_kiwix_opds;
use std::error::Error;
use url::Url;

fn owned_feed(relation: &str) -> String {
    format!(
        r#"<feed><id>owned-feed</id><title>Owned feed</title><entry><id>owned-entry</id><title>Owned entry</title><link rel="{relation}" href="https://example.invalid/payload.zim" type="application/x-zim" length="7" checksum="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"/></entry></feed>"#
    )
}

#[test]
fn unrelated_relation_fragment_does_not_enter_opds_discovery() -> Result<(), Box<dyn Error>> {
    let base = Url::parse("https://example.invalid/owned-catalog.xml")?;
    for relation in [
        "https://example.invalid/not-opds-spec.org/acquisition/decoy",
        "alternate https://example.invalid/opds-spec.org/acquisition/decoy",
    ] {
        let feed = parse_kiwix_opds(owned_feed(relation).as_bytes(), &base)?;
        assert_eq!(feed.entries.len(), 1);
        assert_eq!(feed.entries[0].links.len(), 1);
        assert_eq!(feed.entries[0].links[0].rel, relation);
        let records = feed.discovery_records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].upstream_id, "owned-entry");
        assert!(
            records[0].assets.is_empty() && !records[0].has_exact_install_metadata(),
            "unrelated relation {relation:?}: assets={:?}, exact_hint={}",
            records[0].assets,
            records[0].has_exact_install_metadata()
        );
    }
    Ok(())
}

#[test]
fn acquisition_prefix_retains_family_and_multiple_relation_tokens() -> Result<(), Box<dyn Error>> {
    let base = Url::parse("https://example.invalid/owned-catalog.xml")?;
    for relation in [
        "http://opds-spec.org/acquisition",
        "http://opds-spec.org/acquisition/open-access",
        "alternate \thttp://opds-spec.org/acquisition/buy \nrelated",
    ] {
        let feed = parse_kiwix_opds(owned_feed(relation).as_bytes(), &base)?;
        assert_eq!(feed.entries.len(), 1);
        assert_eq!(feed.entries[0].links.len(), 1);
        let records = feed.discovery_records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].assets.len(), 1, "valid relation {relation:?}");
        assert!(records[0].has_exact_install_metadata());
        let asset = &records[0].assets[0];
        assert_eq!(asset.href, "https://example.invalid/payload.zim");
        assert_eq!(asset.expected_bytes, Some(7));
        assert_eq!(asset.media_type.as_deref(), Some("application/x-zim"));
        assert_eq!(
            asset.sha256.as_deref(),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
        assert!(!asset.requires_resolution);
    }
    Ok(())
}
