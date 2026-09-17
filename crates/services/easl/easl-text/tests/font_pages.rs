use easl::font::FontAtlas;
use std::path::Path;

fn assert_page_ink_matches(whole: &FontAtlas, all_pixels: &[u8], page: &FontAtlas, pixels: &[u8]) {
    assert_eq!(page.metrics, whole.metrics);
    assert_eq!(page.decorations, whole.decorations);
    assert_eq!(page.glyphs.len(), whole.glyphs.len());
    for &id in &page.rasterized_glyphs {
        let a = &whole.glyphs[id as usize];
        let b = &page.glyphs[id as usize];
        assert_eq!(a.offset, b.offset);
        assert_eq!(a.advance, b.advance);
        assert_eq!(&a.rect[2..], &b.rect[2..]);
        for row in 0..a.rect[3] {
            let ai = ((a.rect[1] + row) * whole.width + a.rect[0]) as usize * 4;
            let bi = ((b.rect[1] + row) * page.width + b.rect[0]) as usize * 4;
            let len = a.rect[2] as usize * 4;
            assert_eq!(&all_pixels[ai..ai + len], &pixels[bi..bi + len]);
        }
    }
}

#[test]
fn requested_page_retains_variable_instance_at_new_resolution() {
    use easl::font::{FontLoadOptions, FontVariation};
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fonts/shantell-sans-variable.ttf");
    let axes = [
        FontVariation {
            tag: u32::from_be_bytes(*b"wght"),
            value: 700.,
        },
        FontVariation {
            tag: u32::from_be_bytes(*b"ital"),
            value: 1.,
        },
    ];
    let options = FontLoadOptions {
        face_index: 0,
        variations: &axes,
    };
    let font = FontAtlas::open_with(&path, 32., options).unwrap();
    let source = "Hello, café";
    let original = font
        .shape_span(source, 0..source.len(), u32::from_be_bytes(*b"latn"), false)
        .unwrap();
    let mut ids: Vec<_> = original.iter().map(|g| g.id).collect();
    ids.push(0); // The missing-glyph image is also an explicit resident.
    let (page, pixels) = font.rasterize(&ids, 64.).unwrap();
    let (whole, all_pixels) = FontAtlas::load_with(&path, 64., options).unwrap();
    assert_page_ink_matches(&whole, &all_pixels, &page, &pixels);
    assert_eq!(page.rasterized_glyphs[0], 0);
    let page_shape = page
        .shape_span(source, 0..source.len(), u32::from_be_bytes(*b"latn"), false)
        .unwrap();
    assert_eq!(
        page_shape,
        whole
            .shape_span(source, 0..source.len(), u32::from_be_bytes(*b"latn"), false)
            .unwrap()
    );
    for (a, b) in original.iter().zip(page_shape) {
        assert_eq!(
            (a.id, a.start, a.end, a.flags),
            (b.id, b.start, b.end, b.flags)
        );
        assert_eq!(a.advance * 2., b.advance);
    }
}

#[test]
fn short_heading_at_large_resolution_has_a_bounded_glyph_atlas() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../easl-native-text/tests/fonts/amiri.ttf");
    let started = std::time::Instant::now();
    let font = FontAtlas::open(&path, 256.).unwrap();
    assert_eq!((font.width, font.height), (1, 1));
    assert!(font.rasterized_glyphs.is_empty());
    let shaped = font
        .shape_span("Heading", 0..7, u32::from_be_bytes(*b"latn"), false)
        .unwrap();
    let ids: Vec<_> = shaped.iter().map(|g| g.id).collect();
    let (page, pixels) = font.rasterize(&ids, 256.).unwrap();
    eprintln!(
        "open and selected heading page: {:?}; {} resident glyphs, {} pixel bytes",
        started.elapsed(),
        page.rasterized_glyphs.len(),
        pixels.len()
    );
    assert!(page.rasterized_glyphs.len() <= 7);
    assert!(pixels.len() < 4096 * 1024 * 4);
    assert!(
        shaped
            .iter()
            .all(|g| page.rasterized_glyphs.contains(&g.id))
    );
    assert_eq!(
        font.shape_span("Heading", 0..7, u32::from_be_bytes(*b"latn"), false)
            .unwrap(),
        shaped
    );
}

