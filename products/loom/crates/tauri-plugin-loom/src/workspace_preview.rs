//! Exact-revision HTML previews have their own inert origin and response CSP.
//! They cannot execute script, contact a network, submit forms, or navigate the
//! parent window, even when retained HTML contains active markup.

use super::*;

pub(super) const SCHEME: &str = "loom-preview";
const MAX_PREVIEW_BYTES: usize = 512 * 1024;
const PREVIEW_CSP: &str = "default-src 'none'; script-src 'none'; style-src 'unsafe-inline'; img-src data:; base-uri 'none'; form-action 'none'; frame-src 'none'; sandbox";

#[derive(Debug)]
struct PreviewRequest {
    project: ProjectId,
    session: CommandId,
    document: DocumentId,
    revision: RevisionId,
    blob: BlobId,
}

impl PreviewRequest {
    fn token(&self) -> String {
        format!(
            "v1-{}-{}-{}-{}-{}",
            self.project, self.session, self.document, self.revision, self.blob
        )
    }

    fn parse(uri: &http::Uri) -> Option<Self> {
        if !matches!(
            (
                uri.scheme_str(),
                uri.authority().map(http::uri::Authority::as_str)
            ),
            (Some(SCHEME), Some("localhost")) | (Some("http"), Some("loom-preview.localhost"))
        ) || uri.query().is_some()
        {
            return None;
        }
        let token = uri.path().strip_prefix('/')?;
        let mut parts = token.strip_prefix("v1-")?.split('-');
        let parsed = Self {
            project: parts.next()?.parse().ok()?,
            session: parts.next()?.parse().ok()?,
            document: parts.next()?.parse().ok()?,
            revision: parts.next()?.parse().ok()?,
            blob: parts.next()?.parse().ok()?,
        };
        (parts.next().is_none() && parsed.token() == token).then_some(parsed)
    }
}

fn html_body(text: &str) -> &[u8] {
    let trimmed = text.trim();
    if let Some((first, rest)) = trimmed.split_once('\n')
        && matches!(first.trim_end(), "```html" | "```HTML" | "```")
        && let Some((body, closing)) = rest.rsplit_once('\n')
        && closing.trim() == "```"
    {
        body.as_bytes()
    } else {
        text.as_bytes()
    }
}

fn read_preview(state: &PluginState, request: &PreviewRequest) -> Option<Vec<u8>> {
    let session = state.session.lock().ok()?;
    if session.phase != SessionPhase::Open || session.active_session_id != Some(request.session) {
        return None;
    }
    let store = session.store.as_ref()?;
    if store.manifest().project_id != request.project {
        return None;
    }
    let summary = store.registered_document(request.document).ok()??;
    let loaded = store.read_document(summary.relative_path).ok()?;
    if loaded.revision_id != request.revision
        || loaded.blob_id != request.blob
        || loaded.text.len() > MAX_PREVIEW_BYTES
    {
        return None;
    }
    Some(html_body(&loaded.text).to_vec())
}

