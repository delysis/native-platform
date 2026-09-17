use easl::{
    CompilerTarget,
    compiler::{
        builtins::built_in_macros, expression::ExpKind, functions::FunctionImplementationKind,
        program::Program,
    },
    external::ExternalVars,
    font::{FontAtlas, FontError, MAX_RUN_BYTES},
    interpreter::{EvaluationEnvironment, StringIO, VmCpuRuntime, eval},
    parse::{EaslMultiDocument, parse_easl_without_comments},
};
use harfrust::{
    Direction, ShapeError, ShapeLimits, ShapeOptions, ShaperData, UnicodeBuffer, script,
};
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

const AMIRI: &str = "../easl-native-text/tests/fonts/amiri.ttf";
const KAWI: &str = "tests/fonts/noto-sans-kawi.ttf";
const ARABIC: u32 = u32::from_be_bytes(*b"arab");
const LATIN: u32 = u32::from_be_bytes(*b"latn");

fn font(path: &str) -> FontAtlas {
    FontAtlas::from_bytes(std::fs::read(root().join(path)).unwrap(), 32.)
        .unwrap()
        .0
}

#[test]
fn glyph_ids_mark_order_offsets_and_safety_flags_match_pinned_harfbuzz_cases() {
    // Independent HarfBuzz 12.3.2 `hb-shape --shapers=ot --no-glyph-names
    // --utf8-clusters --bot --eot --unsafe-to-concat --show-flags --language=und`.
    // Fonts have 1000 units/em. Expectations use logical cluster order while
    // preserving HarfBuzz's visual mark order within each cluster. This is a
    // finite regression oracle, not a general HarfBuzz equivalence claim.
    type Expected = (u32, u32, u32, i32, i32, u32);
    type Case<'a> = (&'a str, &'a str, u32, bool, &'a [Expected]);
    let cases: &[Case<'_>] = &[
        (
            AMIRI,
            "سَلام",
            ARABIC,
            true,
            &[
                (94, 0, 4, 46, 0, 2),
                (1855, 0, 4, 0, 568, 2),
                (3009, 4, 6, 0, 245, 3),
                (3014, 6, 8, 0, 446, 3),
                (85, 8, 10, 0, 452, 2),
            ],
        ),
        (
            KAWI,
            "\u{11f12}\u{11f42}\u{11f12}\u{11f36}",
            u32::from_be_bytes(*b"kawi"),
            false,
            &[
                (14, 0, 16, 0, 760, 2),
                (134, 0, 16, -667, 0, 2),
                (108, 0, 16, -648, 0, 2),
            ],
        ),
    ];
    for &(path, text, script, rtl, expected) in cases {
        let actual = font(path)
            .shape_span(text, 0..text.len(), script, rtl)
            .unwrap();
        assert_eq!(actual.len(), expected.len(), "{text}");
        for (actual, &(id, start, end, x, advance, flags)) in actual.iter().zip(expected) {
            assert_eq!((actual.id, actual.start, actual.end), (id, start, end));
            assert!((actual.x - x as f32 * 0.032).abs() < 0.00001);
            assert!((actual.advance - advance as f32 * 0.032).abs() < 0.00001);
            assert_eq!(actual.y, 0.);
            assert_eq!(actual.flags, flags);
        }
    }
}

#[test]
fn span_context_preserves_arabic_joining_and_explicit_text_edges_end_it() {
    let font = font(AMIRI);
    let text = "ببب";
    let span = font.shape_span(text, 2..4, ARABIC, true).unwrap();
    let isolated = font.shape_span("ب", 0..2, ARABIC, true).unwrap();
    // The same independent hb-shape oracle with/without --text-before/after=ب.
    assert_eq!(span.len(), 1);
    assert_eq!(span[0].id, 1588);
    assert_eq!((span[0].start, span[0].end), (0, 2));
    assert!((span[0].advance - 244. * 0.032).abs() < 0.00001);
    assert_eq!(span[0].flags, 2);
    assert_eq!(isolated.len(), 1);
    assert_eq!(isolated[0].id, 56);
    assert!((isolated[0].advance - 926. * 0.032).abs() < 0.00001);

    let entire = font.shape_span(text, 0..text.len(), ARABIC, true).unwrap();
    let bytes: Vec<_> = text.bytes().map(u32::from).collect();
    assert_eq!(font.shape(&bytes, ARABIC, true).unwrap(), entire);
    assert_eq!(font.shape_span(text, 2..2, ARABIC, true).unwrap(), []);
}