#[test]
fn pages_match_full_atlas_ink_and_reject_late_invalid_requests_atomically() {
    use easl::font::FontError;
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../easl-native-text/tests/fonts/amiri.ttf");
    let (whole, all_pixels) = FontAtlas::load(&path, 32.).unwrap();
    let source = "Office café e\u{301}";
    let shaped = whole
        .shape_span(source, 0..source.len(), u32::from_be_bytes(*b"latn"), false)
        .unwrap();
    let ids: Vec<_> = shaped.iter().map(|g| g.id).collect();
    let (page, pixels) = whole.rasterize(&ids, 32.).unwrap();
    assert!(pixels.len() < all_pixels.len());
    assert_page_ink_matches(&whole, &all_pixels, &page, &pixels);
    assert_eq!(
        page.shape_span(source, 0..source.len(), u32::from_be_bytes(*b"latn"), false)
            .unwrap(),
        shaped
    );
    let mut invalid = ids.clone();
    invalid.push(u32::MAX);
    assert!(matches!(
        page.rasterize(&invalid, 32.),
        Err(FontError::InvalidGlyphs)
    ));
    assert!(matches!(
        page.rasterize(&vec![0; easl::font::MAX_RUN_BYTES + 1], 32.),
        Err(FontError::InvalidGlyphs)
    ));
    assert!(matches!(
        page.rasterize(&ids, f32::NAN),
        Err(FontError::InvalidResolution)
    ));
    let all_ids: Vec<_> = (0..page.glyphs.len() as u32).collect();
    assert!(matches!(
        page.rasterize(&all_ids, 256.),
        Err(FontError::Limit)
    ));
    let (again, again_pixels) = page.rasterize(&ids, 32.).unwrap();
    assert_eq!(again.glyphs, page.glyphs);
    assert_eq!(again_pixels, pixels);
    let (empty, blank) = page.rasterize(&[], 64.).unwrap();
    assert_eq!((empty.width, empty.height, blank), (1, 1, vec![0; 4]));
    assert!(empty.rasterized_glyphs.is_empty());
    let larger = empty
        .shape_span(source, 0..source.len(), u32::from_be_bytes(*b"latn"), false)
        .unwrap();
    for (a, b) in shaped.iter().zip(&larger) {
        assert_eq!(
            (a.id, a.start, a.end, a.flags),
            (b.id, b.start, b.end, b.flags)
        );
        assert_eq!(a.advance * 2., b.advance);
    }
    // Public metric tables are not authority for the immutable font's ID range.
    let mut altered = FontAtlas::open(&path, 32.).unwrap();
    let invalid_id = altered.glyphs.len() as u32;
    altered.glyphs.push(Default::default());
    assert!(matches!(
        altered.rasterize(&[invalid_id], 32.),
        Err(FontError::InvalidGlyphs)
    ));
}

