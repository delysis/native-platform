use easl_native_text::{
    CaretStop, EditCommand, ParagraphLayout, StyledSpan, TextEditor, TextStyle, TextSystem,
    parley::{Affinity, Cursor},
};
use easl_text::HitTesting;

fn stop(byte: usize, x: f32, affinity: Affinity) -> CaretStop {
    CaretStop {
        byte,
        affinity,
        line: 0,
        x,
        other_x: x,
        top: 0.,
        bottom: 20.,
    }
}

#[test]
fn batches_preserve_ties_and_invalid_input_cannot_poison_the_next_query() {
    let mut hits = HitTesting::new().unwrap();
    let mut stops = (0..1025)
        .map(|i| stop(i, i as f32 * 10., Affinity::Upstream))
        .collect::<Vec<_>>();
    // Equal-distance candidates on opposite sides of a batch boundary.
    stops[255] = stop(91, 35., Affinity::Upstream);
    stops[256] = stop(92, 35., Affinity::Downstream);
    let expected = stops[256];
    for items in [stops.clone(), stops.iter().copied().rev().collect()] {
        assert_eq!(hits.hit_stops(items, [35., 10.]).unwrap(), expected);
    }
    let mut invalid = stops.clone();
    invalid[900].x = f32::NAN;
    assert!(hits.hit_stops(invalid, [35., 10.]).is_err());
    assert!(hits.hit_stops([], [35., 10.]).is_err());
    assert!(hits.hit_stops(stops.clone(), [f32::INFINITY, 0.]).is_err());
    assert_eq!(hits.hit_stops(stops, [35., 10.]).unwrap(), expected);
    let distant = CaretStop {
        x: 1e10,
        top: 1e9,
        bottom: 1e9 + 1024.,
        ..expected
    };
    assert_eq!(hits.hit_stops([distant], [1e10, 1e9]).unwrap(), distant);
    assert_eq!(hits.decisions, 4);
}

#[test]
fn shaped_stops_match_painted_carets_and_never_split_complex_graphemes() {
    let mut system = TextSystem::new();
    let mut hits = HitTesting::new().unwrap();
    for source in [
        "",
        "\r\n",
        "office e\u{301} 👩🏽‍🚀\r\nend\n",
        "abc العربية def\nשלום\n",
        "\u{600}a bc",
    ] {
        let style = TextStyle {
            size: 23.,
            line_height: 35.,
            ..TextStyle::default()
        };
        let mut editor = TextEditor::new(source, style.clone()).unwrap();
        editor
            .ensure_layout(&mut system, &style, 140., 200.)
            .unwrap();
        let lines = editor
            .inner()
            .try_layout()
            .unwrap()
            .lines()
            .map(|line| {
                let m = line.metrics();
                (m.block_min_coord + m.block_max_coord) * 0.5
            })
            .collect::<Vec<_>>();
        for y in lines {
            let mut stops = Vec::new();
            editor
                .visit_caret_stops::<easl_native_text::Error>(&mut system, y, |stop| {
                    stops.push(stop);
                    Ok(())
                })
                .unwrap();
            assert!(!stops.is_empty());
            for edge in &stops {
                let layout = editor.inner().try_layout().unwrap();
                let caret = Cursor::from_byte_index(layout, edge.byte, edge.affinity);
                assert_eq!(caret.index(), edge.byte, "{source:?} {edge:?}");
                let rect = caret.geometry(layout, 1.);
                assert!(
                    (rect.x0 as f32 - edge.x).abs() < 0.001,
                    "{source:?} {edge:?} {rect:?}"
                );
                assert!(
                    (rect.y0 as f32 - edge.top).abs() < 0.001,
                    "{source:?} {edge:?} {rect:?}"
                );
                // Native selection admission independently checks the complete EGC boundary.
                editor
                    .select_pointer_target(&mut system, edge.byte, edge.affinity, 1, false)
                    .unwrap();
                assert_eq!(editor.selection_bytes(), (edge.byte, edge.byte));
            }
            for x in [-30., 11.25, 50.25, 95.25, 200.] {
                let hit = hits.hit_editor(&mut editor, &mut system, [x, y]).unwrap();
                assert!(stops.contains(&hit));
                if source == "\u{600}a bc" {
                    assert_ne!(hit.byte, 2);
                }
                if source.contains("\r\n") {
                    assert_ne!(hit.byte, source.find("\r\n").unwrap() + 1);
                }
            }
        }
        assert_eq!(editor.text(), source);
    }
}

