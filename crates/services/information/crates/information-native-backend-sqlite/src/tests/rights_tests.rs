use super::*;
use std::cell::Cell;

type OwnedSourceBytes = [Option<Vec<u8>>; 4];

#[derive(Clone, Copy, Debug)]
enum PublicRoute {
    Search,
    Lookup,
}

struct Observation {
    hit: EvidenceHit,
    complete: bool,
    warnings: Vec<String>,
}

struct PublicFixture {
    _directory: tempfile::TempDir,
    path: PathBuf,
    backend: AlexandriaBackend,
    original_bytes: OwnedSourceBytes,
    evidence: Option<PathBuf>,
    next_capture: Cell<u32>,
}

fn export_policy() -> UsePolicy {
    UsePolicy {
        model_context: UsePermission::Allowed,
        excerpt_export: UsePermission::Allowed,
        redistribution: UsePermission::Allowed,
        ..UsePolicy::default()
    }
}

fn source_bytes(path: &Path) -> Result<OwnedSourceBytes, Box<dyn Error>> {
    let mut observed = [None, None, None, None];
    for (index, suffix) in ["", "-wal", "-shm", "-journal"].into_iter().enumerate() {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        observed[index] = match fs::read(PathBuf::from(name)) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
    }
    Ok(observed)
}

impl PublicFixture {
    fn new(
        label: &str,
        status: &str,
        encoding: &str,
        corpus_rights: Option<RedistributionPolicy>,
        policy: UsePolicy,
    ) -> Result<Self, Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("rights.db");
        let connection = Connection::open(&path)?;
        connection.pragma_update(None, "encoding", encoding)?;
        create_fixture(&connection)?;
        connection.execute(
            "UPDATE documents SET rights_status = ?1 WHERE doc_id = ?2",
            params![status, "D1"],
        )?;
        let stored: String = connection.query_row(
            "SELECT rights_status FROM documents WHERE doc_id = 'D1'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(stored.as_bytes(), status.as_bytes(), "owned status bytes");
        let actual_encoding: String =
            connection.query_row("PRAGMA encoding", [], |row| row.get(0))?;
        drop(connection);
        let original_bytes = source_bytes(&path)?;
        let evidence = std::env::var_os("INFORMATION_SQLITE_RIGHTS_EVIDENCE_ROOT")
            .map(|root| {
                let leaf = directory
                    .path()
                    .file_name()
                    .ok_or_else(|| std::io::Error::other("fixture directory has no leaf"))?;
                let output =
                    PathBuf::from(root).join(format!("{label}-{}", leaf.to_string_lossy()));
                fs::create_dir_all(&output)?;
                fs::write(output.join("rights-status.utf8"), status.as_bytes())?;
                fs::write(output.join("encoding.txt"), actual_encoding)?;
                for (index, suffix) in ["db", "wal", "shm", "journal"].into_iter().enumerate() {
                    if let Some(bytes) = &original_bytes[index] {
                        fs::write(output.join(format!("original.{suffix}")), bytes)?;
                    } else {
                        fs::write(output.join(format!("original.{suffix}.absent")), b"")?;
                    }
                }
                Ok::<_, Box<dyn Error>>(output)
            })
            .transpose()?;
        let mut config = fixture_config(&path)?;
        config.use_policy = policy;
        if let Some(redistribution) = corpus_rights {
            config.rights.push(RightsStatement {
                scope: "fixture-corpus".to_string(),
                expression: "owned fixture corpus rights".to_string(),
                license_url: None,
                license_text_sha256: None,
                attribution: Some("Fixture Publisher".to_string()),
                obligations: Vec::new(),
                redistribution,
            });
        }
        let backend = AlexandriaBackend::open(config)?;
        let fixture = Self {
            _directory: directory,
            path,
            backend,
            original_bytes,
            evidence,
            next_capture: Cell::new(0),
        };
        fixture.preserve_source()?;
        Ok(fixture)
    }

