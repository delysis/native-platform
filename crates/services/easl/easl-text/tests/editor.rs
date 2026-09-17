use easl::{
    CompilerTarget,
    compiler::{builtins::built_in_macros, program::Program},
    interpreter::{
        CpuRuntime, IOEvent, StringIO, VmCpuRuntime,
        run_program_entry_with_io_and_runtime_from_path,
    },
    parse::{EaslMultiDocument, parse_easl_without_comments},
};

fn program(body: &str) -> Program {
    // EASL quoted strings are literal; retain real CRLF bytes in this corpus.
    let body = body.replace(r"\r\n", "\r\n");
    let source = format!(
        r#"
      (var editor: TextEditorState)
      (var second: TextEditorState)
      (var assertion-id: u32)
      (defn expect [value: bool] (+= assertion-id 1u) (when (not value) (print assertion-id)))
      (defn expect-text [value: TextBuffer expected: TextBuffer]
        (expect (text-equal value expected)) (text-release expected))
      (defn act [kind: u32 position-byte: u32 extend: u32]: u32
        (text-editor-edit editor (TextEditCommand kind position-byte 0u extend) (TextBuffer 0u)))
      @cpu (defn main [] {body} (text-editor-close editor) (text-editor-close second) (print "done"))
    "#
    );
    let mut docs = EaslMultiDocument::from_singular_document(
        parse_easl_without_comments(&source),
        "editor-test.easl".into(),
        source,
    );
    for (name, source) in [
        ("editor.easl", include_str!("../library/editor.easl")),
        ("input.easl", include_str!("../library/input.easl")),
        ("editing.easl", include_str!("../library/editing.easl")),
    ] {
        let parsed = parse_easl_without_comments(source);
        assert!(
            parsed.parsing_failures.is_empty(),
            "{name}: {:?}",
            parsed.parsing_failures
        );
        docs.add_document(parsed, name.into(), source.into());
    }
    let (mut program, errors) = Program::from_easl_documents(&docs, built_in_macros());
    assert!(errors.is_empty(), "{errors:?}");
    let errors = program.validate_raw_program(CompilerTarget::WGSL);
    assert!(errors.is_empty(), "{errors:?}");
    program
}

fn check(body: &str) {
    let program = program(body);
    let expected = vec![IOEvent::Print("done".into())];
    let io = run_program_entry_with_io_and_runtime_from_path(
        program.clone(),
        Some("main"),
        StringIO::new(),
        std::path::Path::new("editor-test.easl"),
        CpuRuntime::TreeWalking,
    )
    .unwrap()
    .0;
    assert_eq!(io.events, expected);
    let mut vm = VmCpuRuntime::new_cpu_with_external(
        program,
        StringIO::new(),
        None::<std::path::PathBuf>,
        None,
    )
    .unwrap();
    vm.run("main").unwrap();
    assert_eq!(vm.env.io.events, expected);
    assert_eq!(
        vm.env.text_values.live_values(),
        0,
        "editor close leaked text values"
    );
    assert_eq!(vm.env.text_values.retained_bytes(), 0);
}

#[test]
fn easl_editor_owns_unicode_edits_history_and_preedit_without_a_product_host() {
    check(
        r#"
      (let [seed (make-text "A👩🏽‍🚀é\r\n")]
        (text-editor-open editor seed) (text-release seed))
      (expect (== (act 3u 16u 0u) 0u))
      (expect (== (act 4u 0u 0u) 0u))
      (expect-text editor.current.text (make-text "Aé\r\n"))
      (expect (text-editor-history editor false))
      (expect-text editor.current.text (make-text "A👩🏽‍🚀é\r\n"))
      (expect (== editor.current.focus 16u))
      (expect (text-editor-history editor true))
      (expect-text editor.current.text (make-text "Aé\r\n"))
      (expect (== (act 0u 0u 0u) 0u))
      (let [preedit (make-text "仮名")]
        (expect (== (text-editor-preedit editor preedit 1u 3u) 1u))
        (expect (not editor.composing))
        (expect (== (text-editor-preedit editor preedit 3u 6u) 0u))
        (expect-text editor.current.text (make-text "Aé\r\n"))
        (expect-text (text-editor-visible editor) (make-text "仮名"))
        (expect (== (act 4u 0u 0u) 3u))
        (text-editor-cancel editor)
        (expect (== editor.undo-count 1u))
        (expect (== (text-editor-preedit editor preedit 3u 6u) 0u))
        (text-release preedit))
      (let [commit (make-text "café")]
        (expect (== (text-editor-commit editor commit) 0u)) (text-release commit))
      (expect-text editor.current.text (make-text "café"))
      (expect (== editor.undo-count 2u))
      (expect (text-editor-history editor false))
      (expect-text editor.current.text (make-text "Aé\r\n"))
      (expect (== editor.current.anchor 0u))
      (expect (== editor.current.focus 6u))
      (let [replacement (make-text "new")]
        (expect (== (text-editor-commit editor replacement) 0u)) (text-release replacement))
      (expect (not (text-editor-history editor true)))
      (expect-text editor.current.text (make-text "new"))
    "#,
    );
}