#[test]
fn rich_wrapping_and_granular_pointer_selection_use_the_same_native_geometry() {
    let source = "alpha beta gamma delta\r\nשלום world\nlast";
    let style = TextStyle::default();
    let mut system = TextSystem::new();
    let mut hits = HitTesting::new().unwrap();
    let mut native = TextEditor::new(source, style.clone()).unwrap();
    let mut easl = TextEditor::new(source, style.clone()).unwrap();
    for editor in [&mut native, &mut easl] {
        editor
            .set_spans(&[StyledSpan {
                range: 0..5,
                style: TextStyle {
                    size: 30.,
                    italic: true,
                    ..style.clone()
                },
            }])
            .unwrap();
        editor
            .set_paragraphs(&[ParagraphLayout {
                start: 0,
                inset_left: 25.,
                first_line_indent: 12.,
                ..ParagraphLayout::default()
            }])
            .unwrap();
        editor
            .ensure_layout(&mut system, &style, 190., 200.)
            .unwrap();
    }
    let mut points = Vec::new();
    easl.visit_caret_stops::<easl_native_text::Error>(&mut system, 10., |stop| {
        if stop.affinity == Affinity::Downstream {
            points.push([stop.x + 0.125, (stop.top + stop.bottom) * 0.5]);
        }
        Ok(())
    })
    .unwrap();
    points.extend((0..19).map(|i| [10.25 * i as f32, 10.]));
    for point in points {
        for (count, extend) in [
            (1, false),
            (2, false),
            (0, false),
            (1, true),
            (3, false),
            (0, false),
        ] {
            let [x, y] = point;
            native
                .command(
                    &mut system,
                    if count == 0 {
                        EditCommand::Drag(x, y + 40.)
                    } else {
                        EditCommand::Click(x, y, count, extend)
                    },
                )
                .unwrap();
            let hit = hits
                .hit_editor(
                    &mut easl,
                    &mut system,
                    [x, if count == 0 { y + 40. } else { y }],
                )
                .unwrap();
            easl.select_pointer_target(&mut system, hit.byte, hit.affinity, count, extend)
                .unwrap();
            assert_eq!(
                easl.selection_bytes(),
                native.selection_bytes(),
                "{point:?} count {count} extend {extend}"
            );
            assert_eq!(
                easl.inner().raw_selection().focus().affinity(),
                native.inner().raw_selection().focus().affinity()
            );
        }
    }
    assert_eq!(easl.text(), source);
    easl.command(&mut system, EditCommand::Undo).unwrap();
    assert_eq!(easl.text(), source);
}

#[test]
fn active_preedit_and_nonfinite_queries_leave_live_selection_unchanged() {
    let mut system = TextSystem::new();
    let mut hits = HitTesting::new().unwrap();
    let mut editor = TextEditor::new("café", TextStyle::default()).unwrap();
    editor
        .command(&mut system, EditCommand::Preedit("仮".into(), Some((0, 3))))
        .unwrap();
    let source = editor.text();
    let selection = editor.selection_bytes();
    assert!(
        hits.hit_editor(&mut editor, &mut system, [4., 10.])
            .is_err()
    );
    assert_eq!(editor.text(), source);
    assert_eq!(editor.selection_bytes(), selection);
    editor
        .command(&mut system, EditCommand::CancelCompose)
        .unwrap();
    assert!(
        hits.hit_editor(&mut editor, &mut system, [f32::NAN, 0.])
            .is_err()
    );
    let hit = hits.hit_editor(&mut editor, &mut system, [0., 0.]).unwrap();
    assert_eq!(hit.byte, 0);
    assert_eq!(editor.text(), "café");
}

#[test]
fn whole_crlf_word_drag_and_empty_final_line_have_explicit_selection_contracts() {
    let mut system = TextSystem::new();
    let mut hits = HitTesting::new().unwrap();
    let style = TextStyle::default();
    let mut editor = TextEditor::new("one two\r\n", style.clone()).unwrap();
    editor
        .ensure_layout(&mut system, &style, 300., 100.)
        .unwrap();
    editor
        .select_pointer_target(&mut system, 0, Affinity::Downstream, 2, false)
        .unwrap();
    assert_eq!(editor.selection_bytes(), (0, 3));
    editor
        .select_pointer_target(&mut system, 7, Affinity::Downstream, 0, false)
        .unwrap();
    assert_eq!(editor.selection_bytes(), (0, 9));
    // The original word anchor is retained when dragging back toward the start.
    editor
        .select_pointer_target(&mut system, 4, Affinity::Downstream, 0, false)
        .unwrap();
    assert_eq!(editor.selection_bytes(), (0, 7));
    let hit = hits
        .hit_editor(&mut editor, &mut system, [0., 100.])
        .unwrap();
    assert_eq!(hit.byte, 9);
    editor
        .select_pointer_target(&mut system, hit.byte, hit.affinity, 2, false)
        .unwrap();
    assert_eq!(editor.selection_bytes(), (9, 9));
    assert!(
        editor
            .select_pointer_target(&mut system, 8, Affinity::Downstream, 1, false)
            .is_err()
    );
    assert_eq!(editor.selection_bytes(), (9, 9));
    assert_eq!(editor.text(), "one two\r\n");
}

#[test]
fn pointer_geometry_visits_only_the_indexed_line_after_reflow() {
    let mut system = TextSystem::new();
    let mut hits = HitTesting::new().unwrap();
    let style = TextStyle::default();
    let mut editor = TextEditor::new(&"alpha beta gamma\n".repeat(1000), style.clone()).unwrap();
    for width in [100., 500.] {
        editor
            .ensure_layout(&mut system, &style, width, 200.)
            .unwrap();
        let layout = editor.inner().try_layout().unwrap();
        let middle = layout.len() / 2;
        let metrics = *layout.get(middle).unwrap().metrics();
        let y = (metrics.block_min_coord + metrics.block_max_coord) * 0.5;
        let mut count = 0;
        editor
            .visit_caret_stops::<easl_native_text::Error>(&mut system, y, |stop| {
                assert_eq!(stop.line, middle);
                count += 1;
                Ok(())
            })
            .unwrap();
        assert!(count <= 2 * "alpha beta gamma\n".len());
        let hit = hits.hit_editor(&mut editor, &mut system, [40., y]).unwrap();
        assert_eq!(hit.line, middle);
    }
}
