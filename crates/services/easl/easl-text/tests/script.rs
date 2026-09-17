use easl::{
    CompilerTarget,
    compiler::{builtins::built_in_macros, program::Program},
    interpreter::{CpuRuntime, IOEvent, StringIO, run_program_entry_with_io_and_runtime_from_path},
    parse::{EaslMultiDocument, parse_easl_without_comments},
};
use std::path::Path;
fn check_extra(body: &str, extra: &str, modules: &[&str]) {
    let source = format!(
        r#"
      (var s-source: TextBuffer)
      (var s-facts: [TextScriptFact])
      (var s-runs: [TextScriptRun])
      (var s-assertion: u32) (var s-writes: u32)
      (defn s-fact-at [i: u32]: TextScriptFact (s-facts i))
      (defn s-emit-run [i: u32 item: TextScriptRun] (+= s-writes 1u) (= (s-runs i) item))
      (defn expect [ok: bool] (+= s-assertion 1u) (when (not ok) (print s-assertion)))
      (defn s-prepare []
        (= s-facts (text-script-facts s-source))
        (= s-runs (zeroed-array (+ 1u (array-length s-facts)))))
      (defn s-itemize [fallback: u32]: TextScriptResult
        (text-itemize-scripts (array-length s-facts) s-fact-at 0u (array-length s-facts)
          fallback (array-length s-runs) s-emit-run 1000000u))
      {extra}
      @cpu (defn main [] {body} (text-release s-source) (print "done"))
    "#
    );
    let parsed = parse_easl_without_comments(&source);
    assert!(
        parsed.parsing_failures.is_empty(),
        "{:?}\n{source}",
        parsed.parsing_failures
    );
    let mut documents =
        EaslMultiDocument::from_singular_document(parsed, "script-test.easl".into(), source);
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
            Path::new("script-test.easl"),
            runtime,
        )
        .unwrap()
        .0;
        assert_eq!(io.events, [IOEvent::Print("done".into())], "{runtime:?}");
    }
}

fn check(body: &str) {
    check_extra(body, "", &["script"]);
}

#[test]
fn native_script_properties_preserve_scalars_clusters_and_extension_sets() {
    let text = "aि ◌ְ é 👩‍👩‍👧‍👦 ーاـ𑼂\r\n";
    let words = easl::text::script_words(text).unwrap();
    let graphemes = easl::text::grapheme_words(text);
    let mut covered = 0;
    for (fact, (start, scalar)) in words.chunks_exact(16).zip(text.char_indices()) {
        assert_eq!(
            &fact[..2],
            &[start as u32, (start + scalar.len_utf8()) as u32]
        );
        assert_eq!(fact[0], covered);
        assert_eq!(fact[4], scalar as u32);
        let cluster = graphemes
            .chunks_exact(4)
            .find(|g| g[0] <= fact[0] && fact[0] < g[1])
            .unwrap();
        assert_eq!(&fact[2..4], &cluster[..2]);
        if ![0, 1, 103].contains(&fact[5]) {
            assert_ne!(fact[8 + (fact[5] / 32) as usize] & (1 << (fact[5] % 32)), 0);
        }
        covered = fact[1];
    }
    assert_eq!(covered as usize, text.len());
    assert_eq!(words.len() / 16, text.chars().count());
    // Hiragana/Katakana prolonged sound mark retains both extensions.
    let prolonged = words.chunks_exact(16).find(|f| f[4] == 0x30fc).unwrap();
    assert_eq!(prolonged[5], 0);
    assert_eq!(prolonged[8], (1 << 20) | (1 << 22));
    assert!(prolonged[9..].iter().all(|&word| word == 0));
    // Kawi was introduced after the old Swash Unicode 13 script table.
    let kawi = words.chunks_exact(16).find(|f| f[4] == 0x11f02).unwrap();
    assert_eq!(
        easl::text::script_tag(kawi[5]).unwrap(),
        u32::from_be_bytes(*b"kawi")
    );
    for (code, tag) in [
        (25, *b"latn"),
        (2, *b"arab"),
        (20, *b"kana"),
        (22, *b"kana"),
        (24, *b"lao "),
        (41, *b"yi  "),
    ] {
        assert_eq!(
            easl::text::script_tag(code).unwrap(),
            u32::from_be_bytes(tag)
        );
    }
    for code in [0, 1, 103, 255, 256, u32::MAX] {
        assert_eq!(
            easl::text::script_tag(code),
            Err(easl::text::TextError::ScriptCode)
        );
    }
    let brackets = easl::text::script_words("(〈〉)").unwrap();
    assert_eq!(&brackets[6..8], &[1, 0x28]);
    assert_eq!(&brackets[22..24], &[1, 0x3008]);
    assert_eq!(&brackets[38..40], &[2, 0x3008]);
    assert_eq!(&brackets[54..56], &[2, 0x28]);
    assert!(easl::text::script_words("").unwrap().is_empty());
    assert_eq!(
        easl::text::script_words(&"x".repeat(easl::text::MAX_TEXT_BYTES + 1)),
        Err(easl::text::TextError::Limit)
    );
}

