use std::fs;

use loom_document::DocumentContent;

use super::*;

fn project() -> (tempfile::TempDir, ProjectStore) {
    let directory = tempfile::tempdir().unwrap();
    let (store, _) = ProjectStore::initialize(directory.path().join("Writing"), "Writing").unwrap();
    (directory, store)
}

fn document(store: &mut ProjectStore, path: &str, text: &str) {
    store
        .create_document_if_absent(path, DocumentContent::Prose(text.into()), "context fixture")
        .unwrap();
}

fn attachment(store: &mut ProjectStore, name: &str, text: &str) -> MaterialEntry {
    let path = store.root().join("source-fixture.txt");
    fs::write(&path, text).unwrap();
    let prepared = crate::context_attachments::import_path(store.root(), &path).unwrap();
    materials::bind_attachment(store, &prepared.id, Some(name)).unwrap()
}

#[test]
fn child_writing_reads_owner_sources_without_mixing_document_identity() {
    let (_owner_directory, mut owner) = project();
    let (_child_directory, mut child) = project();
    let source = attachment(&mut owner, "Research", "The nightjar sings at dusk.");
    let inline = attachment(&mut child, "Inline", "A source dropped into this document.");
    document(&mut child, "Notes.md", "My local note.");
    let context = ReadContext {
        documents: &child,
        materials: &owner,
    };
    assert_eq!(
        exact(&context.resolve(&inline.id).unwrap()).unwrap(),
        "A source dropped into this document."
    );
    let plan = markdown_plan_with_budget(
        context,
        "@Research @Notes",
        "nightjar",
        4096,
        ReferenceRequirement::All,
    )
    .unwrap();
    assert!(plan.text.contains("The nightjar sings at dusk."));
    assert!(plan.text.contains("My local note."));
    let note = child.read_document("Notes.md").unwrap();
    assert_eq!(
        local_artifact_ids(&child, plan.bindings.values()).unwrap(),
        vec![note.artifact_id]
    );
    let snapshot: ContextPlan =
        serde_json::from_slice(&serde_json::to_vec(&plan).unwrap()).unwrap();
    assert_eq!(
        exact(&snapshot.bindings["Research"]).unwrap(),
        "The nightjar sings at dusk."
    );
    let found = context
        .search_with_cancel(
            &plan.bindings["Research"],
            "nightjar",
            &FolderScanBudget::default(),
            &|| false,
        )
        .unwrap();
    assert!(exact(&found).unwrap().contains("nightjar"));
    assert!(local_artifact_ids(&child, [&found]).unwrap().is_empty());
    let Value::Scoped {
        origin: source_origin,
        ..
    } = &plan.bindings["Research"]
    else {
        panic!("source origin")
    };
    let Value::Scoped {
        origin: evidence_origin,
        ..
    } = &found
    else {
        panic!("evidence origin")
    };
    assert_eq!(source_origin, evidence_origin);
    let Value::Evidence { evidence, .. } = found.unscoped() else {
        panic!("evidence")
    };
    let retained = context
        .resolve(&format!("evidence/{}", evidence[0].id))
        .unwrap();
    assert_eq!(exact(&retained).unwrap(), exact(&found).unwrap());
    assert!(
        ReadContext::from(&child)
            .search_with_cancel(&found, "nightjar", &FolderScanBudget::default(), &|| false)
            .is_err()
    );
    document(&mut child, "nested/Research.md", "Conflicting local alias.");
    assert!(
        ReadContext {
            documents: &child,
            materials: &owner
        }
        .resolve("Research")
        .is_err()
    );
    assert_eq!(
        exact(
            &ReadContext {
                documents: &child,
                materials: &owner
            }
            .resolve(&source.id)
            .unwrap()
        )
        .unwrap(),
        "The nightjar sings at dusk."
    );
}

