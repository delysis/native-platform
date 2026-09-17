//! Native import and export boundaries, independent of any editor.
use easl::parse::{
    ImportLimits, load_easl_imports_with_lookup_function, parse_easl, read_easl_source,
};
use std::fs;

#[test]
fn diamond_and_cyclic_imports_read_each_dependency_once() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("root.easl");
    let source = "(import \"a.easl\") (import \"b.easl\") (import \"./a.easl\")";
    fs::write(&root, "disk must not be read").unwrap();
    fs::write(directory.path().join("a.easl"), "(import \"common.easl\")").unwrap();
    fs::write(
        directory.path().join("b.easl"),
        "(import \"common.easl\") (import \"root.easl\")",
    )
    .unwrap();
    fs::write(
        directory.path().join("common.easl"),
        "(defn shared-value []: u32 1u)",
    )
    .unwrap();
    let mut reads = vec![];
    let documents = load_easl_imports_with_lookup_function(
        parse_easl(source),
        Some(&root),
        source.into(),
        ImportLimits::default(),
        |path| {
            reads.push(path.to_owned());
            read_easl_source(path, 1024)
        },
    )
    .unwrap()
    .unwrap();
    assert_eq!(reads.len(), 3);
    assert_eq!(documents.sources.len(), 4);
    assert!(!reads.contains(&root));
    for (index, (document, _, _)) in documents.sources.iter().enumerate() {
        assert!(
            document
                .syntax_trees
                .iter()
                .all(|tree| tree.position().path.first() == Some(&index))
        );
    }
}

#[test]
fn limits_reject_the_graph_before_opening_excess_sources() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("root.easl");
    fs::write(directory.path().join("a.easl"), "0123456789").unwrap();
    fs::write(directory.path().join("b.easl"), "0123456789").unwrap();
    let source = "(import \"a.easl\") (import \"b.easl\")";
    let mut reads = 0;
    let result = load_easl_imports_with_lookup_function(
        parse_easl(source),
        Some(&root),
        source.into(),
        ImportLimits {
            documents: 2,
            utf8_bytes: 1000,
        },
        |path| {
            reads += 1;
            read_easl_source(path, 1024)
        },
    );
    assert!(result.unwrap().is_err());
    assert_eq!(reads, 0);
    let result = load_easl_imports_with_lookup_function(
        parse_easl(source),
        Some(&root),
        source.into(),
        ImportLimits {
            documents: 3,
            utf8_bytes: source.len() + 19,
        },
        |path| read_easl_source(path, 1024),
    );
    assert!(result.unwrap().is_err());
    assert!(read_easl_source(&directory.path().join("a.easl"), 9).is_err());
    assert!(read_easl_source(directory.path(), 1024).is_err());
}

#[test]
fn bounded_exports_reject_private_and_oversized_values_before_copying() {
    use easl::{
        CompilerTarget,
        compiler::{builtins::built_in_macros, program::Program},
        external::{ExternalVarError, ExternalVars},
    };
    let documents = easl::parse::EaslMultiDocument::from_singular_document_sourceless(parse_easl(
        "@external (var exported: [u32]) (var private: u32 1u) @cpu (defn main [] ())",
    ));
    let (mut program, errors) = Program::from_easl_documents(&documents, built_in_macros());
    assert!(errors.is_empty());
    assert!(
        program
            .validate_raw_program(CompilerTarget::WGSL)
            .is_empty()
    );
    let external = ExternalVars::new(&program);
    external
        .write_external_var_raw("exported", &[1, 2, 3, 4])
        .unwrap();
    assert!(matches!(
        external.read_external_var_raw_bounded("exported", 3),
        Err(ExternalVarError::ReadLimitExceeded { limit: 3, got: 4 })
    ));
    assert_eq!(
        external
            .read_external_var_raw_bounded("exported", 4)
            .unwrap(),
        [1, 2, 3, 4]
    );
    assert!(
        external
            .read_external_var_raw_bounded("private", 4)
            .is_err()
    );
    external.write_external_var_raw("exported", &[9]).unwrap();
    assert_eq!(
        external
            .read_external_var_raw_bounded("exported", 1)
            .unwrap(),
        [9]
    );
}
