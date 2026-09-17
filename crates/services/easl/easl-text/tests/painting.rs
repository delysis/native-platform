use easl_native_text::{
    Alignment, Error as NativeError, GlyphPaint, PaintGlyph, RasterSurface, StyledSpan, TextStyle,
    TextSystem, WhiteSpace, parley::PositionedLayoutItem, vello_cpu::peniko::Blob,
};
use easl_text::GlyphPainting;
use std::{path::Path, sync::Arc};

fn system() -> TextSystem {
    let mut system = TextSystem::new();
    for name in ["amiri.ttf", "shantell-sans-regular.ttf"] {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../easl-native-text/tests/fonts")
            .join(name);
        system
            .fonts
            .collection
            .register_fonts(Blob::new(Arc::new(std::fs::read(path).unwrap())), None);
    }
    system
}

#[test]
fn actual_shaped_runs_keep_positions_pixels_clipping_and_repaint_cache() {
    let mut system = system();
    let body = TextStyle {
        family: "Amiri".into(),
        size: 24.,
        line_height: 39.,
        ..Default::default()
    };
    let text = "A café é — العربية\nA bold accent and a link.";
    let spans = [
        StyledSpan {
            range: 0..1,
            style: TextStyle {
                size: 38.,
                line_height: 50.,
                ..body.clone()
            },
        },
        StyledSpan {
            range: text.find("accent").unwrap()..text.len(),
            style: TextStyle {
                family: "Shantell Sans".into(),
                underline: true,
                strike: true,
                letter_spacing: 0.25,
                color: [151, 39, 26, 255],
                ..body.clone()
            },
        },
    ];
    let mut prepared = system
        .prepare(text, &body, WhiteSpace::Preserve, &spans, &[])
        .unwrap();
    let mut painting = GlyphPainting::new().unwrap();
    for width in [390., 173.] {
        prepared.reflow(width, Alignment::Start).unwrap();
        let mut glyphs = 0;
        for line in prepared.layout().lines() {
            for item in line.items() {
                if let PositionedLayoutItem::GlyphRun(run) = item {
                    let paint = painting.prepare(&run).unwrap();
                    let expected: Vec<_> = run
                        .positioned_glyphs()
                        .map(|g| PaintGlyph {
                            id: g.id,
                            point: [g.x, g.y],
                        })
                        .collect();
                    assert_eq!(paint.glyphs, expected);
                    glyphs += paint.glyphs.len();
                    for d in &paint.decorations {
                        assert_eq!(d.color, run.style().brush);
                        assert_eq!(d.bounds[0], run.offset());
                        assert_eq!(d.bounds[2], run.advance());
                    }
                }
            }
        }
        assert!(glyphs > 20);
        let count = painting.preparations;
        for scale in [1., 1.5, 2.] {
            let mut native = RasterSurface::new(800, 600, scale).unwrap();
            let mut easl = RasterSurface::new(800, 600, scale).unwrap();
            for surface in [&mut native, &mut easl] {
                surface.begin(800, 600, scale).unwrap();
                surface
                    .rect([0., 0., 800., 600.], [247, 245, 241, 255])
                    .unwrap();
            }
            let origin = [12.25, -6.75];
            let clip = [23., 5., 315., 220.];
            native.text(prepared.layout(), origin, clip).unwrap();
            painting
                .paint(&mut easl, prepared.layout(), origin, clip)
                .unwrap();
            assert_eq!(
                easl.finish(),
                native.finish(),
                "width {width}, scale {scale}"
            );
        }
        assert_eq!(
            painting.preparations, count,
            "paint origin/display scale should reuse geometry"
        );
        assert!(painting.cache_hits > 0);
    }
    assert_eq!(prepared.source(), text);
}

#[test]
fn long_visual_run_preserves_accumulation_across_transport_batches() {
    let mut system = system();
    let source = "ái ".repeat(1300);
    let style = TextStyle {
        family: "Amiri".into(),
        size: 12.,
        line_height: 20.,
        wrap: false,
        ..Default::default()
    };
    let mut prepared = system
        .prepare(&source, &style, WhiteSpace::Preserve, &[], &[])
        .unwrap();
    prepared.reflow(400., Alignment::Start).unwrap();
    let mut painting = GlyphPainting::new().unwrap();
    let mut largest = 0;
    for line in prepared.layout().lines() {
        for item in line.items() {
            if let PositionedLayoutItem::GlyphRun(run) = item {
                let placed = painting.prepare(&run).unwrap();
                largest = largest.max(placed.glyphs.len());
                assert_eq!(
                    placed.glyphs,
                    run.positioned_glyphs()
                        .map(|g| PaintGlyph {
                            id: g.id,
                            point: [g.x, g.y]
                        })
                        .collect::<Vec<_>>()
                );
            }
        }
    }
    assert!(largest > 2048);
}