#[test]
fn explicit_owner_media_uses_owner_bytes_and_binding() {
    let (_owner_directory, mut owner) = project();
    let (_child_directory, child) = project();
    let png = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/native.png");
    let imported = crate::context_attachments::import_path(owner.root(), &png).unwrap();
    let material = materials::bind_attachment(&mut owner, &imported.id, Some("Picture")).unwrap();
    let context = ReadContext {
        documents: &child,
        materials: &owner,
    };
    let value = context.resolve("Picture").unwrap();
    let expected = materials::native_media(&owner, &material.id).unwrap();
    assert!(!expected.is_empty());
    assert_eq!(context.native_media([&value]).unwrap(), expected);
    assert!(ReadContext::from(&child).native_media([&value]).is_err());
    materials::remove(&mut owner, &material.id).unwrap();
    assert!(
        ReadContext {
            documents: &child,
            materials: &owner
        }
        .native_media([&value])
        .is_err()
    );
}

#[test]
fn copied_projects_cannot_supply_local_artifact_fks_or_ambiguous_evidence() {
    fn copy_tree(source: &std::path::Path, destination: &std::path::Path) {
        fs::create_dir_all(destination).unwrap();
        for entry in fs::read_dir(source).unwrap() {
            let entry = entry.unwrap();
            let target = destination.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_tree(&entry.path(), &target);
            } else {
                fs::copy(entry.path(), target).unwrap();
            }
        }
    }
    let (directory, mut original) = project();
    document(&mut original, "Notes/Field.md", "The nightjar stays here.");
    let resolved = ReadContext::from(&original)
        .resolve("Notes/Field.md")
        .unwrap();
    let folder = ReadContext::from(&original).resolve("Notes/").unwrap();
    let found = ReadContext::from(&original)
        .search_with_cancel(&folder, "nightjar", &FolderScanBudget::default(), &|| false)
        .unwrap();
    let Value::Evidence { evidence, .. } = found.unscoped() else {
        panic!("evidence")
    };
    let reference = format!("evidence/{}", evidence[0].id);
    let original_root = original.root().to_owned();
    drop(original);
    let copied_root = directory.path().join("Copy");
    copy_tree(&original_root, &copied_root);
    let original = ProjectStore::open(&original_root).unwrap();
    let copy = ProjectStore::open(&copied_root).unwrap();
    assert_eq!(original.manifest().project_id, copy.manifest().project_id);
    assert!(
        !local_artifact_ids(&original, [&resolved, &found])
            .unwrap()
            .is_empty()
    );
    assert!(
        local_artifact_ids(&copy, [&resolved, &found])
            .unwrap()
            .is_empty()
    );
    let context = ReadContext {
        documents: &copy,
        materials: &original,
    };
    assert_eq!(
        context.resolve(&reference).unwrap_err().code,
        "material_context_invalid"
    );
    // A corrupt copy must not silently fall through to the valid original.
    let evidence_file = copy
        .root()
        .join(".loom/materials/evidence")
        .join(format!("{}.json", evidence[0].id));
    assert!(evidence_file.is_file());
    fs::write(&evidence_file, "corrupt").unwrap();
    assert_eq!(
        context.resolve(&reference).unwrap_err().code,
        "material_failed"
    );
}

#[test]
fn writing_continues_with_recorded_missing_references_and_smart_quoted_context() {
    let (_directory, mut store) = project();
    document(&mut store, "Notes.md", "A known source.");
    document(&mut store, "left/Ambiguous.md", "Left source");
    document(&mut store, "right/Ambiguous.md", "Right source");
    let manuscript = "The quiet moon.  @Missing\n\n@“Notes.md” @Ambiguous ";
    let plan = markdown_plan_with_budget(
        &store,
        manuscript,
        manuscript,
        4000,
        ReferenceRequirement::AvailableForWriting,
    )
    .unwrap();
    assert_eq!(
        plan.bindings.keys().map(String::as_str).collect::<Vec<_>>(),
        ["Notes.md"]
    );
    assert!(plan.text.contains("A known source."));
    assert_eq!(
        plan.unresolved_references
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["Ambiguous", "Missing"]
    );
    let receipt = serde_json::to_value(&plan).unwrap();
    assert!(
        receipt["unresolved_references"]["Missing"]
            .as_str()
            .unwrap()
            .contains("Missing")
    );
    assert_eq!(
        markdown_plan(&store, manuscript, manuscript)
            .unwrap_err()
            .code,
        "document_reference_missing"
    );
    assert_eq!(
        std::fs::read_to_string(store.root().join("Notes.md")).unwrap(),
        "A known source."
    );
}

