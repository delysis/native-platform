use easl::font::{
    FontAtlas, FontError, FontFeature, FontLoadOptions, FontShapeOptions, FontVariation,
};
use easl::{
    CompilerTarget,
    compiler::{builtins::built_in_macros, program::Program},
    external::ExternalVars,
    interpreter::{
        CpuRuntime, StringIO, VmCpuRuntime, run_program_entry_with_io_and_runtime_from_path,
    },
    parse::{EaslMultiDocument, load_and_parse_easl_multidocument, parse_easl_without_comments},
};
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}
fn program(documents: EaslMultiDocument) -> Program {
    let (mut program, errors) = Program::from_easl_documents(&documents, built_in_macros());
    assert!(errors.is_empty(), "{errors:?}");
    let errors = program.validate_raw_program(CompilerTarget::WGSL);
    assert!(errors.is_empty(), "{errors:?}");
    program
}
fn specimen() -> Program {
    program(
        load_and_parse_easl_multidocument(&root().join("examples/font_atlas.easl"))
            .unwrap()
            .unwrap()
            .unwrap(),
    )
}

#[test]
fn ordinary_easl_import_loads_shapes_and_places_font_atlas_in_both_runtimes() {
    let program = specimen();
    let external = ExternalVars::new(&program);
    let mut vm = VmCpuRuntime::new_cpu_with_external(
        program.clone(),
        StringIO::new(),
        Some(root().join("examples")),
        Some(external.clone()),
    )
    .unwrap();
    vm.run("prepare").unwrap();
    let count = external.read_external_var_raw("specimen-count").unwrap()[0];
    let advance = f32::from_bits(external.read_external_var_raw("specimen-advance").unwrap()[0]);
    assert!((10..50).contains(&count));
    assert!((300. ..1200.).contains(&advance), "{advance}");
    // Print the actual quad buffer from both evaluators; captures include exact
    // source ranges, mark offsets and normalized texture coordinates.
    let source = std::fs::read_to_string(root().join("examples/font_atlas.easl"))
        .unwrap()
        .replace("(defn main []", "(defn render-specimen []")
        + "\n@cpu (defn main [] (prepare) (print specimen-quads))";
    let parsed = parse_easl_without_comments(&source);
    let docs = easl::parse::load_easl_imports_with_lookup_function(
        parsed,
        Some(&root().join("examples/font_atlas.easl")),
        source,
        easl::parse::ImportLimits::default(),
        |path| std::fs::read_to_string(path),
    )
    .unwrap()
    .unwrap();
    let program = self::program(docs);
    let run = |runtime| {
        run_program_entry_with_io_and_runtime_from_path(
            program.clone(),
            Some("main"),
            StringIO::new(),
            &root().join("examples/font_atlas.easl"),
            runtime,
        )
        .unwrap()
        .0
    };
    assert_eq!(
        run(CpuRuntime::TreeWalking).events,
        run(CpuRuntime::BytecodeVm).events
    );
}

#[test]
fn native_font_primitive_retains_clusters_and_validates_inputs() {
    let data = std::fs::read(root().join("../easl-native-text/tests/fonts/amiri.ttf")).unwrap();
    let (font, pixels) = FontAtlas::from_bytes(data.clone(), 32.).unwrap();
    assert_eq!(pixels.len(), (font.width * font.height * 4) as usize);
    assert!(pixels.chunks_exact(4).any(|p| p[3] > 0));
    for glyph in &font.glyphs {
        assert!(glyph.rect[0] + glyph.rect[2] <= font.width);
        assert!(glyph.rect[1] + glyph.rect[3] <= font.height);
    }
    for (text, script, rtl) in [
        ("office café a\u{301}", u32::from_be_bytes(*b"latn"), false),
        ("السَّلَامُ", u32::from_be_bytes(*b"arab"), true),
    ] {
        let shaped = font
            .shape(
                &text.bytes().map(u32::from).collect::<Vec<_>>(),
                script,
                rtl,
            )
            .unwrap();
        assert!(!shaped.is_empty());
        assert!(
            shaped
                .iter()
                .all(|g| text.is_char_boundary(g.start as usize)
                    && text.is_char_boundary(g.end as usize)
                    && g.start < g.end
                    && (g.id as usize) < font.glyphs.len())
        );
        assert!(shaped.windows(2).all(|pair| pair[0].start <= pair[1].start));
    }
    assert!(matches!(
        font.shape(&[255], u32::from_be_bytes(*b"latn"), false),
        Err(FontError::InvalidText)
    ));
    assert!(matches!(
        font.shape(&[65], 0, false),
        Err(FontError::InvalidScript)
    ));
    assert!(FontAtlas::from_bytes(vec![0; 100], 32.).is_err());
    assert!(FontAtlas::from_bytes(data, f32::NAN).is_err());
}

