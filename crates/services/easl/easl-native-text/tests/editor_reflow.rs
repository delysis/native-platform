use easl_native_text::{
    EditCommand, ParagraphLayout, StyledSpan, TextEditor, TextStyle, TextSystem,
    parley::{
        Affinity, Alignment, Cursor, Layout, PlainEditor, PositionedLayoutItem, StyleProperty,
        StyleSet,
    },
};
use unicode_segmentation::UnicodeSegmentation;

fn close(a: f64, b: f64) {
    assert!((a - b).abs() <= 0.0002, "{a} != {b}");
}

fn same_geometry(a: &Layout<[u8; 4]>, b: &Layout<[u8; 4]>, text: &str) {
    assert_eq!(a.len(), b.len());
    close(a.height().into(), b.height().into());
    for (a, b) in a.lines().zip(b.lines()) {
        assert_eq!(a.text_range(), b.text_range());
        assert_eq!(a.break_reason(), b.break_reason());
        let metrics = |m: &easl_native_text::parley::LineMetrics| {
            [
                m.ascent,
                m.descent,
                m.leading,
                m.line_height,
                m.baseline,
                m.offset,
                m.advance,
                m.trailing_whitespace,
                m.inline_min_coord,
                m.inline_max_coord,
                m.block_min_coord,
                m.block_max_coord,
            ]
        };
        for (a, b) in metrics(a.metrics()).into_iter().zip(metrics(b.metrics())) {
            close(a.into(), b.into());
        }
        let glyphs = |line: easl_native_text::parley::Line<'_, [u8; 4]>| {
            line.items()
                .flat_map(|item| match item {
                    PositionedLayoutItem::GlyphRun(run) => run
                        .positioned_glyphs()
                        .map(|glyph| (run.run().font().data.id(), run.run().font_size(), glyph))
                        .collect::<Vec<_>>(),
                    PositionedLayoutItem::InlineBox(_) => unreachable!(),
                })
                .collect::<Vec<_>>()
        };
        let (a, b) = (glyphs(a), glyphs(b));
        assert_eq!(a.len(), b.len());
        for ((af, asize, a), (bf, bsize, b)) in a.into_iter().zip(b) {
            assert_eq!(
                (af, asize, a.id, a.style_index),
                (bf, bsize, b.id, b.style_index)
            );
            close(a.x.into(), b.x.into());
            close(a.y.into(), b.y.into());
            close(a.advance.into(), b.advance.into());
        }
    }
    for byte in text
        .grapheme_indices(true)
        .map(|(i, _)| i)
        .chain([text.len()])
    {
        for affinity in [Affinity::Upstream, Affinity::Downstream] {
            let ac = Cursor::from_byte_index(a, byte, affinity);
            let bc = Cursor::from_byte_index(b, byte, affinity);
            assert_eq!((ac.index(), ac.affinity()), (bc.index(), bc.affinity()));
            let (ar, br) = (ac.geometry(a, 1.), bc.geometry(b, 1.));
            for (x, y) in [ar.x0, ar.y0, ar.x1, ar.y1]
                .into_iter()
                .zip([br.x0, br.y0, br.x1, br.y1])
            {
                close(x, y);
            }
        }
    }
    for line in a.lines() {
        for x in [-10., 0., 35., 91., 221., 500.] {
            let y = line.metrics().baseline;
            let ac = Cursor::from_point(a, x, y);
            let bc = Cursor::from_point(b, x, y);
            assert_eq!((ac.index(), ac.affinity()), (bc.index(), bc.affinity()));
        }
    }
}

fn rich_editor(text: &str, boxed: bool) -> PlainEditor<[u8; 4]> {
    let mut editor = PlainEditor::new(20.);
    editor.set_quantize(false);
    editor.set_text(text);
    let mut heading = StyleSet::new(32.);
    heading.insert(StyleProperty::Underline(true));
    let first_end = text.find(['\r', '\n']).unwrap_or(text.len());
    let mut empty = StyleSet::new(28.);
    empty.insert(StyleProperty::FontSize(28.));
    assert!(editor.set_ranged_styles(vec![
        (0..first_end, heading),
        (text.len()..text.len(), empty)
    ]));
    if boxed {
        assert!(
            editor.set_paragraph_layouts(
                std::iter::once(0)
                    .chain(text.match_indices('\n').map(|(i, _)| i + 1))
                    .map(|start| ParagraphLayout {
                        start,
                        inset_left: 18.,
                        inset_right: 11.,
                        first_line_indent: -7.,
                        space_before: 8.,
                        space_after: 13.,
                    })
                    .collect()
            )
        );
    }
    editor
}