#[test]
fn overlapping_tracking_keeps_signed_advances_and_decoration_widths() {
    let mut system = system();
    let mut painting = GlyphPainting::new().unwrap();
    for underline in [false, true] {
        let style = TextStyle {
            family: "Amiri".into(),
            size: 12.,
            line_height: 20.,
            letter_spacing: -20.,
            underline,
            wrap: false,
            ..Default::default()
        };
        let mut prepared = system
            .prepare("aaa", &style, WhiteSpace::Preserve, &[], &[])
            .unwrap();
        prepared.reflow(300., Alignment::Start).unwrap();
        let mut negative = false;
        for line in prepared.layout().lines() {
            for item in line.items() {
                if let PositionedLayoutItem::GlyphRun(run) = item {
                    negative |= run.advance() < 0.;
                    let output = painting.prepare(&run).unwrap();
                    if underline {
                        assert_eq!(output.decorations[0].bounds[2], run.advance());
                    }
                }
            }
        }
        assert!(negative);
        let mut native = RasterSurface::new(300, 100, 1.).unwrap();
        let mut easl = RasterSurface::new(300, 100, 1.).unwrap();
        native
            .text(prepared.layout(), [100., 10.], [0., 0., 300., 100.])
            .unwrap();
        painting
            .paint(
                &mut easl,
                prepared.layout(),
                [100., 10.],
                [0., 0., 300., 100.],
            )
            .unwrap();
        assert_eq!(easl.finish(), native.finish());
    }
}

#[test]
fn late_policy_failure_does_not_change_the_native_surface_or_clip_state() {
    let mut system = system();
    let style = TextStyle {
        family: "Amiri".into(),
        ..Default::default()
    };
    let mut prepared = system
        .prepare(
            "one two",
            &style,
            WhiteSpace::Preserve,
            &[StyledSpan {
                range: 4..7,
                style: TextStyle {
                    size: 30.,
                    ..style.clone()
                },
            }],
            &[],
        )
        .unwrap();
    prepared.reflow(400., Alignment::Start).unwrap();
    let mut surface = RasterSurface::new(400, 200, 1.).unwrap();
    surface
        .rect([0., 0., 400., 200.], [19, 25, 30, 255])
        .unwrap();
    let before = surface.finish().to_vec();
    for invalid_output in [false, true] {
        let mut visited = 0;
        let result = surface.text_with(prepared.layout(), [0., 0.], [5., 5., 300., 180.], |run| {
            visited += 1;
            if visited == 2 && !invalid_output {
                return Err(NativeError::InvalidGeometry);
            }
            let mut glyphs: Vec<_> = run
                .positioned_glyphs()
                .map(|g| PaintGlyph {
                    id: g.id,
                    point: [g.x, g.y],
                })
                .collect();
            if visited == 2 {
                glyphs[0].point[0] = f32::NAN;
            }
            Ok(Arc::new(GlyphPaint {
                glyphs,
                decorations: Vec::new(),
            }))
        });
        assert!(matches!(result, Err(NativeError::InvalidGeometry)));
        assert_eq!(visited, 2);
        assert_eq!(surface.finish(), before);
    }
    let mut painting = GlyphPainting::new().unwrap();
    painting
        .paint(
            &mut surface,
            prepared.layout(),
            [0., 0.],
            [0., 0., 400., 200.],
        )
        .unwrap();
    assert_ne!(surface.finish(), before);
}