#[test]
fn easl_pages_retain_instances_and_recover_from_invalid_requests_in_both_evaluators() {
    use easl::{
        CompilerTarget,
        compiler::{
            builtins::built_in_macros, expression::ExpKind, functions::FunctionImplementationKind,
            program::Program,
        },
        interpreter::{EvaluationEnvironment, StringIO, VmCpuRuntime, eval},
        parse::{EaslMultiDocument, parse_easl_without_comments},
    };
    let source = r#"
@{group 0 binding 0} (var face: (Texture2D f32))
@{group 0 binding 1} (var page: (Texture2D f32))
@{group 0 binding 2} (var retained: (Texture2D f32))
(var axes: [FontVariation]) (var ids: [u32]) (var bad: [u32]) (var resident: [u32])
(var text: TextBuffer) (var glyphs: [FontShapedGlyph]) (var ink: [FontAtlasGlyph])
@cpu (defn setup []
  (= axes (zeroed-array 2u))
  (= (axes 0u) (FontVariation 2003265652u 700.)) (= (axes 1u) (FontVariation 1769234796u 1.))
  (= face (open-font "tests/fonts/shantell-sans-variable.ttf" 32. 0u axes))
  (= text (make-text "Hi"))
  (= glyphs (shape-font-span face text 0u 2u 1818326126u false))
  (= ids (zeroed-array 3u))
  (= (ids 0u) (.id (glyphs 0u))) (= (ids 1u) (.id (glyphs 1u))) (= (ids 2u) (ids 0u))
  (= page (rasterize-font face ids 64.)))
@cpu (defn inspect []
  (= resident (font-rasterized-glyphs page)) (= ink (font-atlas-glyphs page))
  (print resident) (print (font-metrics page)) (print (ink (ids 0u)))
  (= glyphs (shape-font-span page text 0u 2u 1818326126u false)) (print glyphs))
@cpu (defn invalid-id []
  (= bad (zeroed-array 2u)) (= (bad 0u) (ids 0u)) (= (bad 1u) 4294967295u)
  (= page (rasterize-font face bad)))
@cpu (defn oversized [] (= bad (zeroed-array 1048577u)) (= page (rasterize-font face bad)))
@cpu (defn invalid-resolution [] (= page (rasterize-font page ids (/ 0. 0.))))
@cpu (defn invalid-face [] (= page (open-font "tests/fonts/shantell-sans-variable.ttf" 32. 9u axes)))
@cpu (defn overwrite [] (set-render-target page))
@cpu (defn rebuild [] (= page (rasterize-font page ids)))
@cpu (defn retain [] (= retained page) (= retained retained))
@cpu (defn replace [] (= bad (zeroed-array 0u)) (= page (rasterize-font page bad 16.)))
@cpu (defn restore [] (= page retained))
@cpu (defn close [] (text-release text))
"#;
    let docs = EaslMultiDocument::from_singular_document(
        parse_easl_without_comments(source),
        "font-pages.easl".into(),
        source.into(),
    );
    let (mut program, errors) = Program::from_easl_documents(&docs, built_in_macros());
    assert!(errors.is_empty(), "{errors:?}");
    assert!(
        program
            .validate_raw_program(CompilerTarget::WGSL)
            .is_empty()
    );
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf();
    let mut vm = VmCpuRuntime::new_cpu_with_external(
        program.clone(),
        StringIO::new(),
        Some(root.clone()),
        None,
    )
    .unwrap();
    let mut tree =
        EvaluationEnvironment::from_program(program.clone(), StringIO::new(), Some(root)).unwrap();
    let tree_run = |name: &str, tree: &mut EvaluationEnvironment<StringIO>| {
        let entries = program.cpu_entry_points();
        let function = entries
            .iter()
            .find(|f| &*f.read().unwrap().name == name)
            .unwrap()
            .read()
            .unwrap();
        let FunctionImplementationKind::Composite(function) = &function.implementation else {
            panic!("expected body")
        };
        let function = function.read().unwrap();
        let ExpKind::Function(_, body) = &function.expression.kind else {
            panic!("expected function")
        };
        eval(*body.clone(), tree)
    };
    vm.run("setup").unwrap();
    tree_run("setup", &mut tree).unwrap();
    vm.run("inspect").unwrap();
    tree_run("inspect", &mut tree).unwrap();
    assert_eq!(vm.env.io.events, tree.io.events);
    let original = vm.env.io.events.clone();
    for name in [
        "invalid-id",
        "oversized",
        "invalid-resolution",
        "invalid-face",
        "overwrite",
    ] {
        assert!(vm.run(name).is_err(), "{name}");
        assert!(tree_run(name, &mut tree).is_err(), "{name}");
        vm.run("inspect").unwrap();
        tree_run("inspect", &mut tree).unwrap();
        assert_eq!(vm.env.io.events, tree.io.events);
        assert_eq!(
            &vm.env.io.events[vm.env.io.events.len() - 4..],
            original.as_slice()
        );
    }
    vm.run("rebuild").unwrap();
    tree_run("rebuild", &mut tree).unwrap();
    vm.run("inspect").unwrap();
    tree_run("inspect", &mut tree).unwrap();
    assert_eq!(vm.env.io.events, tree.io.events);
    assert_eq!(
        &vm.env.io.events[vm.env.io.events.len() - 4..],
        original.as_slice()
    );
    for name in ["retain", "replace", "restore", "inspect"] {
        vm.run(name).unwrap();
        tree_run(name, &mut tree).unwrap();
    }
    assert_eq!(vm.env.io.events, tree.io.events);
    assert_eq!(
        &vm.env.io.events[vm.env.io.events.len() - 4..],
        original.as_slice()
    );
    vm.run("close").unwrap();
    tree_run("close", &mut tree).unwrap();
    assert_eq!(vm.env.text_values.live_values(), 0);
    assert_eq!(tree.text_values.live_values(), 0);
}