#[test]
fn spans_reject_partial_graphemes_bad_ranges_unknown_scripts_and_oversized_text() {
    let font = font(AMIRI);
    let text = "a\u{301}👩🏽‍🚀\r\n";
    for (start, end) in [
        (0, 1),
        (1, 3),
        (3, 7),
        (3, 11),
        (3, 14),
        (18, 19),
        (2, 3),
        (5, 4),
        (0, usize::MAX),
    ] {
        let range = start..end;
        assert!(
            matches!(
                font.shape_span(text, range.clone(), LATIN, false),
                Err(FontError::InvalidSpan)
            ),
            "{range:?}"
        );
    }
    for tag in [
        0,
        u32::from_be_bytes(*b"xxxx"),
        u32::from_be_bytes(*b"DFLT"),
        u32::from_be_bytes(*b"zyyy"),
        u32::from_be_bytes(*b"zinh"),
        u32::from_be_bytes(*b"zzzz"),
        u32::from_be_bytes(*b"Kawi"),
    ] {
        assert!(matches!(
            font.shape_span("a", 0..1, tag, false),
            Err(FontError::InvalidScript)
        ));
    }
    // Property recognition alone does not establish coverage: this only checks
    // that every concrete ICU script tag reaches the newer shaping boundary.
    for code in 0..256 {
        if let Ok(tag) = easl::text::script_tag(code) {
            font.shape_span("a", 0..1, tag, false).unwrap();
        }
    }
    let oversized = "a".repeat(MAX_RUN_BYTES + 1);
    assert!(matches!(
        font.shape_span(&oversized, 0..1, LATIN, false),
        Err(FontError::InvalidText)
    ));
    assert!(matches!(
        font.shape(&[256], LATIN, false),
        Err(FontError::InvalidText)
    ));
    assert!(matches!(
        font.shape(&[0xc0, 0x80], LATIN, false),
        Err(FontError::InvalidText)
    ));
}

#[test]
fn long_runs_and_large_mark_clusters_keep_full_byte_ranges() {
    let font = font(AMIRI);
    let text = "ab ".repeat(24_000);
    let glyphs = font.shape_span(&text, 0..text.len(), LATIN, false).unwrap();
    assert_eq!(glyphs.len(), 72_000);
    assert_eq!(glyphs.last().unwrap().end, 72_000);
    assert!(glyphs.windows(2).all(|g| g[0].end == g[1].start));
    let text = format!("س{}", "\u{64e}".repeat(300));
    let glyphs = font.shape_span(&text, 0..text.len(), ARABIC, true).unwrap();
    assert_eq!(glyphs.len(), 301);
    assert!(
        glyphs
            .iter()
            .all(|g| g.start == 0 && g.end == text.len() as u32)
    );
}

