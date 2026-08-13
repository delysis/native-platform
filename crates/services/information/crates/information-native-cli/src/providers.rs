use crate::{
    CliResult, KiwixDiscoveryArgs, MetalinkDiscoveryArgs, StacDiscoveryArgs, StacTraversal, fail,
    json_value, transport_client, transport_policy,
};
use information_native_catalog::{
    DiscoveryRecord, MetalinkFile, parse_kiwix_opds_with_limit, parse_metalink4_with_limit,
    parse_stac_document_with_limit,
};
use serde::Serialize;
use std::collections::{BTreeSet, VecDeque};
use url::Url;

#[derive(Debug, Serialize)]
struct FetchPage {
    requested_uri: String,
    effective_uri: String,
    bytes: u64,
    network_used: bool,
    redirects: usize,
    source_attestation: Option<information_native_acquire::SourceAttestation>,
}

#[derive(Debug, Serialize)]
struct DiscoveryOutput {
    provider: &'static str,
    traversal: &'static str,
    complete: bool,
    bytes_fetched: u64,
    records_returned: usize,
    records_with_exact_install_metadata: usize,
    pages: Vec<FetchPage>,
    warnings: Vec<String>,
    records: Vec<DiscoveryRecord>,
}

#[derive(Debug, Serialize)]
struct MetalinkOutput {
    provider: &'static str,
    complete: bool,
    fetch: FetchPage,
    file: MetalinkFile,
    resolved_asset: information_native_catalog::DiscoveryAsset,
}

pub(crate) fn discover_kiwix(args: &KiwixDiscoveryArgs) -> CliResult<serde_json::Value> {
    validate_fetch_uri(&args.source_uri)?;
    let client = transport_client(&args.transport)?;
    let policy = transport_policy(&args.transport)?;
    let parser_limit = usize::try_from(args.max_total_bytes)
        .map_err(|_| crate::message_error("discovery byte limit does not fit this platform"))?;
    let mut next_uri = Some(args.source_uri.clone());
    let mut visited = BTreeSet::new();
    let mut pages = Vec::new();
    let mut records = Vec::new();
    let mut warnings = Vec::new();
    let mut bytes_fetched = 0_u64;
    let mut complete = true;

    while let Some(requested_uri) = next_uri.take() {
        if pages.len() >= args.max_pages {
            complete = false;
            warnings.push(format!(
                "page traversal stopped at the {}-page limit",
                args.max_pages
            ));
            break;
        }
        if !visited.insert(requested_uri.clone()) {
            complete = false;
            warnings.push("provider pagination encountered a repeated URI".to_string());
            break;
        }
        let remaining_bytes = args
            .max_total_bytes
            .checked_sub(bytes_fetched)
            .ok_or_else(|| crate::message_error("discovery byte accounting underflow"))?;
        if remaining_bytes == 0 {
            complete = false;
            warnings.push("provider traversal exhausted its byte budget".to_string());
            break;
        }

        let fetched =
            client.fetch_catalogue_with_policy(&requested_uri, remaining_bytes, &policy)?;
        let page_bytes = u64::try_from(fetched.bytes.len())
            .map_err(|_| crate::message_error("provider response length overflow"))?;
        bytes_fetched = bytes_fetched
            .checked_add(page_bytes)
            .ok_or_else(|| crate::message_error("discovery byte accounting overflow"))?;
        let effective_uri = fetched
            .final_source_uri
            .clone()
            .unwrap_or_else(|| requested_uri.clone());
        let source_url = validate_fetch_uri(&effective_uri)?;
        let feed = parse_kiwix_opds_with_limit(&fetched.bytes, &source_url, parser_limit)?;
        let following_uri = feed.next_page_uri().map(ToOwned::to_owned);
        let mut page_records = feed.discovery_records();
        let remaining_records = args.max_records.saturating_sub(records.len());
        if page_records.len() > remaining_records {
            page_records.truncate(remaining_records);
            complete = false;
            warnings.push(format!(
                "record collection stopped at the {}-record limit",
                args.max_records
            ));
        }
        records.extend(page_records);
        pages.push(FetchPage {
            requested_uri,
            effective_uri,
            bytes: page_bytes,
            network_used: fetched.network_used,
            redirects: fetched.redirects,
            source_attestation: fetched.source_attestation,
        });

        if records.len() == args.max_records {
            if following_uri.is_some() {
                complete = false;
                warnings.push("additional provider pages were not fetched".to_string());
            }
            break;
        }
        next_uri = following_uri;
    }

    warnings.sort();
    warnings.dedup();
    let records_with_exact_install_metadata = records
        .iter()
        .filter(|record| record.has_exact_install_metadata())
        .count();
    json_value(&DiscoveryOutput {
        provider: "kiwix_opds",
        traversal: "next_pages",
        complete,
        bytes_fetched,
        records_returned: records.len(),
        records_with_exact_install_metadata,
        pages,
        warnings,
        records,
    })
}

