use easl_native_text::*;
use std::sync::Arc;
use unicode_segmentation::UnicodeSegmentation;
fn system() -> TextSystem {
    let mut system = TextSystem::new();
    for data in [
        include_bytes!("fonts/shantell-sans-regular.ttf").as_slice(),
        include_bytes!("fonts/amiri.ttf").as_slice(),
    ] {
        let families = system
            .fonts
            .collection
            .register_fonts(vello_cpu::peniko::Blob::new(Arc::new(data)), None);
        assert!(!families.is_empty());
    }
    system
}
fn style() -> TextStyle {
    TextStyle {
        family: "Shantell Sans, Amiri, sans-serif".into(),
        size: 20.,
        line_height: 30.,
        ..Default::default()
    }
}
#[test]
fn prepared_reflow_streaming_ranges_and_source_are_stable() {
    let mut system = system();
    let text = "A quiet sentence, with café and Arabic العربية.\nA second paragraph with exact source boundaries.\n";
    let mut p = system
        .prepare(text, &style(), WhiteSpace::Preserve, &[], &[])
        .unwrap();
    let preparations = system.preparations;
    for width in [0., 1., 19.5, 80., 140., 300., 500., 1000.] {
        let stats = p.reflow(width, Alignment::Start).unwrap();
        let lines = p.lines().collect::<Vec<_>>();
        assert_eq!(stats.line_count, lines.len());
        let mut cursor = 0;
        for line in &lines {
            assert_eq!(line.source.start, cursor);
            assert!(text.is_char_boundary(line.source.end));
            cursor = line.source.end;
            assert!(line.width.is_finite());
            assert!(p.materialize(line).is_ok());
        }
        assert_eq!(cursor, text.len());
        let again = p.reflow(width, Alignment::Start).unwrap();
        assert_eq!(stats.line_count, again.line_count);
        assert_eq!(stats.width, again.width);
    }
    assert_eq!(p.source(), text);
    assert_eq!(system.preparations, preparations);
}
#[test]
fn emergency_wrapping_never_splits_extended_graphemes() {
    let mut system = system();
    for text in [
        "e\u{301}",
        "👩🏽‍🚀",
        "🇯🇵",
        "क्‍ष",
        "a\u{1ab0}\u{1ab1}",
        "🤦🏽‍♀️",
        "abc👨‍👩‍👧‍👦def",
    ] {
        let mut p = system
            .prepare(text, &style(), WhiteSpace::Preserve, &[], &[])
            .unwrap();
        p.reflow(1., Alignment::Start).unwrap();
        let boundaries = text
            .grapheme_indices(true)
            .map(|(i, _)| i)
            .chain([text.len()])
            .collect::<Vec<_>>();
        for line in p.lines() {
            assert!(
                boundaries.contains(&line.source.start),
                "{text:?}: {line:?}"
            );
            assert!(boundaries.contains(&line.source.end), "{text:?}: {line:?}");
        }
    }
}
#[test]
fn mixed_styles_inline_boxes_and_variable_width_flow_share_geometry() {
    let mut system = system();
    let text = "A finely drawn paragraph can flow around an illustration, preserve its emphasis, and keep a native chip in the line.";
    let bold = StyledSpan {
        range: 2..14,
        style: TextStyle {
            weight: 700.,
            color: [150, 40, 40, 255],
            ..style()
        },
    };
    let boxes = [InlineBox {
        id: 42,
        index: 25,
        width: 65.,
        height: 35.,
    }];
    let mut p = system
        .prepare(text, &style(), WhiteSpace::Preserve, &[bold], &boxes)
        .unwrap();
    p.flow(500., Alignment::Start, |i, y| LineBox {
        x: if i < 3 { 150. } else { 0. },
        y,
        width: if i < 3 { 350. } else { 500. },
    })
    .unwrap();
    assert!(p.lines().take(3).all(|l| l.x >= 150.));
    let found = p.layout().lines().flat_map(|l| l.items()).any(
        |i| matches!(i,parley::PositionedLayoutItem::InlineBox(b) if b.id==42 && b.width==65.),
    );
    assert!(found);
    assert!(p.layout().lines().flat_map(|l|l.items()).any(|i|matches!(i,parley::PositionedLayoutItem::GlyphRun(g) if g.style().brush==[150,40,40,255])));
}
#[test]
fn normalization_keeps_nbsp_and_maps_original_bytes() {
    let mut system = system();
    let text = "  one\t\n two\u{a0}three  ";
    let mut p = system
        .prepare(text, &style(), WhiteSpace::Collapse, &[], &[])
        .unwrap();
    assert_eq!(p.normalized_text(), "one two\u{a0}three");
    p.reflow(500., Alignment::Start).unwrap();
    assert_eq!(p.source(), text);
    assert!(
        p.lines()
            .all(|l| text.is_char_boundary(l.source.start) && text.is_char_boundary(l.source.end))
    );
}
#[test]
fn invalid_inputs_fail_before_native_layout_or_allocation() {
    let mut system = system();
    let mut p = system
        .prepare("safe", &style(), WhiteSpace::Preserve, &[], &[])
        .unwrap();
    for width in [f32::NAN, f32::INFINITY, -1.] {
        assert!(p.reflow(width, Alignment::Start).is_err());
    }
    assert!(
        system
            .prepare(
                "é",
                &style(),
                WhiteSpace::Preserve,
                &[StyledSpan {
                    range: 1..2,
                    style: style()
                }],
                &[]
            )
            .is_err()
    );
    assert!(
        system
            .prepare(
                "x",
                &TextStyle {
                    size: f32::NAN,
                    ..style()
                },
                WhiteSpace::Preserve,
                &[],
                &[]
            )
            .is_err()
    );
    assert!(RasterSurface::new(u16::MAX, u16::MAX, 1.).is_err());
    assert!(RasterSurface::new(10, 10, f64::NAN).is_err());
}
#[test]
fn editing_selection_undo_composition_and_line_endings_preserve_source() {
    let mut system = system();
    let original = "café — 你好\r\nA final line.\n";
    let mut editor = TextEditor::new(original, style()).unwrap();
    editor.command(&mut system, EditCommand::SelectAll).unwrap();
    assert_eq!(editor.selected_text(), Some(original));
    editor
        .command(&mut system, EditCommand::Preedit("仮".into(), Some((3, 3))))
        .unwrap();
    editor
        .command(&mut system, EditCommand::CancelCompose)
        .unwrap();
    assert_eq!(editor.text(), original);
    editor
        .command(&mut system, EditCommand::Preedit("仮".into(), Some((3, 3))))
        .unwrap();
    editor
        .command(&mut system, EditCommand::Commit("確定\n".into()))
        .unwrap();
    assert_eq!(editor.text(), "確定\n");
    editor.command(&mut system, EditCommand::Undo).unwrap();
    assert_eq!(editor.text(), original);
    editor.command(&mut system, EditCommand::Redo).unwrap();
    assert_eq!(editor.text(), "確定\n");
}
#[test]
fn clipped_native_raster_paints_glyphs_and_keeps_outside_transparent() {
    let mut system = system();
    let mut p = system
        .prepare(
            "Beautiful type العربية",
            &style(),
            WhiteSpace::Preserve,
            &[],
            &[],
        )
        .unwrap();
    p.reflow(300., Alignment::Start).unwrap();
    let mut raster = RasterSurface::new(360, 100, 1.).unwrap();
    raster
        .text(p.layout(), [15., 15.], [15., 15., 300., 50.])
        .unwrap();
    let rgba = raster.finish();
    let mut ink = 0;
    for (i, pixel) in rgba.chunks_exact(4).enumerate() {
        if pixel[3] > 0 {
            ink += 1;
            let x = i % 360;
            let y = i / 360;
            assert!((15..315).contains(&x) && (15..65).contains(&y));
        }
    }
    assert!(ink > 100);
}
#[test]
fn seeded_unicode_width_sweep_terminates_and_covers_text() {
    let mut system = system();
    let samples = [
        "Latin café",
        "العربية 123 English",
        "日本語の美しい文字",
        "한국어 문장",
        "ภาษาไทยไม่มีช่องว่าง",
        "हिन्दी शब्द",
        "မြန်မာစာ",
        "ខ្មែរ",
        "👩🏽‍🚀 e\u{301}",
        "\u{2067}RTL 123\u{2069}",
        "x\u{200b}y\u{2060}z",
        "line\n\nnext",
    ];
    let mut seed = 37u32;
    for _ in 0..256 {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        let text = format!(
            "{} {}",
            samples[seed as usize % samples.len()],
            samples[(seed >> 8) as usize % samples.len()]
        );
        let mut p = system
            .prepare(&text, &style(), WhiteSpace::Preserve, &[], &[])
            .unwrap();
        let width = (seed % 701) as f32 / 2.;
        p.reflow(width, Alignment::Start).unwrap();
        assert_eq!(p.lines().last().unwrap().source.end, text.len());
        assert!(p.stats().height.is_finite());
    }
}
#[test]
fn tabs_advance_to_eight_space_stops_and_survive_reflow() {
    let mut system = system();
    let mut spaces = system
        .prepare("        ", &style(), WhiteSpace::Preserve, &[], &[])
        .unwrap();
    spaces.reflow(1000., Alignment::Start).unwrap();
    let stop = spaces.layout().full_width();
    let mut b = system
        .prepare("b", &style(), WhiteSpace::Preserve, &[], &[])
        .unwrap();
    let b_width = b.natural_width().unwrap();
    let mut text = system
        .prepare("a\tb", &style(), WhiteSpace::Preserve, &[], &[])
        .unwrap();
    let width = text.natural_width().unwrap();
    assert!(
        (width - stop - b_width).abs() < 0.05,
        "tab width {width}, expected {}",
        stop + b_width
    );
    text.reflow(30., Alignment::Start).unwrap();
    assert!((text.natural_width().unwrap() - width).abs() < 0.05);
}
#[test]
fn soft_hyphen_is_invisible_until_its_break_is_selected() {
    let mut system = system();
    let mut prefix = system
        .prepare("extra-", &style(), WhiteSpace::Preserve, &[], &[])
        .unwrap();
    let width = prefix.natural_width().unwrap() + 0.05;
    let mut plain = system
        .prepare("extraordinary", &style(), WhiteSpace::Preserve, &[], &[])
        .unwrap();
    let natural = plain.natural_width().unwrap();
    let mut soft = system
        .prepare(
            "extra\u{ad}ordinary",
            &style(),
            WhiteSpace::Preserve,
            &[],
            &[],
        )
        .unwrap();
    assert!((soft.natural_width().unwrap() - natural).abs() < 0.05);
    soft.reflow(width, Alignment::Start).unwrap();
    let first = soft.lines().next().unwrap();
    assert_eq!(first.source.end, "extra\u{ad}".len());
    assert!(
        (first.width - (width - 0.05)).abs() < 0.05,
        "hyphen must contribute to the chosen line: {first:?}"
    );
    assert!((soft.natural_width().unwrap() - natural).abs() < 0.05);
}