#[test]
fn repeated_reflow_matches_fresh_shaping_for_glyphs_carets_and_paragraphs() {
    let mut system = TextSystem::new();
    for text in [
        "",
        "\r\n",
        "A heading\r\nTabs\tand soft\u{ad}hyphens join long words.\n",
        "العربية café\n日本語 e\u{301} 👩🏽‍🚀 mixed עברית spaces   \n",
        "one\n\nlast",
        "one\rtwo\u{2028}three\u{2029}",
    ] {
        for boxed in [false, true] {
            let mut editor = rich_editor(text, boxed);
            editor.refresh_layout(&mut system.fonts, &mut system.layouts);
            let shaped = editor.shaping_generation();
            for alignment in [
                Alignment::Justify,
                Alignment::Center,
                Alignment::End,
                Alignment::Start,
            ] {
                for width in [450., 90., 221., 0., 90., 450.] {
                    editor.set_width(Some(width));
                    editor.set_alignment(alignment);
                    editor.refresh_layout(&mut system.fonts, &mut system.layouts);
                    assert_eq!(editor.shaping_generation(), shaped, "{text:?}");
                    let mut reference = rich_editor(text, boxed);
                    reference.set_width(Some(width));
                    reference.set_alignment(alignment);
                    reference.refresh_layout(&mut system.fonts, &mut system.layouts);
                    same_geometry(
                        editor.try_layout().unwrap(),
                        reference.try_layout().unwrap(),
                        text,
                    );
                }
            }
        }
    }
}

#[test]
fn resizing_preedit_keeps_composition_selection_source_and_undo() {
    let mut system = TextSystem::new();
    let style = TextStyle::default();
    let text = "first words with length\r\nlast";
    let mut editor = TextEditor::new(text, style.clone()).unwrap();
    editor
        .ensure_layout(&mut system, &style, 350., 80.)
        .unwrap();
    editor
        .command(
            &mut system,
            EditCommand::Preedit("仮名".into(), Some((3, 6))),
        )
        .unwrap();
    let shaping = editor.inner().shaping_generation();
    let selection = editor.selection_bytes();
    let raw = editor.inner().raw_text().to_owned();
    for width in [100., 500., 0., 120.] {
        editor
            .ensure_layout(&mut system, &style, width, 80.)
            .unwrap();
        assert_eq!(editor.inner().shaping_generation(), shaping);
        assert_eq!(editor.text(), text);
        assert_eq!(editor.inner().raw_text(), raw);
        assert_eq!(editor.selection_bytes(), selection);
        assert_eq!(*editor.inner().raw_compose(), Some(0..6));
        let mut reference = editor.inner().clone();
        // Explicitly invalidate shaping without changing the live composition.
        reference.edit_styles();
        reference.refresh_layout(&mut system.fonts, &mut system.layouts);
        same_geometry(
            editor.inner().try_layout().unwrap(),
            reference.try_layout().unwrap(),
            &raw,
        );
    }
    editor
        .command(&mut system, EditCommand::Commit("日本語".into()))
        .unwrap();
    assert_ne!(editor.inner().shaping_generation(), shaping);
    assert_eq!(editor.text(), format!("日本語{text}"));
    editor.command(&mut system, EditCommand::Undo).unwrap();
    assert_eq!(editor.text(), text);
}

#[test]
fn geometry_changes_do_not_downgrade_pending_text_or_style_changes() {
    let mut system = TextSystem::new();
    let style = TextStyle::default();
    let mut editor = TextEditor::new("old", style.clone()).unwrap();
    editor
        .ensure_layout(&mut system, &style, 300., 200.)
        .unwrap();
    let before = editor.inner().shaping_generation();
    editor
        .set_spans(&[StyledSpan {
            range: 0..3,
            style: TextStyle {
                size: 42.,
                ..style.clone()
            },
        }])
        .unwrap();
    editor
        .set_paragraphs(&[ParagraphLayout {
            inset_left: 20.,
            ..Default::default()
        }])
        .unwrap();
    editor
        .ensure_layout(&mut system, &style, 200., 200.)
        .unwrap();
    assert_ne!(editor.inner().shaping_generation(), before);
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
        42.
    );
    let before = editor.inner().shaping_generation();
    editor.set_paragraphs(&[]).unwrap();
    editor
        .ensure_layout(&mut system, &style, 200., 200.)
        .unwrap();
    assert_eq!(editor.inner().shaping_generation(), before);
    let mut direct = editor.inner().clone();
    direct.set_text("new\ntext");
    direct.set_width(Some(110.));
    direct.set_alignment(Alignment::Center);
    direct.refresh_layout(&mut system.fonts, &mut system.layouts);
    assert_ne!(direct.shaping_generation(), before);
    assert_eq!(
        direct
            .try_layout()
            .unwrap()
            .lines()
            .last()
            .unwrap()
            .text_range()
            .end,
        8
    );
    let before = direct.shaping_generation();
    direct.set_scale(2.);
    direct.set_width(Some(200.));
    direct.refresh_layout(&mut system.fonts, &mut system.layouts);
    assert_ne!(direct.shaping_generation(), before);
    let before = direct.shaping_generation();
    direct.set_quantize(true);
    direct.set_width(Some(180.));
    direct.refresh_layout(&mut system.fonts, &mut system.layouts);
    assert_ne!(direct.shaping_generation(), before);
}
