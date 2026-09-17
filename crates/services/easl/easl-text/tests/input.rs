use easl::{
    CompilerTarget,
    compiler::{builtins::built_in_macros, program::Program},
    external::ExternalVars,
    input::{InputArea, InputFrame, InputKind},
    interpreter::{
        BufferUpload, CpuRuntime, EvalError, FrameDriver, IOManager, WindowEvent,
        run_program_entry_with_io_runtime_and_external_from_path,
    },
    parse::{EaslMultiDocument, load_and_parse_easl_multidocument, parse_easl_without_comments},
};

struct InputIo {
    frame: InputFrame,
    area: Option<InputArea>,
}
impl IOManager for InputIo {
    fn text_input_words(&self, text: bool) -> Result<Vec<u32>, EvalError> {
        self.frame
            .words(text)
            .map_err(|e| easl::interpreter::UserspaceEvalError::RuntimeError(e.into()).into())
    }
    fn set_text_input_area(&mut self, area: InputArea) -> Result<(), EvalError> {
        self.area = Some(area);
        Ok(())
    }
    fn println(&mut self, _: &str) {
        panic!("Unexpected print");
    }
    fn record_draw(
        &mut self,
        _: u16,
        _: u16,
        _: &str,
        _: &str,
        _: u32,
        _: Vec<((u8, u8), BufferUpload)>,
        _: easl::interpreter::RenderBlend,
        _: Option<(u8, u8)>,
    ) -> Result<(), EvalError> {
        panic!("Unexpected drawing");
    }
    fn record_compute(
        &mut self,
        _: u16,
        _: &str,
        _: (u32, u32, u32),
        _: Vec<((u8, u8), BufferUpload)>,
    ) -> Result<(), EvalError> {
        panic!("Unexpected compute");
    }
    fn take_frame_draw_calls(&mut self) -> Vec<WindowEvent> {
        Vec::new()
    }
    fn record_close_window(&mut self) {
        panic!("Unexpected close");
    }
    fn sync_gpu_to_cpu(&mut self, _: u8, _: u8, _: u64) -> Option<Vec<u8>> {
        None
    }
    fn run_spawn_window_driver<D: FrameDriver<IO = Self>>(_: &mut D) -> Result<bool, EvalError> {
        panic!("Unexpected window");
    }
}

#[test]
fn ordered_unicode_repeat_composition_and_focus_events_reach_easl_library() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/input_policy.easl");
    let docs = load_and_parse_easl_multidocument(&path)
        .unwrap()
        .unwrap()
        .unwrap();
    let (mut program, errors) = Program::from_easl_documents(&docs, built_in_macros());
    assert!(errors.is_empty(), "{errors:?}");
    let errors = program.validate_raw_program(CompilerTarget::WGSL);
    assert!(errors.is_empty(), "{errors:?}");
    let mut frame = InputFrame::default();
    frame.push(InputKind::KeyDown, 1, 4, "", None);
    frame.push(InputKind::KeyDown, 1, 20, "", None);
    frame.push(InputKind::Preedit, 0, 0, "仮名", Some((3, 6)));
    frame.push(InputKind::Preedit, 0, 0, "", None);
    frame.push(InputKind::Text, 0, 0, "é👩🏽‍🚀", None);
    frame.push(InputKind::KeyDown, 14, 8, "", None);
    frame.push(InputKind::KeyUp, 14, 8, "", None);
    frame.push(InputKind::Blur, 0, 0, "", None);
    for runtime in [CpuRuntime::TreeWalking, CpuRuntime::BytecodeVm] {
        let external = ExternalVars::new(&program);
        let (io, _) = run_program_entry_with_io_runtime_and_external_from_path(
            program.clone(),
            Some("main"),
            InputIo {
                frame: frame.clone(),
                area: None,
            },
            &path,
            runtime,
            Some(external.clone()),
        )
        .unwrap();
        let bytes = external.read_external_var_raw("input-bytes").unwrap();
        assert_eq!(bytes, "仮名é👩🏽‍🚀".bytes().map(u32::from).collect::<Vec<_>>());
        let decisions = external.read_external_var_raw("input-decisions").unwrap();
        assert_eq!(
            decisions.chunks_exact(5).map(|c| c[0]).collect::<Vec<_>>(),
            vec![1, 1, 2, 2, 1, 1, 0, 3]
        );
        assert_eq!(decisions[1], 6); // word backspace, including repeat
        assert_eq!(decisions[6], 6);
        assert_eq!(decisions[23], "é👩🏽‍🚀".len() as u32);
        assert_eq!(
            io.area,
            Some(InputArea::new(true, [32., 64., 2., 32.]).unwrap())
        );
        assert_eq!(io.frame.words(false).unwrap(), frame.words(false).unwrap()); // reads do not consume
    }
}