#[test]
fn bounded_shaper_rejects_input_growth_and_work_exhaustion_without_partial_output() {
    let data = std::fs::read(root().join(KAWI)).unwrap();
    let font = harfrust::FontRef::from_index(&data, 0).unwrap();
    let cache = ShaperData::new(&font);
    let shaper = cache.shaper(&font).build();
    let buffer = |text: &str, reserve: usize| {
        let mut buffer = UnicodeBuffer::new();
        assert!(buffer.reserve(reserve));
        buffer.push_str(text);
        buffer.set_script(script::KAWI);
        buffer.set_direction(Direction::LeftToRight);
        buffer
    };
    let limits = ShapeLimits {
        max_glyphs: 128,
        max_operations: 65_536,
    };
    for invalid in [
        ShapeLimits {
            max_glyphs: 0,
            ..limits
        },
        ShapeLimits {
            max_operations: 0,
            ..limits
        },
        ShapeLimits {
            max_glyphs: usize::MAX,
            ..limits
        },
        ShapeLimits {
            max_operations: u32::MAX,
            ..limits
        },
    ] {
        assert!(matches!(
            shaper.shape_bounded(buffer("a", 0), ShapeOptions::new(), invalid),
            Err(ShapeError::InvalidLimits)
        ));
    }
    assert!(matches!(
        shaper.shape_bounded(
            buffer("abc", 0),
            ShapeOptions::new(),
            ShapeLimits {
                max_glyphs: 2,
                ..limits
            }
        ),
        Err(ShapeError::LimitExceeded)
    ));
    // An isolated vowel needs an inserted dotted circle. Test both fresh and
    // preallocated buffers so existing allocation cannot bypass the glyph cap.
    for reserve in [0, 1024] {
        assert!(matches!(
            shaper.shape_bounded(
                buffer("\u{11f36}", reserve),
                ShapeOptions::new(),
                ShapeLimits {
                    max_glyphs: 1,
                    ..limits
                }
            ),
            Err(ShapeError::LimitExceeded)
        ));
    }
    let shaped = shaper
        .shape_bounded(buffer("\u{11f36}", 0), ShapeOptions::new(), limits)
        .unwrap();
    assert_eq!(shaped.len(), 2);
    assert!(matches!(
        shaper.shape_bounded(
            buffer("\u{11f12}\u{11f42}\u{11f12}\u{11f36}", 0),
            ShapeOptions::new(),
            ShapeLimits {
                max_operations: 1,
                ..limits
            }
        ),
        Err(ShapeError::LimitExceeded)
    ));
    let unlimited = shaper.shape(buffer("\u{11f36}", 0), ShapeOptions::new());
    assert_eq!(
        unlimited
            .glyph_infos()
            .iter()
            .map(|g| g.glyph_id)
            .collect::<Vec<_>>(),
        shaped
            .glyph_infos()
            .iter()
            .map(|g| g.glyph_id)
            .collect::<Vec<_>>()
    );
}

#[test]
fn shaping_limit_sweep_covers_script_preprocessing_and_buffer_reuse() {
    let cases = [
        ("\u{11f36}", script::KAWI, Direction::LeftToRight),
        (
            "\u{11f12}\u{11f42}\u{11f12}\u{11f36}",
            script::KAWI,
            Direction::LeftToRight,
        ),
        ("\u{e33}", script::THAI, Direction::LeftToRight),
        (
            "\u{e01}\u{e48}\u{e33}",
            script::THAI,
            Direction::LeftToRight,
        ),
        ("\u{eb3}", script::LAO, Direction::LeftToRight),
        ("\u{302e}", script::HANGUL, Direction::LeftToRight),
        (
            "\u{1100}\u{1161}\u{302e}",
            script::HANGUL,
            Direction::LeftToRight,
        ),
        ("\u{905}\u{93e}", script::DEVANAGARI, Direction::LeftToRight),
        ("\u{985}\u{9be}", script::BENGALI, Direction::LeftToRight),
        (
            "ffi a\u{301}\u{fe0f}\u{fe0f}",
            script::LATIN,
            Direction::LeftToRight,
        ),
        ("السَّلَامُ", script::ARABIC, Direction::RightToLeft),
    ];
    for path in [KAWI, AMIRI] {
        let data = std::fs::read(root().join(path)).unwrap();
        let font = harfrust::FontRef::from_index(&data, 0).unwrap();
        let cache = ShaperData::new(&font);
        let shaper = cache.shaper(&font).build();
        for (text, script, direction) in cases {
            let buffer = |reserve| {
                let mut buffer = UnicodeBuffer::new();
                assert!(buffer.reserve(reserve));
                buffer.push_str(text);
                buffer.set_script(script);
                buffer.set_direction(direction);
                buffer
            };
            let fields = |shaped: &harfrust::GlyphBuffer| {
                shaped
                    .glyph_infos()
                    .iter()
                    .zip(shaped.glyph_positions())
                    .map(|(g, p)| {
                        (
                            g.glyph_id,
                            g.cluster,
                            g.flags().to_bits() & 3,
                            p.x_offset,
                            p.y_offset,
                            p.x_advance,
                            p.y_advance,
                        )
                    })
                    .collect::<Vec<_>>()
            };
            let expected = fields(&shaper.shape(buffer(0), ShapeOptions::new()));
            let mut successes = 0;
            for reserve in [0, 128] {
                for max_glyphs in [text.chars().count(), text.chars().count() + 1, 128] {
                    for max_operations in [1, 2, 8, 64, 1024, 65_536] {
                        let limits = ShapeLimits {
                            max_glyphs,
                            max_operations,
                        };
                        match shaper.shape_bounded(buffer(reserve), ShapeOptions::new(), limits) {
                            Ok(shaped) => {
                                assert_eq!(fields(&shaped), expected, "{path} {text:?} {limits:?}");
                                successes += 1;
                            }
                            Err(error) => assert_eq!(error, ShapeError::LimitExceeded),
                        }
                    }
                }
            }
            assert!(successes > 0, "{path} {text:?}");
        }
    }
}