    fn preserve_source(&self) -> Result<(), Box<dyn Error>> {
        let observed = source_bytes(&self.path)?;
        if let Some(root) = &self.evidence {
            let capture = self.next_capture.get();
            self.next_capture.set(
                capture
                    .checked_add(1)
                    .ok_or_else(|| std::io::Error::other("fixture capture count overflow"))?,
            );
            let output = root.join(format!("capture-{capture:04}"));
            fs::create_dir(&output)?;
            for (index, suffix) in ["db", "wal", "shm", "journal"].into_iter().enumerate() {
                for (label, sources) in
                    [("original", &self.original_bytes), ("observed", &observed)]
                {
                    let name = format!("{label}.{suffix}");
                    if let Some(bytes) = &sources[index] {
                        fs::write(output.join(name), bytes)?;
                    } else {
                        fs::write(output.join(format!("{name}.absent")), b"")?;
                    }
                }
            }
        }
        assert_eq!(
            observed, self.original_bytes,
            "source and sidecar bytes/state"
        );
        Ok(())
    }

    fn observe(
        &self,
        route: PublicRoute,
        purpose: RetrievalPurpose,
    ) -> Result<Observation, Box<dyn Error>> {
        let public: &dyn ResourceBackend = &self.backend;
        let result = match route {
            PublicRoute::Search => {
                let mut query = fixture_query("prayer quiet", QuerySyntax::NaturalTerms)?;
                query.purpose = purpose;
                public.search(&query).and_then(|result| {
                    let hit = result.hits.into_iter().next().ok_or_else(|| {
                        integrity_error("fixture_empty", "public fixture returned no matching hit")
                    })?;
                    Ok(Observation {
                        hit,
                        complete: result.complete,
                        warnings: result.warnings,
                    })
                })
            }
            PublicRoute::Lookup => {
                let descriptor = public.descriptor();
                public
                    .lookup(&LookupRequest {
                        resource_id: descriptor.resource_id.clone(),
                        release_id: descriptor.release_id.clone(),
                        representation_id: descriptor.representation_id.clone(),
                        purpose,
                        collection: None,
                        key: "D1:B000002".to_string(),
                        max_context_chars: 1_500,
                        timeout_ms: 5_000,
                    })
                    .map(|result| Observation {
                        hit: result.hit,
                        complete: result.complete,
                        warnings: result.warnings,
                    })
            }
        };
        self.preserve_source()?;
        result.map_err(Into::into)
    }

    fn read(&self, hit: &EvidenceHit) -> Result<BackendReadResult, Box<dyn Error>> {
        let public: &dyn ResourceBackend = &self.backend;
        let descriptor = public.descriptor();
        let result = public.read(&ReadRequest {
            resource_id: descriptor.resource_id.clone(),
            release_id: descriptor.release_id.clone(),
            representation_id: descriptor.representation_id.clone(),
            purpose: RetrievalPurpose::LocalUi,
            locator: hit.locator.clone(),
            max_context_chars: 1_500,
            timeout_ms: 5_000,
        });
        self.preserve_source()?;
        result.map_err(Into::into)
    }
}

fn document_rights(hit: &EvidenceHit) -> Result<&RightsStatement, Box<dyn Error>> {
    hit.rights
        .iter()
        .find(|rights| rights.scope == "document:D1")
        .ok_or_else(|| std::io::Error::other("public result has no document rights").into())
}

fn assert_purpose_refusal(
    fixture: &PublicFixture,
    route: PublicRoute,
    purpose: RetrievalPurpose,
) -> Result<(), Box<dyn Error>> {
    let error = fixture
        .observe(route, purpose)
        .err()
        .ok_or_else(|| std::io::Error::other("requested purpose was unexpectedly allowed"))?;
    let error = error
        .downcast_ref::<InformationError>()
        .ok_or_else(|| std::io::Error::other("purpose refusal lost its public typed error"))?;
    assert_eq!(error.class, ErrorClass::Permission);
    assert_eq!(error.code, "retrieval_purpose_not_permitted");
    Ok(())
}

