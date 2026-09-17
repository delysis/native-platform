use easl::{
    CompilerTarget,
    compiler::{builtins::built_in_macros, program::Program},
    interpreter::{CpuRuntime, IOEvent, StringIO, run_program_entry_with_io_and_runtime_from_path},
    parse::{load_easl_imports_with_lookup_function, parse_easl_without_comments},
};
use std::path::Path;

fn check(body: &str, extra: &str) {
    let source = format!(
        r#"
(import "../library/rich.easl")
(import "../library/geometry.easl")
(var r-assertions: u32) (var r-writes: u32)
(var r-value: TextBuffer) (var r-facts: [TextGrapheme])
(var r-glyphs: [FontShapedGlyph]) (var r-styled: [FontShapedGlyph])
(var r-style: TextRunStyle)
(var r-quads: [TextGlyphQuad]) (var r-clusters: [TextClusterBox]) (var r-cells: [TextCell])
(var r-slots: [TextMeasureSlot]) (var r-prefix: [TextShapedPrefix])
(var r-decor: [TextDecoration])
(defn r-check [ok: bool] (+= r-assertions 1u) (when (not ok) (print r-assertions)))
(defn r-fact [i: u32]: TextGrapheme (r-facts i))
(defn r-glyph [i: u32]: FontShapedGlyph (r-glyphs i))
(defn r-output [i: u32]: FontShapedGlyph (r-styled i))
(defn r-emit [i: u32 g: FontShapedGlyph] (+= r-writes 1u) (= (r-styled i) g))
(defn r-transform [capacity: u32 budget: u32]: TextRunResult
  (text-style-glyphs (array-length r-facts) r-fact (array-length r-glyphs) r-glyph
    0u r-style 40. capacity r-emit budget))
{extra}
@cpu (defn main []
  (= r-value (make-text "á b c")) (= r-facts (text-graphemes r-value))
  (= r-style (TextRunStyle 0u 20. 28. 400. 2. 3. 0u 0u 4294967295u (vec4u 1u 2u 3u 255u) 22u))
  (= r-glyphs (zeroed-array 6u)) (= r-styled (zeroed-array 6u))
  (= (r-glyphs 0u) (FontShapedGlyph 1u 0u 3u 0. 0. 12. 0u))
  (= (r-glyphs 1u) (FontShapedGlyph 2u 0u 3u 3. -2. 0. 1u))
  (= (r-glyphs 2u) (FontShapedGlyph 3u 3u 4u 0. 0. 4. 0u))
  (= (r-glyphs 3u) (FontShapedGlyph 4u 4u 5u 0. 0. 10. 0u))
  (= (r-glyphs 4u) (FontShapedGlyph 5u 5u 7u 0. 0. 4. 0u))
  (= (r-glyphs 5u) (FontShapedGlyph 6u 7u 8u 0. 0. 10. 0u))
  {body}
  (text-release r-value) (print "done"))
"#
    );
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/rich-test.easl");
    let parsed = parse_easl_without_comments(&source);
    assert!(
        parsed.parsing_failures.is_empty(),
        "{:?}",
        parsed.parsing_failures
    );
    let docs = load_easl_imports_with_lookup_function(
        parsed,
        Some(&path),
        source,
        easl::parse::ImportLimits::default(),
        |p| std::fs::read_to_string(p),
    )
    .unwrap_or_else(|docs| {
        let errors: Vec<_> = docs
            .sources
            .iter()
            .filter(|(doc, _, _)| !doc.parsing_failures.is_empty())
            .map(|(doc, path, _)| (path, &doc.parsing_failures))
            .collect();
        panic!("Import parse errors: {errors:?}")
    })
    .unwrap_or_else(|(_, errors)| panic!("Import errors: {errors:?}"));
    let (mut program, errors) = Program::from_easl_documents(&docs, built_in_macros());
    assert!(errors.is_empty(), "{errors:?}");
    let errors = program.validate_raw_program(CompilerTarget::WGSL);
    assert!(errors.is_empty(), "{errors:?}");
    for runtime in [CpuRuntime::TreeWalking, CpuRuntime::BytecodeVm] {
        let io = run_program_entry_with_io_and_runtime_from_path(
            program.clone(),
            Some("main"),
            StringIO::new(),
            &path,
            runtime,
        )
        .unwrap()
        .0;
        assert_eq!(io.events, [IOEvent::Print("done".into())], "{runtime:?}");
    }
}