#[test]
fn easl_resolves_extensions_brackets_and_whole_combining_sequences() {
    // UAX24 sections 5.1-5.3 specify the examples/policy, not ICU script-run output.
    type ScriptCase<'a> = (&'a str, u32, &'a [(&'a str, u32)]);
    let cases: &[ScriptCase<'_>] = &[
        ("", 25, &[]),
        (
            "gamma (γ) is",
            25,
            &[("gamma (", 25), ("γ", 14), (") is", 25)],
        ),
        (" (γ) ", 25, &[(" (γ) ", 14)]),
        (
            "a[(β)б]z",
            25,
            &[("a[(", 25), ("β", 14), (")", 25), ("б", 8), ("]z", 25)],
        ),
        ("a(β]γ)z", 25, &[("a(", 25), ("β]γ", 14), (")z", 25)]),
        ("a)β", 25, &[("a)", 25), ("β", 14)]),
        ("aिβ", 25, &[("aि", 25), ("β", 14)]),
        ("◌ְ a", 25, &[("◌ְ ", 19), ("a", 25)]),
        ("é👩‍👩‍👧‍👦x", 25, &[("é👩‍👩‍👧‍👦x", 25)]),
        (
            "Latin ーかな カナ",
            25,
            &[("Latin ", 25), ("ーかな ", 20), ("カナ", 22)],
        ),
        ("カナー", 25, &[("カナー", 22)]),
        ("ー", 22, &[("ー", 22)]),
        ("aـاب", 25, &[("a", 25), ("ـاب", 2)]),
        ("ܐـܒ", 25, &[("ܐـܒ", 34)]),
        ("हिन्दी Latin", 25, &[("हिन्दी ", 10), ("Latin", 25)]),
        ("123 😀\t ", 14, &[("123 😀\t ", 14)]),
        ("\u{e000} x", 14, &[("\u{e000} x", 25)]),
        ("abc\r\n", 25, &[("abc\r\n", 25)]),
        ("\u{11f02}", 25, &[("\u{11f02}", 198)]),
    ];
    let mut body = String::new();
    for &(text, fallback, runs) in cases {
        body +=
            &format!("(text-release s-source) (= s-source (make-text \"{text}\")) (s-prepare)\n");
        body += &format!(
            "(let [result (s-itemize {fallback}u)] (expect (== result.status 0u)) (expect (== result.count {}u)) (expect (<= result.work 1000000u)))\n",
            runs.len()
        );
        let mut offset = 0;
        for (i, &(part, script)) in runs.iter().enumerate() {
            assert_eq!(&text[offset..offset + part.len()], part);
            body += &format!(
                "(expect (== (.start (s-runs {i}u)) {offset}u)) (expect (== (.end (s-runs {i}u)) {}u)) (expect (== (.script-code (s-runs {i}u)) {script}u))\n",
                offset + part.len()
            );
            offset += part.len();
        }
        assert_eq!(offset, text.len());
    }
    body += "(expect (== (text-script-tag 25u) 1818326126u)) (expect (== (text-script-tag 20u) 1801547361u))";
    check(&body);
}