#[test]
fn rejected_flow_preserves_previous_drawable_layout() {
    let mut system = system();
    let mut text = system
        .prepare(
            "Several lines of writing must survive a rejected column geometry.",
            &style(),
            WhiteSpace::Preserve,
            &[],
            &[],
        )
        .unwrap();
    text.reflow(150., Alignment::Center).unwrap();
    let before = serde_json::to_string(&text.lines().collect::<Vec<_>>()).unwrap();
    assert!(
        text.flow(150., Alignment::Start, |i, y| LineBox {
            x: 0.,
            y,
            width: if i == 1 { f32::NAN } else { 40. }
        })
        .is_err()
    );
    assert_eq!(
        before,
        serde_json::to_string(&text.lines().collect::<Vec<_>>()).unwrap()
    );
}

#[test]
fn adversarial_composition_is_rejected_before_shaping_and_empty_preedit_cancels() {
    let mut system = system();
    let mut editor = TextEditor::new("Preserve this selection", style()).unwrap();
    editor.command(&mut system, EditCommand::SelectAll).unwrap();
    let hostile = "a\u{ad}".repeat(3000);
    for command in [
        EditCommand::Insert(hostile.clone()),
        EditCommand::Preedit(hostile.clone(), None),
        EditCommand::Commit(hostile),
    ] {
        assert!(editor.command(&mut system, command).is_err());
        assert_eq!(editor.text(), "Preserve this selection");
        assert_eq!(editor.selected_text(), Some("Preserve this selection"));
    }
    editor
        .command(&mut system, EditCommand::Preedit("仮".into(), Some((3, 3))))
        .unwrap();
    assert_eq!(
        editor.text(),
        "Preserve this selection",
        "Saving during preedit must see committed text"
    );
    assert!(editor.equals("Preserve this selection"));
    editor
        .command(&mut system, EditCommand::Preedit(String::new(), None))
        .unwrap();
    assert_eq!(editor.text(), "Preserve this selection");
}

