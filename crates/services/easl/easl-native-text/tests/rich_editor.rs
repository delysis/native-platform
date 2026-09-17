use easl_native_text::{EditCommand, StyledSpan, TextEditor, TextStyle, TextSystem};

#[test]
fn rich_projection_uses_the_same_geometry_for_glyphs_selection_and_composition() {
    let mut fonts = TextSystem::new();
    let body = TextStyle::default();
    let heading = TextStyle {
        size: 40.,
        line_height: 54.,
        weight: 700.,
        ..body.clone()
    };
    let mut editor = TextEditor::new("Heading\nbody", body.clone()).unwrap();
    let spans = [StyledSpan {
        range: 0..7,
        style: heading,
    }];
    editor.set_spans(&spans).unwrap();
    editor.ensure_layout(&mut fonts, &body, 400., 300.).unwrap();
    assert_eq!(
        editor
            .inner()
            .try_layout()
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .runs()
            .next()
            .unwrap()
            .font_size(),
        40.
    );
    editor
        .command(&mut fonts, EditCommand::Click(2., 10., 1, false))
        .unwrap();
    let before = editor.selection_bytes();
    assert_eq!(before.0, before.1);
    editor
        .command(
            &mut fonts,
            EditCommand::Preedit("日本語".into(), Some((9, 9))),
        )
        .unwrap();
    assert_eq!(editor.text(), "Heading\nbody");
    assert!(editor.inner().ime_cursor_area().height() > 30.);
    editor
        .command(&mut fonts, EditCommand::CancelCompose)
        .unwrap();
    editor.set_spans(&spans).unwrap();
    editor.ensure_layout(&mut fonts, &body, 400., 300.).unwrap();
    assert_eq!(editor.text(), "Heading\nbody");
    assert_eq!(editor.selection_bytes(), before);
    assert_eq!(
        editor
            .inner()
            .try_layout()
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .runs()
            .next()
            .unwrap()
            .font_size(),
        40.
    );
}

#[test]
fn invalid_projection_leaves_the_current_widget_intact() {
    let mut fonts = TextSystem::new();
    let mut editor = TextEditor::new("café", TextStyle::default()).unwrap();
    let invalid = [StyledSpan {
        range: 1..999,
        style: TextStyle::default(),
    }];
    assert!(
        editor
            .set_projection(&mut fonts, "other", &invalid, (0, 0))
            .is_err()
    );
    assert_eq!(editor.text(), "café");
}