fn assert_public_fault(
    label: &str,
    route: PublicRoute,
    status: &str,
) -> Result<(), Box<dyn Error>> {
    let ordinary = PublicFixture::new(
        "ordinary",
        "public_domain",
        "UTF-8",
        Some(RedistributionPolicy::Allowed),
        export_policy(),
    )?;
    let positive = ordinary.observe(route, RetrievalPurpose::LocalUi)?;
    assert_eq!(
        document_rights(&positive.hit)?.redistribution,
        RedistributionPolicy::Allowed
    );
    assert_eq!(positive.hit.passage_id.as_deref(), Some("D1:B000002"));
    assert_eq!(
        positive.hit.excerpt_sha256,
        evidence_text_sha256(&positive.hit.snippet, &positive.hit.context)
    );
    assert!(positive.hit.context.contains(
        "[D1:B000002; index=2; location=chapter 1]\nThe prayer of quiet gathers the powers for contemplation."
    ));
    positive.hit.validate()?;
    assert!(positive.complete);
    ordinary.observe(route, RetrievalPurpose::ExcerptExport)?;
    let fixture = PublicFixture::new(
        label,
        status,
        "UTF-8",
        Some(RedistributionPolicy::Allowed),
        export_policy(),
    )?;
    let returned = fixture.observe(route, RetrievalPurpose::LocalUi)?;
    let rights = document_rights(&returned.hit)?;
    let returned_width = returned
        .hit
        .metadata
        .get("rights_status")
        .and_then(JsonValue::as_str)
        .map(|value| value.chars().count());
    // This is the first fault assertion on unchanged BEFORE production.
    assert_eq!(
        rights.redistribution,
        RedistributionPolicy::Unknown,
        "stored_width={}, returned_width={returned_width:?}, rights={rights:?}, policy={:?}, complete={}",
        status.chars().count(),
        returned.hit.use_policy,
        returned.complete,
    );
    assert_eq!(
        returned.hit.metadata.get("rights_status_complete"),
        Some(&json!(false))
    );
    assert!(!returned.complete);
    assert!(!returned.warnings.is_empty());
    assert_eq!(returned.hit.snippet, positive.hit.snippet);
    assert_eq!(returned.hit.context, positive.hit.context);
    assert_eq!(returned.hit.excerpt_sha256, positive.hit.excerpt_sha256);
    assert_eq!(returned.hit.locator, positive.hit.locator);
    assert_eq!(
        returned.hit.use_policy.excerpt_export,
        UsePermission::Unknown
    );
    assert_eq!(
        returned.hit.use_policy.redistribution,
        UsePermission::Unknown
    );
    assert!(rights.license_url.is_none());
    assert!(rights.obligations.is_empty());
    returned.hit.validate()?;
    let model = fixture.observe(route, RetrievalPurpose::ModelContext)?;
    assert_eq!(model.hit.snippet, returned.hit.snippet);
    assert_eq!(model.hit.context, returned.hit.context);
    let read = fixture.read(&returned.hit)?;
    assert_eq!(read.hit.locator, returned.hit.locator);
    assert_eq!(
        read.hit.excerpt_sha256,
        evidence_text_sha256(&read.hit.snippet, &read.hit.context)
    );
    assert!(!read.complete);
    assert_purpose_refusal(&fixture, route, RetrievalPurpose::ExcerptExport)?;
    fixture.preserve_source()?;
    Ok(())
}

#[test]
fn public_search_rights_ascii_width_does_not_grant_export() -> Result<(), Box<dyn Error>> {
    let status = format!("public_domain{}restricted", " ".repeat(499));
    assert_public_fault("ascii-search", PublicRoute::Search, &status)
}

#[test]
fn public_lookup_rights_ascii_width_does_not_grant_export() -> Result<(), Box<dyn Error>> {
    let status = format!("public_domain{}restricted", " ".repeat(499));
    assert_public_fault("ascii-lookup", PublicRoute::Lookup, &status)
}