#[test]
fn document_and_material_aliases_cannot_silently_choose_one_another() {
    let (_directory, mut store) = project();
    document(&mut store, "left/Research.md", "First document");
    let material = attachment(&mut store, "Research", "Retained source");
    assert_eq!(
        resolve(&store, "Research").unwrap_err().code,
        "material_context_invalid"
    );
    assert_eq!(
        exact(&resolve(&store, &material.id).unwrap()).unwrap(),
        "Retained source"
    );
    assert_eq!(
        exact(&resolve(&store, "left/Research.md").unwrap()).unwrap(),
        "First document"
    );

    document(&mut store, "right/Research.md", "Second document");
    assert_eq!(
        resolve(&store, "Research").unwrap_err().code,
        "document_reference_ambiguous"
    );
}

#[test]
fn exact_document_path_precedes_conflicting_material_aliases() {
    let (_directory, mut store) = project();
    document(&mut store, "Draft.md", "Exact document");
    attachment(&mut store, "Draft.md", "First material");
    attachment(&mut store, "Draft.md", "Second material");
    assert_eq!(
        exact(&resolve(&store, "Draft.md").unwrap()).unwrap(),
        "Exact document"
    );
}

#[test]
fn a_large_source_requires_explicit_retrieval_instead_of_silent_truncation() {
    let (_directory, mut store) = project();
    let source = format!(
        "{}\nThe distinctive nightjar is awake.\n",
        "Plain prose. ".repeat(6000)
    );
    let material = attachment(&mut store, "Long source", &source);
    let value = resolve(&store, &material.id).unwrap();
    assert!(exact(&value).is_err());
    let result = search(&store, &value, "nightjar").unwrap();
    let text = exact(&result).unwrap();
    assert!(text.contains("nightjar"));
    assert!(text.len() < source.len());
    let Value::Evidence {
        evidence,
        retrieval,
    } = result
    else {
        panic!("search evidence")
    };
    let retrieval = retrieval.unwrap();
    assert_eq!(retrieval.query, "nightjar");
    assert!(!evidence.is_empty());
    assert!(
        evidence
            .iter()
            .all(|hit| hit.source_revision == retrieval.source_revision)
    );
}

#[test]
fn partial_preparation_never_becomes_an_exact_whole_source_argument() {
    let (_directory, mut store) = project();
    let material = attachment(&mut store, "Partial", "The surviving passage.");
    let mut read = materials::read(&store, &material.id).unwrap();
    // Exercise the value boundary with incomplete preparation metadata. The
    // retained text's presence alone must never imply whole-source coverage.
    read.complete = false;
    let value = Value::Material {
        material: read.into(),
    };
    assert!(
        exact(&value)
            .unwrap_err()
            .message
            .contains("partially prepared")
    );
    assert!(matches!(
        consult(&store, &value, "surviving").unwrap(),
        Value::Evidence { .. }
    ));
}

#[test]
fn retained_material_links_do_not_rebind_when_the_friendly_name_is_reused() {
    let (_directory, mut store) = project();
    let original = attachment(&mut store, "Research", "Original evidence");
    let snapshot = resolve(&store, &original.id).unwrap();
    materials::remove(&mut store, &original.id).unwrap();
    let replacement = attachment(&mut store, "Research", "Unrelated replacement");
    assert_ne!(original.id, replacement.id);
    assert_eq!(exact(&snapshot).unwrap(), "Original evidence");
    let markdown = format!("Use [@Research](loom-material:{}).", original.id);
    assert!(markdown_plan(&store, &markdown, "evidence").is_err());
    assert_eq!(
        exact(&resolve(&store, "Research").unwrap()).unwrap(),
        "Unrelated replacement"
    );
}