#[test]
fn shader_locations_accept_matching_types_and_reject_mismatches() {
    for (input_type, expected_valid) in [("vec2f", true), ("vec3f", false)] {
        let source = format!(
            "(struct V @{{builtin position}} clip: vec4f @{{location 0}} uv: vec2f) @vertex (defn vertex []: V (V (vec4f 0.) (vec2f 0.))) @fragment (defn fragment [@{{location 0}} input: {input_type}]: @{{location 0}} vec4f (vec4f input.x)) @cpu (defn main [] (dispatch-render-shaders vertex fragment 3u))"
        );
        let docs = EaslMultiDocument::from_singular_document(
            parse_easl_without_comments(&source),
            "locations.easl".into(),
            source,
        );
        let (mut program, errors) = Program::from_easl_documents(&docs, built_in_macros());
        assert!(errors.is_empty());
        assert_eq!(
            program
                .validate_raw_program(CompilerTarget::WGSL)
                .is_empty(),
            expected_valid
        );
    }
}

#[test]
fn invalid_font_reload_preserves_the_loaded_resource_and_atlases_are_read_only() {
    let source = "@{group 0 binding 0} (var font: (Texture2D f32)) @external (var metrics: vec4f) @cpu (defn load [] (= font (load-font \"../easl-native-text/tests/fonts/amiri.ttf\" 32.))) @cpu (defn invalid [] (= font (load-font \"../easl-native-text/tests/fonts/amiri-OFL.txt\" 32.))) @cpu (defn measure [] (= metrics (font-metrics font))) @cpu (defn overwrite [] (set-render-target font))";
    let program = program(EaslMultiDocument::from_singular_document(
        parse_easl_without_comments(source),
        "font-resource.easl".into(),
        source.into(),
    ));
    let external = ExternalVars::new(&program);
    let mut vm = VmCpuRuntime::new_cpu_with_external(
        program,
        StringIO::new(),
        Some(root()),
        Some(external.clone()),
    )
    .unwrap();
    vm.run("load").unwrap();
    vm.run("measure").unwrap();
    let metrics = external.read_external_var_raw("metrics").unwrap();
    assert!(vm.run("invalid").is_err());
    assert!(vm.run("overwrite").is_err());
    vm.run("measure").unwrap();
    assert_eq!(external.read_external_var_raw("metrics").unwrap(), metrics);
}

#[test]
fn unbound_runtime_arrays_are_rejected_in_gpu_entry_points() {
    for declaration in [
        "(var bytes: [u32])",
        "(def bytes: [u32] (into-dynamic-array [1u]))",
    ] {
        let source = format!(
            "{declaration} @vertex (defn vertex []: @{{builtin position}} vec4f (vec4f (f32 (bytes 0u))))"
        );
        let docs = EaslMultiDocument::from_singular_document(
            parse_easl_without_comments(&source),
            "cpu-array.easl".into(),
            source,
        );
        let (mut program, errors) = Program::from_easl_documents(&docs, built_in_macros());
        assert!(errors.is_empty(), "{errors:?}");
        let errors = program.validate_raw_program(CompilerTarget::WGSL);
        assert!(!errors.is_empty(), "{declaration} was accepted in a shader");
        assert!(format!("{errors:?}").contains("unbound CPU resource bytes"));
    }
}