#[test]
fn control_word_commands_preserve_altgr_without_turning_shortcuts_into_text() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/input_policy.easl");
    let source = include_str!("../examples/input_policy.easl")
        .replace("(input-events i) 8u 4u", "(input-events i) 2u 2u");
    let mut docs = EaslMultiDocument::from_singular_document(
        parse_easl_without_comments(&source),
        "input_policy.easl".into(),
        source,
    );
    for (name, source) in [
        ("input.easl", include_str!("../library/input.easl")),
        ("editing.easl", include_str!("../library/editing.easl")),
    ] {
        docs.add_document(
            parse_easl_without_comments(source),
            name.into(),
            source.into(),
        );
    }
    let (mut program, errors) = Program::from_easl_documents(&docs, built_in_macros());
    assert!(errors.is_empty(), "{errors:?}");
    let errors = program.validate_raw_program(CompilerTarget::WGSL);
    assert!(errors.is_empty(), "{errors:?}");
    let mut frame = InputFrame::default();
    frame.push(InputKind::KeyDown, 1, 2, "", None); // Control+Backspace
    frame.push(InputKind::KeyDown, 2, 18, "", None); // repeated Control+Delete
    frame.push(InputKind::Text, 0, 2, "c", None); // shortcut text must stay unhandled
    frame.push(InputKind::KeyDown, 14, 6, "", None); // AltGr+A must not select all
    frame.push(InputKind::Text, 0, 6, "€", None); // Control+Alt from AltGr
    frame.push(InputKind::KeyDown, 14, 2, "", None); // Control+A
    frame.push(InputKind::KeyDown, 15, 2, "", None); // Control+C belongs to host
    for runtime in [CpuRuntime::TreeWalking, CpuRuntime::BytecodeVm] {
        let external = ExternalVars::new(&program);
        run_program_entry_with_io_runtime_and_external_from_path(
            program.clone(),
            Some("main"),
            InputIo {
                frame: frame.clone(),
                area: None,
            },
            &path,
            runtime,
            Some(external.clone()),
        )
        .unwrap();
        let decisions = external.read_external_var_raw("input-decisions").unwrap();
        let commands = decisions
            .chunks_exact(5)
            .map(|c| (c[0], c[1], c[3]))
            .collect::<Vec<_>>();
        assert_eq!(
            commands,
            vec![
                (1, 6, 0),
                (1, 7, 0),
                (0, 0, 0),
                (0, 0, 0),
                (1, 9, 3),
                (1, 0, 0),
                (0, 0, 0)
            ]
        );
    }
}

