use easl_native_text::{EditCommand, Movement, TextEditor, TextStyle, TextSystem};
#[test]
fn unwrapped_ime_geometry_contains_the_caret_beyond_the_viewport_and_during_preedit() {
    let mut system = TextSystem::new();
    let style = TextStyle {
        wrap: false,
        ..Default::default()
    };
    let mut editor =
        TextEditor::new("https://example.com/a/very/long/path", style.clone()).unwrap();
    editor.ensure_layout(&mut system, &style, 60., 35.).unwrap();
    editor
        .command(&mut system, EditCommand::Move(Movement::TextEnd, false))
        .unwrap();
    for composing in [false, true] {
        if composing {
            editor
                .command(&mut system, EditCommand::Preedit("仮".into(), Some((3, 3))))
                .unwrap();
        }
        editor.ensure_layout(&mut system, &style, 60., 35.).unwrap();
        let layout = editor.inner().try_layout().unwrap();
        let focus = editor.inner().raw_selection().focus().geometry(layout, 0.);
        let area = editor.inner().ime_cursor_area();
        assert!(focus.x0 > 60.);
        assert!(
            area.x0 <= focus.x0 && area.x1 >= focus.x1,
            "{area:?}, {focus:?}"
        );
        assert!(area.x0 <= area.x1 && area.y0 <= area.y1);
    }
}