#[test]
fn real_latin_arabic_and_kawi_share_glyph_and_grapheme_cell_positions() {
    for (font_path, text, script, rtl, boundaries) in [
        (
            "../easl-native-text/tests/fonts/amiri.ttf",
            "ffi é",
            u32::from_be_bytes(*b"latn"),
            false,
            vec![0, 1, 2, 3, 4, 7],
        ),
        (
            "../easl-native-text/tests/fonts/amiri.ttf",
            "سَلام",
            u32::from_be_bytes(*b"arab"),
            true,
            vec![0, 4, 6, 8, 10],
        ),
        (
            "tests/fonts/noto-sans-kawi.ttf",
            "\u{11f12}\u{11f42}\u{11f12}\u{11f36}",
            u32::from_be_bytes(*b"kawi"),
            false,
            vec![0, 16],
        ),
    ] {
        let source = format!(
            r#"
          @{{group 0 binding 0}} (var t-font: (Texture2D f32))
          (var t-source: TextBuffer)
          (var t-bytes: [u32]) (var t-glyphs: [FontShapedGlyph])
          (var t-ink: [FontAtlasGlyph]) (var t-clusters: [TextClusterBox])
          @external (var t-cells: [TextCell]) @external (var t-count: u32)
          (defn read-glyph [i: u32]: FontShapedGlyph (t-glyphs i))
          (defn read-ink [i: u32]: FontAtlasGlyph (t-ink i))
          (defn read-cluster [i: u32]: TextClusterBox (t-clusters i))
          (defn save-cluster [i: u32 item: TextClusterBox] (= (t-clusters i) item))
          (defn save-cell [i: u32 item: TextCell] (= (t-cells i) item))
          (defn inspect-quad [i: u32 item: TextGlyphQuad] (print item))
          @cpu (defn main []
            (= t-font (load-font "{font_path}" 32.))
            (= t-source (make-text "{text}")) (= t-bytes (text-utf8 t-source))
            (= t-ink (font-atlas-glyphs t-font))
            (= t-glyphs (shape-font-span t-font t-source 0u (text-length t-source) {script}u {rtl}))
            (= t-clusters (zeroed-array (array-length t-glyphs)))
            (= t-cells (zeroed-array (+ (text-length t-source) 1u)))
            (let [placed (text-place-run-geometry (array-length t-glyphs) read-glyph read-ink
                    (vec2f (texture-dimensions t-font)) (vec2f 30. 40.) {rtl} inspect-quad
                    0u (vec2f 5. 48.) 0u save-cluster)
                  cells (text-build-cells t-source placed.clusters read-cluster (array-length t-cells) save-cell)]
              (print cells.status) (= t-count cells.count))
            (print t-cells) (text-release t-source))
        "#
        );
        let mut docs = EaslMultiDocument::from_singular_document(
            parse_easl_without_comments(&source),
            "real-cells.easl".into(),
            source,
        );
        for (name, content) in [
            ("atlas.easl", include_str!("../library/atlas.easl")),
            ("geometry.easl", include_str!("../library/geometry.easl")),
        ] {
            docs.add_document(
                parse_easl_without_comments(content),
                name.into(),
                content.into(),
            );
        }
        let program = program(docs);
        let external = ExternalVars::new(&program);
        let mut vm = VmCpuRuntime::new_cpu_with_external(
            program.clone(),
            StringIO::new(),
            Some(root()),
            Some(external.clone()),
        )
        .unwrap();
        vm.run("main").unwrap();
        let tree = run_program_entry_with_io_and_runtime_from_path(
            program,
            Some("main"),
            StringIO::new(),
            &root().join("real-cells.easl"),
            CpuRuntime::TreeWalking,
        )
        .unwrap()
        .0;
        assert_eq!(tree.events, vm.env.io.events, "{text}");
        let count = external.read_external_var_raw("t-count").unwrap()[0] as usize;
        assert_eq!(count, boundaries.len() - 1, "{text}");
        let words = external.read_external_var_raw("t-cells").unwrap();
        let mut ranges: Vec<_> = words[..count * 8]
            .chunks_exact(8)
            .map(|cell| {
                assert!(f32::from_bits(cell[0]).is_finite());
                assert!(f32::from_bits(cell[2]) >= 0.);
                assert_eq!(cell[7], u32::from(rtl));
                (cell[4], cell[5])
            })
            .collect();
        ranges.sort_unstable();
        assert_eq!(
            ranges,
            boundaries
                .windows(2)
                .map(|w| (w[0], w[1]))
                .collect::<Vec<_>>(),
            "{text}"
        );
        assert_eq!(vm.env.text_values.live_values(), 0);
    }
}

