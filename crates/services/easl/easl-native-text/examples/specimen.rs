//! Render a standalone typography specimen with the production native text API.
use easl_native_text::*;
use std::{error::Error, path::PathBuf, sync::Arc};

fn draw(
    system: &mut TextSystem,
    surface: &mut RasterSurface,
    text: &str,
    style: &TextStyle,
    box_: [f32; 4],
    optimal: bool,
) -> Result<f32, Box<dyn Error>> {
    let mut prepared = system.prepare(text, style, WhiteSpace::Preserve, &[], &[])?;
    if optimal {
        prepared.optimize(box_[2], Alignment::Justify)?;
    } else {
        prepared.reflow(box_[2], Alignment::Start)?;
    }
    surface.text(
        prepared.layout(),
        [f64::from(box_[0]), f64::from(box_[1])],
        box_.map(f64::from),
    )?;
    Ok(prepared.stats().height)
}
fn main() -> Result<(), Box<dyn Error>> {
    let output = std::env::args_os().nth(1).map_or_else(
        || PathBuf::from("target/easl-type-specimen.png"),
        PathBuf::from,
    );
    let mut system = TextSystem::new();
    for bytes in [
        include_bytes!("../tests/fonts/amiri.ttf").as_slice(),
        include_bytes!("../tests/fonts/shantell-sans-regular.ttf").as_slice(),
    ] {
        system
            .fonts
            .collection
            .register_fonts(vello_cpu::peniko::Blob::new(Arc::new(bytes)), None);
    }
    let mut surface = RasterSurface::new(1680, 2160, 1.5)?;
    surface.begin(1680, 2160, 1.5)?;
    let ink = [43, 53, 45, 255];
    let moss = [77, 103, 78, 255];
    surface.rect([0., 0., 1120., 1440.], [249, 246, 237, 255])?;
    let body = TextStyle {
        family: "Baskerville, Georgia, serif".into(),
        size: 22.,
        line_height: 34.,
        color: ink,
        ..Default::default()
    };
    let small = TextStyle {
        family: "Helvetica Neue, sans-serif".into(),
        size: 11.,
        line_height: 18.,
        letter_spacing: 1.8,
        color: moss,
        ..body.clone()
    };
    draw(
        &mut system,
        &mut surface,
        "EASL    /    NATIVE TYPOGRAPHY",
        &small,
        [76., 56., 968., 32.],
        false,
    )?;
    let title = TextStyle {
        size: 72.,
        line_height: 78.,
        ..body.clone()
    };
    let mut heading = system.prepare(
        "Words deserve a beautiful place to live.",
        &title,
        WhiteSpace::Preserve,
        &[],
        &[],
    )?;
    heading.balance(960., Alignment::Start)?;
    surface.text(heading.layout(), [76., 109.], [76., 109., 968., 170.])?;
    surface.rect([76., 302., 968., 1.], [197, 202, 187, 255])?;
    let subhead = TextStyle {
        size: 29.,
        line_height: 36.,
        ..body.clone()
    };
    draw(
        &mut system,
        &mut surface,
        "The shape of a paragraph",
        &subhead,
        [76., 334., 442., 52.],
        false,
    )?;
    let paragraph = "A page begins with proportion: the width of a line, the space between its neighbours, and the small intervals that let one word meet the next. Good typography gives these relationships a quiet order. Here, a paragraph considers its possible endings together, choosing breaks across the whole passage instead of accepting the first line that happens to fit.";
    draw(
        &mut system,
        &mut surface,
        paragraph,
        &body,
        [76., 394., 442., 450.],
        true,
    )?;
    draw(
        &mut system,
        &mut surface,
        "Room for the unexpected",
        &subhead,
        [602., 334., 442., 52.],
        false,
    )?;
    surface.rect([602., 397., 108., 119.], moss)?;
    let white = TextStyle {
        size: 19.,
        line_height: 26.,
        color: [249, 246, 237, 255],
        ..body.clone()
    };
    draw(
        &mut system,
        &mut surface,
        "A native\nfigure",
        &white,
        [616., 421., 86., 76.],
        false,
    )?;
    let flow_text = "Text can make room for an illustration and return to its full measure as the figure falls behind. The same prepared glyphs can follow a changing margin, cross into another column, or settle into a narrower pane. Their positions remain available to selection and hit testing. The source stays intact through every rearrangement.";
    let emphasis_end = "Text can make room".len();
    let span = StyledSpan {
        range: 0..emphasis_end,
        style: TextStyle {
            italic: true,
            color: moss,
            ..body.clone()
        },
    };
    let mut flow = system.prepare(flow_text, &body, WhiteSpace::Preserve, &[span], &[])?;
    flow.flow(442., Alignment::Start, |i, y| LineBox {
        x: if i < 4 { 134. } else { 0. },
        y,
        width: if i < 4 { 308. } else { 442. },
    })?;
    surface.text(flow.layout(), [602., 394.], [602., 394., 442., 450.])?;
    surface.rect([76., 861., 968., 1.], [197, 202, 187, 255])?;
    draw(
        &mut system,
        &mut surface,
        "One page, many voices",
        &TextStyle {
            size: 40.,
            line_height: 48.,
            ..body.clone()
        },
        [76., 895., 968., 65.],
        false,
    )?;
    draw(
        &mut system,
        &mut surface,
        "Café, Ελληνικά, العربية — a family of words.",
        &TextStyle {
            size: 28.,
            line_height: 43.,
            ..body.clone()
        },
        [76., 970., 968., 52.],
        false,
    )?;
    draw(
        &mut system,
        &mut surface,
        "لكل كلمة مكان، ولكل سطر إيقاع.",
        &TextStyle {
            family: "Amiri".into(),
            size: 37.,
            line_height: 58.,
            ..body.clone()
        },
        [76., 1031., 968., 76.],
        false,
    )?;
    draw(
        &mut system,
        &mut surface,
        "日本語の美しい文字。   한 페이지의 여러 목소리.",
        &TextStyle {
            family: "Hiragino Mincho ProN, serif".into(),
            size: 26.,
            line_height: 42.,
            ..body.clone()
        },
        [76., 1120., 968., 65.],
        false,
    )?;
    draw(
        &mut system,
        &mut surface,
        "office · affinity · fine details · é · 👩🏽‍🚀",
        &TextStyle {
            size: 28.,
            line_height: 42.,
            ..body.clone()
        },
        [76., 1195., 968., 65.],
        false,
    )?;
    surface.rect([76., 1323., 968., 1.], [197, 202, 187, 255])?;
    draw(
        &mut system,
        &mut surface,
        "SHAPED GLYPHS    /    PARAGRAPH BREAKS    /    FLEXIBLE FLOW",
        &small,
        [76., 1350., 968., 35.],
        false,
    )?;
    surface.finish();
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&output, surface.png()?)?;
    println!(
        "{} ({} native preparations)",
        output.display(),
        system.preparations
    );
    Ok(())
}