pub(super) fn response(
    state: &PluginState,
    webview_label: &str,
    request: &http::Request<Vec<u8>>,
) -> http::Response<Vec<u8>> {
    if webview_label != "main" {
        return empty_loom_asset_response(http::StatusCode::FORBIDDEN);
    }
    let head = request.method() == http::Method::HEAD;
    if request.method() != http::Method::GET && !head {
        return empty_loom_asset_response_with_header(
            http::StatusCode::METHOD_NOT_ALLOWED,
            http::header::ALLOW,
            "GET, HEAD",
        );
    }
    if !request.body().is_empty() || request.headers().contains_key(http::header::RANGE) {
        return empty_loom_asset_response(http::StatusCode::BAD_REQUEST);
    }
    let Some(selected) = PreviewRequest::parse(request.uri()) else {
        return empty_loom_asset_response(http::StatusCode::BAD_REQUEST);
    };
    let Some(body) = read_preview(state, &selected) else {
        return empty_loom_asset_response(http::StatusCode::NOT_FOUND);
    };
    http::Response::builder()
        .status(http::StatusCode::OK)
        .header(http::header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(http::header::CONTENT_LENGTH, body.len().to_string())
        .header(http::header::CACHE_CONTROL, "no-store")
        .header(http::header::CONTENT_SECURITY_POLICY, PREVIEW_CSP)
        .header("x-content-type-options", "nosniff")
        .header("referrer-policy", "no-referrer")
        .body(if head { Vec::new() } else { body })
        .expect("static preview response headers are valid")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_is_bound_to_session_and_exact_visible_revision() {
        let directory = tempfile::tempdir().unwrap();
        let (mut store, _) =
            ProjectStore::initialize(directory.path().join("Writing"), "Writing").unwrap();
        let html = "<style>body{color:red}</style><h1>My page</h1><script>run()</script>";
        store
            .create_document_if_absent(
                "Page.md",
                DocumentContent::Prose(format!("```html\n{html}\n```")),
                "page",
            )
            .unwrap();
        let loaded = store.read_document("Page.md").unwrap();
        let selected = PreviewRequest {
            project: store.manifest().project_id,
            session: CommandId::new(),
            document: loaded.document_id,
            revision: loaded.revision_id,
            blob: loaded.blob_id,
        };
        let state = PluginState::with_app_local_data_root(
            Some(directory.path().join("app")),
            true,
            BuildModelPolicy::default(),
        );
        {
            let mut session = state.session.lock().unwrap();
            session.phase = SessionPhase::Open;
            session.active_session_id = Some(selected.session);
            session.store = Some(store);
        }
        let request = http::Request::builder()
            .uri(format!("loom-preview://localhost/{}", selected.token()))
            .body(Vec::new())
            .unwrap();
        let reply = response(&state, "main", &request);
        assert_eq!(reply.status(), http::StatusCode::OK);
        assert_eq!(reply.body(), html.as_bytes());
        assert_eq!(
            reply.headers()[http::header::CONTENT_SECURITY_POLICY],
            PREVIEW_CSP
        );
        assert_eq!(
            response(&state, "other", &request).status(),
            http::StatusCode::FORBIDDEN
        );
        {
            let mut session = state.session.lock().unwrap();
            session
                .store
                .as_mut()
                .unwrap()
                .save_document(
                    "Page.md",
                    DocumentContent::Prose("Changed".into()),
                    "author edit",
                )
                .unwrap();
        }
        assert_eq!(
            response(&state, "main", &request).status(),
            http::StatusCode::NOT_FOUND
        );
        state.session.lock().unwrap().active_session_id = Some(CommandId::new());
        assert!(read_preview(&state, &selected).is_none());
    }

    #[test]
    fn preview_tokens_are_canonical_and_fence_removal_preserves_html() {
        let selected = PreviewRequest {
            project: ProjectId::new(),
            session: CommandId::new(),
            document: DocumentId::new(),
            revision: RevisionId::new(),
            blob: BlobId::digest(b"page"),
        };
        for origin in ["loom-preview://localhost", "http://loom-preview.localhost"] {
            assert!(
                PreviewRequest::parse(&format!("{origin}/{}", selected.token()).parse().unwrap())
                    .is_some()
            );
        }
        for suffix in ["?other", "/extra", "%20"] {
            assert!(
                PreviewRequest::parse(
                    &format!("loom-preview://localhost/{}{suffix}", selected.token())
                        .parse()
                        .unwrap()
                )
                .is_none()
            );
        }
        assert!(
            PreviewRequest::parse(
                &format!("https://elsewhere/{}", selected.token())
                    .parse()
                    .unwrap()
            )
            .is_none()
        );
        assert_eq!(
            html_body("```html\r\n<h1>é</h1>\r\n```\r\n"),
            "<h1>é</h1>\r".as_bytes()
        );
        assert_eq!(
            html_body("<p>literal ``` marks</p>"),
            b"<p>literal ``` marks</p>"
        );
    }
}
