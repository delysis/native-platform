//! Explicit read-only Google account imports. No scheduling or credential
//! persistence: the product owns both. Every endpoint is fixed, redirects and
//! ambient proxies are disabled, and every response is bounded while reading.
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::{Client, Response, redirect::Policy};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::time::Duration;
use thiserror::Error;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpListener,
};
use url::Url;

mod revisions;
pub use revisions::{DriveRevision, DriveRevisionPage, RevisionCoverage, RevisionModifier};

const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const MAX_RESPONSE: usize = 32 * 1024 * 1024;
pub const MAX_PAGE_FILES: usize = 16;

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GoogleService {
    Gmail,
    Drive,
}

impl GoogleService {
    pub fn scope(self) -> &'static str {
        match self {
            Self::Gmail => "https://www.googleapis.com/auth/gmail.readonly",
            Self::Drive => "https://www.googleapis.com/auth/drive.readonly",
        }
    }
}

#[derive(Debug, Error)]
pub enum ImportError {
    #[error("Invalid import request: {0}")]
    Invalid(&'static str),
    #[error("The account request failed; check connectivity and try again.")]
    Network,
    #[error("The account request timed out.")]
    Timeout,
    #[error(
        "Google rejected the account request (HTTP {0}). Reconnect if authorization has expired."
    )]
    Http(u16),
    #[error("The provider response was malformed or exceeded the import limit.")]
    Response,
    #[error("Account authorization was denied or the callback did not match this request.")]
    Authorization,
}

// Deliberately no Debug: neither OAuth tokens nor client secrets belong in logs.
#[derive(Clone, Deserialize, Serialize)]
pub struct GoogleCredentials {
    pub client_id: String,
    pub client_secret: String,
    pub refresh_token: String,
    pub account_email: String,
    pub service: GoogleService,
}

pub struct PendingAuthorization {
    listener: TcpListener,
    client_id: String,
    client_secret: String,
    state: String,
    verifier: String,
    redirect: String,
    service: GoogleService,
    pub authorization_url: String,
}

fn random_token() -> Result<String, ImportError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| ImportError::Authorization)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn client() -> Result<Client, ImportError> {
    Client::builder()
        .redirect(Policy::none())
        .no_proxy()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(45))
        .user_agent("native-kit-explicit-import/1")
        .build()
        .map_err(|_| ImportError::Network)
}

pub async fn begin_authorization(
    client_id: String,
    client_secret: String,
    service: GoogleService,
) -> Result<PendingAuthorization, ImportError> {
    if client_id.len() > 512
        || !client_id.ends_with(".apps.googleusercontent.com")
        || client_id.chars().any(char::is_whitespace)
        || client_secret.len() > 1024
    {
        return Err(ImportError::Invalid(
            "Use a Google Desktop app OAuth client.",
        ));
    }
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|_| ImportError::Network)?;
    let redirect = format!(
        "http://127.0.0.1:{}/oauth/callback",
        listener
            .local_addr()
            .map_err(|_| ImportError::Network)?
            .port()
    );
    let state = random_token()?;
    let verifier = random_token()?;
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let mut url = Url::parse("https://accounts.google.com/o/oauth2/v2/auth")
        .map_err(|_| ImportError::Authorization)?;
    url.query_pairs_mut().extend_pairs([
        ("client_id", client_id.as_str()),
        ("redirect_uri", redirect.as_str()),
        ("response_type", "code"),
        ("scope", service.scope()),
        ("state", state.as_str()),
        ("code_challenge", challenge.as_str()),
        ("code_challenge_method", "S256"),
        ("access_type", "offline"),
        ("prompt", "consent select_account"),
    ]);
    Ok(PendingAuthorization {
        listener,
        client_id,
        client_secret,
        state,
        verifier,
        redirect,
        service,
        authorization_url: url.into(),
    })
}

impl PendingAuthorization {
    pub async fn finish(self) -> Result<GoogleCredentials, ImportError> {
        tokio::time::timeout(Duration::from_secs(300), self.finish_inner())
            .await
            .map_err(|_| ImportError::Timeout)?
    }