const LATIN_TAG: u32 = u32::from_be_bytes(*b"latn");
const WGHT_TAG: u32 = u32::from_be_bytes(*b"wght");
const ITAL_TAG: u32 = u32::from_be_bytes(*b"ital");

#[test]
fn feature_and_language_settings_match_independent_harfbuzz_glyphs() {
    // HarfBuzz 12.3.2, --shapers=ot --no-glyph-names --utf8-clusters
    // --bot --eot --unsafe-to-concat --show-flags. Font UPEM is 1000.
    let (font, _) = FontAtlas::load(
        &root().join("../easl-native-text/tests/fonts/amiri.ttf"),
        32.,
    )
    .unwrap();
    let disabled = [FontFeature {
        tag: u32::from_be_bytes(*b"liga"),
        value: 0,
    }];
    let plain = font.shape_span("office", 0..6, LATIN_TAG, false).unwrap();
    let separate = font
        .shape_span_with(
            "office",
            0..6,
            LATIN_TAG,
            false,
            FontShapeOptions {
                features: &disabled,
                language: "en",
            },
        )
        .unwrap();
    assert!(plain.len() < separate.len());
    for (i, (g, (id, advance))) in separate
        .iter()
        .zip([
            (5539, 497.),
            (5530, 300.),
            (5530, 300.),
            (5533, 263.),
            (5527, 413.),
            (5529, 419.),
        ])
        .enumerate()
    {
        assert_eq!((g.id, g.start, g.end), (id, i as u32, i as u32 + 1));
        assert!((g.advance - advance * 0.032).abs() < 0.00001);
    }
    let text = "۴۶۷";
    let glyphs = font
        .shape_span_with(
            text,
            0..text.len(),
            u32::from_be_bytes(*b"arab"),
            true,
            FontShapeOptions {
                features: &[],
                language: "ur",
            },
        )
        .unwrap();
    assert_eq!(
        glyphs.iter().map(|g| g.id).collect::<Vec<_>>(),
        [1407, 1408, 1409]
    );
    for (i, glyph) in glyphs.iter().enumerate() {
        assert_eq!((glyph.start, glyph.end), (i as u32 * 2, i as u32 * 2 + 2));
        assert!((glyph.advance - 585. * 0.032).abs() < 0.00001);
    }
    assert_eq!(
        font.shape_span_with(
            text,
            0..text.len(),
            u32::from_be_bytes(*b"arab"),
            true,
            FontShapeOptions {
                features: &[],
                language: "ar"
            }
        )
        .unwrap()
        .iter()
        .map(|g| g.id)
        .collect::<Vec<_>>(),
        [260, 262, 263]
    );
}