#[test]
fn unbound_texture_values_stage_pages_without_shader_bindings() {
    use easl::{
        CompilerTarget,
        compiler::{builtins::built_in_macros, program::Program},
        interpreter::{
            CpuRuntime, IOEvent, StringIO, derive_gpu_interface,
            run_program_entry_with_io_and_runtime_from_path,
        },
        parse::{EaslMultiDocument, parse_easl_without_comments},
    };
    let compile = |source: &str| {
        let docs = EaslMultiDocument::from_singular_document(
            parse_easl_without_comments(source),
            "cpu-texture.easl".into(),
            source.into(),
        );
        let (mut program, errors) = Program::from_easl_documents(&docs, built_in_macros());
        assert!(errors.is_empty(), "{errors:?}");
        let errors = program.validate_raw_program(CompilerTarget::WGSL);
        assert!(errors.is_empty(), "{errors:?}");
        program
    };
    let source = r#"
(var stagedpage: (Texture2D f32))
@{address handle} (var savedpage: (Texture2D f32))
@{group 0 binding 0} (var displayed: (Texture2D f32))
(var ids: [u32]) (var residents: [u32])
@cpu (defn main []
  (= stagedpage (open-font "../easl-native-text/tests/fonts/amiri.ttf" 32.))
  (= ids (into-dynamic-array [0u]))
  (= savedpage (rasterize-font stagedpage ids))
  (= displayed savedpage)
  (= stagedpage (blank-texture 1u 1u))
  (= residents (font-rasterized-glyphs displayed))
  (print residents) (print (texture-dimensions stagedpage)))
"#;
    let program = compile(source);
    let bindings = derive_gpu_interface(&program).binding_infos();
    assert_eq!(bindings.len(), 1);
    assert_eq!(&*bindings[0].name, "displayed");
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("cpu-texture.easl");
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
        assert_eq!(
            io.events,
            [
                IOEvent::Print("[0u]".into()),
                IOEvent::Print("(vec2u 1u 1u)".into())
            ]
        );
        for source in [
            "(var pending: (Texture2D f32)) @cpu (defn main [] (= pending (blank-texture 1u 1u)) (set-render-target pending))",
            "(var pending: (Texture2D f32)) @{group 0 binding 0} (var bound: (Texture2D f32)) @cpu (defn main [] (= bound (blank-texture 1u 1u)) (= pending bound) (set-render-target pending))",
        ] {
            let result = run_program_entry_with_io_and_runtime_from_path(
                compile(source),
                Some("main"),
                StringIO::new(),
                &path,
                runtime,
            );
            let Err(error) = result else {
                panic!("unbound render target accepted")
            };
            assert!(format!("{error:?}").contains("requires a GPU-bound texture"));
        }
    }
}

#[test]
fn unbound_textures_cannot_escape_into_shader_entry_points() {
    use easl::{
        CompilerTarget,
        compiler::{builtins::built_in_macros, program::Program},
        parse::{EaslMultiDocument, parse_easl_without_comments},
    };
    for expression in ["(texture-dimensions stagedpage)", "(size)"] {
        let source = format!(
            "(var stagedpage: (Texture2D f32)) (defn size []: vec2u (texture-dimensions stagedpage)) @vertex (defn vertex []: @{{builtin position}} vec4f (vec4f (vec2f {expression}) 0. 1.))"
        );
        let docs = EaslMultiDocument::from_singular_document(
            parse_easl_without_comments(&source),
            "cpu-texture.easl".into(),
            source,
        );
        let (mut program, errors) = Program::from_easl_documents(&docs, built_in_macros());
        assert!(errors.is_empty(), "{errors:?}");
        let errors = program.validate_raw_program(CompilerTarget::WGSL);
        assert!(
            format!("{errors:?}").contains("unbound CPU resource stagedpage"),
            "{errors:?}"
        );
    }
}
