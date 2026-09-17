//! Available Drive revision metadata. This is not a complete edit log: Google
//! can omit revisions, and a revision's last modifier is not span authorship.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use url::Url;

use super::{GoogleService, GoogleSession, ImportError, json, parse_url, valid_id};

const MAX_REVISIONS: usize = 16;
const FIELDS: &str =
    "id,mimeType,modifiedTime,lastModifyingUser(displayName,emailAddress,permissionId),exportLinks";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RevisionCoverage {
    /// Even when there is no next page, Google's API may omit older revisions.
    ProviderMayOmitRevisions,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RevisionModifier {
    pub display_name: Option<String>,
    pub email_address: Option<String>,
    pub permission_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DriveRevision {
    pub id: String,
    pub mime_type: String,
    pub modified_time: Option<String>,
    /// Provider-reported last modifier, never an assertion of sole authorship.
    pub last_modifying_user: Option<RevisionModifier>,
    /// Available export MIME types, not bearer URLs or fetched content.
    pub export_mime_types: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DriveRevisionPage {
    /// Bind pagination to this file and the session's account at the product.
    pub file_id: String,
    pub revisions: Vec<DriveRevision>,
    pub next_page_token: Option<String>,
    pub coverage: RevisionCoverage,
}

impl GoogleSession {
    /// Reads one bounded page for an explicitly selected Drive file. The caller
    /// chooses whether to request another page. No hidden account-wide scan.
    /// Read-only OAuth scope does not grant access to a file's revision history
    /// when the signed-in user's file role lacks that permission.
    pub async fn drive_revisions(
        &self,
        file_id: &str,
        page_token: Option<&str>,
    ) -> Result<DriveRevisionPage, ImportError> {
        self.require_drive()?;
        let url = revision_url(file_id, None, page_token)?;
        let response = json(self.get(url).await?, 512 * 1024).await?;
        parse_page(file_id, response)
    }

    /// Metadata only. Historical Docs content must be exported from the
    /// selected revision's own export link; files.export would return head.
    pub async fn drive_revision(
        &self,
        file_id: &str,
        revision_id: &str,
    ) -> Result<DriveRevision, ImportError> {
        self.require_drive()?;
        let url = revision_url(file_id, Some(revision_id), None)?;
        let response: WireRevision = json(self.get(url).await?, 64 * 1024).await?;
        let revision = parse_revision(response)?;
        if revision.id != revision_id {
            return Err(ImportError::Response);
        }
        Ok(revision)
    }

    fn require_drive(&self) -> Result<(), ImportError> {
        if self.service != GoogleService::Drive {
            return Err(ImportError::Invalid(
                "Revision history requires a Drive connection.",
            ));
        }
        Ok(())
    }
}

fn revision_url(
    file_id: &str,
    revision_id: Option<&str>,
    page_token: Option<&str>,
) -> Result<Url, ImportError> {
    if !valid_id(file_id) || revision_id.is_some_and(|id| !valid_id(id)) {
        return Err(ImportError::Invalid("Invalid Drive file or revision ID."));
    }
    if page_token.is_some_and(|s| s.is_empty() || s.len() > 4096) {
        return Err(ImportError::Invalid("Invalid revision page token."));
    }
    let mut url = parse_url(&format!(
        "https://www.googleapis.com/drive/v3/files/{file_id}/revisions"
    ))?;
    if let Some(id) = revision_id {
        url.path_segments_mut()
            .map_err(|_| ImportError::Response)?
            .push(id);
        url.query_pairs_mut().append_pair("fields", FIELDS);
    } else {
        url.query_pairs_mut()
            .append_pair("pageSize", "16")
            .append_pair("fields", &format!("nextPageToken,revisions({FIELDS})"));
        if let Some(token) = page_token {
            url.query_pairs_mut().append_pair("pageToken", token);
        }
    }
    Ok(url)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WirePage {
    #[serde(default)]
    revisions: Vec<WireRevision>,
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireRevision {
    id: String,
    mime_type: String,
    modified_time: Option<String>,
    last_modifying_user: Option<WireModifier>,
    #[serde(default)]
    export_links: BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireModifier {
    display_name: Option<String>,
    email_address: Option<String>,
    permission_id: Option<String>,
}

fn parse_page(file_id: &str, wire: WirePage) -> Result<DriveRevisionPage, ImportError> {
    if wire.revisions.len() > MAX_REVISIONS
        || wire
            .next_page_token
            .as_ref()
            .is_some_and(|s| s.is_empty() || s.len() > 4096)
    {
        return Err(ImportError::Response);
    }
    let revisions = wire
        .revisions
        .into_iter()
        .map(parse_revision)
        .collect::<Result<Vec<_>, _>>()?;
    let mut ids = std::collections::BTreeSet::new();
    if revisions.iter().any(|r| !ids.insert(&r.id)) {
        return Err(ImportError::Response);
    }
    Ok(DriveRevisionPage {
        file_id: file_id.into(),
        revisions,
        next_page_token: wire.next_page_token,
        coverage: RevisionCoverage::ProviderMayOmitRevisions,
    })
}

fn parse_revision(wire: WireRevision) -> Result<DriveRevision, ImportError> {
    if !valid_id(&wire.id)
        || wire.mime_type.is_empty()
        || wire.mime_type.len() > 256
        || too_long(&wire.modified_time, 128)
        || wire.export_links.len() > 32
        || wire
            .export_links
            .iter()
            .any(|(mime, url)| mime.is_empty() || mime.len() > 256 || url.len() > 8192)
    {
        return Err(ImportError::Response);
    }
    let last_modifying_user = wire
        .last_modifying_user
        .map(|user| {
            if too_long(&user.display_name, 1024)
                || too_long(&user.email_address, 320)
                || too_long(&user.permission_id, 256)
            {
                return Err(ImportError::Response);
            }
            Ok(RevisionModifier {
                display_name: user.display_name,
                email_address: user.email_address,
                permission_id: user.permission_id,
            })
        })
        .transpose()?;
    Ok(DriveRevision {
        id: wire.id,
        mime_type: wire.mime_type,
        modified_time: wire.modified_time,
        last_modifying_user,
        export_mime_types: wire.export_links.into_keys().collect(),
    })
}

fn too_long(value: &Option<String>, limit: usize) -> bool {
    value.as_ref().is_some_and(|s| s.len() > limit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn revision_requests_bind_file_revision_fields_and_opaque_page_token() {
        let url = revision_url("file_1", None, Some("opaque+/=&next")).expect("valid page request");
        assert_eq!(url.host_str(), Some("www.googleapis.com"));
        assert_eq!(url.path(), "/drive/v3/files/file_1/revisions");
        let pairs: BTreeMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(pairs["pageToken"], "opaque+/=&next");
        assert_eq!(pairs["pageSize"], "16");
        assert!(pairs["fields"].contains("lastModifyingUser"));
        let exact = revision_url("file_1", Some("rev-2"), None).expect("valid revision request");
        assert!(exact.path().ends_with("/revisions/rev-2"));
        assert!(!exact.path().ends_with("/export"));
        for bad in ["../other", "a/b", "https://evil.test", "", "a?alt=media"] {
            assert!(revision_url(bad, None, None).is_err());
            assert!(revision_url("file", Some(bad), None).is_err());
        }
    }

    #[test]
    fn exhausted_page_is_still_partial_history_and_unknown_author_stays_unknown() {
        let page: WirePage =
            serde_json::from_value(json!({"revisions":[{"id":"r", "mimeType":"text/plain"}]}))
                .expect("valid wire fixture");
        let page = parse_page("f", page).expect("valid revision page");
        assert_eq!(page.coverage, RevisionCoverage::ProviderMayOmitRevisions);
        assert!(page.next_page_token.is_none());
        assert!(page.revisions[0].last_modifying_user.is_none());
        assert!(page.revisions[0].modified_time.is_none());
        assert_eq!(page.file_id, "f");
    }

    #[test]
    fn export_urls_are_not_exposed_as_history_or_download_authority() {
        let wire = serde_json::from_value(json!({
            "id":"r", "mimeType":"application/vnd.google-apps.document",
            "exportLinks":{"text/plain":"https://evil.test/?secret=token"},
            "lastModifyingUser":{"displayName":"Writer"}
        }))
        .expect("valid wire fixture");
        let revision = parse_revision(wire).expect("valid revision metadata");
        assert_eq!(revision.export_mime_types, vec!["text/plain"]);
        assert!(
            !serde_json::to_string(&revision)
                .expect("serialize metadata")
                .contains("secret")
        );
        assert_eq!(
            revision
                .last_modifying_user
                .expect("modifier supplied by fixture")
                .display_name
                .as_deref(),
            Some("Writer")
        );
    }

    #[test]
    fn pages_reject_oversize_duplicates_and_malformed_metadata() {
        let oversized: Vec<_> = (0..17)
            .map(|index| json!({"id":format!("r{index}"), "mimeType":"text/plain"}))
            .collect();
        let duplicates = vec![json!({"id":"r", "mimeType":"text/plain"}); 2];
        for rows in [oversized, duplicates] {
            let wire =
                serde_json::from_value(json!({"revisions":rows})).expect("valid wire fixture");
            assert!(parse_page("f", wire).is_err());
        }
        assert!(serde_json::from_value::<WirePage>(json!({"revisions":"bad"})).is_err());
        let wire =
            serde_json::from_value(json!({"revisions":[], "nextPageToken":"x".repeat(4097)}))
                .expect("valid wire fixture");
        assert!(parse_page("f", wire).is_err());
        let wire = serde_json::from_value(json!({"id":"r", "mimeType":"text/plain", "lastModifyingUser":{"emailAddress":"x".repeat(321)}})).expect("valid wire fixture");
        assert!(parse_revision(wire).is_err());
    }

    #[test]
    fn gmail_session_cannot_query_drive_revisions() {
        let session = GoogleSession {
            account_email: String::new(),
            client: super::super::client().expect("HTTP client"),
            token: String::new(),
            service: GoogleService::Gmail,
        };
        assert!(matches!(
            session.require_drive(),
            Err(ImportError::Invalid(_))
        ));
    }
}