#[test]
fn variable_font_ink_metrics_and_shaping_share_the_selected_instance() {
    let data = std::fs::read(root().join("tests/fonts/shantell-sans-variable.ttf")).unwrap();
    let build = |weight, italic| {
        FontAtlas::from_bytes_with(
            data.clone(),
            32.,
            FontLoadOptions {
                face_index: 0,
                variations: &[
                    FontVariation {
                        tag: WGHT_TAG,
                        value: weight,
                    },
                    FontVariation {
                        tag: ITAL_TAG,
                        value: italic,
                    },
                ],
            },
        )
        .unwrap()
    };
    let (regular, regular_pixels) = build(400., 0.);
    let (heavy, heavy_pixels) = build(700., 1.);
    assert_ne!(regular_pixels, heavy_pixels);
    assert_ne!(regular.glyphs, heavy.glyphs);
    // Independent hb-shape: 'a' with --variations=wght=400,ital=0 and
    // wght=700,ital=1 returns glyph 1272 with advances 622 and 681 / 1000.
    for (font, advance) in [(&regular, 622.), (&heavy, 681.)] {
        let glyphs = font.shape_span("a", 0..1, LATIN_TAG, false).unwrap();
        assert_eq!(glyphs.len(), 1);
        let glyph = glyphs[0];
        assert_eq!((glyph.id, glyph.start, glyph.end), (1272, 0, 1));
        assert!((glyph.advance - advance * 0.032).abs() < 0.00001);
        // Swash retains fractional HVAR deltas; HarfRust rounds them to
        // integer font units, independently confirmed by hb-shape above.
        assert!(
            (font.glyphs[glyph.id as usize].advance - glyph.advance).abs() <= 0.5 * 0.032 + 0.00001,
            "atlas advance {}, shaped advance {}",
            font.glyphs[glyph.id as usize].advance,
            glyph.advance
        );
        let ink = font.glyphs[glyph.id as usize];
        assert!(ink.rect[2] > 0 && ink.rect[3] > 0);
    }
    // OpenType clamps the user-space position to each font's declared axis.
    let (clamped, pixels) = build(1000000., 1000000.);
    let (maximum, max_pixels) = build(800., 1.);
    assert_eq!(pixels, max_pixels);
    assert_eq!(clamped.glyphs, maximum.glyphs);
    assert_eq!(
        clamped.shape_span("a", 0..1, LATIN_TAG, false).unwrap(),
        maximum.shape_span("a", 0..1, LATIN_TAG, false).unwrap()
    );
}

#[test]
fn collection_face_index_selects_matching_shaping_and_raster_data() {
    let files = [
        "../easl-native-text/tests/fonts/amiri.ttf",
        "tests/fonts/shantell-sans-variable.ttf",
    ]
    .map(|p| std::fs::read(root().join(p)).unwrap());
    // Test-only TTC: preserve each table's bytes, changing directory offsets
    // to be collection-relative. No font files are rewritten on disk.
    let mut collection = b"ttcf\0\x01\0\0\0\0\0\x02\0\0\0\0\0\0\0\0".to_vec();
    for (i, data) in files.iter().enumerate() {
        while !collection.len().is_multiple_of(4) {
            collection.push(0);
        }
        let start = collection.len();
        collection[12 + i * 4..16 + i * 4].copy_from_slice(&(start as u32).to_be_bytes());
        collection.extend_from_slice(data);
        let count = u16::from_be_bytes([data[4], data[5]]) as usize;
        for table in 0..count {
            let at = start + 12 + table * 16 + 8;
            let offset = u32::from_be_bytes(collection[at..at + 4].try_into().unwrap());
            collection[at..at + 4].copy_from_slice(&(offset + start as u32).to_be_bytes());
        }
    }
    for (i, data) in files.into_iter().enumerate() {
        let (expected, pixels) = FontAtlas::from_bytes(data, 20.).unwrap();
        let (actual, actual_pixels) = FontAtlas::from_bytes_with(
            collection.clone(),
            20.,
            FontLoadOptions {
                face_index: i as u32,
                variations: &[],
            },
        )
        .unwrap();
        assert_eq!(actual_pixels, pixels);
        assert_eq!(actual.metrics, expected.metrics);
        assert_eq!(actual.glyphs, expected.glyphs);
        assert_eq!(
            actual.shape_span("a", 0..1, LATIN_TAG, false).unwrap(),
            expected.shape_span("a", 0..1, LATIN_TAG, false).unwrap()
        );
    }
    assert!(matches!(
        FontAtlas::from_bytes_with(
            collection,
            20.,
            FontLoadOptions {
                face_index: 2,
                variations: &[]
            }
        ),
        Err(FontError::InvalidFont)
    ));
}