#[test]
fn styled_advances_ink_and_editable_cells_use_the_same_scale_and_spacing() {
    check(
        r#"
      (r-check (== (.status (r-transform 6u 10000u)) 0u)) (r-check (== r-writes 6u))
      (r-check (== (.advance (r-styled 0u)) 6.)) (r-check (== (.advance (r-styled 1u)) 2.))
      (r-check (== (.x (r-styled 1u)) 1.5)) (r-check (== (.y (r-styled 1u)) -1.))
      (r-check (== (.flags (r-styled 1u)) 1u))
      (= r-quads (zeroed-array 6u)) (= r-clusters (zeroed-array 5u)) (= r-cells (zeroed-array 5u))
      (= r-slots (zeroed-array 6u)) (= r-prefix (zeroed-array 6u))
      (let [measured (text-measure-run 5u r-fact 6u r-output 0u 6u r-slot r-emit-slot 10000u)
            prefixes (text-measure-prefixes 5u r-slot 6u r-emit-prefix)
            placed (text-place-scaled-run-geometry 6u r-output r-ink (vec2f 64.) (vec2f 10. 20.)
              0.5 false r-quad 0u (vec2f 2. 28.) 0u r-cluster)]
        (r-check (== measured.status 0u)) (r-check (== prefixes.status 0u))
        (r-check (== (.x (.advance (r-prefix 5u))) 36.))
        (r-check (== placed.advance 36.)) (r-check (== placed.clusters 5u))
        (r-check (all (== (.bounds (r-quads 0u)) (vec4f 9.5 17.5 2. 3.))))
        (r-check (all (== (.uv (r-quads 0u)) (/ (vec4f 1. 2. 5. 8.) 64.))))
        (let [cells (text-build-cells r-value placed.clusters r-cluster-at 5u r-cell)]
          (r-check (== cells.status 0u)) (r-check (== cells.count 5u))
          (r-check (== (.start (r-cells 0u)) 0u)) (r-check (== (.end (r-cells 0u)) 3u))
          (r-check (== (.z (.bounds (r-cells 0u))) 8.))))
      (let [placed (text-place-scaled-run-geometry 6u r-output r-ink (vec2f 64.) (vec2f 10. 20.)
              0.5 true r-quad 0u (vec2f 2. 28.) 0u r-cluster)]
        (r-check (== placed.advance 36.))
        (r-check (== (.start (r-quads 0u)) 7u)) (r-check (== (.start (r-quads 1u)) 5u))
        (r-check (== (.start (r-quads 4u)) 0u)) (r-check (== (.start (r-quads 5u)) 0u)))
    "#,
        r#"
      (defn r-ink [i: u32]: FontAtlasGlyph (FontAtlasGlyph (vec4u 1u 2u 4u 6u) (vec2f -1. -5.) 0.))
      (defn r-quad [i: u32 q: TextGlyphQuad] (= (r-quads i) q))
      (defn r-cluster [i: u32 c: TextClusterBox] (= (r-clusters i) c))
      (defn r-cluster-at [i: u32]: TextClusterBox (r-clusters i))
      (defn r-cell [i: u32 c: TextCell] (= (r-cells i) c))
      (defn r-slot [i: u32]: TextMeasureSlot (r-slots i))
      (defn r-emit-slot [i: u32 s: TextMeasureSlot] (= (r-slots i) s))
      (defn r-emit-prefix [i: u32 p: TextShapedPrefix] (= (r-prefix i) p))
    "#,
    );
}

#[test]
fn invalid_late_glyphs_and_exhausted_work_do_not_publish_partial_styling() {
    check(
        r#"
      (r-check (== (.status (r-transform 5u 10000u)) 2u)) (r-check (== r-writes 0u))
      (r-check (== (.status (r-transform 6u 10u)) 2u)) (r-check (== r-writes 0u))
      (= (.end (r-glyphs 5u)) 9u)
      (r-check (== (.status (r-transform 6u 10000u)) 1u)) (r-check (== r-writes 0u))
      (= (.end (r-glyphs 5u)) 8u) (= (.y (r-glyphs 5u)) (/ 0. 0.))
      (r-check (== (.status (r-transform 6u 10000u)) 1u)) (r-check (== r-writes 0u))
      (= (.y (r-glyphs 5u)) 0.) (= r-style.letter-spacing -20.)
      (r-check (== (.status (r-transform 6u 10000u)) 4u)) (r-check (== r-writes 0u))
      (= r-style.letter-spacing 2.)
      (r-check (== (.status (r-transform 6u 10000u)) 0u))
      (r-check (== r-writes 6u))
    "#,
        "",
    );
}