#[test]
fn paint_policy_validates_before_writing_and_preserves_explicit_decoration_values() {
    use easl::{
        CompilerTarget,
        compiler::{builtins::built_in_macros, program::Program},
        interpreter::{
            CpuRuntime, IOEvent, StringIO, run_program_entry_with_io_and_runtime_from_path,
        },
        parse::{
            ImportLimits, load_easl_imports_with_lookup_function, parse_easl_without_comments,
        },
    };
    let source = r#"
(import "../library/painting.easl")
(var p-input: [TextPaintGlyph]) (var p-output: [TextPlacedGlyph])
(var p-decorations: [TextDecoration]) (var p-writes: u32) (var p-assertions: u32)
(var p-bad-color: bool)
(defn p-check [ok: bool] (+= p-assertions 1u) (when (not ok) (print p-assertions)))
(defn p-color [i: u32]: vec4u
  (if (== i 0u) (vec4u 1u 2u 3u 255u)
    (if p-bad-color (vec4u 256u) (vec4u 4u 5u 6u 255u))))
(defn p-place [capacity: u32 budget: u32]: TextPaintResult
  (text-position-glyphs 3u (fn [i] (p-input i)) (vec2f 10. 30.) capacity
    (fn [i g] (+= p-writes 1u) (= (p-output i) g)) budget))
(defn p-decorate [flags: u32 capacity: u32]: TextDecorationResult
  (text-paint-decorations (vec2f 10. 40.) 50. (vec4f -2. 1.5 5. 2.)
    (vec4f -4. 0. 7. 0.) flags p-color capacity
    (fn [i d] (+= p-writes 1u) (= (p-decorations i) d))))
@cpu (defn main []
  (= p-input (zeroed-array 3u)) (= p-output (zeroed-array 3u))
  (= p-decorations (zeroed-array 2u))
  (= (p-input 0u) (TextPaintGlyph 4u (vec2f -1. 2.) 10.))
  (= (p-input 1u) (TextPaintGlyph 5u (vec2f 3. -4.) 0.))
  (= (p-input 2u) (TextPaintGlyph 6u (vec2f 0.) -2.))
  (let [result (p-place 3u 12u)]
    (p-check (== result.status 0u)) (p-check (== result.count 3u))
    (p-check (== result.pen 18.)))
  (p-check (all (== (.point (p-output 0u)) (vec2f 9. 32.))))
  (p-check (all (== (.point (p-output 1u)) (vec2f 23. 26.))))
  (p-check (all (== (.point (p-output 2u)) (vec2f 20. 30.))))
  (= p-writes 0u)
  (p-check (== (.status (p-place 2u 12u)) 2u))
  (p-check (== (.status (p-place 3u 11u)) 2u))
  (= (p-input 0u) (TextPaintGlyph 4u (vec2f 99.) 10.))
  (= (p-input 2u) (TextPaintGlyph 65536u (vec2f 0.) 0.))
  (p-check (== (.status (p-place 3u 12u)) 1u))
  (= (p-input 2u) (TextPaintGlyph 6u (vec2f (/ 0. 0.) 0.) 0.))
  (p-check (== (.status (p-place 3u 12u)) 1u))
  (p-check (== p-writes 0u))
  (p-check (all (== (.point (p-output 0u)) (vec2f 9. 32.))))
  (= p-bad-color true)
  (p-check (== (.status (p-decorate 27u 2u)) 1u))
  (p-check (== p-writes 0u)) (= p-bad-color false)
  (p-check (== (.status (p-decorate 27u 1u)) 2u))
  (p-check (== (.status (p-decorate 2u 2u)) 1u))
  (p-check (== p-writes 0u))
  (let [result (p-decorate 27u 2u)]
    (p-check (== result.status 0u)) (p-check (== result.count 2u)))
  (p-check (all (== (.bounds (p-decorations 0u)) (vec4f 10. 44. 50. 1.5))))
  (p-check (all (== (.bounds (p-decorations 1u)) (vec4f 10. 33. 50. 2.))))
  (p-check (all (== (.color (p-decorations 1u)) (vec4u 4u 5u 6u 255u))))
  ; Explicit zero thickness must not become a font/default fallback stroke.
  (p-check (== (.status (p-decorate 5u 2u)) 0u))
  (p-check (all (== (.bounds (p-decorations 0u)) (vec4f 10. 42. 50. 0.))))
  (print "done"))
"#;
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/paint-policy.easl");
    let parsed = parse_easl_without_comments(source);
    assert!(
        parsed.parsing_failures.is_empty(),
        "{:?}",
        parsed.parsing_failures
    );
    let docs = load_easl_imports_with_lookup_function(
        parsed,
        Some(&path),
        source.into(),
        ImportLimits::default(),
        |p| std::fs::read_to_string(p),
    )
    .unwrap()
    .unwrap();
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