    async fn finish_inner(self) -> Result<GoogleCredentials, ImportError> {
        // Ignore favicon probes and unrelated callbacks, but cap all work.
        for _ in 0..16 {
            let (mut stream, peer) = self
                .listener
                .accept()
                .await
                .map_err(|_| ImportError::Network)?;
            if !peer.ip().is_loopback() {
                continue;
            }
            let mut bytes = Vec::new();
            let read = tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    let mut chunk = [0u8; 1024];
                    let n = stream.read(&mut chunk).await?;
                    if n == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&chunk[..n]);
                    if bytes.windows(4).any(|window| window == b"\r\n\r\n") || bytes.len() >= 8192 {
                        break;
                    }
                }
                Ok::<(), std::io::Error>(())
            })
            .await;
            if !matches!(read, Ok(Ok(()))) {
                continue;
            }
            let Some(code) = callback_code(&bytes, &self.redirect, &self.state) else {
                let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\nContent-Length: 0\r\n\r\n").await;
                if callback_parameters(&bytes, &self.redirect, &self.state)
                    .is_some_and(|pairs| pairs.iter().any(|(key, _)| key == "error"))
                {
                    return Err(ImportError::Authorization);
                }
                continue;
            };
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nCache-Control: no-store\r\nConnection: close\r\n\r\nAuthorization received. Return to Loom to check the connection result.").await;
            drop(stream);
            let response = client()?
                .post(TOKEN_URL)
                .form(&[
                    ("client_id", self.client_id.as_str()),
                    ("client_secret", self.client_secret.as_str()),
                    ("code", code.as_str()),
                    ("code_verifier", self.verifier.as_str()),
                    ("redirect_uri", self.redirect.as_str()),
                    ("grant_type", "authorization_code"),
                ])
                .send()
                .await
                .map_err(|_| ImportError::Network)?;
            let token: TokenResponse = json(response, 64 * 1024).await?;
            validate_scope(&token, self.service)?;
            let refresh_token = token
                .refresh_token
                .filter(|token| !token.is_empty())
                .ok_or(ImportError::Authorization)?;
            let session = GoogleSession {
                account_email: String::new(),
                client: client()?,
                token: token.access_token,
                service: self.service,
            };
            let account_email = session.account_email().await?;
            return Ok(GoogleCredentials {
                client_id: self.client_id,
                client_secret: self.client_secret,
                refresh_token,
                account_email,
                service: self.service,
            });
        }
        Err(ImportError::Authorization)
    }
}

fn callback_parameters(bytes: &[u8], redirect: &str, state: &str) -> Option<Vec<(String, String)>> {
    if bytes.len() >= 8192 {
        return None;
    }
    let request = std::str::from_utf8(bytes).ok()?;
    let mut parts = request.lines().next()?.split_whitespace();
    if parts.next()? != "GET" {
        return None;
    }
    let target = parts.next()?;
    if !target.starts_with("/oauth/callback?") {
        return None;
    }
    let base = Url::parse(redirect).ok()?;
    let url = base.join(target).ok()?;
    if url.origin() != base.origin() || url.path() != base.path() {
        return None;
    }
    let pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
    let states: Vec<_> = pairs.iter().filter(|(k, _)| k == "state").collect();
    if states.len() != 1 || states[0].1 != state {
        return None;
    }
    Some(pairs)
}

fn callback_code(bytes: &[u8], redirect: &str, state: &str) -> Option<String> {
    let pairs = callback_parameters(bytes, redirect, state)?;
    let codes: Vec<_> = pairs.iter().filter(|(key, _)| key == "code").collect();
    if codes.len() != 1 || codes[0].1.is_empty() || pairs.iter().any(|(key, _)| key == "error") {
        return None;
    }
    Some(codes[0].1.clone())
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    scope: Option<String>,
    token_type: String,
}

fn validate_scope(token: &TokenResponse, service: GoogleService) -> Result<(), ImportError> {
    if token.access_token.is_empty()
        || !token.token_type.eq_ignore_ascii_case("bearer")
        || token
            .scope
            .as_deref()
            .is_some_and(|scope| !scope.split_whitespace().any(|s| s == service.scope()))
    {
        return Err(ImportError::Authorization);
    }
    Ok(())
}

