use easl::{
    CompilerTarget,
    compiler::{builtins::built_in_macros, program::Program},
    interpreter::{CpuRuntime, IOEvent, StringIO, run_program_entry_with_io_and_runtime_from_path},
    parse::{EaslMultiDocument, parse_easl_without_comments},
};
use std::path::Path;

fn check(body: &str, extra: &str) {
    let source = format!(
        r#"
      (var c-source: TextBuffer) (var c-assertion: u32)
      (defn expect [ok: bool] (+= c-assertion 1u) (when (not ok) (print c-assertion)))
      {extra}
      @cpu (defn main [] (= c-source (make-text "")) {body} (text-release c-source) (print "done"))
    "#
    );
    let parsed = parse_easl_without_comments(&source);
    assert!(
        parsed.parsing_failures.is_empty(),
        "{:?}",
        parsed.parsing_failures
    );
    let mut docs =
        EaslMultiDocument::from_singular_document(parsed, "fallback-test.easl".into(), source);
    for module in [
        "atlas",
        "paragraph",
        "flow",
        "script",
        "bidi",
        "runs",
        "measure",
        "fallback",
    ] {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("library/{module}.easl"));
        let source = std::fs::read_to_string(path).unwrap();
        let parsed = parse_easl_without_comments(&source);
        assert!(
            parsed.parsing_failures.is_empty(),
            "{module}: {:?}",
            parsed.parsing_failures
        );
        docs.add_document(parsed, format!("{module}.easl"), source);
    }
    let (mut program, errors) = Program::from_easl_documents(&docs, built_in_macros());
    assert!(errors.is_empty(), "{errors:?}");
    let errors = program.validate_raw_program(CompilerTarget::WGSL);
    assert!(errors.is_empty(), "{errors:?}");
    for runtime in [CpuRuntime::TreeWalking, CpuRuntime::BytecodeVm] {
        let io = run_program_entry_with_io_and_runtime_from_path(
            program.clone(),
            Some("main"),
            StringIO::new(),
            Path::new("fallback-test.easl"),
            runtime,
        )
        .unwrap()
        .0;
        assert_eq!(io.events, [IOEvent::Print("done".into())], "{runtime:?}");
    }
}

const STORAGE: &str = r#"
  (var f-facts: [TextGrapheme]) (var f-glyphs: [FontShapedGlyph])
  (var f-choices: [TextFontChoice]) (var f-runs: [TextFontRun]) (var f-writes: u32)
  (defn f-fact [i: u32]: TextGrapheme (f-facts i))
  (defn f-glyph [i: u32]: FontShapedGlyph (f-glyphs i))
  (defn f-choice [i: u32]: TextFontChoice (f-choices i))
  (defn f-emit-choice [i: u32 item: TextFontChoice] (+= f-writes 1u) (= (f-choices i) item))
  (defn f-emit-run [i: u32 item: TextFontRun] (+= f-writes 1u) (= (f-runs i) item))
  (defn f-prepare []
    (= f-facts (text-graphemes c-source))
    (= f-choices (zeroed-array (array-length f-facts)))
    (= f-runs (zeroed-array (array-length f-facts)))
    (for [i (array-length f-facts)] (= (f-choices i) (text-font-unset)))
    (= f-writes 0u))
  (defn f-consider [font: u32 budget: u32]: TextRunResult
    (text-font-consider (array-length f-facts) f-fact (array-length f-glyphs) f-glyph
      0u (text-length c-source) font (array-length f-choices) f-choice f-emit-choice budget))
  (defn f-publish [capacity: u32 budget: u32]: TextFontResult
    (text-font-runs (array-length f-facts) f-fact f-choice (fn [i] 19u) capacity f-emit-run budget))
"#;

#[test]
fn fallback_coalescing_preserves_style_edges_and_checks_capacity_before_writing() {
    check(
        r#"
      (text-release c-source) (= c-source (make-text "ábcd")) (f-prepare)
      (for [i 4u] (= (f-choices i) (TextFontChoice 0u 0u)))
      (let [small (text-font-runs 4u f-fact f-choice styled 1u f-emit-run 100u)]
        (expect (== small.status 2u)) (expect (== f-writes 0u)))
      (let [out (text-font-runs 4u f-fact f-choice styled 4u f-emit-run 100u)]
        (expect (== out.status 0u)) (expect (== out.count 3u)) (expect (== out.missing 0u)))
      (expect (== (.end (f-runs 0u)) 3u)) (expect (== (.style (f-runs 0u)) 8u))
      (expect (== (.start (f-runs 1u)) 3u)) (expect (== (.end (f-runs 1u)) 5u))
      (expect (== (.style (f-runs 1u)) 9u)) (expect (== (.style (f-runs 2u)) 8u))
      (= (f-choices 2u) (TextFontChoice 1u 0u)) (= f-writes 0u)
      (let [out (text-font-runs 4u f-fact f-choice styled 4u f-emit-run 100u)]
        (expect (== out.status 0u)) (expect (== out.count 4u)))
      (expect (== (.font (f-runs 2u)) 1u)) (expect (== (.style (f-runs 2u)) 9u))
    "#,
        &format!("{STORAGE}\n(defn styled [i: u32]: u32 (if (or (== i 0u) (== i 3u)) 8u 9u))"),
    );
}