#[test]
fn both_easl_evaluators_shape_borrowed_spans_and_preserve_runs_after_failures() {
    let source = format!(
        r#"
      @{{group 0 binding 0}} (var sp-font: (Texture2D f32))
      (var sp-source: TextBuffer) (var sp-dead: TextBuffer)
      (var sp-run: [FontShapedGlyph]) @external (var sp-copy: [FontShapedGlyph])
      @cpu (defn setup-span []
        (= sp-font (load-font "{AMIRI}" 32.))
        (= sp-source (make-text "ببب")) (= sp-dead (make-text "gone"))
        (text-release sp-dead)
        (= sp-run (shape-font-span sp-font sp-source 2u 4u {ARABIC}u true)))
      @cpu (defn inspect-span [] (= sp-copy sp-run) (print sp-run) (print (text-length sp-source)))
      @cpu (defn split-span [] (= sp-run (shape-font-span sp-font sp-source 1u 4u {ARABIC}u true)))
      @cpu (defn bad-script-span [] (= sp-run (shape-font-span sp-font sp-source 2u 4u 0u true)))
      @cpu (defn dead-span [] (= sp-run (shape-font-span sp-font sp-dead 0u 0u {ARABIC}u true)))
      @cpu (defn end-span [] (text-release sp-source))
    "#
    );
    let documents = EaslMultiDocument::from_singular_document(
        parse_easl_without_comments(&source),
        "span.easl".into(),
        source,
    );
    let (mut program, errors) = Program::from_easl_documents(&documents, built_in_macros());
    assert!(errors.is_empty(), "{errors:?}");
    let errors = program.validate_raw_program(CompilerTarget::WGSL);
    assert!(errors.is_empty(), "{errors:?}");
    let external = ExternalVars::new(&program);
    let mut vm = VmCpuRuntime::new_cpu_with_external(
        program.clone(),
        StringIO::new(),
        Some(root()),
        Some(external.clone()),
    )
    .unwrap();
    let mut tree =
        EvaluationEnvironment::from_program(program.clone(), StringIO::new(), Some(root()))
            .unwrap();
    let tree_run = |name: &str, tree: &mut EvaluationEnvironment<StringIO>| {
        let functions = program.cpu_entry_points();
        let f = functions
            .iter()
            .find(|f| &*f.read().unwrap().name == name)
            .unwrap()
            .read()
            .unwrap();
        let FunctionImplementationKind::Composite(f) = &f.implementation else {
            panic!("expected body")
        };
        let f = f.read().unwrap();
        let ExpKind::Function(_, body) = &f.expression.kind else {
            panic!("expected function")
        };
        eval(*body.clone(), tree)
    };
    vm.run("setup-span").unwrap();
    tree_run("setup-span", &mut tree).unwrap();
    vm.run("inspect-span").unwrap();
    tree_run("inspect-span", &mut tree).unwrap();
    let saved = external.read_external_var_raw("sp-copy").unwrap();
    assert_eq!(saved.len(), 7);
    assert_eq!(&saved[..3], &[1588, 0, 2]);
    assert_eq!(saved[6], 2);
    for invalid in ["split-span", "bad-script-span", "dead-span"] {
        assert!(vm.run(invalid).is_err());
        assert!(tree_run(invalid, &mut tree).is_err());
        vm.run("inspect-span").unwrap();
        tree_run("inspect-span", &mut tree).unwrap();
        assert_eq!(external.read_external_var_raw("sp-copy").unwrap(), saved);
        assert_eq!(tree.io.events, vm.env.io.events);
        assert_eq!(tree.text_values.live_values(), 1);
        assert_eq!(vm.env.text_values.live_values(), 1);
    }
    vm.run("end-span").unwrap();
    tree_run("end-span", &mut tree).unwrap();
    assert_eq!(tree.text_values.live_values(), 0);
    assert_eq!(vm.env.text_values.live_values(), 0);
}