#[test]
fn mixed_sizes_share_a_baseline_and_decorations_use_font_metrics() {
    check(
        r#"
      (let [empty (text-rich-line 0u r-line-style r-line-metrics r-style (vec4f 28. 12. 4. 40.))
            mixed (text-rich-line 2u r-line-style r-line-metrics r-style (vec4f 28. 12. 4. 40.))]
        (r-check (== empty.status 0u)) (r-check (== empty.ascent 18.)) (r-check (== empty.descent 10.))
        (r-check (== mixed.status 0u)) (r-check (== mixed.ascent 33.)) (r-check (== mixed.descent 17.)))
      (= r-decor (zeroed-array 2u))
      (r-check (== (.status (text-rich-decorations r-style 40. (vec4f -4. 2. 12. 2.)
        (vec2f 10. 20.) 36. 1u r-decoration)) 2u)) (r-check (== r-writes 0u))
      (r-check (== (.status (text-rich-decorations r-style 40. (vec4f -4. 2. 12. 2.)
        (vec2f 10. 20.) 36. 2u r-decoration)) 0u)) (r-check (== r-writes 2u))
      (r-check (all (== (.bounds (r-decor 0u)) (vec4f 10. 22. 36. 1.))))
      (r-check (all (== (.bounds (r-decor 1u)) (vec4f 10. 14. 36. 1.))))
      (r-check (all (== (.color (r-decor 0u)) r-style.color)))
    "#,
        r#"
      (defn r-line-style [i: u32]: TextRunStyle
        (let [@var s r-style] (when (== i 1u) (= s.size 40.) (= s.line-height 50.)) s))
      (defn r-line-metrics [i: u32]: vec4f (vec4f 28. 12. 4. 40.))
      (defn r-decoration [i: u32 d: TextDecoration] (+= r-writes 1u) (= (r-decor i) d))
    "#,
    );
}

#[test]
fn font_decoration_builtin_exposes_the_loaded_faces_metrics_in_both_evaluators() {
    // Independent pinned Amiri tables: head unitsPerEm=1000, post underline
    // position=-512/thickness=50, OS/2 strikeout position=260/thickness=50.
    check(
        r#"
      (= r-font (load-font "../../easl-native-text/tests/fonts/amiri.ttf" 32.))
      (let [d (font-decoration-metrics r-font)]
        (r-check (all (< (abs (- d (vec4f -16.384 1.6 8.32 1.6))) (vec4f 0.0001)))))
    "#,
        "@{group 0 binding 0} (var r-font: (Texture2D f32))",
    );
}

#[test]
fn underline_and_strikeout_keep_distinct_font_table_thicknesses() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../easl-native-text/tests/fonts/amiri.ttf");
    let mut bytes = std::fs::read(path).unwrap();
    let count = u16::from_be_bytes(bytes[4..6].try_into().unwrap()) as usize;
    let os2 = (0..count)
        .map(|i| 12 + i * 16)
        .find(|&at| &bytes[at..at + 4] == b"OS/2")
        .map(|at| u32::from_be_bytes(bytes[at + 8..at + 12].try_into().unwrap()) as usize)
        .unwrap();
    // A controlled in-memory fixture, not a changed font asset: distinct
    // strikeout size must survive even though Swash reports one stroke_size.
    bytes[os2 + 26..os2 + 28].copy_from_slice(&75_i16.to_be_bytes());
    let (font, _) = easl::font::FontAtlas::from_bytes(bytes, 32.).unwrap();
    for (got, expected) in font.decorations.into_iter().zip([-16.384, 1.6, 8.32, 2.4]) {
        assert!((got - expected).abs() < 0.0001, "{got} != {expected}");
    }
}