pub(crate) fn discover_stac(args: &StacDiscoveryArgs) -> CliResult<serde_json::Value> {
    validate_fetch_uri(&args.source_uri)?;
    let client = transport_client(&args.transport)?;
    let policy = transport_policy(&args.transport)?;
    let parser_limit = usize::try_from(args.max_total_bytes)
        .map_err(|_| crate::message_error("discovery byte limit does not fit this platform"))?;
    let mut queue = VecDeque::from([args.source_uri.clone()]);
    let mut visited = BTreeSet::new();
    let mut pages = Vec::new();
    let mut records = Vec::new();
    let mut warnings = Vec::new();
    let mut bytes_fetched = 0_u64;
    let mut complete = true;

    while let Some(requested_uri) = queue.pop_front() {
        if pages.len() >= args.max_documents {
            complete = false;
            warnings.push(format!(
                "STAC traversal stopped at the {}-document limit",
                args.max_documents
            ));
            break;
        }
        if !visited.insert(requested_uri.clone()) {
            continue;
        }
        let remaining_bytes = args
            .max_total_bytes
            .checked_sub(bytes_fetched)
            .ok_or_else(|| crate::message_error("discovery byte accounting underflow"))?;
        if remaining_bytes == 0 {
            complete = false;
            warnings.push("STAC traversal exhausted its byte budget".to_string());
            break;
        }

        let fetched =
            client.fetch_catalogue_with_policy(&requested_uri, remaining_bytes, &policy)?;
        let page_bytes = u64::try_from(fetched.bytes.len())
            .map_err(|_| crate::message_error("provider response length overflow"))?;
        bytes_fetched = bytes_fetched
            .checked_add(page_bytes)
            .ok_or_else(|| crate::message_error("discovery byte accounting overflow"))?;
        let effective_uri = fetched
            .final_source_uri
            .clone()
            .unwrap_or_else(|| requested_uri.clone());
        let source_url = validate_fetch_uri(&effective_uri)?;
        let document = parse_stac_document_with_limit(&fetched.bytes, &source_url, parser_limit)?;

        let mut document_records = document.discovery_records();
        let remaining_records = args.max_records.saturating_sub(records.len());
        if document_records.len() > remaining_records {
            document_records.truncate(remaining_records);
            complete = false;
            warnings.push(format!(
                "record collection stopped at the {}-record limit",
                args.max_records
            ));
        }
        records.extend(document_records);

        let child_uris = match args.traversal {
            StacTraversal::RootOnly => Vec::new(),
            StacTraversal::LatestChain => document
                .latest_child()
                .map(|link| vec![link.href.clone()])
                .unwrap_or_default(),
            StacTraversal::Children => document
                .child_links()
                .map(|link| link.href.clone())
                .collect(),
        };
        pages.push(FetchPage {
            requested_uri,
            effective_uri,
            bytes: page_bytes,
            network_used: fetched.network_used,
            redirects: fetched.redirects,
            source_attestation: fetched.source_attestation,
        });

        if records.len() == args.max_records {
            if !child_uris.is_empty() || !queue.is_empty() {
                complete = false;
                warnings.push("additional STAC documents were not fetched".to_string());
            }
            break;
        }

        let queue_ceiling = args.max_documents.saturating_sub(pages.len());
        for child_uri in child_uris {
            if queue.len() >= queue_ceiling {
                complete = false;
                warnings.push(
                    "STAC child fan-out was bounded by the document traversal limit".to_string(),
                );
                break;
            }
            queue.push_back(child_uri);
        }
    }

    if !queue.is_empty() {
        complete = false;
    }
    warnings.sort();
    warnings.dedup();
    let records_with_exact_install_metadata = records
        .iter()
        .filter(|record| record.has_exact_install_metadata())
        .count();
    json_value(&DiscoveryOutput {
        provider: "overture_stac",
        traversal: args.traversal.as_str(),
        complete,
        bytes_fetched,
        records_returned: records.len(),
        records_with_exact_install_metadata,
        pages,
        warnings,
        records,
    })
}

pub(crate) fn discover_metalink(args: &MetalinkDiscoveryArgs) -> CliResult<serde_json::Value> {
    validate_fetch_uri(&args.source_uri)?;
    let client = transport_client(&args.transport)?;
    let policy = transport_policy(&args.transport)?;
    let fetched = client.fetch_catalogue_with_policy(&args.source_uri, args.max_bytes, &policy)?;
    let effective_uri = fetched
        .final_source_uri
        .clone()
        .unwrap_or_else(|| args.source_uri.clone());
    let parser_limit = usize::try_from(args.max_bytes)
        .map_err(|_| crate::message_error("Metalink byte limit does not fit this platform"))?;
    let file = parse_metalink4_with_limit(&fetched.bytes, parser_limit)?;
    let resolved_asset = file.to_discovery_asset(file.file_name.clone(), None);
    let bytes = u64::try_from(fetched.bytes.len())
        .map_err(|_| crate::message_error("Metalink response length overflow"))?;
    json_value(&MetalinkOutput {
        provider: "metalink_4",
        complete: true,
        fetch: FetchPage {
            requested_uri: args.source_uri.clone(),
            effective_uri,
            bytes,
            network_used: fetched.network_used,
            redirects: fetched.redirects,
            source_attestation: fetched.source_attestation,
        },
        file,
        resolved_asset,
    })
}

fn validate_fetch_uri(value: &str) -> CliResult<Url> {
    let url = Url::parse(value)
        .map_err(|_| crate::message_error(format!("invalid provider URI: {value:?}")))?;
    if !matches!(url.scheme(), "http" | "https" | "file") {
        return fail(format!(
            "provider URI scheme {:?} is not supported",
            url.scheme()
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return fail("credentials embedded in provider URIs are forbidden");
    }
    Ok(url)
}