#[test]
fn whole_graphemes_choose_the_first_best_font_and_report_unresolved_text() {
    check(
        r#"
      (text-release c-source) (= c-source (make-text "ábcd")) (f-prepare)
      (= f-glyphs (zeroed-array 5u))
      (= (f-glyphs 0u) (FontShapedGlyph 7u 0u 3u 0. 0. 10. 0u))
      (= (f-glyphs 1u) (FontShapedGlyph 0u 0u 3u 0. 0. 0. 0u))
      (= (f-glyphs 2u) (FontShapedGlyph 8u 3u 4u 0. 0. 10. 0u))
      (= (f-glyphs 3u) (FontShapedGlyph 0u 4u 5u 0. 0. 10. 0u))
      (= (f-glyphs 4u) (FontShapedGlyph 0u 5u 6u 0. 0. 10. 0u))
      (expect (== (.status (f-consider 0u 1000000u)) 0u))
      (= (f-glyphs 2u) (FontShapedGlyph 0u 3u 4u 0. 0. 10. 0u))
      (= (f-glyphs 3u) (FontShapedGlyph 40u 4u 5u 0. 0. 10. 0u))
      (expect (== (.status (f-consider 1u 1000000u)) 0u))
      (= (f-glyphs 1u) (FontShapedGlyph 50u 0u 3u 0. 0. 0. 0u))
      (= (f-glyphs 2u) (FontShapedGlyph 51u 3u 4u 0. 0. 10. 0u))
      (expect (== (.status (f-consider 2u 1000000u)) 0u))
      (expect (== (.font (f-choices 0u)) 2u)) (expect (== (.font (f-choices 1u)) 0u))
      (expect (== (.font (f-choices 2u)) 1u)) (expect (== (.font (f-choices 3u)) 0u))
      (= f-writes 0u)
      (expect (== (.status (f-publish 3u 1000000u)) 2u)) (expect (== f-writes 0u))
      (let [result (f-publish 4u 1000000u)]
        (expect (== result.status 0u)) (expect (== result.count 4u))
        (expect (== result.missing 1u)) (expect (== f-writes 4u)))
      (expect (== (.start (f-runs 0u)) 0u)) (expect (== (.end (f-runs 0u)) 3u))
      (expect (== (.style (f-runs 3u)) 19u)) (expect (== (.end (f-runs 3u)) 6u))
      (= f-writes 0u)
      (expect (== (.status (f-publish 4u 1u)) 2u)) (expect (== f-writes 0u))
    "#,
        STORAGE,
    );
}

#[test]
fn malformed_partial_and_exhausted_probes_leave_prior_choices_untouched() {
    check(
        r#"
      (text-release c-source) (= c-source (make-text "éx")) (f-prepare)
      (= f-glyphs (zeroed-array 1u))
      (= (f-glyphs 0u) (FontShapedGlyph 1u 0u 3u 0. 0. 10. 0u))
      (expect (== (.status (f-consider 0u 1000000u)) 1u))
      (= (f-glyphs 0u) (FontShapedGlyph 1u 0u 1u 0. 0. 10. 0u))
      (expect (== (.status (f-consider 0u 1000000u)) 1u))
      (= (f-glyphs 0u) (FontShapedGlyph 1u 1u 4u 0. 0. 10. 0u))
      (expect (== (.status (f-consider 0u 1000000u)) 1u))
      (= (f-glyphs 0u) (FontShapedGlyph 1u 0u 4u 0. 0. 10. 0u))
      (expect (== (.status (f-consider 64u 1000000u)) 1u))
      (expect (== (.status (f-consider 0u 1u)) 2u))
      (expect (== (.status (f-publish 2u 1000000u)) 1u))
      (expect (== f-writes 0u))
      (expect (== (.font (f-choices 0u)) 4294967295u))
      (expect (== (.status (f-consider 0u 1000000u)) 0u))
      (= f-writes 0u)
      (= (f-glyphs 0u) (FontShapedGlyph 0u 0u 4u 0. 0. 10. 0u))
      (expect (== (.status (f-consider 1u 1000000u)) 0u))
      (expect (== f-writes 0u)) (expect (== (.font (f-choices 0u)) 0u))
      (let [result (f-publish 1u 1000000u)]
        (expect (== result.status 0u)) (expect (== result.count 1u)) (expect (== result.missing 0u)))
      (expect (== (.end (f-runs 0u)) 4u))
    "#,
        STORAGE,
    );
}

#[test]
fn real_cyrillic_fallback_retains_primary_latin_and_combining_graphemes() {
    let fonts = Path::new(env!("CARGO_MANIFEST_DIR")).join("../easl-native-text/tests/fonts");
    let extra = format!(
        r#"{STORAGE}
      @{{group 0 binding 0}} (var f-primary: (Texture2D f32))
      @{{group 0 binding 1}} (var f-secondary: (Texture2D f32))
    "#
    );
    check(
        &format!(
            r#"
      (text-release c-source) (= c-source (make-text "Привет é")) (f-prepare)
      (= f-primary (load-font "{}" 32.)) (= f-secondary (load-font "{}" 32.))
      (= f-glyphs (shape-font-span f-primary c-source 0u (text-length c-source) 1668903532u false))
      (expect (== (.status (f-consider 0u 1000000u)) 0u))
      (expect (> (.missing (f-choices 0u)) 0u))
      (= f-glyphs (shape-font-span f-secondary c-source 0u (text-length c-source) 1668903532u false))
      (expect (== (.status (f-consider 1u 1000000u)) 0u))
      (for [i 6u] (expect (== (.font (f-choices i)) 1u)))
      (for [i 6u (< i 8u) (+= i 1u)] (expect (== (.font (f-choices i)) 0u)))
      (let [result (f-publish 8u 1000000u)]
        (expect (== result.status 0u)) (expect (== result.count 2u)) (expect (== result.missing 0u)))
      (expect (== (.end (f-runs 0u)) 12u)) (expect (== (.start (f-runs 1u)) 12u))
      (expect (== (.end (f-runs 1u)) (text-length c-source)))
    "#,
            fonts.join("amiri.ttf").display(),
            fonts.join("shantell-sans-regular.ttf").display()
        ),
        &extra,
    );
}
