//! Explicit, bounded context from registered documents. References never open
//! arbitrary filesystem paths or recursively evaluate other documents.

use std::collections::HashSet;
use std::fmt::Write as _;

use loom_store::{DocumentSummary, ProjectStore};
use loom_types::{ArtifactId, BlobId, DocumentId, RevisionId};
use serde::{Deserialize, Serialize};

use super::{IpcFailure, title_for_path};

const MAX_REFERENCES: usize = 256;
const MAX_DOCUMENTS: usize = 32;
const MAX_CONTEXT_BYTES: usize = 65_536;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct ResolvedDocument {
    pub name: String,
    pub path: String,
    pub document_id: DocumentId,
    pub revision_id: RevisionId,
    pub blob_id: BlobId,
    pub artifact_id: ArtifactId,
    pub text: String,
}

/// Exact paths win over aliases. Otherwise an extensionless path or a display
/// title must identify exactly one document. Folder members are path-sorted;
/// overlapping references retain each document only at its first occurrence.
pub(super) fn resolve_references(
    store: &ProjectStore,
    names: &[String],
) -> Result<Vec<ResolvedDocument>, IpcFailure> {
    if names.len() > MAX_REFERENCES {
        return Err(limit_failure(
            "At most 256 document references are supported.",
        ));
    }
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let registry = store.list_documents().map_err(IpcFailure::store)?;
    let mut selected = Vec::new();
    let mut seen = HashSet::new();
    for name in names {
        validate_name(name)?;
        if name.ends_with('/') {
            let mut found = false;
            for document in &registry {
                if document.relative_path.starts_with(name) {
                    found = true;
                    select_document(document, &mut selected, &mut seen)?;
                }
            }
            if !found {
                return Err(missing_reference(name));
            }
        } else {
            select_document(resolve_document(&registry, name)?, &mut selected, &mut seen)?;
        }
    }

    let mut bytes = 0;
    let mut resolved = Vec::with_capacity(selected.len());
    for document in selected {
        // The store binds the visible bytes to their current revision and
        // rejects uncheckpointed edits and symlinks instead of reading history.
        let loaded = store
            .read_document(&document.relative_path)
            .map_err(IpcFailure::store)?;
        bytes += loaded.text.len();
        if bytes > MAX_CONTEXT_BYTES {
            return Err(limit_failure(
                "Referenced documents exceed 64 KiB. Choose fewer or smaller documents.",
            ));
        }
        resolved.push(ResolvedDocument {
            name: document
                .display_title
                .clone()
                .unwrap_or_else(|| title_for_path(&loaded.relative_path)),
            path: loaded.relative_path,
            document_id: loaded.document_id,
            revision_id: loaded.revision_id,
            blob_id: loaded.blob_id,
            artifact_id: loaded.artifact_id,
            text: loaded.text,
        });
    }
    Ok(resolved)
}

pub(super) fn context_for_markdown(
    store: &ProjectStore,
    markdown: &str,
) -> Result<String, IpcFailure> {
    let names = loom_document::document_references(markdown)
        .map_err(|error| IpcFailure::new("document_reference_syntax", error.to_string(), false))?
        .into_iter()
        .map(|reference| reference.name)
        .collect::<Vec<_>>();
    let documents = resolve_references(store, &names)?;
    if documents.is_empty() {
        return Ok(String::new());
    }
    let mut context = String::from("Referenced documents (context for the current request):\n");
    for document in documents {
        // Debug string quoting keeps a title/path from forging a header line.
        let _ = write!(
            context,
            "\n--- Document {:?}; path {:?} ---\n",
            document.name, document.path
        );
        context.push_str(&document.text);
        context.push_str("\n--- End document ---\n");
        if context.len() > MAX_CONTEXT_BYTES {
            return Err(limit_failure(
                "Document context including its labels exceeds 64 KiB. Choose fewer or smaller documents.",
            ));
        }
    }
    Ok(context)
}