#[test]
fn font_options_reject_ambiguous_unbounded_and_nonfinite_settings() {
    let data = std::fs::read(root().join("../easl-native-text/tests/fonts/amiri.ttf")).unwrap();
    let (font, _) = FontAtlas::from_bytes(data.clone(), 20.).unwrap();
    let weight = FontVariation {
        tag: WGHT_TAG,
        value: 400.,
    };
    for variations in [
        vec![weight; 2],
        vec![weight; 65],
        vec![FontVariation {
            value: f32::NAN,
            ..weight
        }],
        vec![FontVariation {
            value: f32::INFINITY,
            ..weight
        }],
        vec![FontVariation {
            value: 1000001.,
            ..weight
        }],
        vec![FontVariation { tag: 0, ..weight }],
    ] {
        assert!(matches!(
            FontAtlas::from_bytes_with(
                data.clone(),
                20.,
                FontLoadOptions {
                    face_index: 0,
                    variations: &variations
                }
            ),
            Err(FontError::InvalidSettings)
        ));
    }
    for features in [
        vec![FontFeature { tag: 0, value: 1 }],
        vec![
            FontFeature {
                tag: u32::from_be_bytes(*b"liga"),
                value: 1
            };
            2
        ],
        vec![
            FontFeature {
                tag: u32::from_be_bytes(*b"liga"),
                value: 1
            };
            65
        ],
    ] {
        assert!(matches!(
            font.shape_span_with(
                "a",
                0..1,
                LATIN_TAG,
                false,
                FontShapeOptions {
                    features: &features,
                    language: "en"
                }
            ),
            Err(FontError::InvalidSettings)
        ));
    }
    for language in [
        "en_US",
        "en\0",
        "-en",
        "en-",
        "123",
        "abcde1234",
        "en--US",
        "日本語",
        "en-aaaaaaaa-aaaaaaaa-aaaaaaaa-aaaaaaaa-aaaaaaaa-aaaaaaaa-aaaaaaaa-aaaaaaaa",
    ] {
        assert!(
            matches!(
                font.shape_span_with(
                    "a",
                    0..1,
                    LATIN_TAG,
                    false,
                    FontShapeOptions {
                        features: &[],
                        language
                    }
                ),
                Err(FontError::InvalidLanguage)
            ),
            "{language:?}"
        );
    }
    for language in ["", "und", "en-US", "sr-Latn", "x-custom"] {
        assert!(
            font.shape_span_with(
                "a",
                0..1,
                LATIN_TAG,
                false,
                FontShapeOptions {
                    features: &[],
                    language
                }
            )
            .is_ok()
        );
    }
}

