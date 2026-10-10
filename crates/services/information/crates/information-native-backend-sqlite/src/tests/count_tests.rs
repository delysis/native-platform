use super::*;

#[test]
fn corpus_count_includes_unindexed_documents_and_preserves_source() -> Result<(), Box<dyn Error>> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("library.db");
    create_file_fixture(&path)?;
    let connection = Connection::open(&path)?;
    for index in 2..=41 {
        connection.execute("INSERT INTO documents (doc_id,title,tradition_tag,source_uri,rights_status,file_ext,ingest_status,block_count,text_chars) VALUES (?1,'Unindexed','Unknown','fixture://unindexed','unknown','txt','pending',0,0)", [format!("D{index}")])?;
    }
    drop(connection);
    let before = fs::read(&path)?;
    let backend = AlexandriaBackend::open(fixture_config(&path)?)?;
    let search = backend.search(&fixture_query("prayer", QuerySyntax::NaturalTerms)?)?;
    assert!(search.hits.len() < 5);
    let count = backend.count_documents(DocumentCountRequest {
        purpose: RetrievalPurpose::LocalUi,
        timeout_ms: 5_000,
    })?;
    assert_eq!(count.value, 41);
    assert_eq!(count.resource_id, backend.descriptor().resource_id);
    assert_eq!(count.source_identity.bytes, u64::try_from(before.len())?);
    assert!(
        count
            .source_fingerprint
            .starts_with("volatile-sqlite-snapshot-v1:")
    );
    assert_eq!(count.provenance.metadata["scope"], "entire_library");
    assert_eq!(count.provenance.metadata["unit"], "document_records");
    assert_eq!(fs::read(&path)?, before);
    assert!(!sibling_wal_path(&path)?.exists());
    assert!(!sibling_sidecar_path(&path, "-journal")?.exists());
    Ok(())
}

#[test]
fn count_rejects_unapproved_purpose_and_pending_wal() -> Result<(), Box<dyn Error>> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("library.db");
    create_file_fixture(&path)?;
    let backend = AlexandriaBackend::open(fixture_config(&path)?)?;
    assert_eq!(
        backend
            .count_documents(DocumentCountRequest {
                purpose: RetrievalPurpose::ModelContext,
                timeout_ms: 5_000
            })
            .expect_err("model use needs permission")
            .code,
        "retrieval_purpose_not_permitted"
    );
    assert_eq!(
        backend
            .count_documents(DocumentCountRequest {
                purpose: RetrievalPurpose::LocalUi,
                timeout_ms: 0
            })
            .expect_err("zero budget cannot execute")
            .code,
        "invalid_count_budget"
    );
    let original = fs::read(&path)?;
    let wal = sibling_wal_path(&path)?;
    fs::write(&wal, b"pending bytes are not ours to discard")?;
    assert!(
        backend
            .count_documents(DocumentCountRequest {
                purpose: RetrievalPurpose::LocalUi,
                timeout_ms: 5_000
            })
            .is_err()
    );
    assert_eq!(fs::read(&path)?, original);
    assert_eq!(fs::read(&wal)?, b"pending bytes are not ours to discard");
    Ok(())
}

#[test]
fn empty_corpus_returns_exact_zero() -> Result<(), Box<dyn Error>> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("empty.db");
    create_file_fixture(&path)?;
    let connection = Connection::open(&path)?;
    connection.execute_batch("DELETE FROM blocks_fts; DELETE FROM block_theme_hits; DELETE FROM blocks; DELETE FROM documents;")?;
    drop(connection);
    let backend = AlexandriaBackend::open(fixture_config(&path)?)?;
    assert_eq!(
        backend
            .count_documents(DocumentCountRequest {
                purpose: RetrievalPurpose::LocalUi,
                timeout_ms: 5_000
            })?
            .value,
        0
    );
    Ok(())
}