fn select_document<'a>(
    document: &'a DocumentSummary,
    selected: &mut Vec<&'a DocumentSummary>,
    seen: &mut HashSet<DocumentId>,
) -> Result<(), IpcFailure> {
    if seen.insert(document.document_id) {
        if selected.len() == MAX_DOCUMENTS {
            return Err(limit_failure(
                "At most 32 distinct documents can be referenced at once.",
            ));
        }
        selected.push(document);
    }
    Ok(())
}

fn resolve_document<'a>(
    registry: &'a [DocumentSummary],
    name: &str,
) -> Result<&'a DocumentSummary, IpcFailure> {
    if let Some(exact) = registry
        .iter()
        .find(|document| document.relative_path == name)
    {
        return Ok(exact);
    }
    let mut matches = registry.iter().filter(|document| {
        extensionless_path(&document.relative_path) == name
            || document.display_title.as_deref().map_or_else(
                || title_for_path(&document.relative_path) == name,
                |title| title == name,
            )
    });
    let first = matches.next().ok_or_else(|| missing_reference(name))?;
    if matches.next().is_some() {
        return Err(IpcFailure::new(
            "document_reference_ambiguous",
            format!(
                "Document reference {name:?} is ambiguous. Use its complete project-relative path."
            ),
            false,
        ));
    }
    Ok(first)
}

fn extensionless_path(path: &str) -> &str {
    match path.rsplit_once('.') {
        Some((stem, extension))
            if ["md", "markdown", "txt"]
                .iter()
                .any(|supported| extension.eq_ignore_ascii_case(supported)) =>
        {
            stem
        }
        _ => path,
    }
}

fn validate_name(name: &str) -> Result<(), IpcFailure> {
    if name.is_empty()
        || name.len() > 4096
        || name.chars().any(char::is_control)
        || name.strip_suffix('/').is_some_and(|folder| {
            folder.contains('\\')
                || folder
                    .split('/')
                    .any(|part| part.is_empty() || part == "." || part == "..")
        })
    {
        return Err(IpcFailure::new(
            "document_reference_invalid",
            "Use a document name or a relative project path; end folder references with '/'.",
            false,
        ));
    }
    Ok(())
}

fn missing_reference(name: &str) -> IpcFailure {
    IpcFailure::new(
        "document_reference_missing",
        format!("No registered document or nonempty folder matches {name:?}."),
        false,
    )
}

fn limit_failure(message: &str) -> IpcFailure {
    IpcFailure::new("document_reference_limit", message, false)
}

#[cfg(all(test, unix))]
mod tests {
    use loom_document::DocumentContent;

    use super::*;

    fn fixture() -> (tempfile::TempDir, ProjectStore) {
        let directory = tempfile::tempdir().expect("temporary project parent");
        let (store, _) = ProjectStore::initialize(directory.path().join("Writing"), "Writing")
            .expect("initialize project");
        (directory, store)
    }

    fn create(store: &mut ProjectStore, path: &str, text: &str) {
        store
            .create_document_if_absent(path, DocumentContent::Prose(text.into()), "fixture")
            .expect("create document");
    }

    fn resolve(store: &ProjectStore, names: &[&str]) -> Result<Vec<ResolvedDocument>, IpcFailure> {
        resolve_references(
            store,
            &names
                .iter()
                .map(|name| (*name).to_owned())
                .collect::<Vec<_>>(),
        )
    }

    #[test]
    fn folders_are_sorted_and_overlap_is_deduplicated_with_current_identities() {
        let (_directory, mut store) = fixture();
        create(&mut store, "notes/z.md", "last");
        create(&mut store, "notes/a.md", "first");
        create(&mut store, "notes-other/secret.md", "outside");
        let documents = resolve(&store, &["notes/", "notes/a", "notes/z.md"]).expect("resolve");
        assert_eq!(
            documents
                .iter()
                .map(|doc| doc.path.as_str())
                .collect::<Vec<_>>(),
            ["notes/a.md", "notes/z.md"]
        );
        let active = store.read_document("notes/a.md").expect("active document");
        assert_eq!(documents[0].document_id, active.document_id);
        assert_eq!(documents[0].revision_id, active.revision_id);
        assert_eq!(documents[0].blob_id, active.blob_id);
        assert_eq!(documents[0].text, "first");
        create(&mut store, "notes/Some-title.md", "named context");
        let named = resolve(&store, &["Some title"]).expect("visible title of imported document");
        assert_eq!(named[0].path, "notes/Some-title.md");
        assert_eq!(named[0].name, "Some title");
    }

