//! Cached EASL glyph runs need ordinary array value semantics in both evaluators.
use easl::{
    CompilerTarget,
    compiler::{builtins::built_in_macros, program::Program},
    interpreter::{CpuRuntime, IOEvent, StringIO, run_program_entry_with_io_and_runtime_from_path},
    parse::{EaslMultiDocument, parse_easl_without_comments},
};

#[test]
fn dynamic_global_array_copy_preserves_independent_values_and_lengths() {
    let source = r#"
    (var cache-source: [vec4u]) (var cache-copy: [vec4u])
    @cpu (defn main []
      (= cache-source (zeroed-array 2u)) (= (cache-source 0u) (vec4u 1u 2u 3u 4u))
      (= cache-copy cache-source) (= (cache-source 0u) (vec4u 9u))
      (print (.x (cache-copy 0u)))
      (= (cache-copy 1u) (vec4u 7u)) (print (.w (cache-source 1u)))
      (= cache-copy cache-copy) (print (array-length cache-copy))
      (= cache-source (zeroed-array 0u)) (= cache-copy cache-source)
      (print (array-length cache-copy))
      (= cache-source (zeroed-array 2u)) (= cache-copy cache-source)
      (= (cache-copy 0u) (vec4u 8u)) (print (.z (cache-source 0u))))
    "#;
    replay(source, &["1u", "0u", "2u", "0u", "0u"]);
}

fn replay(source: &str, expected: &[&str]) {
    let docs =
        EaslMultiDocument::from_singular_document_sourceless(parse_easl_without_comments(source));
    let (mut program, errors) = Program::from_easl_documents(&docs, built_in_macros());
    assert!(errors.is_empty(), "{errors:?}");
    let errors = program.validate_raw_program(CompilerTarget::WGSL);
    assert!(errors.is_empty(), "{errors:?}");
    for runtime in [CpuRuntime::TreeWalking, CpuRuntime::BytecodeVm] {
        let label = format!("{runtime:?}");
        let (io, _) = run_program_entry_with_io_and_runtime_from_path(
            program.clone(),
            Some("main"),
            StringIO::new(),
            std::path::Path::new("array-copy.easl"),
            runtime,
        )
        .unwrap();
        assert_eq!(
            io.events,
            expected
                .iter()
                .map(|s| IOEvent::Print((*s).into()))
                .collect::<Vec<_>>(),
            "{label}"
        );
    }
}

#[test]
fn nested_array_projections_return_independent_values_and_preserve_lazy_zeros() {
    replay(
        r#"
        (struct CachedRow lanes: [2: vec4u] id: u32)
        (var rows: [CachedRow])
        @cpu (defn main []
          (= rows (zeroed-array 2u))
          (print (.id (rows 1u)))
          (= (rows 0u) (CachedRow [(vec4u 1u 2u 3u 4u) (vec4u 5u 6u 7u 8u)] 7u))
          (let [slot 0u @var row-copy (rows slot) saved-lanes (.lanes (rows slot))]
            (= (.id row-copy) 99u)
            (= ((.lanes row-copy) slot) (vec4u 9u))
            (print (.id (rows slot)))
            (print (.x ((.lanes (rows slot)) slot)))
            (print (.w ((.lanes row-copy) 1u)))
            (= ((.lanes (rows slot)) slot) (vec4u 4u))
            (print (.x (saved-lanes slot)))
            (print (.x ((.lanes (rows slot)) slot))))
          (print (.id (rows 1u))))
        "#,
        &["0u", "7u", "1u", "8u", "1u", "4u", "0u"],
    );
}

#[test]
fn computed_indices_execute_once_and_vector_swizzles_keep_component_order() {
    replay(
        r#"
        (var access-count: u32)
        (var rows: [vec4u])
        (defn next-slot []: u32 (+= access-count 1u) 1u)
        @cpu (defn main []
          (= rows (zeroed-array 2u))
          (= (rows 0u) (vec4u 1u 2u 3u 4u))
          (= (rows 1u) (vec4u 5u 6u 7u 8u))
          (print (.x (rows (next-slot))))
          (print access-count)
          (let [slot 0u parts (.wzx (rows slot))]
            (print parts.x) (print parts.y) (print parts.z))
          (print (.y (rows (+ 0u 1u)))))
        "#,
        &["5u", "1u", "4u", "3u", "1u", "6u"],
    );
}

#[test]
fn nested_array_mutable_arguments_write_children_back_before_parents() {
    replay(
        r#"
        (struct CachedGrid tiles: [2: [2: vec4u]] untouched: u32)
        (var grids: [CachedGrid])
        (defn update-tiles [@ref @var first: vec4u @ref @var second: vec4u]
          (+= first.x 3u) (= second (vec4u 8u)))
        @cpu (defn main []
          (= grids (zeroed-array 2u))
          (= (.untouched (grids 0u)) 42u)
          (= (((.tiles (grids 0u)) 1u) 0u) (vec4u 4u))
          (+= (.x (((.tiles (grids 0u)) 1u) 0u)) 2u)
          (update-tiles (((.tiles (grids 0u)) 1u) 0u)
                        (((.tiles (grids 1u)) 0u) 1u))
          (print (.x (((.tiles (grids 0u)) 1u) 0u)))
          (print (.w (((.tiles (grids 0u)) 1u) 0u)))
          (print (.y (((.tiles (grids 1u)) 0u) 1u)))
          (print (.untouched (grids 0u)))
          (print (.z (((.tiles (grids 0u)) 0u) 0u))))
        "#,
        &["9u", "4u", "8u", "42u", "0u"],
    );
}