#[test]
fn long_runs_and_large_graphemes_keep_all_source_and_glyphs() {
    let mut system = system();
    let long = "A quiet sentence. ".repeat(4000);
    let mut prepared = system
        .prepare(&long, &style(), WhiteSpace::Preserve, &[], &[])
        .unwrap();
    prepared.reflow(620., Alignment::Start).unwrap();
    let mut end = 0;
    for line in prepared.lines() {
        assert_eq!(line.source.start, end);
        end = line.source.end;
    }
    assert_eq!(end, long.len());
    let marks = format!("b{}", "\u{301}".repeat(300));
    let mut prepared = system
        .prepare(&marks, &style(), WhiteSpace::Preserve, &[], &[])
        .unwrap();
    prepared.reflow(1., Alignment::Start).unwrap();
    assert_eq!(prepared.stats().line_count, 1);
    assert_eq!(prepared.lines().next().unwrap().source, 0..marks.len());
    let glyphs = prepared
        .layout()
        .lines()
        .flat_map(|l| l.items())
        .map(|item| match item {
            parley::PositionedLayoutItem::GlyphRun(run) => run.glyphs().count(),
            _ => 0,
        })
        .sum::<usize>();
    assert!(glyphs >= 300, "Combining marks were dropped: {glyphs}");
}