#[test]
fn real_rich_line_preserves_style_boundaries_features_cells_and_empty_insertion_style() {
    use easl::font::{FontAtlas, FontFeature, FontLoadOptions, FontShapeOptions, FontVariation};
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let bytes = std::fs::read(root.join("tests/fonts/shantell-sans-variable.ttf")).unwrap();
    let make_font = |weight, italic| {
        FontAtlas::from_bytes_with(
            bytes.clone(),
            40.,
            FontLoadOptions {
                face_index: 0,
                variations: &[
                    FontVariation {
                        tag: u32::from_be_bytes(*b"wght"),
                        value: weight,
                    },
                    FontVariation {
                        tag: u32::from_be_bytes(*b"ital"),
                        value: italic,
                    },
                ],
            },
        )
        .unwrap()
        .0
    };
    let regular = make_font(400., 0.);
    let heavy = make_font(700., 1.);
    let value = "Plain bold tail";
    let features = [FontFeature {
        tag: u32::from_be_bytes(*b"kern"),
        value: 1,
    }];
    let options = FontShapeOptions {
        features: &features,
        language: "en",
    };
    let measured = |font: &FontAtlas, range, size| {
        font.shape_span_with(value, range, u32::from_be_bytes(*b"latn"), false, options)
            .unwrap()
            .iter()
            .map(|g| g.advance * size / 40.)
            .sum::<f32>()
    };
    let expected = measured(&regular, 0..6, 24.)
        + 6. * 1.25
        + 3.
        + measured(&heavy, 6..10, 40.)
        + measured(&regular, 10..15, 28.)
        + 5. * 1.25
        + 3.;
    let path = root.join("examples/style_specimen.easl");
    let original = std::fs::read_to_string(&path).unwrap();
    let source = format!(
        r#"{original}
(var proof-value: TextBuffer) (var proof-checks: u32)
(defn proof-check [ok: bool] (+= proof-checks 1u) (when (not ok) (print proof-checks)))
(defn proof-span [i: u32]: TextStyledSpan
  (let [@var s (style-record (if (== i 1u) 1u 0u))]
    (if (== i 0u) (let [] (= s.size 24.) (= s.flags 16u))
      (if (== i 1u) (= s.flags 19u) (let [] (= s.size 28.) (= s.flags 20u))))
    (TextStyledSpan (match i 0u 0u 1u 6u _ 10u) (match i 0u 6u 1u 10u _ 15u) s)))
@cpu (defn rich-proof []
  (style-prepare) (proof-check (== style-status 0u))
  (= proof-value (make-text "Plain bold tail"))
  (let [status (line-layout-styled proof-value 3u proof-span style-font-for
          (.style (proof-span 0u)) (vec2f 10. 60.))]
    (proof-check (== status 0u)) (proof-check (== line-run-count 3u))
    (proof-check (== line-cell-count 15u)) (proof-check (== line-decoration-count 2u))
    (proof-check (< (abs (- line-advance {expected:.9})) 0.002))
    (for [i line-quad-count]
      (let [q (line-quads i)]
        (proof-check (<= q.end 15u))
        (when (and (>= q.start 6u) (< q.start 10u))
          (proof-check (== (line-quad-fonts i) 1u))
          (proof-check (all (== (line-quad-colors i) (vec4u 140u 26u 15u 255u)))))))
    (proof-check (== (text-length proof-value) 15u)))
  ; Transformed runs honor the remaining composition budget. The native
  ; identity run needs no extra EASL glyph pass, including at zero budget.
  (line-shape-budget proof-value (line-runs 0u) 1u)
  (proof-check (== line-shape-status 2u))
  (line-shape-budget proof-value (line-runs 1u) 0u)
  (proof-check (== line-shape-status 0u)) (proof-check (== line-style-work 0u))
  (proof-check (> (array-length line-glyphs) 0u))
  (proof-check (== (line-layout-styled proof-value 3u proof-span style-font-for
    (.style (proof-span 0u)) (vec2f 10. 60.)) 0u))
  (proof-check (< (abs (- line-advance {expected:.9})) 0.002))
  (text-release proof-value) (= proof-value (make-text ""))
  (let [s (.style (proof-span 1u))
        status (line-layout-styled proof-value 1u (fn [i] (TextStyledSpan 0u 0u s))
          style-font-for (.style (proof-span 0u)) (vec2f 10. 60.))]
    (proof-check (== status 0u)) (proof-check (== line-cell-count 1u))
    (proof-check (== (.w (.bounds (line-cells 0u))) s.line-height)))
  (text-release proof-value) (= proof-value (make-text "é"))
  (proof-check (== (line-prepare-styled proof-value 2u
    (fn [i] (TextStyledSpan (if (== i 0u) 0u 1u) (if (== i 0u) 1u 3u) (style-record 0u)))
    style-font-for (style-record 0u)) 4u))
  (text-release proof-value) (style-close) (print "done"))
"#
    );
    let parsed = parse_easl_without_comments(&source);
    assert!(
        parsed.parsing_failures.is_empty(),
        "{:?}",
        parsed.parsing_failures
    );
    let docs = load_easl_imports_with_lookup_function(
        parsed,
        Some(&path),
        source,
        easl::parse::ImportLimits::default(),
        |p| std::fs::read_to_string(p),
    )
    .unwrap_or_else(|docs| {
        panic!(
            "Parse errors: {:?}",
            docs.sources
                .iter()
                .filter(|(d, _, _)| !d.parsing_failures.is_empty())
                .map(|(d, p, _)| (p, &d.parsing_failures))
                .collect::<Vec<_>>()
        )
    })
    .unwrap_or_else(|(_, e)| panic!("Import errors: {e:?}"));
    let (mut program, errors) = Program::from_easl_documents(&docs, built_in_macros());
    assert!(errors.is_empty(), "{errors:?}");
    let errors = program.validate_raw_program(CompilerTarget::WGSL);
    assert!(errors.is_empty(), "{errors:?}");
    for runtime in [CpuRuntime::TreeWalking, CpuRuntime::BytecodeVm] {
        let io = run_program_entry_with_io_and_runtime_from_path(
            program.clone(),
            Some("rich-proof"),
            StringIO::new(),
            &path,
            runtime,
        )
        .unwrap()
        .0;
        assert_eq!(io.events, [IOEvent::Print("done".into())], "{runtime:?}");
    }
}