#[test]
fn os_input_facts_drive_the_easl_editor_and_preserve_exact_undo_and_commit_bytes() {
    let source = r#"
      (var input-editor: TextEditorState)
      (var events: [TextInputEvent])
      (var input-event-bytes: [u32])
      @external (var current-bytes: [u32])
      @external (var visible-bytes: [u32])
      @external (var undo-count: u32)
      @cpu (defn main []
        (let [seed (make-text "alpha beta")]
          (text-editor-open input-editor seed) (text-release seed))
        (text-editor-edit input-editor (TextEditCommand 3u 10u 0u 0u) (TextBuffer 0u))
        (= events (text-input-events)) (= input-event-bytes (text-input-bytes))
        (for [i (array-length events)]
          (let [event (events i) payload (text-from-utf8 input-event-bytes event.text-start event.text-end)]
            (text-editor-input input-editor event payload 2u 2u)
            (text-release payload)))
        (= current-bytes (text-utf8 input-editor.current.text))
        (= visible-bytes (text-utf8 (text-editor-visible input-editor)))
        (= undo-count input-editor.undo-count)
        (text-editor-close input-editor))
    "#;
    let mut docs = EaslMultiDocument::from_singular_document(
        parse_easl_without_comments(source),
        "input-editor.easl".into(),
        source.into(),
    );
    for (name, source) in [
        ("editor.easl", include_str!("../library/editor.easl")),
        ("input.easl", include_str!("../library/input.easl")),
        ("editing.easl", include_str!("../library/editing.easl")),
    ] {
        docs.add_document(
            parse_easl_without_comments(source),
            name.into(),
            source.into(),
        );
    }
    let (mut program, errors) = Program::from_easl_documents(&docs, built_in_macros());
    assert!(errors.is_empty(), "{errors:?}");
    let errors = program.validate_raw_program(CompilerTarget::WGSL);
    assert!(errors.is_empty(), "{errors:?}");
    let mut frame = InputFrame::default();
    frame.push(InputKind::KeyDown, 1, 2, "", None);
    frame.push(InputKind::Text, 0, 0, "café", None);
    frame.push(InputKind::Preedit, 0, 0, "仮名", Some((3, 6)));
    frame.push(InputKind::Text, 0, 0, "日本", None);
    frame.push(InputKind::KeyDown, 18, 2, "", None);
    frame.push(InputKind::KeyDown, 18, 3, "", None);
    frame.push(InputKind::KeyDown, 14, 2, "", None);
    frame.push(InputKind::Text, 0, 0, "exact\r\n", None);
    frame.push(InputKind::Preedit, 0, 0, "te\u{301}", Some((1, 4)));
    frame.push(InputKind::Blur, 0, 0, "", None);
    for runtime in [CpuRuntime::TreeWalking, CpuRuntime::BytecodeVm] {
        let external = ExternalVars::new(&program);
        run_program_entry_with_io_runtime_and_external_from_path(
            program.clone(),
            Some("main"),
            InputIo {
                frame: frame.clone(),
                area: None,
            },
            std::path::Path::new("input-editor.easl"),
            runtime,
            Some(external.clone()),
        )
        .unwrap();
        let expected = "exact\r\n".bytes().map(u32::from).collect::<Vec<_>>();
        assert_eq!(
            external.read_external_var_raw("current-bytes").unwrap(),
            expected
        );
        assert_eq!(
            external.read_external_var_raw("visible-bytes").unwrap(),
            expected
        );
        assert_eq!(
            external.read_external_var_raw("undo-count").unwrap(),
            vec![4]
        );
    }
}

#[test]
fn input_overflow_and_invalid_composition_are_explicit_and_recover_next_frame() {
    let mut frame = InputFrame::default();
    frame.push(InputKind::Preedit, 0, 0, "é", Some((1, 1)));
    assert!(frame.words(false).is_err());
    assert!(frame.words(true).is_err());
    frame.clear();
    for _ in 0..1025 {
        frame.push(InputKind::KeyDown, 1, 16, "", None);
    }
    assert!(frame.words(false).is_err());
    frame.clear();
    frame.push(InputKind::Text, 0, 0, "ok", None);
    assert_eq!(frame.words(true).unwrap(), vec![111, 107]);
    assert!(InputArea::new(true, [0., f32::NAN, 1., 1.]).is_err());
    assert!(InputArea::new(true, [0., 0., -1., 1.]).is_err());
}
