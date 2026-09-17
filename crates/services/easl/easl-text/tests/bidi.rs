use easl::{
    CompilerTarget,
    compiler::{builtins::built_in_macros, program::Program},
    interpreter::{CpuRuntime, IOEvent, StringIO, run_program_entry_with_io_and_runtime_from_path},
    parse::{EaslMultiDocument, parse_easl_without_comments},
};
use std::path::Path;
use unicode_bidi::{BidiInfo, Level};

fn check(body: &str) {
    check_extra(body, "", &["bidi"]);
}

fn check_extra(body: &str, extra: &str, modules: &[&str]) {
    let source = format!(
        r#"
      (var source: TextBuffer)
      (var b-facts: [TextDirectionalFact])
      (var b-slots: [TextBidiSlot])
      (var b-runs: [TextBidiRun])
      (var assertion: u32)
      (defn b-fact-at [i: u32]: TextDirectionalFact (b-facts i))
      (defn b-slot-at [i: u32]: TextBidiSlot (b-slots i))
      (defn b-emit-slot [i: u32 slot: TextBidiSlot] (= (b-slots i) slot))
      (defn b-emit-run [i: u32 run: TextBidiRun] (= (b-runs i) run))
      (defn expect [ok: bool] (+= assertion 1u) (when (not ok) (print assertion)))
      (defn prepare [direction: u32]
        (= b-facts (text-directional-facts source direction))
        (= b-slots (zeroed-array (array-length b-facts)))
        (= b-runs (zeroed-array (array-length b-facts))))
      {extra}
      @cpu (defn main [] {body} (text-release source) (print "done"))
    "#
    );
    let parsed = parse_easl_without_comments(&source);
    assert!(
        parsed.parsing_failures.is_empty(),
        "{:?}",
        parsed.parsing_failures
    );
    let mut documents =
        EaslMultiDocument::from_singular_document(parsed, "bidi-test.easl".into(), source);
    for module in modules {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("library/{module}.easl"));
        let library = std::fs::read_to_string(path).unwrap();
        let parsed = parse_easl_without_comments(&library);
        assert!(
            parsed.parsing_failures.is_empty(),
            "{:?}",
            parsed.parsing_failures
        );
        documents.add_document(parsed, format!("{module}.easl"), library);
    }
    let (mut program, errors) = Program::from_easl_documents(&documents, built_in_macros());
    assert!(errors.is_empty(), "{errors:?}");
    let errors = program.validate_raw_program(CompilerTarget::WGSL);
    assert!(errors.is_empty(), "{errors:?}");
    for runtime in [CpuRuntime::TreeWalking, CpuRuntime::BytecodeVm] {
        let io = run_program_entry_with_io_and_runtime_from_path(
            program.clone(),
            Some("main"),
            StringIO::new(),
            Path::new("bidi-test.easl"),
            runtime,
        )
        .unwrap()
        .0;
        assert_eq!(io.events, [IOEvent::Print("done".into())], "{runtime:?}");
    }
}

#[test]
fn native_directional_facts_preserve_every_scalar_and_utf8_byte() {
    let text = "Office (العربية 123) é 🌍\r\nעברית\u{2066}L\u{2069}";
    for direction in 0..=2 {
        let words = easl::text::directional_words(text, direction).unwrap();
        let mut covered = 0;
        for (fact, (start, ch)) in words.chunks_exact(8).zip(text.char_indices()) {
            assert_eq!(
                &fact[..3],
                &[start as u32, (start + ch.len_utf8()) as u32, ch as u32]
            );
            assert_eq!(fact[0], covered);
            assert!(fact[3] <= 126 && fact[5] <= 1 && fact[6] <= 3);
            covered = fact[1];
        }
        assert_eq!(covered as usize, text.len());
        assert_eq!(words.len() / 8, text.chars().count());
    }
    assert!(easl::text::directional_words("", 0).unwrap().is_empty());
    assert_eq!(
        easl::text::directional_words("text", 3),
        Err(easl::text::TextError::Arguments)
    );
    assert_eq!(
        easl::text::directional_words(&"x".repeat(easl::text::MAX_TEXT_BYTES + 1), 0),
        Err(easl::text::TextError::Limit)
    );
}