#[test]
fn styled_font_specimen_prepares_real_instances_in_both_evaluators() {
    let path = root().join("examples/style_specimen.easl");
    let source = std::fs::read_to_string(&path)
        .unwrap()
        .replace("(defn main []", "(defn style-window []")
        + "\n@cpu (defn main [] (style-prepare) (print style-first) (print style-second))";
    let docs = easl::parse::load_easl_imports_with_lookup_function(
        parse_easl_without_comments(&source),
        Some(&path),
        source,
        easl::parse::ImportLimits::default(),
        |path| std::fs::read_to_string(path),
    )
    .unwrap()
    .unwrap();
    let program = self::program(docs);
    let run = |runtime| {
        run_program_entry_with_io_and_runtime_from_path(
            program.clone(),
            Some("main"),
            StringIO::new(),
            &path,
            runtime,
        )
        .unwrap()
        .0
    };
    let tree = run(CpuRuntime::TreeWalking);
    assert_eq!(tree.events.len(), 2);
    assert_eq!(tree.events, run(CpuRuntime::BytecodeVm).events);
    let mut vm = VmCpuRuntime::new_cpu_with_external(
        program,
        StringIO::new(),
        path.parent().map(PathBuf::from),
        None,
    )
    .unwrap();
    vm.run("style-prepare").unwrap();
    vm.run("style-prepare").unwrap();
    // The prepared consumer retains its language tables until explicit close.
    assert_eq!(vm.env.text_values.live_values(), 1);
    vm.run("style-close").unwrap();
    vm.run("style-close").unwrap();
    assert_eq!(vm.env.text_values.live_values(), 0);
}

#[test]
fn rejected_style_settings_keep_prior_font_and_glyphs_in_both_evaluators() {
    use easl::{
        compiler::{expression::ExpKind, functions::FunctionImplementationKind},
        interpreter::{EvaluationEnvironment, eval},
    };
    let source = r#"
      @{group 0 binding 0} (var font: (Texture2D f32))
      (var axes: [FontVariation]) (var features: [FontFeature])
      (var source: TextBuffer) (var language: TextBuffer) (var bad-language: TextBuffer) (var dead: TextBuffer)
      (var glyphs: [FontShapedGlyph]) (var ink: [FontAtlasGlyph])
      @external (var saved: [FontShapedGlyph])
      @cpu (defn setup []
        (= axes (zeroed-array 2u))
        (= (axes 0u) (FontVariation 2003265652u 700.)) (= (axes 1u) (FontVariation 1769234796u 1.))
        (= font (load-font "tests/fonts/shantell-sans-variable.ttf" 32. 0u axes))
        (= features (zeroed-array 1u)) (= (features 0u) (FontFeature 1801810542u 1u))
        (= source (make-text "a")) (= language (make-text "en"))
        (= bad-language (make-text "en_US")) (= dead (make-text "en")) (text-release dead)
        (= glyphs (shape-font-span font source 0u 1u 1818326126u false features language)))
      @cpu (defn inspect []
        (= saved glyphs) (print glyphs) (print (font-metrics font))
        (= ink (font-atlas-glyphs font)) (print (ink 1272u)))
      @cpu (defn wrong-face [] (= font (load-font "tests/fonts/shantell-sans-variable.ttf" 32. 9u axes)))
      @cpu (defn duplicate-axes []
        (= (axes 1u) (axes 0u)) (= font (load-font "tests/fonts/shantell-sans-variable.ttf" 32. 0u axes)))
      @cpu (defn oversized-axes []
        (= axes (zeroed-array 65u)) (= font (load-font "tests/fonts/shantell-sans-variable.ttf" 32. 0u axes)))
      @cpu (defn invalid-language []
        (= glyphs (shape-font-span font source 0u 1u 1818326126u false features bad-language)))
      @cpu (defn dead-language []
        (= glyphs (shape-font-span font source 0u 1u 1818326126u false features dead)))
      @cpu (defn oversized-features []
        (= features (zeroed-array 65u))
        (= glyphs (shape-font-span font source 0u 1u 1818326126u false features language)))
      @cpu (defn close [] (text-release source) (text-release language) (text-release bad-language))
    "#;
    let program = self::program(EaslMultiDocument::from_singular_document(
        parse_easl_without_comments(source),
        "font-settings.easl".into(),
        source.into(),
    ));
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
        let entries = program.cpu_entry_points();
        let f = entries
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
    vm.run("setup").unwrap();
    tree_run("setup", &mut tree).unwrap();
    vm.run("inspect").unwrap();
    tree_run("inspect", &mut tree).unwrap();
    let saved = external.read_external_var_raw("saved").unwrap();
    assert_eq!(&saved[..3], &[1272, 0, 1]);
    assert!((f32::from_bits(saved[5]) - 681. * 0.032).abs() < 0.00001);
    let initial = vm.env.io.events.clone();
    for entry in [
        "wrong-face",
        "duplicate-axes",
        "oversized-axes",
        "invalid-language",
        "dead-language",
        "oversized-features",
    ] {
        assert!(vm.run(entry).is_err(), "{entry}");
        assert!(tree_run(entry, &mut tree).is_err(), "{entry}");
        vm.run("inspect").unwrap();
        tree_run("inspect", &mut tree).unwrap();
        assert_eq!(external.read_external_var_raw("saved").unwrap(), saved);
        assert_eq!(vm.env.io.events, tree.io.events);
        assert_eq!(
            &vm.env.io.events[vm.env.io.events.len() - 3..],
            initial.as_slice()
        );
    }
    vm.run("close").unwrap();
    tree_run("close", &mut tree).unwrap();
    assert_eq!(vm.env.text_values.live_values(), 0);
    assert_eq!(tree.text_values.live_values(), 0);
}

