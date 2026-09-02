use super::*;
use chrono::{TimeZone, Utc};
use information_native_types::{
    ArtifactId, EvidenceLocator, ManagedMaterializationId, RedistributionPolicy, ReleaseId,
    RepresentationId, ResourceId, RetrievalPurpose, UsePermission,
};
use ruzstd::encoding::{CompressionLevel, compress_to_vec};
use sha2::{Digest, Sha256};
use std::error::Error;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

#[derive(Clone, Copy)]
enum FixtureCompression {
    None,
    Zstd,
    ZstdWithTrailingBytes,
    UnsupportedLzma,
}

struct FixtureEntry<'a> {
    namespace: u8,
    path: &'a str,
    title: &'a str,
    mime_index: u16,
    blob: &'a [u8],
}

fn build_fixture(entries: &[FixtureEntry<'_>], compression: FixtureCompression) -> Vec<u8> {
    let mut bytes = vec![0_u8; 80];
    bytes.extend_from_slice(b"text/html\0text/plain\0\0");
    bytes.resize(256, 0);
    let cluster_offset = bytes.len() as u64;

    let width = 4_usize;
    let table_bytes = entries.len().saturating_add(1).saturating_mul(width);
    let mut decoded = Vec::new();
    let mut offset = table_bytes;
    decoded.extend_from_slice(&(offset as u32).to_le_bytes());
    for entry in entries {
        offset = offset.saturating_add(entry.blob.len());
        decoded.extend_from_slice(&(offset as u32).to_le_bytes());
    }
    for entry in entries {
        decoded.extend_from_slice(entry.blob);
    }
    let (cluster_info, payload) = match compression {
        FixtureCompression::None => (1_u8, decoded),
        FixtureCompression::Zstd => (
            5_u8,
            compress_to_vec(decoded.as_slice(), CompressionLevel::Fastest),
        ),
        FixtureCompression::ZstdWithTrailingBytes => {
            let mut compressed = compress_to_vec(decoded.as_slice(), CompressionLevel::Fastest);
            compressed.extend_from_slice(b"trailing");
            (5_u8, compressed)
        }
        FixtureCompression::UnsupportedLzma => (4_u8, decoded),
    };
    bytes.push(cluster_info);
    bytes.extend_from_slice(&payload);

    let mut dirent_offsets = Vec::new();
    for (blob_index, entry) in entries.iter().enumerate() {
        dirent_offsets.push(bytes.len() as u64);
        bytes.extend_from_slice(&entry.mime_index.to_le_bytes());
        bytes.push(0);
        bytes.push(entry.namespace);
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        bytes.extend_from_slice(&(blob_index as u32).to_le_bytes());
        bytes.extend_from_slice(entry.path.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(entry.title.as_bytes());
        bytes.push(0);
    }
    let url_pointer_position = bytes.len() as u64;
    for offset in &dirent_offsets {
        bytes.extend_from_slice(&offset.to_le_bytes());
    }
    let cluster_pointer_position = bytes.len() as u64;
    bytes.extend_from_slice(&cluster_offset.to_le_bytes());
    let checksum_position = bytes.len() as u64;
    bytes.extend_from_slice(&[0_u8; 16]);

    write_u32(&mut bytes, 0, 0x044d_495a);
    write_u16(&mut bytes, 4, 6);
    write_u16(&mut bytes, 6, 1);
    for (index, byte) in bytes[8..24].iter_mut().enumerate() {
        *byte = index as u8;
    }
    write_u32(&mut bytes, 24, entries.len() as u32);
    write_u32(&mut bytes, 28, 1);
    write_u64(&mut bytes, 32, url_pointer_position);
    write_u64(&mut bytes, 40, u64::MAX);
    write_u64(&mut bytes, 48, cluster_pointer_position);
    write_u64(&mut bytes, 56, 80);
    write_u32(&mut bytes, 64, 0);
    write_u32(&mut bytes, 68, u32::MAX);
    write_u64(&mut bytes, 72, checksum_position);
    bytes
}

fn write_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn request(path: &Path, bytes: &[u8]) -> Result<ZimMaterializationRequest, Box<dyn Error>> {
    Ok(ZimMaterializationRequest::private(
        path.to_path_buf(),
        bytes.len() as u64,
        format!("{:x}", Sha256::digest(bytes)),
        ArtifactId::parse("kiwix-zim")?,
        "https://download.kiwix.org/test.zim".to_string(),
        ResourceId::parse("kiwix-test")?,
        ReleaseId::parse("2026-08")?,
        RepresentationId::parse("zim")?,
        ManagedMaterializationId::parse("kiwix-test-2026-08")?,
        "Kiwix".to_string(),
        Utc.with_ymd_and_hms(2026, 8, 24, 12, 0, 0)
            .single()
            .ok_or("invalid test timestamp")?,
    ))
}

fn write_fixture(
    temp: &TempDir,
    name: &str,
    bytes: &[u8],
) -> Result<std::path::PathBuf, Box<dyn Error>> {
    let path = temp.path().join(name);
    fs::write(&path, bytes)?;
    Ok(path)
}

#[test]
fn uncompressed_html_becomes_hash_bound_inert_managed_text() -> Result<(), Box<dyn Error>> {
    let fixture = build_fixture(
        &[FixtureEntry {
            namespace: b'C',
            path: "A/Test",
            title: "Test article",
            mime_index: 0,
            blob: b"<h1>Hello</h1><script>steal()</script><p>World &amp; all</p>",
        }],
        FixtureCompression::None,
    );
    let temp = TempDir::new()?;
    let path = write_fixture(&temp, "test.zim", &fixture)?;
    let before = fs::read(&path)?;
    let produced = produce_managed_documents(&request(&path, &fixture)?)?;

    assert_eq!(
        fs::read(&path)?,
        before,
        "source archive must remain read-only"
    );
    assert_eq!(produced.report.archive.bytes, fixture.len() as u64);
    assert_eq!(
        produced.report.archive.sha256,
        format!("{:x}", Sha256::digest(&fixture))
    );
    assert_eq!(
        produced.report.archive.uuid_hex,
        "000102030405060708090a0b0c0d0e0f"
    );
    assert_eq!(produced.report.validated_directory_entry_count, 1);
    assert_eq!(produced.report.codecs_used, vec!["none"]);
    let document = &produced.documents.documents[0];
    assert_eq!(document.segments[0].text, "Hello\nWorld & all");
    assert!(!document.segments[0].text.contains('<'));
    assert!(!document.segments[0].text.contains("steal"));
    assert_eq!(
        document
            .use_policy
            .permission_for(RetrievalPurpose::LocalUi),
        UsePermission::Allowed
    );
    assert_eq!(
        document
            .use_policy
            .permission_for(RetrievalPurpose::ModelContext),
        UsePermission::Unknown
    );
    assert_eq!(
        document
            .use_policy
            .permission_for(RetrievalPurpose::ExcerptExport),
        UsePermission::Forbidden
    );
    assert_eq!(document.use_policy.redistribution, UsePermission::Forbidden);
    assert_eq!(
        document.rights[0].redistribution,
        RedistributionPolicy::PrivateUseOnly
    );
    assert!(matches!(
        &document.locator,
        EvidenceLocator::ZimArticle {
            archive_uuid: Some(uuid),
            internal_path,
            entry_index: Some(0),
        } if uuid == "000102030405060708090a0b0c0d0e0f" && internal_path == "C/A/Test"
    ));
    assert_eq!(
        document.lineage[0].source_record_sha256,
        format!(
            "{:x}",
            Sha256::digest(b"<h1>Hello</h1><script>steal()</script><p>World &amp; all</p>")
        )
    );
    produced.documents.validate()?;
    Ok(())
}

#[test]
fn zstd_cluster_uses_bounded_pure_rust_decoder() -> Result<(), Box<dyn Error>> {
    let fixture = build_fixture(
        &[FixtureEntry {
            namespace: b'C',
            path: "A/Zstd",
            title: "Zstandard",
            mime_index: 1,
            blob: b"compressed article text",
        }],
        FixtureCompression::Zstd,
    );
    let temp = TempDir::new()?;
    let path = write_fixture(&temp, "zstd.zim", &fixture)?;
    let produced = produce_managed_documents(&request(&path, &fixture)?)?;
    assert_eq!(produced.report.codecs_used, vec!["zstd"]);
    assert_eq!(
        produced.documents.documents[0].segments[0].text,
        "compressed article text"
    );
    Ok(())
}

#[test]
fn hostile_out_of_bounds_pointer_table_is_rejected() -> Result<(), Box<dyn Error>> {
    let mut fixture = build_fixture(
        &[FixtureEntry {
            namespace: b'C',
            path: "A/Test",
            title: "Test",
            mime_index: 1,
            blob: b"text",
        }],
        FixtureCompression::None,
    );
    let invalid_position = fixture.len() as u64 - 4;
    write_u64(&mut fixture, 32, invalid_position);
    let temp = TempDir::new()?;
    let path = write_fixture(&temp, "hostile.zim", &fixture)?;
    assert!(matches!(
        produce_managed_documents(&request(&path, &fixture)?),
        Err(ZimError::OutOfBounds("URL pointer table"))
    ));
    Ok(())
}

#[test]
fn truncated_header_is_rejected_before_table_allocation() -> Result<(), Box<dyn Error>> {
    let mut fixture = build_fixture(
        &[FixtureEntry {
            namespace: b'C',
            path: "A/Test",
            title: "Test",
            mime_index: 1,
            blob: b"text",
        }],
        FixtureCompression::None,
    );
    fixture.truncate(40);
    let temp = TempDir::new()?;
    let path = write_fixture(&temp, "truncated.zim", &fixture)?;
    assert!(matches!(
        produce_managed_documents(&request(&path, &fixture)?),
        Err(ZimError::Truncated("header"))
    ));
    Ok(())
}

#[test]
fn unsorted_namespace_path_identity_is_rejected() -> Result<(), Box<dyn Error>> {
    let fixture = build_fixture(
        &[
            FixtureEntry {
                namespace: b'C',
                path: "Z/Last",
                title: "Last",
                mime_index: 1,
                blob: b"last",
            },
            FixtureEntry {
                namespace: b'C',
                path: "A/First",
                title: "First",
                mime_index: 1,
                blob: b"first",
            },
        ],
        FixtureCompression::None,
    );
    let temp = TempDir::new()?;
    let path = write_fixture(&temp, "unsorted.zim", &fixture)?;
    assert!(matches!(
        produce_managed_documents(&request(&path, &fixture)?),
        Err(ZimError::InvalidDirectoryEntry { entry_index: 1, .. })
    ));
    Ok(())
}

#[test]
fn invalid_blob_offset_table_is_rejected() -> Result<(), Box<dyn Error>> {
    let mut fixture = build_fixture(
        &[FixtureEntry {
            namespace: b'C',
            path: "A/Test",
            title: "Test",
            mime_index: 1,
            blob: b"text",
        }],
        FixtureCompression::None,
    );
    // Cluster starts at byte 256; byte 257 begins the decoded u32 offset table.
    write_u32(&mut fixture, 257, 7);
    let temp = TempDir::new()?;
    let path = write_fixture(&temp, "bad-cluster.zim", &fixture)?;
    assert!(matches!(
        produce_managed_documents(&request(&path, &fixture)?),
        Err(ZimError::InvalidCluster {
            cluster_index: 0,
            ..
        })
    ));
    Ok(())
}

#[test]
fn historical_lzma_cluster_is_explicitly_unsupported() -> Result<(), Box<dyn Error>> {
    let fixture = build_fixture(
        &[FixtureEntry {
            namespace: b'C',
            path: "A/Test",
            title: "Test",
            mime_index: 1,
            blob: b"text",
        }],
        FixtureCompression::UnsupportedLzma,
    );
    let temp = TempDir::new()?;
    let path = write_fixture(&temp, "lzma.zim", &fixture)?;
    assert!(matches!(
        produce_managed_documents(&request(&path, &fixture)?),
        Err(ZimError::UnsupportedCompression(4))
    ));
    Ok(())
}

#[test]
fn zstd_cluster_rejects_trailing_frames_or_bytes() -> Result<(), Box<dyn Error>> {
    let fixture = build_fixture(
        &[FixtureEntry {
            namespace: b'C',
            path: "A/Test",
            title: "Test",
            mime_index: 1,
            blob: b"text",
        }],
        FixtureCompression::ZstdWithTrailingBytes,
    );
    let temp = TempDir::new()?;
    let path = write_fixture(&temp, "zstd-trailing.zim", &fixture)?;
    assert!(matches!(
        produce_managed_documents(&request(&path, &fixture)?),
        Err(ZimError::Zstd(_))
    ));
    Ok(())
}

#[test]
fn exact_hash_and_split_archive_guards_fail_closed() -> Result<(), Box<dyn Error>> {
    let fixture = build_fixture(
        &[FixtureEntry {
            namespace: b'C',
            path: "A/Test",
            title: "Test",
            mime_index: 1,
            blob: b"text",
        }],
        FixtureCompression::None,
    );
    let temp = TempDir::new()?;
    let path = write_fixture(&temp, "split.zimaa", &fixture)?;
    assert!(matches!(
        produce_managed_documents(&request(&path, &fixture)?),
        Err(ZimError::SplitArchiveUnsupported)
    ));

    let path = write_fixture(&temp, "hash.zim", &fixture)?;
    let mut wrong = request(&path, &fixture)?;
    wrong.expected_archive_sha256 = "0".repeat(64);
    assert!(matches!(
        produce_managed_documents(&wrong),
        Err(ZimError::ArchiveHashMismatch)
    ));
    Ok(())
}