#[test]
fn easl_visual_order_and_logical_runs_match_unicode_bidi_per_selected_line() {
    // Native BidiInfo supplies paragraph facts to production. Its independent
    // per-line L1/L2 implementation is used only here as the reference oracle.
    let examples = [
        "plain café é 🌍",
        "العربية 123 English",
        "English (العربية ١٢٣) end",
        "עברית 123 (English)",
        "a\u{2067}ع 123\u{2069} b",
        "ع\u{2066}hello 12\u{2069}م",
        "a\u{202b}ع\u{202c} \t ",
        "ع\u{202a}abc  \u{202c}  ",
        "\u{202e}abc\u{202c} \t",
        "\u{2068}العربية\u{2069} x",
        "אב\t 12\t xy  ",
        "a\nעברית\r\nالعربية\u{2029}z",
        "\u{202b}\u{202b}ع  \u{202c}\u{202c}",
        "ع\u{200d}ر\u{200c}ب\u{200f}  ",
        "",
    ];
    let mut body = String::from("(= source (make-text \"\"))\n");
    let mut lines = 0;
    for text in examples {
        for (direction, base) in [(0, None), (1, Some(Level::ltr())), (2, Some(Level::rtl()))] {
            let bidi = BidiInfo::new(text, base);
            let indices: Vec<_> = text
                .char_indices()
                .map(|(i, _)| i)
                .chain([text.len()])
                .collect();
            body += &format!(
                "(text-release source) (= source (make-text \"{text}\")) (prepare {direction}u)\n"
            );
            for paragraph in &bidi.paragraphs {
                let first = indices.binary_search(&paragraph.range.start).unwrap();
                let last = indices.binary_search(&paragraph.range.end).unwrap();
                // Whole paragraph and each scalar-prefix soft line exercise
                // trailing WS/isolate resets independently of paragraph analysis.
                for end in first + 1..=last {
                    let start = if end == last {
                        first
                    } else {
                        (first + end) / 2
                    };
                    let range = indices[start]..indices[end];
                    let (levels, expected_runs) = bidi.visual_runs(paragraph, range.clone());
                    let scalar_levels: Vec<_> =
                        indices[start..end].iter().map(|&i| levels[i]).collect();
                    let order = BidiInfo::reorder_visual(&scalar_levels);
                    body += &format!(
                        "(let [result (text-bidi-order (array-length b-facts) b-fact-at {start}u {end}u (array-length b-slots) b-slot-at b-emit-slot 1000000u)] (expect (== result.status 0u)) (expect (== result.count {}u)))\n",
                        end - start
                    );
                    for (visual, &logical) in order.iter().enumerate() {
                        body += &format!(
                            "(expect (== (.fact (b-slots {visual}u)) {}u)) (expect (== (.level (b-slots {visual}u)) {}u))\n",
                            start + logical,
                            scalar_levels[logical].number()
                        );
                    }
                    body += &format!(
                        "(let [result (text-bidi-runs {}u b-slot-at b-fact-at (array-length b-runs) b-emit-run)] (expect (== result.status 0u)) (expect (== result.count {}u)))\n",
                        end - start,
                        expected_runs.len()
                    );
                    for (i, run) in expected_runs.iter().enumerate() {
                        body += &format!(
                            "(expect (== (.start (b-runs {i}u)) {}u)) (expect (== (.end (b-runs {i}u)) {}u)) (expect (== (.level (b-runs {i}u)) {}u))\n",
                            run.start,
                            run.end,
                            levels[run.start].number()
                        );
                    }
                    lines += 1;
                }
            }
        }
    }
    eprintln!("{lines} selected lines compared with Unicode bidi L1/L2");
    check(&body);
}

#[test]
fn malformed_or_exhausted_line_order_publishes_no_partial_permutation() {
    check(
        r#"
      (= source (make-text "a ع b")) (prepare 0u)
      (= (b-slots 0u) (TextBidiSlot 99u 99u))
      (expect (== (.status (text-bidi-order 5u b-fact-at 0u 5u 4u b-slot-at b-emit-slot 1000000u)) 2u))
      (expect (== (.fact (b-slots 0u)) 99u))
      (expect (== (.status (text-bidi-order 5u b-fact-at 0u 5u 5u b-slot-at b-emit-slot 0u)) 2u))
      (expect (== (.fact (b-slots 0u)) 99u))
      (= (.level (b-facts 4u)) 127u)
      (expect (== (.status (text-bidi-order 5u b-fact-at 0u 5u 5u b-slot-at b-emit-slot 1000000u)) 1u))
      (expect (== (.fact (b-slots 0u)) 99u))
      (= (.level (b-facts 4u)) 0u) (= (.paragraph (b-facts 4u)) 99u)
      (expect (== (.status (text-bidi-order 5u b-fact-at 0u 5u 5u b-slot-at b-emit-slot 1000000u)) 1u))
      (expect (== (.fact (b-slots 0u)) 99u))
      (= (.paragraph (b-facts 4u)) 0u) (= (.start (b-facts 4u)) 0u)
      (expect (== (.status (text-bidi-order 5u b-fact-at 0u 5u 5u b-slot-at b-emit-slot 1000000u)) 1u))
      (expect (== (.fact (b-slots 0u)) 99u))
      (expect (== (.status (text-bidi-order 5u b-fact-at 4u 3u 5u b-slot-at b-emit-slot 1000000u)) 1u))
      (expect (== (.status (text-bidi-order 5u b-fact-at 0u 6u 5u b-slot-at b-emit-slot 1000000u)) 1u))
      (expect (== (.status (text-bidi-order 0u b-fact-at 0u 0u 0u b-slot-at b-emit-slot 0u)) 0u))
      (let [reader (fn [i] (TextDirectionalFact i (+ i 1u) 97u 126u 0u 0u 0u 1818326126u))]
        (expect (== (.status (text-bidi-order 4096u reader 0u 4096u 4096u b-slot-at b-emit-slot 1000000u)) 2u)))
      (expect (== (.fact (b-slots 0u)) 99u))
      (prepare 0u)
      (expect (== (.status (text-bidi-order 5u b-fact-at 0u 5u 5u b-slot-at b-emit-slot 1000000u)) 0u))
      (= (.start (b-runs 0u)) 99u)
      (expect (== (.status (text-bidi-runs 5u b-slot-at b-fact-at 0u b-emit-run)) 2u))
      (expect (== (.start (b-runs 0u)) 99u))
    "#,
    );
}