#[test]
fn easl_span_features_and_language_reach_shaping_in_both_evaluators() {
    let source = r#"
      @{group 0 binding 0} (var font: (Texture2D f32))
      (var features: [FontFeature]) (var glyphs: [FontShapedGlyph]) (var text: TextBuffer)
      (var language: TextBuffer) (var check: u32) (var axes: [FontVariation])
      (defn expect [ok: bool] (+= check 1u) (when (not ok) (print check)))
      @cpu (defn main []
        (= axes (zeroed-array 0u))
        (= font (load-font "../easl-native-text/tests/fonts/amiri.ttf" 32. 0u axes))
        (= features (zeroed-array 1u)) (= (features 0u) (FontFeature 1818847073u 0u))
        (= language (make-text "en")) (= text (make-text "office"))
        (= glyphs (shape-font-span font text 0u 6u 1818326126u false features language))
        (expect (== (array-length glyphs) 6u))
        (expect (== (.id (glyphs 1u)) 5530u)) (expect (== (.id (glyphs 2u)) 5530u))
        (expect (== (.id (glyphs 3u)) 5533u))
        (expect (== (.start (glyphs 3u)) 3u)) (expect (== (.end (glyphs 3u)) 4u))
        (= (features 0u) (FontFeature 1818847073u 1u))
        (= glyphs (shape-font-span font text 0u 6u 1818326126u false features language))
        (expect (< (array-length glyphs) 6u))
        (text-release text) (text-release language)
        (= language (make-text "ur")) (= text (make-text "۴۶۷"))
        (= features (zeroed-array 0u))
        (= glyphs (shape-font-span font text 0u 6u 1634885986u true features language))
        (expect (== (array-length glyphs) 3u))
        (expect (== (.id (glyphs 0u)) 1407u)) (expect (== (.id (glyphs 1u)) 1408u))
        (expect (== (.id (glyphs 2u)) 1409u))
        (for [i 3u] (expect (== (.start (glyphs i)) (* i 2u)))
          (expect (== (.end (glyphs i)) (+ (* i 2u) 2u))))
        (text-release text) (text-release language) (print "done"))
    "#;
    let program = self::program(EaslMultiDocument::from_singular_document(
        parse_easl_without_comments(source),
        "span-settings.easl".into(),
        source.into(),
    ));
    for runtime in [CpuRuntime::TreeWalking, CpuRuntime::BytecodeVm] {
        let io = run_program_entry_with_io_and_runtime_from_path(
            program.clone(),
            Some("main"),
            StringIO::new(),
            &root().join("span-settings.easl"),
            runtime,
        )
        .unwrap()
        .0;
        assert_eq!(
            io.events,
            [easl::interpreter::IOEvent::Print("done".into())],
            "{runtime:?}"
        );
    }
}