    #[test]
    fn extension_aliases_must_be_unique_and_exact_paths_disambiguate() {
        let (_directory, mut store) = fixture();
        create(&mut store, "notes.md", "markdown");
        create(&mut store, "notes.txt", "text");
        assert_eq!(
            resolve(&store, &["notes"]).expect_err("ambiguous").code,
            "document_reference_ambiguous"
        );
        assert_eq!(
            resolve(&store, &["notes.txt"]).expect("exact")[0].text,
            "text"
        );
        assert_eq!(
            resolve(&store, &["missing"]).expect_err("missing").code,
            "document_reference_missing"
        );
        assert_eq!(
            resolve(&store, &["../notes.md"])
                .expect_err("traversal")
                .code,
            "document_reference_missing"
        );
    }

    #[cfg(any(
        target_vendor = "apple",
        target_os = "linux",
        target_os = "android",
        target_os = "redox"
    ))]
    #[test]
    fn display_titles_preserve_punctuation_and_ambiguity_requires_a_path() {
        let (_directory, mut store) = fixture();
        create(&mut store, "one/original.md", "first");
        create(&mut store, "two/original.md", "second");
        let title = "Why — how & when";
        let mut first = store
            .open_document_file("one/original.md")
            .expect("first authority");
        let renamed = store
            .rename_document(&mut first, title)
            .expect("first title");
        assert_eq!(resolve(&store, &[title]).expect("title")[0].name, title);
        let mut second = store
            .open_document_file("two/original.md")
            .expect("second authority");
        store
            .rename_document(&mut second, title)
            .expect("second title");
        assert_eq!(
            resolve(&store, &[title]).expect_err("ambiguous title").code,
            "document_reference_ambiguous"
        );
        assert_eq!(
            resolve(&store, &[&renamed.relative_path]).expect("exact path")[0].text,
            "first"
        );
    }

    #[test]
    fn context_ignores_code_literals_and_does_not_expand_referenced_documents() {
        let (_directory, mut store) = fixture();
        create(
            &mut store,
            "notes.md",
            "Source text with @unresolved embedded.",
        );
        let context =
            context_for_markdown(&store, "Use @notes. `@missing`\n```\n@also-missing\n```\n")
                .expect("explicit references only");
        assert!(context.contains("Source text with @unresolved embedded."));
        assert_eq!(context.matches("--- Document ").count(), 1);
        assert!(
            context_for_markdown(&store, "Only `@notes`.")
                .expect("no references")
                .is_empty()
        );
    }

    #[test]
    fn external_changes_and_symlinks_cannot_substitute_context() {
        let (directory, mut store) = fixture();
        create(&mut store, "notes.md", "registered text");
        let visible = store.root().join("notes.md");
        std::fs::write(&visible, "uncheckpointed edit").expect("external edit");
        assert!(resolve(&store, &["notes"]).is_err());
        std::fs::remove_file(&visible).expect("remove edited fixture");
        let outside = directory.path().join("outside.md");
        std::fs::write(&outside, "registered text").expect("outside fixture");
        std::os::unix::fs::symlink(outside, visible).expect("external symlink");
        assert!(resolve(&store, &["notes"]).is_err());
    }

    #[test]
    fn oversized_documents_and_folders_fail_without_partial_results() {
        let (_directory, mut store) = fixture();
        create(&mut store, "large.md", &"x".repeat(MAX_CONTEXT_BYTES + 1));
        assert_eq!(
            resolve(&store, &["large"])
                .expect_err("too many bytes")
                .code,
            "document_reference_limit"
        );
        for index in 0..=MAX_DOCUMENTS {
            create(&mut store, &format!("folder/{index}.md"), "small");
        }
        assert_eq!(
            resolve(&store, &["folder/"])
                .expect_err("too many documents")
                .code,
            "document_reference_limit"
        );
    }
}