#[test]
fn tracking_disables_optional_ligatures_and_respects_explicit_author_features() {
    check(
        r#"
      (= r-feature-input (zeroed-array 2u)) (= r-feature-output (zeroed-array 3u))
      (r-check (== (.status (text-tracking-features 0u r-feature false 0u r-emit-feature)) 0u))
      (r-check (== r-writes 0u))
      (r-check (== (.status (text-tracking-features 0u r-feature true 1u r-emit-feature)) 2u))
      (r-check (== r-writes 0u))
      (= (r-feature-input 0u) (FontFeature 1801810542u 1u))
      (= (r-feature-input 1u) (FontFeature 1818847073u 1u))
      (let [settings (text-tracking-features 2u r-feature true 3u r-emit-feature)]
        (r-check (== settings.status 0u)) (r-check (== settings.count 3u))
        (r-check (== (.tag (r-feature-output 1u)) 1818847073u))
        (r-check (== (.value (r-feature-output 1u)) 1u))
        (r-check (== (.tag (r-feature-output 2u)) 1668049255u))
        (r-check (== (.value (r-feature-output 2u)) 0u)))
      (= r-writes 0u) (= (r-feature-input 1u) (r-feature-input 0u))
      (r-check (== (.status (text-tracking-features 2u r-feature true 3u r-emit-feature)) 1u))
      (r-check (== r-writes 0u))
      (= r-feature-output (zeroed-array 2u))
      (r-check (== (.status (text-tracking-features 0u r-feature true 2u r-emit-feature)) 0u))
      (= r-font (load-font "../../easl-native-text/tests/fonts/amiri.ttf" 32.))
      (= r-word (make-text "office")) (= r-language (make-text "en"))
      (= r-normal (shape-font-span r-font r-word 0u 6u (text-script-latin) false))
      (= r-unjoined (shape-font-span r-font r-word 0u 6u (text-script-latin) false r-feature-output r-language))
      (r-check (< (array-length r-normal) (array-length r-unjoined)))
      (r-check (== (array-length r-unjoined) 6u))
      (text-release r-word) (text-release r-language)
    "#,
        r#"
      (var r-feature-input: [FontFeature]) (var r-feature-output: [FontFeature])
      (defn r-feature [i: u32]: FontFeature (r-feature-input i))
      (defn r-emit-feature [i: u32 f: FontFeature] (+= r-writes 1u) (= (r-feature-output i) f))
      @{group 0 binding 0} (var r-font: (Texture2D f32))
      (var r-word: TextBuffer) (var r-language: TextBuffer)
      (var r-normal: [FontShapedGlyph]) (var r-unjoined: [FontShapedGlyph])
    "#,
    );
}
