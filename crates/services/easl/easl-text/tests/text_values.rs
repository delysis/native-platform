use easl::{
    CompilerTarget,
    compiler::{builtins::built_in_macros, program::Program},
    interpreter::{
        CpuRuntime, StringIO, VmCpuRuntime, run_program_entry_with_io_and_runtime_from_path,
    },
    parse::{EaslMultiDocument, parse_easl_without_comments},
    text::{MAX_TEXT_BYTES, TextError, TextOp, TextValues},
};

#[test]
fn immutable_text_edits_retain_source_and_reject_invalid_utf8_ranges() {
    let mut values = TextValues::default();
    let source = "A👩🏽‍🚀e\u{301}\r\n";
    let original = values.insert(source).unwrap();
    let insert = values.insert("café").unwrap();
    let edited = values
        .run(TextOp::Replace, &[original, 1, 16, insert])
        .unwrap();
    assert_eq!(values.get(original).unwrap(), source);
    assert_eq!(values.get(edited).unwrap(), "Acafée\u{301}\r\n");
    let before = (values.live_values(), values.retained_bytes());
    assert_eq!(
        values.run(TextOp::Replace, &[original, 2, 16, insert]),
        Err(TextError::Range)
    );
    assert_eq!(
        values.run(TextOp::Slice, &[original, 16, 1]),
        Err(TextError::Range)
    );
    assert_eq!(values.insert_utf8(&[0xff], 0, 1), Err(TextError::Utf8));
    assert_eq!(values.insert_utf8(&[256], 0, 1), Err(TextError::Utf8));
    assert_eq!(values.run(TextOp::Length, &[]), Err(TextError::Arguments));
    assert_eq!((values.live_values(), values.retained_bytes()), before);
}

#[test]
fn text_lifetimes_do_not_reuse_released_handles_or_keep_disposed_values() {
    let mut values = TextValues::default();
    let a = values.insert("original").unwrap();
    let b = values.run(TextOp::Retain, &[a]).unwrap();
    values.run(TextOp::Release, &[a]).unwrap();
    let c = values.insert("new").unwrap();
    assert_ne!(a, c);
    assert_eq!(values.get(a), Err(TextError::Handle));
    assert_eq!(values.get(b).unwrap(), "original");
    assert_eq!(values.run(TextOp::Release, &[a]), Err(TextError::Handle));
    for key in [b, c, 0] {
        values.run(TextOp::Release, &[key]).unwrap();
    }
    assert_eq!((values.live_values(), values.retained_bytes()), (0, 0));
    assert_eq!(values.get(0).unwrap(), "");
    assert_eq!(
        values.insert(&"x".repeat(MAX_TEXT_BYTES + 1)),
        Err(TextError::Limit)
    );
}

#[test]
fn retained_text_budget_rejects_an_allocation_without_invalidating_existing_values() {
    let mut values = TextValues::default();
    let original = values.insert(&"x".repeat(MAX_TEXT_BYTES)).unwrap();
    let mut handles = vec![original];
    // Retains share physical immutable storage but conservatively reserve bytes
    // per handle. Exercise the documented 128 MiB evaluator budget cheaply.
    for _ in 1..128 {
        handles.push(values.run(TextOp::Retain, &[original]).unwrap());
    }
    assert_eq!(
        values.run(TextOp::Retain, &[original]),
        Err(TextError::Limit)
    );
    assert_eq!(values.get(original).unwrap().len(), MAX_TEXT_BYTES);
    assert_eq!(values.live_values(), 128);
    assert_eq!(values.insert(""), Ok(0));
    for handle in handles {
        values.run(TextOp::Release, &[handle]).unwrap();
    }
    assert_eq!(values.retained_bytes(), 0);
    assert!(values.insert("recovered").is_ok());
}

#[test]
fn unicode_boundary_facts_never_split_emoji_combining_text_or_crlf() {
    let mut values = TextValues::default();
    let key = values.insert("A👩🏽‍🚀e\u{301}\r\nB").unwrap();
    let boundaries = [0, 1, 16, 19, 21, 22];
    for position in 0..=22 {
        for mode in 0..=5 {
            let boundary = values
                .run(TextOp::Boundary, &[key, position, mode])
                .unwrap();
            assert!(
                boundaries.contains(&boundary),
                "{position} {mode} {boundary}"
            );
        }
    }
    assert_eq!(values.run(TextOp::Boundary, &[key, 22, 6]).unwrap(), 21);
    assert_eq!(values.run(TextOp::Boundary, &[key, 16, 7]).unwrap(), 19);
    assert_eq!(
        values.run(TextOp::Boundary, &[key, 23, 0]),
        Err(TextError::Range)
    );
}