#[test]
fn stable_material_identity_cannot_be_shadowed_by_a_document_path() {
    let (_directory, mut store) = project();
    let material = attachment(&mut store, "Research", "Retained original");
    document(&mut store, &material.id, "Unrelated document");
    let markdown = format!("[@Research](loom-material:{})", material.id);
    let plan = markdown_plan(&store, &markdown, "original").unwrap();
    assert!(plan.text.contains("Retained original"));
    assert!(!plan.text.contains("Unrelated document"));
    materials::remove(&mut store, &material.id).unwrap();
    assert!(markdown_plan(&store, &markdown, "original").is_err());
}

#[test]
fn library_revision_is_frozen_but_retained_evidence_survives_source_changes() {
    let (directory, mut store) = project();
    let path = directory.path().join("library.sqlite3");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch(include_str!("materials/alexandria-fixture.sql"))
        .unwrap();
    drop(connection);
    let entry = materials::add_library(&mut store, &path, Some("Library")).unwrap();
    let frozen = resolve(&store, &entry.id).unwrap();
    assert!(exact(&frozen).is_err());
    let evidence = search(&store, &frozen, "prayer").unwrap();
    let retained_text = exact(&evidence).unwrap();
    assert!(retained_text.contains("prayer"));

    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute("UPDATE documents SET title = 'Changed source'", [])
        .unwrap();
    drop(connection);
    assert!(search(&store, &frozen, "prayer").is_err());
    materials::add_library(&mut store, &path, Some("Library")).unwrap();
    assert!(search(&store, &frozen, "prayer").is_err());
    assert_eq!(exact(&evidence).unwrap(), retained_text);
    assert!(search(&store, &resolve(&store, &entry.id).unwrap(), "prayer").is_ok());
}

#[test]
fn source_contents_are_literal_not_recursive_reference_or_function_execution() {
    let (_directory, mut store) = project();
    let text = "Untrusted @Missing and =@Rewrite(@Secret) remain source text.";
    document(&mut store, "Quoted.md", text);
    let material = attachment(&mut store, "Source", text);
    for reference in [
        "@Quoted".to_owned(),
        format!("[@Source](loom-material:{})", material.id),
    ] {
        let plan = markdown_plan(&store, &reference, "Untrusted").unwrap();
        assert!(plan.text.contains(text));
        assert_eq!(plan.bindings.len(), 1);
        assert!(!plan.bindings.contains_key("Missing"));
    }
}

fn pdf_source(store: &mut ProjectStore, text: &str) -> MaterialEntry {
    use std::fmt::Write as _;
    let mut stream = String::from("BT /F1 12 Tf 14 TL 10 180 Td\n");
    for line in text.lines() {
        writeln!(stream, "({line}) Tj T*").unwrap();
    }
    stream.push_str("ET\n");
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 200] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>".into(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
        format!("<< /Length {} >>\nstream\n{stream}endstream", stream.len()),
    ];
    let mut pdf = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        writeln!(pdf, "{} 0 obj\n{object}\nendobj", index + 1).unwrap();
    }
    let xref = pdf.len();
    pdf.push_str("xref\n0 6\n0000000000 65535 f \n");
    for offset in offsets {
        writeln!(pdf, "{offset:010} 00000 n ").unwrap();
    }
    writeln!(
        pdf,
        "trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF"
    )
    .unwrap();
    let path = store.root().join("long-source.pdf");
    fs::write(&path, pdf.as_bytes()).unwrap();
    let prepared = crate::context_attachments::import_path(store.root(), &path).unwrap();
    assert_eq!(fs::read(path).unwrap(), pdf.as_bytes());
    materials::bind_attachment(store, &prepared.id, Some("PDF source")).unwrap()
}