#[test]
fn independent_easl_editors_copy_paste_reversed_selections_and_bounded_history() {
    check(
        r#"
      (let [seed (make-text "café")]
        (text-editor-open editor seed) (text-editor-open second seed) (text-release seed))
      (expect (== (act 3u 5u 0u) 0u))
      (expect (== (act 3u 0u 1u) 0u))
      (let [copy (text-editor-copy editor)]
        (expect (== (text-editor-commit second copy) 0u)) (text-release copy))
      (expect-text second.current.text (make-text "cafécafé"))
      (expect-text editor.current.text (make-text "café"))
      (expect (== (act 8u 0u 0u) 0u))
      (let [letter (make-text "x")]
        (for [i 80u] (expect (== (text-editor-commit editor letter) 0u)))
        (text-release letter))
      (expect (== editor.undo-count 64u))
      (for [i 64u] (expect (text-editor-history editor false)))
      (expect (not (text-editor-history editor false)))
      (expect (== (text-length editor.current.text) 16u))
      (expect (== editor.redo-count 64u))
      (for [i 64u] (expect (text-editor-history editor true)))
      (expect (== (text-length editor.current.text) 80u))
    "#,
    );
}

#[test]
fn easl_editor_rejects_mid_grapheme_selection_and_keeps_noop_edits_out_of_history() {
    check(
        r#"
      (let [seed (make-text "é🌍")]
        (text-editor-open editor seed) (text-release seed))
      (expect (== (act 3u 1u 0u) 1u))
      (expect (== editor.current.focus 0u))
      (expect (== (act 0u 0u 0u) 0u))
      (let [same (make-text "é🌍")]
        (expect (== (text-editor-commit editor same) 0u)) (text-release same))
      (expect (== editor.undo-count 0u))
      (expect (== editor.current.focus 7u))
      (expect (== (text-editor-navigate editor 3u false false true) 0u))
      (expect (== editor.current.anchor 7u))
      (expect (== editor.current.focus 3u))
      (expect (== (act 4u 0u 0u) 0u))
      (expect-text editor.current.text (make-text "é"))
      (expect (text-editor-history editor false))
      (expect (== editor.current.anchor 7u))
      (expect (== editor.current.focus 3u))
    "#,
    );
}

#[test]
fn malformed_input_payload_preserves_composition_and_committed_history() {
    check(
        r#"
      (let [seed (make-text "original") preview (make-text "仮") payload (make-text "x")]
        (text-editor-open editor seed)
        (expect (== (text-editor-preedit editor preview 0u 3u) 0u))
        (expect (== (text-editor-input editor
          (TextInputEvent 0u 0u 0u 0u 2u 0u 0u) payload 8u 4u) 1u))
        (expect editor.composing)
        (expect-text (text-editor-visible editor) (make-text "仮original"))
        (expect (== (text-editor-input editor
          (TextInputEvent 3u 0u 0u 2u 1u 0u 1u) payload 8u 4u) 1u))
        (expect-text (text-editor-visible editor) (make-text "仮original"))
        (expect-text editor.current.text (make-text "original"))
        (expect (== editor.undo-count 0u))
        (text-release seed) (text-release preview) (text-release payload))
    "#,
    );
}