#[test]
fn invalid_or_exhausted_itemization_never_publishes_partial_runs() {
    check_extra(
        r#"
      (= s-source (make-text "á(β)z")) (s-prepare)
      (s-rejected 1u 6u 25u 100u 1000000u 1u)
      (s-rejected 0u 1u 25u 100u 1000000u 1u)
      (s-rejected 0u 99u 25u 100u 1000000u 1u)
      (s-rejected 3u 2u 25u 100u 1000000u 1u)
      (s-rejected 0u 6u 103u 100u 1000000u 1u)
      (s-rejected 0u 6u 25u 1u 1000000u 2u)
      (s-rejected 0u 6u 25u 100u 0u 2u)
      (s-rejected 0u 6u 25u 100u 120u 2u)
      (s-rejected 0u 6u 25u 100u 1000001u 2u)
      (let [good (s-itemize 25u) before s-writes]
        (expect (== good.status 0u))
        (s-rejected 0u 6u 25u 100u (- good.work 1u) 2u)
        (expect (== before s-writes))
        (expect (== (.status (text-itemize-scripts 6u s-fact-at 0u 6u 25u
          100u s-emit-run good.work)) 0u)))
      (= (.primary (s-facts 3u)) 256u)
      (s-rejected 0u 6u 25u 100u 1000000u 1u) (s-prepare)
      (= (.extensions-low (s-facts 0u)) (vec4u 0u))
      (s-rejected 0u 6u 25u 100u 1000000u 1u) (s-prepare)
      (= (.start (s-facts 3u)) 999u)
      (s-rejected 0u 6u 25u 100u 1000000u 1u) (s-prepare)
      (= (.grapheme-end (s-facts 0u)) 1u)
      (s-rejected 0u 6u 25u 100u 1000000u 1u)
      (text-release s-source) (= s-source (make-text "a
b")) (s-prepare)
      (s-rejected 0u 3u 25u 100u 1000000u 1u)
      (expect (== (.status (text-itemize-scripts 3u s-fact-at 0u 2u 25u
        100u s-emit-run 1000000u)) 0u))
      (expect (== (.status (text-itemize-scripts 3u s-fact-at 2u 3u 25u
        100u s-emit-run 1000000u)) 0u))
      (expect (== (.start (s-runs 0u)) 2u))
      (expect (== (.end (s-runs 0u)) 3u))
    "#,
        r#"
      (defn s-rejected [begin: u32 end: u32 fallback: u32 capacity: u32 budget: u32 status: u32]
        (let [before s-writes]
          (= (s-runs 0u) (TextScriptRun 99u 99u 99u))
          (let [result (text-itemize-scripts (array-length s-facts) s-fact-at begin end
            fallback capacity s-emit-run budget)]
            (expect (== result.status status)) (expect (== result.count 0u)))
          (expect (== s-writes before))
          (expect (== (.start (s-runs 0u)) 99u))))
    "#,
        &["script"],
    );
}

#[test]
fn nesting_and_long_paragraph_work_are_bounded_on_both_evaluators() {
    let mut body = String::new();
    for depth in [127, 128, 129] {
        let text = format!("a{}β{}z", "(".repeat(depth), ")".repeat(depth));
        body +=
            &format!("(text-release s-source) (= s-source (make-text \"{text}\")) (s-prepare)\n");
        body += &format!(
            "(let [before s-writes result (s-itemize 25u)] (expect (== result.status {}u)) {} )\n",
            if depth > 128 { 2 } else { 0 },
            if depth > 128 {
                "(expect (== before s-writes))"
            } else {
                "(expect (== result.count 3u))"
            }
        );
    }
    // Linear pending-bracket fixup must handle many script changes without
    // rescanning every older opening; the work budget also bounds failed searches.
    for text in [
        "a(β)".repeat(1000),
        format!("a{}{}", "(".repeat(128), "]".repeat(4000)),
    ] {
        let too_much = text.ends_with(']');
        body +=
            &format!("(text-release s-source) (= s-source (make-text \"{text}\")) (s-prepare)\n");
        body += &format!(
            "(let [before s-writes result (s-itemize 25u)] (expect (== result.status {}u)) {} )\n",
            if too_much { 2 } else { 0 },
            if too_much {
                "(expect (== before s-writes))"
            } else {
                "(expect (<= result.work 1000000u))"
            }
        );
    }
    check(&body);
}

#[test]
fn all_unicode_scalar_script_values_fit_the_checked_transport() {
    let mut chunk = String::new();
    let mut seen = [false; 256];
    let mut scalar_count = 0;
    for scalar in (0..=0x10ffff).filter_map(char::from_u32) {
        chunk.push(scalar);
        scalar_count += 1;
        if chunk.len() >= 32_000 || scalar == '\u{10ffff}' {
            let words = easl::text::script_words(&chunk).unwrap();
            assert_eq!(words.len() / 16, chunk.chars().count());
            for fact in words.chunks_exact(16) {
                for (word, &bits) in fact[8..].iter().enumerate() {
                    for bit in 0..32 {
                        let code = word * 32 + bit;
                        if bits & (1 << bit) != 0 && ![0, 1, 103].contains(&code) && !seen[code] {
                            assert!(easl::text::script_tag(code as u32).is_ok(), "script {code}");
                            seen[code] = true;
                        }
                    }
                }
            }
            chunk.clear();
        }
    }
    assert_eq!(scalar_count, 0x110000 - 0x800);
    assert!(seen.iter().filter(|&&item| item).count() > 170);
}

#[test]
fn script_mask_transport_and_set_operations_match_on_both_evaluators() {
    check(
        r#"
      (let [a (vec4u 1u 2u 128u 2147483648u) b (vec4u 7u 3u 255u 4294967295u)]
        (expect (all (== (& a b) a)))
        (expect (all (== (| a b) b)))
        (expect (all (== (^ a b) (vec4u 6u 1u 127u 2147483647u))))
        (expect (all (== (& a 255u) (vec4u 1u 2u 128u 0u))))
        (expect (all (== (| 255u a) (vec4u 255u 255u 255u 2147483903u))))
        (expect (all (== (<< a (vec4u 1u 2u 3u 1u)) (vec4u 2u 8u 1024u 0u))))
        (expect (all (== (>> a 1u) (vec4u 0u 1u 64u 1073741824u))))
        (expect (all (== (<< 1u (vec4u 0u 2u 7u 31u)) (vec4u 1u 4u 128u 2147483648u))))
        (expect (all (== (>> (vec4i -1i -2i -128i -2147483648i) (vec4i 1i 1i 3i 31i))
          (vec4i -1i -1i -16i -1i))))
        (expect (all (== (>> -128i (vec4i 1i 2i 3i 4i)) (vec4i -64i -32i -16i -8i)))))
      (= s-source (make-text "aβ𑼂")) (s-prepare)
      (expect (== (.primary (s-facts 0u)) 25u))
      (expect (== (.primary (s-facts 1u)) 14u))
      (expect (== (.primary (s-facts 2u)) 198u))
      (expect (== (.x (.extensions-low (s-facts 0u))) 33554432u))
      (expect (== (.x (.extensions-low (s-facts 1u))) 16384u))
      (expect (== (.z (.extensions-high (s-facts 2u))) 64u))
      (let [latin (text-script-options (s-facts 0u)) greek (text-script-options (s-facts 1u))
            kawi (text-script-options (s-facts 2u))]
        (expect (not (text-script-empty latin)))
        (expect (not (text-script-empty greek)))
        (expect (text-script-empty (text-script-intersection latin greek)))
        (expect (text-script-has latin 25u))
        (expect (text-script-has greek 14u))
        (expect (text-script-has kawi 198u))
        (expect (not (text-script-has latin 14u)))
        (expect (== (text-script-pick latin 0u 25u) 25u))
        (expect (== (text-script-pick greek 0u 25u) 14u))
        (expect (== (text-script-pick kawi 0u 25u) 198u)))
      (let [singleton (text-script-single 198u)]
        (expect (text-script-has singleton 198u))
        (expect (not (text-script-has singleton 25u))))
    "#,
    );
}

#[test]
fn itemized_same_direction_scripts_feed_real_shaping_and_exact_caret_cells() {
    let font = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../easl-native-text/tests/fonts/shantell-sans-regular.ttf");
    let body = format!(
        r#"
      (= s-source (make-text "Office (Привет) café")) (s-prepare)
      (= s-font (load-font "{}" 28.)) (= s-ink (font-atlas-glyphs s-font))
      (= s-quads (zeroed-array 128u)) (= s-clusters (zeroed-array 128u))
      (= s-cells (zeroed-array 128u))
      (let [items (s-itemize 25u) @var x 20.]
        (expect (== items.status 0u)) (expect (== items.count 3u))
        (for [i items.count]
          (let [run (s-runs i) part (text-slice s-source run.start run.end)]
            (= s-bytes (text-utf8 part)) (text-release part)
            (= s-glyphs (shape-font-run s-font s-bytes (text-script-tag run.script-code) false))
            (for [j (array-length s-glyphs)] (expect (!= (.id (s-glyphs j)) 0u)))
            (let [placed (text-place-run-geometry (array-length s-glyphs) s-glyph-at s-ink-at
                (vec2f (texture-dimensions s-font)) (vec2f x 60.) false
                s-emit-quad 0u (vec2f 20. 45.) run.start s-emit-cluster)]
              (expect (> placed.advance 0.)) (+= x placed.advance)
              (+= s-quad-base (array-length s-glyphs)) (+= s-cluster-base placed.clusters))))
        (let [cells (text-build-cells s-source s-cluster-base s-cluster-at 128u s-emit-cell)
              @var covered 0u]
          (expect (== cells.status 0u)) (expect (== cells.count 20u))
          (for [i cells.count]
            (let [cell (s-cells i)]
              (expect (== cell.start covered)) (= covered cell.end)))
          (expect (== covered (text-length s-source)))
          (for [i items.count]
            (let [run (s-runs i) caret (text-caret-at cells.count s-cell-at run.start 1u)]
              (expect (== caret.status 0u))
              (expect (== (.byte (text-hit-test cells.count s-cell-at
                (vec2f caret.rect.x (+ caret.rect.y 1.)))) run.start))))))
    "#,
        font.display()
    );
    check_extra(
        &body,
        r#"
      @{group 0 binding 0} (var s-font: (Texture2D f32))
      (var s-ink: [FontAtlasGlyph]) (var s-bytes: [u32])
      (var s-glyphs: [FontShapedGlyph]) (var s-quads: [TextGlyphQuad])
      (var s-clusters: [TextClusterBox]) (var s-cells: [TextCell])
      (var s-quad-base: u32) (var s-cluster-base: u32)
      (defn s-glyph-at [i: u32]: FontShapedGlyph (s-glyphs i))
      (defn s-ink-at [i: u32]: FontAtlasGlyph (s-ink i))
      (defn s-cluster-at [i: u32]: TextClusterBox (s-clusters i))
      (defn s-cell-at [i: u32]: TextCell (s-cells i))
      (defn s-emit-quad [i: u32 item: TextGlyphQuad] (= (s-quads (+ s-quad-base i)) item))
      (defn s-emit-cluster [i: u32 item: TextClusterBox] (= (s-clusters (+ s-cluster-base i)) item))
      (defn s-emit-cell [i: u32 item: TextCell] (= (s-cells i) item))
    "#,
        &["script", "atlas", "geometry"],
    );
}