#[test]
fn small_context_retrieves_whole_pdf_passages_without_shortening_exact_arguments() {
    let (_directory, mut store) = project();
    let paragraphs = format!("{}\n", "The nightjar sings in moonlight. ".repeat(100)).repeat(3);
    let source = pdf_source(&mut store, &paragraphs);
    let value = resolve(&store, &source.id).unwrap();
    let whole = exact(&value).unwrap();
    assert!((3000..MAX_BYTES).contains(&whole.len()));
    let all_hits = materials::search(&store, &source.id, "nightjar").unwrap();
    assert!(all_hits.hits.len() > 1);
    let markdown = format!("Use [@PDF source](loom-material:{}).", source.id);
    let plan = markdown_plan_with_budget(
        &store,
        &markdown,
        "nightjar",
        2800,
        ReferenceRequirement::All,
    )
    .unwrap();
    assert!(plan.text.len() <= 2800);
    assert!(!plan.evidence.is_empty());
    assert!(plan.evidence.len() < all_hits.hits.len());
    assert_eq!(plan.evidence[0].text, all_hits.hits[0].text);
    assert_eq!(plan.evidence[0].id, all_hits.hits[0].id);
    let hit = &plan.evidence[0];
    let start = usize::try_from(hit.locator["start_byte"].as_u64().unwrap()).unwrap();
    let end = usize::try_from(hit.locator["end_byte"].as_u64().unwrap()).unwrap();
    assert_eq!(&whole[start..end], hit.text);
    let Value::Evidence {
        retrieval: Some(retrieval),
        ..
    } = plan.bindings[&source.id].unscoped()
    else {
        panic!("frozen retrieval")
    };
    assert!(!retrieval.complete);
    assert!(
        retrieval
            .warnings
            .iter()
            .any(|warning| warning.contains("context budget"))
    );
    assert_eq!(
        plan.omitted_evidence[&source.id].len() + plan.evidence.len(),
        all_hits.hits.len()
    );
    assert_eq!(exact(&value).unwrap(), whole);
    assert!(
        markdown_plan_with_budget(
            &store,
            &markdown,
            "nightjar",
            100,
            ReferenceRequirement::All
        )
        .is_err()
    );
}

#[test]
fn writing_defers_oversized_pdf_evidence_but_keeps_later_context_and_exact_receipts() {
    let (_directory, mut store) = project();
    let source = pdf_source(&mut store, &"The nightjar sings in moonlight. ".repeat(100));
    let retained = materials::read(&store, &source.id)
        .unwrap()
        .evidence
        .remove(0);
    document(&mut store, "Notes.md", "A small useful note.");
    let name = format!("evidence/{}", retained.id);
    let markdown = format!("[@Paper](loom-evidence:{}) @Notes", retained.id);
    let plan = markdown_plan_with_budget(
        &store,
        &markdown,
        "nightjar",
        600,
        ReferenceRequirement::AvailableForWriting,
    )
    .unwrap();
    assert!(plan.text.contains("A small useful note."));
    assert!(!plan.text.contains("nightjar"));
    assert!(plan.text.len() <= 600);
    assert!(plan.evidence.is_empty());
    assert!(!plan.bindings.contains_key(&name));
    assert!(
        native_media(&store, plan.bindings.values())
            .unwrap()
            .is_empty()
    );
    let Value::Evidence { evidence, .. } = plan.budget_omissions[&name].unscoped() else {
        panic!("the frozen, unconsumed evidence must remain in the receipt")
    };
    assert_eq!(evidence[0].id, retained.id);
    assert_eq!(evidence[0].text, retained.text);
    let receipt: ContextPlan =
        serde_json::from_value(serde_json::to_value(&plan).unwrap()).unwrap();
    assert_eq!(
        exact(&receipt.budget_omissions[&name]).unwrap(),
        evidence_text(std::slice::from_ref(&retained)).unwrap()
    );
    assert_eq!(
        markdown_plan_with_budget(
            &store,
            &markdown,
            "nightjar",
            600,
            ReferenceRequirement::All
        )
        .unwrap_err()
        .code,
        "material_context_budget_exceeded"
    );
    let full = markdown_plan_with_budget(
        &store,
        &markdown,
        "nightjar",
        MAX_BYTES,
        ReferenceRequirement::All,
    )
    .unwrap();
    assert_eq!(full.evidence[0].text, retained.text);
    assert!(full.budget_omissions.is_empty());

    fs::write(
        store
            .root()
            .join(".loom/materials/evidence")
            .join(format!("{}.json", retained.id)),
        b"{}",
    )
    .unwrap();
    assert_eq!(
        markdown_plan_with_budget(
            &store,
            &markdown,
            "nightjar",
            600,
            ReferenceRequirement::AvailableForWriting,
        )
        .unwrap_err()
        .code,
        "material_failed"
    );
}