#[test]
fn columns_hit_test_in_their_own_geometry() {
    let mut system = system();
    let mut text = system.prepare("The first column carries one thought. The next column carries another thought with more words.", &style(), WhiteSpace::Preserve, &[], &[]).unwrap();
    text.flow(220., Alignment::Start, |i, y| LineBox {
        x: if i < 2 { 0. } else { 260. },
        y: if i == 2 { 0. } else { y },
        width: 220.,
    })
    .unwrap();
    let lines = text.lines().collect::<Vec<_>>();
    assert!(lines.len() > 2);
    assert_eq!(text.hit_test(260., 1.).unwrap(), lines[2].source.start);
    assert_eq!(text.hit_test(0., 1.).unwrap(), lines[0].source.start);
    assert!(text.bounds()[2] > 260.);
}

#[test]
fn optimized_paragraphs_keep_source_fit_and_native_geometry() {
    let mut system = system();
    let source = "Careful typography considers a whole paragraph before choosing its breaks. Words should gather into a quiet, readable page.\n\nA second paragraph preserves its own beginning and end.\n";
    let mut text = system
        .prepare(source, &style(), WhiteSpace::Preserve, &[], &[])
        .unwrap();
    let count = system.preparations;
    for width in [220., 330., 470.] {
        text.optimize(width, Alignment::Justify).unwrap();
        let mut end = 0;
        for line in text.lines() {
            assert_eq!(line.source.start, end);
            end = line.source.end;
        }
        assert_eq!(end, source.len());
        for line in text.layout().lines() {
            let m = line.metrics();
            assert!(m.advance - m.trailing_whitespace <= width + 0.1, "{m:?}");
        }
    }
    assert_eq!(system.preparations, count);
}

#[test]
fn editing_treats_combining_sequences_and_emoji_as_graphemes() {
    let mut system = system();
    let mut editor = TextEditor::new("A👩🏽‍🚀e\u{301}", style()).unwrap();
    editor
        .command(&mut system, EditCommand::Move(Movement::TextEnd, false))
        .unwrap();
    editor.command(&mut system, EditCommand::Backspace).unwrap();
    assert_eq!(editor.text(), "A👩🏽‍🚀");
    editor.command(&mut system, EditCommand::Backspace).unwrap();
    assert_eq!(editor.text(), "A");
    editor.command(&mut system, EditCommand::Undo).unwrap();
    assert_eq!(editor.text(), "A👩🏽‍🚀");
    editor
        .command(&mut system, EditCommand::Move(Movement::TextStart, false))
        .unwrap();
    editor
        .command(&mut system, EditCommand::Move(Movement::Right, false))
        .unwrap();
    editor
        .command(&mut system, EditCommand::Move(Movement::Right, false))
        .unwrap();
    assert_eq!(editor.inner().raw_selection().focus().index(), "A👩🏽‍🚀".len());
    editor
        .command(&mut system, EditCommand::Move(Movement::Left, false))
        .unwrap();
    editor.command(&mut system, EditCommand::Delete).unwrap();
    assert_eq!(editor.text(), "A");
}