#[test]
fn public_search_rights_nul_does_not_grant_export() -> Result<(), Box<dyn Error>> {
    assert_public_fault(
        "nul-search",
        PublicRoute::Search,
        "public_domain\0restricted",
    )
}

#[test]
fn public_lookup_rights_nul_does_not_grant_export() -> Result<(), Box<dyn Error>> {
    assert_public_fault(
        "nul-lookup",
        PublicRoute::Lookup,
        "public_domain\0restricted",
    )
}

#[test]
fn public_rights_encoding_and_byte_bounds_preserve_local_text() -> Result<(), Box<dyn Error>> {
    let cases = [
        (
            format!("public_domain{}", " ".repeat(499)),
            true,
            RedistributionPolicy::Allowed,
        ),
        (
            format!("public_domain{}", " ".repeat(500)),
            false,
            RedistributionPolicy::Unknown,
        ),
        (
            format!("public_domain{}", "\u{3000}".repeat(499)),
            true,
            RedistributionPolicy::Allowed,
        ),
        (
            format!("public_domain{}", "\u{3000}".repeat(500)),
            false,
            RedistributionPolicy::Unknown,
        ),
        ("🙂".repeat(512), true, RedistributionPolicy::Unknown),
        (
            format!("{}x", "🙂".repeat(512)),
            false,
            RedistributionPolicy::Unknown,
        ),
        ("x".repeat(2048), false, RedistributionPolicy::Unknown),
        ("x".repeat(2049), false, RedistributionPolicy::Unknown),
        (
            format!(
                "public_domain{}restricted{}",
                "\u{3000}".repeat(499),
                " ".repeat(233)
            ),
            false,
            RedistributionPolicy::Unknown,
        ),
        (
            "public_domain\0".to_string(),
            false,
            RedistributionPolicy::Unknown,
        ),
    ];
    assert_eq!(cases[4].0.len(), 2048);
    assert_eq!(cases[5].0.len(), 2049);
    assert_eq!(cases[8].0.encode_utf16().count() * 2, 1510);
    for encoding in ["UTF-8", "UTF-16le", "UTF-16be"] {
        for (status, complete, expected) in &cases {
            let fixture = PublicFixture::new(
                "encoding-bound",
                status,
                encoding,
                Some(RedistributionPolicy::Allowed),
                export_policy(),
            )?;
            for route in [PublicRoute::Search, PublicRoute::Lookup] {
                let observed = fixture.observe(route, RetrievalPurpose::LocalUi)?;
                assert_eq!(
                    document_rights(&observed.hit)?.redistribution,
                    *expected,
                    "encoding={encoding}, chars={}",
                    status.chars().count()
                );
                assert_eq!(
                    observed.hit.metadata.get("rights_status_complete"),
                    Some(&json!(complete))
                );
                assert_eq!(observed.complete, *complete);
                assert_eq!(
                    observed
                        .warnings
                        .iter()
                        .any(|warning| warning.contains("rights metadata")),
                    !*complete,
                    "encoding={encoding}, route={route:?}, warnings={:?}",
                    observed.warnings,
                );
                assert_eq!(observed.hit.passage_id.as_deref(), Some("D1:B000002"));
                let prefix = observed
                    .hit
                    .metadata
                    .get("rights_status")
                    .and_then(JsonValue::as_str)
                    .ok_or_else(|| std::io::Error::other("rights metadata is not text"))?;
                assert!(prefix.chars().count() <= 512);
                fixture.observe(route, RetrievalPurpose::ModelContext)?;
                let read = fixture.read(&observed.hit)?;
                assert_eq!(read.complete, *complete);
                if *expected == RedistributionPolicy::Allowed {
                    fixture.observe(route, RetrievalPurpose::ExcerptExport)?;
                } else {
                    assert_purpose_refusal(&fixture, route, RetrievalPurpose::ExcerptExport)?;
                }
            }
        }
    }
    Ok(())
}