#[test]
fn easl_directional_runs_feed_real_shaping_and_preserve_bidi_caret_affinity() {
    // This fixture has one strong script per level run. General script
    // itemization/fallback is deliberately not inferred from this test.
    let font =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../easl-native-text/tests/fonts/amiri.ttf");
    let body = format!(
        r#"
      (= source (make-text "Office العربية 123")) (prepare 0u)
      (= b-font (load-font "{}" 32.))
      (= b-ink (font-atlas-glyphs b-font))
      (= b-quads (zeroed-array 256u)) (= b-clusters (zeroed-array 256u))
      (= b-cells (zeroed-array 256u))
      (let [ordered (text-bidi-order (array-length b-facts) b-fact-at 0u (array-length b-facts)
                      (array-length b-slots) b-slot-at b-emit-slot 1000000u)]
        (expect (== ordered.status 0u))
        (let [grouped (text-bidi-runs ordered.count b-slot-at b-fact-at (array-length b-runs) b-emit-run)
              @var x 40.]
          (expect (== grouped.status 0u)) (expect (== grouped.count 3u))
          (for [i grouped.count]
            (let [run (b-runs i) part (text-slice source run.start run.end) @var script 0u]
              (for [j (array-length b-facts)]
                (let [fact (b-facts j)]
                  (when (and (>= fact.start run.start) (and (< fact.start run.end) (!= fact.script 0u)))
                    (when (!= script 0u) (expect (== script fact.script)))
                    (= script fact.script))))
              (= b-bytes (text-utf8 part)) (text-release part)
              (= b-glyphs (shape-font-run b-font b-bytes
                (if (== script 0u) (text-script-latin) script) (!= (& run.level 1u) 0u)))
              (for [j (array-length b-glyphs)] (expect (!= (.id (b-glyphs j)) 0u)))
              (let [placed (text-place-run-geometry (array-length b-glyphs) b-glyph-at b-ink-at
                              (vec2f (texture-dimensions b-font)) (vec2f x 80.) (!= (& run.level 1u) 0u)
                              b-emit-quad 0u (vec2f 40. 60.) run.start b-emit-cluster)]
                (+= x placed.advance) (+= b-cluster-base placed.clusters)
                (+= b-quad-base (array-length b-glyphs)))))
          (let [cells (text-build-cells source b-cluster-base b-cluster-at 256u b-emit-cell)]
            (expect (== cells.status 0u)) (expect (== cells.count 18u))
            ; Byte 7 is both the English trailing edge and Arabic leading edge.
            ; Their different physical positions must survive real shaping.
            (let [left (text-caret-at cells.count b-cell-at 7u 0u)
                  right (text-caret-at cells.count b-cell-at 7u 1u)]
              (expect (== left.status 0u)) (expect (== right.status 0u))
              (expect (> right.rect.x left.rect.x)))
            (expect (== (.byte (text-hit-test cells.count b-cell-at (vec2f 40. 60.))) 0u)))))
    "#,
        font.display()
    );
    check_extra(
        &body,
        r#"
      @{group 0 binding 0} (var b-font: (Texture2D f32))
      (var b-ink: [FontAtlasGlyph])
      (var b-bytes: [u32])
      (var b-glyphs: [FontShapedGlyph])
      (var b-quads: [TextGlyphQuad])
      (var b-clusters: [TextClusterBox])
      (var b-cells: [TextCell])
      (var b-quad-base: u32) (var b-cluster-base: u32)
      (defn b-glyph-at [i: u32]: FontShapedGlyph (b-glyphs i))
      (defn b-ink-at [i: u32]: FontAtlasGlyph (b-ink i))
      (defn b-cluster-at [i: u32]: TextClusterBox (b-clusters i))
      (defn b-cell-at [i: u32]: TextCell (b-cells i))
      (defn b-emit-quad [i: u32 quad: TextGlyphQuad] (= (b-quads (+ b-quad-base i)) quad))
      (defn b-emit-cluster [i: u32 cluster: TextClusterBox] (= (b-clusters (+ b-cluster-base i)) cluster))
      (defn b-emit-cell [i: u32 cell: TextCell] (= (b-cells i) cell))
    "#,
        &["bidi", "atlas", "geometry"],
    );
}
