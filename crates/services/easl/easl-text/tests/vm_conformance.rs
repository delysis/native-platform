use easl::{
    CompilerTarget,
    compiler::{builtins::built_in_macros, program::Program},
    interpreter::{CpuRuntime, run_program_test_io_with_runtime},
    parse::{EaslMultiDocument, parse_easl_without_comments},
};

#[test]
fn owned_local_copies_preserve_reference_and_closure_conformance() {
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../vendor/easl/data/conformance");
    for name in [
        "mut_ref_scalar_multiple_calls",
        "mut_ref_struct",
        "mut_ref_struct_field",
        "mut_ref_struct_multiple_calls",
        "mut_ref_indirect_struct_field",
        "closure_scope_multi_capture",
        "closure_capturing_closure_mutation",
        "for_loop",
        "for_loop_init",
        "match_enum_struct_data",
        "struct_constructor_evaluates_once",
        "repeated_higher_order_caller",
        "array_struct_field_writeback",
        "nested_shared_specialization",
        "vector_ordering",
        "lexical_unwind",
    ] {
        let source = format!(
            "{}\n@cpu (defn main [] (print (f)))",
            std::fs::read_to_string(root.join(format!("{name}.easl"))).unwrap()
        );
        let parsed = parse_easl_without_comments(&source);
        assert!(parsed.parsing_failures.is_empty(), "{name}");
        let documents = EaslMultiDocument::from_singular_document(parsed, name.into(), source);
        let (mut program, errors) = Program::from_easl_documents(&documents, built_in_macros());
        assert!(errors.is_empty(), "{name}: {errors:?}");
        let errors = program.validate_raw_program(CompilerTarget::WGSL);
        assert!(errors.is_empty(), "{name}: {errors:?}");
        let tree =
            run_program_test_io_with_runtime(program.clone(), CpuRuntime::TreeWalking).unwrap();
        let vm = run_program_test_io_with_runtime(program, CpuRuntime::BytecodeVm).unwrap();
        assert!(!tree.events.is_empty(), "{name}");
        assert_eq!(tree.events, vm.events, "{name}");
    }
}

#[test]
fn lifted_argument_blocks_preserve_earlier_reads_and_mutation_order() {
    let source = r#"
      @cpu (defn main []
        (let [@var value 10u]
          (print (+ value (let [] (= value 20u) value)))
          (print (+ (let [] (= value 30u) value) value))
          (print value)))
    "#;
    let docs =
        EaslMultiDocument::from_singular_document_sourceless(parse_easl_without_comments(source));
    let (mut program, errors) = Program::from_easl_documents(&docs, built_in_macros());
    assert!(errors.is_empty(), "{errors:?}");
    let errors = program.validate_raw_program(CompilerTarget::WGSL);
    assert!(errors.is_empty(), "{errors:?}");
    for runtime in [CpuRuntime::TreeWalking, CpuRuntime::BytecodeVm] {
        let result = run_program_test_io_with_runtime(program.clone(), runtime).unwrap();
        assert_eq!(
            result.events,
            ["30u", "60u", "30u"].map(|s| easl::interpreter::IOEvent::Print(s.into())),
            "{runtime:?}",
        );
    }
}