#[test]
fn boundary_queries_at_every_byte_match_known_extended_grapheme_clusters() {
    let clusters = [
        "A",
        "👩🏽‍🚀",
        "e\u{301}",
        "\r\n",
        "🇺🇸",
        "🇦",
        "!",
        "각",
        "\u{600}a",
    ];
    let source = clusters.concat();
    let mut offsets = vec![0u32];
    for cluster in clusters {
        offsets.push(offsets.last().unwrap() + cluster.len() as u32);
    }
    let mut values = TextValues::default();
    let key = values.insert(&source).unwrap();
    for position in 0..=source.len() as u32 {
        let expected = [
            offsets
                .iter()
                .copied()
                .rfind(|&i| i < position)
                .unwrap_or(0),
            offsets
                .iter()
                .copied()
                .find(|&i| i > position)
                .unwrap_or(source.len() as u32),
            offsets
                .iter()
                .copied()
                .rfind(|&i| i <= position)
                .unwrap_or(0),
            offsets.iter().copied().find(|&i| i >= position).unwrap(),
        ];
        for (mode, expected) in expected.into_iter().enumerate() {
            assert_eq!(
                values
                    .run(TextOp::Boundary, &[key, position, mode as u32])
                    .unwrap(),
                expected,
                "position={position}, mode={mode}"
            );
        }
    }
}

#[test]
fn text_values_pass_through_easl_functions_and_structs_in_both_runtimes() {
    let source = r#"
      (struct Document text: TextBuffer caret: u32)
      (var bytes: [u32])
      (var output: [u32])
      (defn replace-word [doc: Document insert: TextBuffer]: Document
        (Document (text-replace doc.text 1u 16u insert) 6u))
      @cpu (defn main []
        (= bytes (utf8-bytes "café"))
        (let [original (make-text "A👩🏽‍🚀é\r\n")
              insert (text-from-utf8 bytes 0u (array-length bytes))
              changed (replace-word (Document original 16u) insert)
              retained (text-retain original)]
          (= output (text-utf8 changed.text))
          (print output)
          (print (text-length changed.text))
          (print (text-boundary original 16u 0u))
          (text-release original)
          (print (text-length retained))
          (text-release changed.text)
          (text-release insert)
          (text-release retained)))
    "#;
    let docs = EaslMultiDocument::from_singular_document(
        parse_easl_without_comments(source),
        "text.easl".into(),
        source.into(),
    );
    let (mut program, errors) = Program::from_easl_documents(&docs, built_in_macros());
    assert!(errors.is_empty(), "{errors:?}");
    let errors = program.validate_raw_program(CompilerTarget::WGSL);
    assert!(errors.is_empty(), "{errors:?}");
    let run = |runtime| {
        run_program_entry_with_io_and_runtime_from_path(
            program.clone(),
            Some("main"),
            StringIO::new(),
            std::path::Path::new("text.easl"),
            runtime,
        )
        .unwrap()
        .0
        .events
    };
    assert_eq!(run(CpuRuntime::TreeWalking), run(CpuRuntime::BytecodeVm));
    let mut vm = VmCpuRuntime::new_cpu_with_external(
        program,
        StringIO::new(),
        None::<std::path::PathBuf>,
        None,
    )
    .unwrap();
    vm.run("main").unwrap();
    assert_eq!(
        vm.env.text_values.live_values(),
        0,
        "unused release results must not be optimized away"
    );
}

#[test]
fn bulk_grapheme_facts_preserve_source_and_distinguish_hard_breaks_from_eof() {
    use easl::text::grapheme_words;
    let source = "e\u{301}👩🏽‍🚀\r\nA\u{a0}B\u{b}\u{c}\u{85}\u{2028}\u{2029}中文";
    let words = grapheme_words(source);
    let records: Vec<_> = words.chunks_exact(4).collect();
    assert_eq!(records[0], [0, 3, 'e' as u32, 1]);
    assert_eq!(records[1][..2], [3, 18]);
    assert_eq!(records[2], [18, 20, '\r' as u32, 2]);
    let mut end = 0;
    for record in &records {
        assert_eq!(record[0], end);
        assert!(source.is_char_boundary(record[1] as usize));
        end = record[1];
        let scalar = char::from_u32(record[2]).unwrap();
        if matches!(
            scalar,
            '\r' | '\u{b}' | '\u{c}' | '\u{85}' | '\u{2028}' | '\u{2029}'
        ) {
            assert_eq!(record[3], 2);
        }
    }
    assert_eq!(end as usize, source.len());
    assert_eq!(records[3][3], 0); // Neither side of NBSP is a wrapping point.
    assert_eq!(records[4][3], 0);
    assert_eq!(records.last().unwrap()[3], 1); // EOF does not add a blank line.
    assert!(grapheme_words("").is_empty());
    let mut values = TextValues::default();
    let text = values.insert("a\u{85}b\u{b}c").unwrap();
    assert_eq!(values.run(TextOp::Boundary, &[text, 0, 7]).unwrap(), 1);
    assert_eq!(values.run(TextOp::Boundary, &[text, 4, 6]).unwrap(), 3);
}