#[test]
fn public_rights_ordinary_private_unknown_and_purpose_controls() -> Result<(), Box<dyn Error>> {
    let cases = [
        ("public_domain", RedistributionPolicy::Allowed),
        (
            "open_public_ia_text_file_not_marked_private",
            RedistributionPolicy::Allowed,
        ),
        (
            "private donor archive",
            RedistributionPolicy::PrivateUseOnly,
        ),
        ("all_rights_reserved", RedistributionPolicy::Forbidden),
        (
            "custom status not in the vocabulary",
            RedistributionPolicy::Unknown,
        ),
    ];
    for (status, expected) in cases {
        let fixture = PublicFixture::new(
            "ordinary-policy",
            status,
            "UTF-8",
            Some(RedistributionPolicy::Allowed),
            export_policy(),
        )?;
        for route in [PublicRoute::Search, PublicRoute::Lookup] {
            let observed = fixture.observe(route, RetrievalPurpose::LocalUi)?;
            assert_eq!(document_rights(&observed.hit)?.redistribution, expected);
            assert_eq!(
                observed.hit.metadata.get("rights_status_complete"),
                Some(&json!(true))
            );
            assert!(observed.complete);
            fixture.observe(route, RetrievalPurpose::ModelContext)?;
            if expected == RedistributionPolicy::Allowed {
                fixture.observe(route, RetrievalPurpose::ExcerptExport)?;
            } else {
                assert_purpose_refusal(&fixture, route, RetrievalPurpose::ExcerptExport)?;
            }
        }
    }
    for corpus in [
        RedistributionPolicy::PrivateUseOnly,
        RedistributionPolicy::Forbidden,
        RedistributionPolicy::Unknown,
    ] {
        let fixture = PublicFixture::new(
            "corpus-policy",
            "public_domain",
            "UTF-8",
            Some(corpus),
            export_policy(),
        )?;
        for route in [PublicRoute::Search, PublicRoute::Lookup] {
            fixture.observe(route, RetrievalPurpose::LocalUi)?;
            fixture.observe(route, RetrievalPurpose::ModelContext)?;
            assert_purpose_refusal(&fixture, route, RetrievalPurpose::ExcerptExport)?;
        }
    }
    let default = PublicFixture::new(
        "default-policy",
        "public_domain",
        "UTF-8",
        None,
        UsePolicy::default(),
    )?;
    for route in [PublicRoute::Search, PublicRoute::Lookup] {
        default.observe(route, RetrievalPurpose::LocalUi)?;
        assert_purpose_refusal(&default, route, RetrievalPurpose::ModelContext)?;
        assert_purpose_refusal(&default, route, RetrievalPurpose::ExcerptExport)?;
    }
    let mut unknown_local = export_policy();
    unknown_local.local_search = UsePermission::Unknown;
    let refused = PublicFixture::new(
        "unknown-local",
        "public_domain",
        "UTF-8",
        Some(RedistributionPolicy::Allowed),
        unknown_local,
    )?;
    for route in [PublicRoute::Search, PublicRoute::Lookup] {
        assert_purpose_refusal(&refused, route, RetrievalPurpose::LocalUi)?;
        assert_purpose_refusal(&refused, route, RetrievalPurpose::ModelContext)?;
        assert_purpose_refusal(&refused, route, RetrievalPurpose::ExcerptExport)?;
    }
    for status in ["".to_string(), format!("{}public_domain", " ".repeat(512))] {
        let fixture = PublicFixture::new(
            "blank-prefix",
            &status,
            "UTF-8",
            Some(RedistributionPolicy::Allowed),
            export_policy(),
        )?;
        for route in [PublicRoute::Search, PublicRoute::Lookup] {
            let error = fixture
                .observe(route, RetrievalPurpose::LocalUi)
                .err()
                .ok_or_else(|| std::io::Error::other("blank prefix was accepted"))?;
            let error = error
                .downcast_ref::<InformationError>()
                .ok_or_else(|| std::io::Error::other("blank-prefix error lost its type"))?;
            assert_eq!(error.class, ErrorClass::Integrity);
            assert_eq!(error.code, "sqlite_empty_rights_status");
        }
    }
    Ok(())
}
