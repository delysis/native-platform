use easl_native_text::{EditCommand, ParagraphLayout, TextEditor, TextStyle, TextSystem};

#[test]
fn paragraph_boxes_drive_wrapping_carets_hit_testing_and_document_height() {
    let text = "First paragraph wraps into several lines when narrow.\nSecond paragraph.\n";
    let next = text.find('\n').unwrap() + 1;
    let paragraphs = [
        ParagraphLayout {
            start: 0,
            inset_left: 30.,
            inset_right: 20.,
            first_line_indent: 15.,
            space_before: 8.,
            space_after: 10.,
        },
        ParagraphLayout {
            start: next,
            inset_left: 60.,
            space_before: 12.,
            space_after: 6.,
            ..Default::default()
        },
        ParagraphLayout {
            start: text.len(),
            inset_left: 90.,
            space_before: 7.,
            space_after: 11.,
            ..Default::default()
        },
    ];
    let mut system = TextSystem::new();
    let style = TextStyle::default();
    let mut editor = TextEditor::new(text, style.clone()).unwrap();
    editor
        .set_document_projection(&mut system, text, &[], &paragraphs, (0, 0))
        .unwrap();
    editor
        .ensure_layout(&mut system, &style, 220., 100.)
        .unwrap();
    let lines: Vec<_> = editor
        .inner()
        .try_layout()
        .unwrap()
        .lines()
        .map(|l| (l.text_range(), *l.metrics()))
        .collect();
    assert!(lines.len() >= 5);
    assert_eq!(lines[0].1.inline_min_coord, 45.);
    assert_eq!(lines[0].1.block_min_coord, 8.);
    for (range, metrics) in &lines {
        if range.start > 0 && range.start < next {
            assert_eq!(metrics.inline_min_coord, 30.);
        }
        if range.start < next {
            assert_eq!(metrics.inline_max_coord, 200.);
        }
    }
    let second = lines.iter().position(|(r, _)| r.start == next).unwrap();
    assert_eq!(lines[second].1.inline_min_coord, 60.);
    assert!(
        (lines[second].1.block_min_coord - lines[second - 1].1.block_max_coord - 22.).abs() < 0.01
    );
    for (range, metrics) in &lines {
        editor
            .command(
                &mut system,
                EditCommand::Click(metrics.inline_min_coord, metrics.baseline, 1, false),
            )
            .unwrap();
        assert_eq!(editor.selection_bytes(), (range.start, range.start));
        let caret = editor.inner().cursor_geometry(1.).unwrap();
        assert!(
            (caret.x0 - f64::from(metrics.inline_min_coord)).abs() < 0.01,
            "range={range:?} caret={caret:?} metrics={metrics:?}"
        );
        assert!((caret.y0 - f64::from(metrics.block_min_coord)).abs() < 0.01);
    }
    let layout = editor.inner().try_layout().unwrap();
    assert_eq!(
        layout.height(),
        lines.last().unwrap().1.block_max_coord + 11.
    );
    editor
        .ensure_layout(&mut system, &style, 220., 100.)
        .unwrap();
    assert!(editor.scroll > 0.);
    assert!(editor.inner().cursor_geometry(1.).unwrap().y1 <= f64::from(editor.scroll + 100.));
}

#[test]
fn paragraphs_survive_preedit_in_an_earlier_paragraph() {
    let text = "one\ntwo";
    let paragraphs = [ParagraphLayout {
        start: 4,
        inset_left: 50.,
        space_before: 20.,
        ..Default::default()
    }];
    let mut system = TextSystem::new();
    let style = TextStyle::default();
    let mut editor = TextEditor::new(text, style.clone()).unwrap();
    editor
        .set_document_projection(&mut system, text, &[], &paragraphs, (1, 1))
        .unwrap();
    editor
        .ensure_layout(&mut system, &style, 400., 300.)
        .unwrap();
    editor
        .command(
            &mut system,
            EditCommand::Preedit("仮名".into(), Some((6, 6))),
        )
        .unwrap();
    let last = editor.inner().try_layout().unwrap().lines().last().unwrap();
    assert_eq!(last.text_range().start, 10);
    assert_eq!(last.metrics().inline_min_coord, 50.);
    assert_eq!(editor.text(), text);
    editor
        .command(&mut system, EditCommand::CancelCompose)
        .unwrap();
    editor.set_paragraphs(&paragraphs).unwrap();
    editor
        .ensure_layout(&mut system, &style, 400., 300.)
        .unwrap();
    let last = editor.inner().try_layout().unwrap().lines().last().unwrap();
    assert_eq!(last.text_range().start, 4);
    assert_eq!(last.metrics().inline_min_coord, 50.);
}

#[test]
fn invalid_paragraphs_do_not_publish_a_different_document() {
    let text = "a\r\nb";
    let mut system = TextSystem::new();
    let mut editor = TextEditor::new("safe", TextStyle::default()).unwrap();
    for paragraph in [
        ParagraphLayout {
            start: 2,
            ..Default::default()
        },
        ParagraphLayout {
            start: 3,
            inset_left: f32::NAN,
            ..Default::default()
        },
        ParagraphLayout {
            start: 1,
            ..Default::default()
        },
        ParagraphLayout {
            start: 999,
            ..Default::default()
        },
        ParagraphLayout {
            start: 3,
            first_line_indent: -10.,
            ..Default::default()
        },
    ] {
        assert!(
            editor
                .set_document_projection(&mut system, text, &[], &[paragraph], (0, 0))
                .is_err()
        );
        assert_eq!(editor.text(), "safe");
    }
}

#[test]
fn narrow_boxes_and_empty_paragraphs_have_finite_addressable_geometry() {
    let mut system = TextSystem::new();
    let style = TextStyle::default();
    for text in ["", "\n", "a\n\nb", "العربية\n日本語\n"] {
        let paragraphs: Vec<_> = std::iter::once(0)
            .chain(text.match_indices('\n').map(|(i, _)| i + 1))
            .map(|start| ParagraphLayout {
                start,
                inset_left: 40.,
                space_before: 9.,
                ..Default::default()
            })
            .collect();
        let mut editor = TextEditor::new(text, style.clone()).unwrap();
        editor
            .set_document_projection(&mut system, text, &[], &paragraphs, (0, 0))
            .unwrap();
        editor.ensure_layout(&mut system, &style, 0., 60.).unwrap();
        let layout = editor.inner().try_layout().unwrap();
        assert!(layout.height().is_finite() && layout.height() > 0.);
        assert!(layout.len() <= text.chars().count() + 1);
        for line in layout.lines() {
            assert!(line.metrics().baseline.is_finite());
            assert!(text.is_char_boundary(line.text_range().start));
        }
    }
}