pub struct GoogleSession {
    account_email: String,
    client: Client,
    token: String,
    service: GoogleService,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ImportQuery {
    /// Gmail search expression, or a Drive folder ID. An empty Drive ID lists
    /// eligible files throughout the authorized account.
    pub query: String,
    pub page_token: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RemoteFile {
    pub id: String,
    pub name: String,
    pub mime_type: String,
    pub source_uri: String,
    pub modified_time: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RemotePage {
    pub files: Vec<RemoteFile>,
    pub next_page_token: Option<String>,
}

impl GoogleSession {
    pub async fn refresh(credentials: &GoogleCredentials) -> Result<Self, ImportError> {
        let client = client()?;
        let response = client
            .post(TOKEN_URL)
            .form(&[
                ("client_id", credentials.client_id.as_str()),
                ("client_secret", credentials.client_secret.as_str()),
                ("refresh_token", credentials.refresh_token.as_str()),
                ("grant_type", "refresh_token"),
            ])
            .send()
            .await
            .map_err(|_| ImportError::Network)?;
        let token: TokenResponse = json(response, 64 * 1024).await?;
        validate_scope(&token, credentials.service)?;
        Ok(Self {
            account_email: credentials.account_email.clone(),
            client,
            token: token.access_token,
            service: credentials.service,
        })
    }

    async fn get(&self, url: Url) -> Result<Response, ImportError> {
        // URL constructors are private, and bearer credentials never follow
        // redirects or user-provided URLs.
        self.client
            .get(url)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|_| ImportError::Network)
    }

    async fn account_email(&self) -> Result<String, ImportError> {
        let endpoint = match self.service {
            GoogleService::Gmail => "https://gmail.googleapis.com/gmail/v1/users/me/profile",
            GoogleService::Drive => {
                "https://www.googleapis.com/drive/v3/about?fields=user(emailAddress)"
            }
        };
        let value: serde_json::Value =
            json(self.get(parse_url(endpoint)?).await?, 64 * 1024).await?;
        let email = match self.service {
            GoogleService::Gmail => value.get("emailAddress"),
            GoogleService::Drive => value.pointer("/user/emailAddress"),
        };
        email
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty() && s.len() <= 320)
            .map(str::to_string)
            .ok_or(ImportError::Response)
    }

    pub async fn list(&self, query: &ImportQuery) -> Result<RemotePage, ImportError> {
        let url = list_url(self.service, query)?;
        let value: serde_json::Value = json(self.get(url).await?, 2 * 1024 * 1024).await?;
        let mut page = parse_page(self.service, value)?;
        if self.service == GoogleService::Gmail {
            for file in &mut page.files {
                let mut url = parse_url("https://mail.google.com/mail/")?;
                url.query_pairs_mut()
                    .append_pair("authuser", &self.account_email);
                url.set_fragment(Some(&format!("all/{}", file.id)));
                file.source_uri = url.into();
            }
        }
        Ok(page)
    }

    pub async fn download(&self, file: &RemoteFile) -> Result<Vec<u8>, ImportError> {
        let url = download_url(self.service, file)?;
        let bytes = bounded(self.get(url).await?, MAX_RESPONSE).await?;
        if self.service == GoogleService::Gmail {
            let value: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(|_| ImportError::Response)?;
            let raw = value
                .get("raw")
                .and_then(serde_json::Value::as_str)
                .ok_or(ImportError::Response)?;
            URL_SAFE_NO_PAD
                .decode(raw.trim_end_matches('='))
                .map_err(|_| ImportError::Response)
        } else {
            Ok(bytes)
        }
    }
}

fn parse_url(value: &str) -> Result<Url, ImportError> {
    Url::parse(value).map_err(|_| ImportError::Invalid("Invalid provider endpoint."))
}
fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn list_url(service: GoogleService, query: &ImportQuery) -> Result<Url, ImportError> {
    if query.query.len() > 2048
        || query
            .page_token
            .as_ref()
            .is_some_and(|token| token.len() > 4096)
    {
        return Err(ImportError::Invalid(
            "Search or page token exceeds the input limit.",
        ));
    }
    let mut url = parse_url(match service {
        GoogleService::Gmail => "https://gmail.googleapis.com/gmail/v1/users/me/messages",
        GoogleService::Drive => "https://www.googleapis.com/drive/v3/files",
    })?;
    match service {
        GoogleService::Gmail => {
            url.query_pairs_mut()
                .extend_pairs([("maxResults", "16"), ("q", query.query.as_str())]);
        }
        GoogleService::Drive => {
            if !query.query.is_empty() && !valid_id(&query.query) {
                return Err(ImportError::Invalid(
                    "Use a Drive folder ID, not a URL or search expression.",
                ));
            }
            let mut filter = "trashed = false and mimeType != 'application/vnd.google-apps.folder' and mimeType != 'application/vnd.google-apps.shortcut'".to_string();
            if !query.query.is_empty() {
                filter.push_str(&format!(" and '{}' in parents", query.query));
            }
            url.query_pairs_mut().extend_pairs([
                ("pageSize", "16"),
                ("q", filter.as_str()),
                (
                    "fields",
                    "nextPageToken,files(id,name,mimeType,modifiedTime,size)",
                ),
                ("orderBy", "modifiedTime desc"),
            ]);
        }
    }
    if let Some(token) = &query.page_token {
        url.query_pairs_mut().append_pair("pageToken", token);
    }
    Ok(url)
}

fn parse_page(service: GoogleService, value: serde_json::Value) -> Result<RemotePage, ImportError> {
    let key = if service == GoogleService::Gmail {
        "messages"
    } else {
        "files"
    };
    let rows = match value.get(key) {
        Some(value) => value.as_array().ok_or(ImportError::Response)?.as_slice(),
        None => &[],
    };
    if rows.len() > MAX_PAGE_FILES {
        return Err(ImportError::Response);
    }
    let mut files = Vec::new();
    for row in rows {
        let id = row
            .get("id")
            .and_then(serde_json::Value::as_str)
            .filter(|id| valid_id(id))
            .ok_or(ImportError::Response)?
            .to_string();
        let (name, mime_type, source_uri) = if service == GoogleService::Gmail {
            (
                format!("Gmail message {id}.eml"),
                "message/rfc822".to_string(),
                format!("https://mail.google.com/mail/u/0/#all/{id}"),
            )
        } else {
            (
                row.get("name")
                    .and_then(serde_json::Value::as_str)
                    .ok_or(ImportError::Response)?
                    .to_string(),
                row.get("mimeType")
                    .and_then(serde_json::Value::as_str)
                    .ok_or(ImportError::Response)?
                    .to_string(),
                format!("https://drive.google.com/file/d/{id}/view"),
            )
        };
        if name.len() > 4096 {
            return Err(ImportError::Response);
        }
        files.push(RemoteFile {
            id,
            name,
            mime_type,
            source_uri,
            modified_time: row
                .get("modifiedTime")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
        });
    }
    let next_page_token = value
        .get("nextPageToken")
        .and_then(serde_json::Value::as_str)
        .filter(|v| !v.is_empty())
        .map(str::to_string);
    if next_page_token.as_ref().is_some_and(|v| v.len() > 4096) {
        return Err(ImportError::Response);
    }
    Ok(RemotePage {
        files,
        next_page_token,
    })
}

fn download_url(service: GoogleService, file: &RemoteFile) -> Result<Url, ImportError> {
    if !valid_id(&file.id) {
        return Err(ImportError::Invalid("Invalid provider file ID."));
    }
    if service == GoogleService::Gmail {
        return parse_url(&format!(
            "https://gmail.googleapis.com/gmail/v1/users/me/messages/{}?format=raw",
            file.id
        ));
    }
    let export = match file.mime_type.as_str() {
        "application/vnd.google-apps.document" => {
            Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document")
        }
        "application/vnd.google-apps.spreadsheet" => {
            Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet")
        }
        "application/vnd.google-apps.presentation" => {
            Some("application/vnd.openxmlformats-officedocument.presentationml.presentation")
        }
        mime if mime.starts_with("application/vnd.google-apps.") => {
            return Err(ImportError::Invalid(
                "This Google Workspace format cannot be exported by this importer.",
            ));
        }
        _ => None,
    };
    let mut url = parse_url(&format!(
        "https://www.googleapis.com/drive/v3/files/{}{}",
        file.id,
        if export.is_some() { "/export" } else { "" }
    ))?;
    url.query_pairs_mut().append_pair(
        if export.is_some() { "mimeType" } else { "alt" },
        export.unwrap_or("media"),
    );
    Ok(url)
}

async fn bounded(mut response: Response, max: usize) -> Result<Vec<u8>, ImportError> {
    if !response.status().is_success() {
        return Err(ImportError::Http(response.status().as_u16()));
    }
    if response
        .content_length()
        .is_some_and(|length| length > max as u64)
    {
        return Err(ImportError::Response);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| ImportError::Network)? {
        if chunk.len() > max.saturating_sub(bytes.len()) {
            return Err(ImportError::Response);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
async fn json<T: serde::de::DeserializeOwned>(
    response: Response,
    max: usize,
) -> Result<T, ImportError> {
    serde_json::from_slice(&bounded(response, max).await?).map_err(|_| ImportError::Response)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn callback_binds_state_path_and_single_values() {
        let redirect = "http://127.0.0.1:4321/oauth/callback";
        assert_eq!(
            callback_code(
                b"GET /oauth/callback?state=s&code=c HTTP/1.1\r\n\r\n",
                redirect,
                "s"
            ),
            Some("c".into())
        );
        for request in [
            "GET /oauth/callback?state=wrong&code=c HTTP/1.1",
            "GET /oauth/callback?state=s&state=s&code=c HTTP/1.1",
            "GET //evil.invalid/oauth/callback?state=s&code=c HTTP/1.1",
            "GET /oauth/callback?state=s&code=c&error=denied HTTP/1.1",
        ] {
            assert!(callback_code(request.as_bytes(), redirect, "s").is_none());
        }
    }
    #[test]
    fn drive_folder_cannot_inject_query_and_download_cannot_change_host() -> Result<(), ImportError>
    {
        assert!(
            list_url(
                GoogleService::Drive,
                &ImportQuery {
                    query: "x' or trashed=true".into(),
                    page_token: None
                }
            )
            .is_err()
        );
        let file = RemoteFile {
            id: "safe_id".into(),
            name: "x".into(),
            mime_type: "application/vnd.google-apps.document".into(),
            source_uri: "https://evil.invalid".into(),
            modified_time: None,
        };
        let url = download_url(GoogleService::Drive, &file)?;
        assert_eq!(url.host_str(), Some("www.googleapis.com"));
        assert!(url.path().ends_with("/export"));
        Ok(())
    }
    #[test]
    fn absent_messages_is_empty_but_invalid_messages_is_failure() {
        assert!(parse_page(GoogleService::Gmail, serde_json::json!({})).is_ok());
        assert!(parse_page(GoogleService::Gmail, serde_json::json!({"messages": "bad"})).is_err());
        assert!(
            parse_page(
                GoogleService::Gmail,
                serde_json::json!({"messages": [{"id": "../other"}]})
            )
            .is_err()
        );
    }
    #[test]
    fn streamed_responses_are_bounded_and_provider_errors_do_not_echo_secrets() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            for response in [
                "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n8\r\n12345678\r\n0\r\n\r\n",
                "HTTP/1.1 401 Unauthorized\r\nContent-Length: 6\r\nConnection: close\r\n\r\nsecret",
            ] {
                let listener = TcpListener::bind("127.0.0.1:0").await.expect("mock listener");
                let address = listener.local_addr().expect("address");
                let server = tokio::spawn(async move {
                    let (mut stream, _) = listener.accept().await.expect("accept");
                    let mut request = [0u8; 2048];
                    let _ = stream.read(&mut request).await.expect("request");
                    stream.write_all(response.as_bytes()).await.expect("response");
                });
                let received = client().expect("client").get(format!("http://{address}")).send().await.expect("mock response");
                let error = bounded(received, 4).await.expect_err("must reject");
                assert!(!error.to_string().contains("secret"));
                server.await.expect("server");
            }
        });
    }

    #[test]
    fn desktop_authorization_uses_fresh_pkce_and_scopes() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let first = begin_authorization(
                "test.apps.googleusercontent.com".into(),
                String::new(),
                GoogleService::Gmail,
            )
            .await
            .expect("first");
            let second = begin_authorization(
                "test.apps.googleusercontent.com".into(),
                String::new(),
                GoogleService::Gmail,
            )
            .await
            .expect("second");
            assert_ne!(first.state, second.state);
            assert_ne!(first.verifier, second.verifier);
            let url = Url::parse(&first.authorization_url).expect("authorization URL");
            let params: std::collections::BTreeMap<_, _> = url.query_pairs().into_owned().collect();
            assert_eq!(params["code_challenge_method"], "S256");
            assert_eq!(params["scope"], GoogleService::Gmail.scope());
            assert_eq!(
                params["code_challenge"],
                URL_SAFE_NO_PAD.encode(Sha256::digest(first.verifier.as_bytes()))
            );
            assert!(params["redirect_uri"].starts_with("http://127.0.0.1:"));
        });
    }
}