#[test]
fn writing_with_no_context_room_or_a_large_document_still_has_a_valid_plan() {
    let (_directory, mut store) = project();
    document(&mut store, "Empty.md", "");
    document(&mut store, "Long.md", &"prose ".repeat(MAX_BYTES));
    for (name, budget, code) in [
        ("Empty", 0, "material_context_budget_exceeded"),
        ("Long", 4000, "document_reference_budget_exceeded"),
    ] {
        let markdown = format!("@{name}");
        let plan = markdown_plan_with_budget(
            &store,
            &markdown,
            "write on",
            budget,
            ReferenceRequirement::AvailableForWriting,
        )
        .unwrap();
        assert!(plan.text.is_empty());
        assert!(plan.bindings.is_empty());
        if name == "Empty" {
            assert!(plan.budget_omissions.contains_key(name));
        } else {
            assert!(plan.unresolved_references[name].contains("64 KiB"));
        }
        assert_eq!(
            markdown_plan_with_budget(
                &store,
                &markdown,
                "write on",
                budget,
                ReferenceRequirement::All
            )
            .unwrap_err()
            .code,
            code
        );
    }
}

#[test]
fn ordinary_budgeted_consultation_reports_zero_matches_explicitly() {
    let (_directory, mut store) = project();
    let source = attachment(&mut store, "Research", &"Quiet prose. ".repeat(1000));
    let plan = markdown_plan_with_budget(
        &store,
        &format!("@{}", source.id),
        "nightjar",
        1000,
        ReferenceRequirement::All,
    )
    .unwrap();
    assert!(plan.evidence.is_empty());
    assert!(
        plan.text
            .contains("No matching source passages were found.")
    );
    let Value::Evidence {
        retrieval: Some(retrieval),
        ..
    } = plan.bindings[&source.id].unscoped()
    else {
        panic!("frozen empty retrieval")
    };
    assert_eq!(retrieval.query, "nightjar");
    assert!(retrieval.hits.is_empty());
}

#[test]
fn corpus_count_retains_scope_and_refuses_snippets_or_replaced_source() {
    let (directory, mut store) = project();
    let path = directory.path().join("count.sqlite3");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch(include_str!("materials/alexandria-fixture.sql"))
        .unwrap();
    drop(connection);
    let entry = materials::add_library(&mut store, &path, Some("Research")).unwrap();
    let context = ReadContext::from(&store);
    let frozen = context.resolve(&entry.id).unwrap();
    let counted = context.count_documents(&frozen).unwrap();
    let Value::Count { count } = counted.unscoped() else {
        panic!("count lost its type");
    };
    assert_eq!(count.result.value, 1);
    assert_eq!(count.material.id, entry.id);
    assert_eq!(
        exact(&counted).unwrap(),
        "1 document record in the entire library \"Research\".\n"
    );
    let serialized = serde_json::to_vec(&counted).unwrap();
    let snippets = context
        .search_with_cancel(&frozen, "prayer", &FolderScanBudget::default(), &|| false)
        .unwrap();
    assert!(context.count_documents(&snippets).is_err());
    assert!(
        context
            .count_documents(&Value::Text("3 retrieved snippets".into()))
            .is_err()
    );
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute("UPDATE documents SET title='Changed'", [])
        .unwrap();
    drop(connection);
    assert!(ReadContext::from(&store).count_documents(&frozen).is_err());
    materials::add_library(&mut store, &path, Some("Research")).unwrap();
    assert!(ReadContext::from(&store).count_documents(&frozen).is_err());
    let replay: Value = serde_json::from_slice(&serialized).unwrap();
    assert_eq!(exact(&replay).unwrap(), exact(&counted).unwrap());
    assert_eq!(serde_json::to_vec(&replay).unwrap(), serialized);
}
