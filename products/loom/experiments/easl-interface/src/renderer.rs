//! Generic native compositor for EASL primitives. Typography and geometry arrive
//! in the scene. EASL places painted glyphs/decorations; native font resources
//! and the retained Parley shaping/caret layout still supply their input facts.
use crate::{
    document::Documents,
    interface::{Draw, Rect, Scene},
};
use easl_native_text::{Alignment, PreparedText, RasterSurface, TextStyle, TextSystem, WhiteSpace};
use std::collections::VecDeque;
const TITLEBAR_ICONS: &str = include_str!(concat!(env!("OUT_DIR"), "/loom_titlebar_icons.json"));
#[cfg(test)]
use std::time::{Duration, Instant};
#[cfg(test)]
#[derive(Debug, Default)]
struct PaintTimings {
    policy_compile: Duration,
    encode: Duration,
    raster: Duration,
    copy: Duration,
}
#[derive(Debug)]
struct CachedLabel {
    text: String,
    style: TextStyle,
    prepared: PreparedText,
    width: f32,
}
#[derive(Debug)]
pub struct Renderer {
    pub text: TextSystem,
    surface: Option<RasterSurface>,
    painting: Option<easl_text::GlyphPainting>,
    labels: VecDeque<CachedLabel>,
    icons: Vec<easl_native_text::VectorIcon>,
    #[cfg(test)]
    timings: PaintTimings,
}
impl Renderer {
    /// Match the reference body's CSS ch unit to its actual zero advance.
    pub fn body_character_width(&mut self) -> Result<f32, easl_native_text::Error> {
        self.text
            .prepare(
                "0",
                &TextStyle {
                    family: "Iowan Old Style, Palatino, Georgia, serif".into(),
                    size: 19.,
                    ..Default::default()
                },
                WhiteSpace::Preserve,
                &[],
                &[],
            )?
            .natural_width()
    }
    pub fn new() -> Self {
        Self {
            text: TextSystem::new(),
            surface: None,
            painting: None,
            labels: VecDeque::new(),
            icons: Vec::new(),
            #[cfg(test)]
            timings: PaintTimings::default(),
        }
    }
    pub fn prepare_assets(&mut self) -> Result<(), easl_native_text::Error> {
        if self.icons.is_empty() {
            let icons: Vec<String> = serde_json::from_str(TITLEBAR_ICONS)
                .map_err(|_| easl_native_text::Error::InvalidGeometry)?;
            let mut parsed = icons
                .iter()
                .map(|svg| easl_native_text::VectorIcon::parse(svg))
                .collect::<Result<Vec<_>, _>>()?;
            // The web reference delegates this affordance to the platform's
            // select widget. A vector avoids font-dependent missing glyphs.
            parsed.push(easl_native_text::VectorIcon::parse(
                r#"<svg viewBox="0 0 24 24"><path d="M7 10L12 15L17 10"/></svg>"#,
            )?);
            self.icons = parsed;
        }
        Ok(())
    }
    #[allow(
        clippy::too_many_arguments,
        reason = "Frame geometry and separate document/input resources"
    )]
    pub fn paint(
        &mut self,
        pixels: &mut [u32],
        width: usize,
        height: usize,
        scale: f32,
        scene: &Scene,
        docs: &mut Documents,
        input: Option<(u32, &mut crate::input_field::InputField)>,
    ) -> Result<(), String> {
        self.paint_inner(pixels, width, height, scale, scene, docs, input)
            .map_err(|e| e.to_string())
    }
    #[allow(
        clippy::too_many_lines,
        clippy::too_many_arguments,
        reason = "A single ordered display-list pass defines compositing order"
    )]
    fn paint_inner(
        &mut self,
        pixels: &mut [u32],
        width: usize,
        height: usize,
        scale: f32,
        scene: &Scene,
        docs: &mut Documents,
        mut input: Option<(u32, &mut crate::input_field::InputField)>,
    ) -> Result<(), easl_text::Error> {
        #[cfg(test)]
        let start = Instant::now();
        self.prepare_assets()?;
        let width = u16::try_from(width).map_err(|_| easl_native_text::Error::Limit)?;
        let height = u16::try_from(height).map_err(|_| easl_native_text::Error::Limit)?;
        if self.surface.is_none() {
            self.surface = Some(RasterSurface::new(width, height, f64::from(scale))?);
        }
        let surface = self
            .surface
            .as_mut()
            .ok_or(easl_native_text::Error::Raster)?;
        surface.begin(width, height, f64::from(scale))?;
        #[cfg(test)]
        let policy_start = Instant::now();
        if self.painting.is_none() {
            self.painting = Some(easl_text::GlyphPainting::new()?);
        }
        #[cfg(test)]
        let policy_compile = policy_start.elapsed();
        let mut painter = TextPaint {
            surface,
            geometry: self.painting.as_mut().ok_or(easl_text::Error::Invalid)?,
        };
        for draw in &scene.draws {
            match draw {
                Draw::WebView(_) | Draw::Drag(_) => {} // The OS owns these surfaces.
                Draw::Rect(rect, rgba) => {
                    painter.surface.rect(rect.0.map(f64::from), color(*rgba))?;
                }
                Draw::RoundedRect(rect, rgba, radius) => {
                    painter.surface.rounded_rect(
                        rect.0.map(f64::from),
                        f64::from(*radius),
                        color(*rgba),
                    )?;
                }
                Draw::Icon(rect, rgba, style) => {
                    let icon = self
                        .icons
                        .get(
                            crate::interface::id(style[0])
                                .map_err(|_| easl_native_text::Error::InvalidGeometry)?
                                as usize,
                        )
                        .ok_or(easl_native_text::Error::InvalidGeometry)?;
                    painter.surface.icon(
                        icon,
                        rect.0.map(f64::from),
                        color(*rgba),
                        f64::from(style[1]),
                        style[2] > 0.,
                    )?;
                }
                Draw::Text(rect, style, text) => label(
                    &mut self.text,
                    &mut self.labels,
                    &mut painter,
                    *rect,
                    *style,
                    text,
                )?,
                Draw::Slot(rect, style, id) => {
                    let text = match *id {
                        0 => docs.title(),
                        1 => {
                            if docs.dirty() {
                                format!("Unsaved changes · {}", docs.status)
                            } else {
                                docs.status.clone()
                            }
                        }
                        100..=115 => docs
                            .project
                            .entries()
                            .get((*id - 100) as usize)
                            .map(|e| {
                                e.display_title.clone().unwrap_or_else(|| {
                                    std::path::Path::new(&e.relative_path)
                                        .file_stem()
                                        .unwrap_or_default()
                                        .to_string_lossy()
                                        .into_owned()
                                })
                            })
                            .unwrap_or_default(),
                        200..=247 => docs.pane_text(*id),
                        _ => String::new(),
                    };
                    label(
                        &mut self.text,
                        &mut self.labels,
                        &mut painter,
                        *rect,
                        *style,
                        &text,
                    )?;
                }
                Draw::StyledText(rect, rgb, role, centered, text) => {
                    let mut style = scene
                        .typography
                        .iter()
                        .find(|(id, _)| id == role)
                        .ok_or(easl_text::Error::Invalid)?
                        .1
                        .clone();
                    style.color = color(*rgb);
                    styled_label(
                        &mut self.text,
                        &mut self.labels,
                        &mut painter,
                        *rect,
                        &style,
                        *centered,
                        text,
                    )?;
                }
                Draw::Input(view) => {
                    let Some((key, field)) = input.as_mut() else {
                        continue;
                    };
                    if *key != view.key {
                        continue;
                    }
                    let mut style = scene
                        .typography
                        .iter()
                        .find(|(id, _)| *id == view.typography)
                        .ok_or(easl_text::Error::Invalid)?
                        .1
                        .clone();
                    style.color = color(view.color);
                    field
                        .layout(&mut self.text, &style, view.content.0[2], view.content.0[3])
                        .map_err(easl_text::Error::Language)?;
                    let origin = [
                        f64::from(view.content.0[0] - field.scroll_x),
                        f64::from(view.content.0[1] - field.field.editor.scroll),
                    ];
                    let clip = view.content.0.map(f64::from);
                    let ink = style.color;
                    if field.value().is_empty() && !field.composing() {
                        style.color[3] = 128;
                        styled_label(
                            &mut self.text,
                            &mut self.labels,
                            &mut painter,
                            view.content,
                            &style,
                            false,
                            &view.placeholder,
                        )?;
                    }
                    paint_widget(
                        &mut painter,
                        &field.field.editor,
                        origin,
                        clip,
                        view.focused,
                        ink,
                        scene.selection_color.map_or([196, 215, 184, 255], color),
                        view.focused,
                    )?;
                }
                Draw::Editor(rect, style, rgb) => {
                    let Ok(field) = docs.field(
                        crate::interface::id(style[2]).unwrap_or_default(),
                        &mut self.text,
                    ) else {
                        continue;
                    };
                    let id = crate::interface::id(style[2]).unwrap_or_default();
                    let typography_id = if id >= 3 { 1 } else { id };
                    let mut typography = scene
                        .typography
                        .iter()
                        .find(|(key, _)| *key == typography_id)
                        .map_or_else(TextStyle::default, |(_, s)| s.clone());
                    typography.size = style[0];
                    typography.line_height = style[1];
                    typography.color = color([rgb[0], rgb[1], rgb[2], 1.]);
                    field.ensure_layout(
                        &mut self.text,
                        &typography,
                        scene,
                        rect.0[2],
                        rect.0[3],
                    )?;
                    let x = f64::from(rect.0[0]);
                    let y = f64::from(rect.0[1] - field.editor.scroll);
                    let clip = rect.0.map(f64::from);
                    paragraph_decorations(
                        &mut self.text,
                        &mut self.labels,
                        &mut painter,
                        field,
                        &typography,
                        [x, y],
                        clip,
                    )?;
                    paint_widget(
                        &mut painter,
                        &field.editor,
                        [x, y],
                        clip,
                        style[2].to_bits() == style[3].to_bits(),
                        typography.color,
                        scene.selection_color.map_or([196, 215, 184, 255], color),
                        true,
                    )?;
                }
            }
        }
        #[cfg(test)]
        let encoded = Instant::now();
        let rgba = painter.surface.finish();
        #[cfg(test)]
        let rasterized = Instant::now();
        for (pixel, c) in pixels.iter_mut().zip(rgba.chunks_exact(4)) {
            *pixel = (u32::from(c[0]) << 16) | (u32::from(c[1]) << 8) | u32::from(c[2]);
        }
        #[cfg(test)]
        {
            self.timings = PaintTimings {
                policy_compile,
                encode: encoded - start,
                raster: rasterized - encoded,
                copy: rasterized.elapsed(),
            };
        }
        Ok(())
    }
}
fn clipped_rect(
    surface: &mut RasterSurface,
    r: [f64; 4],
    c: [f64; 4],
    color: [u8; 4],
) -> Result<(), easl_native_text::Error> {
    let x = r[0].max(c[0]);
    let y = r[1].max(c[1]);
    let right = (r[0] + r[2]).min(c[0] + c[2]);
    let bottom = (r[1] + r[3]).min(c[1] + c[3]);
    if right > x && bottom > y {
        surface.rect([x, y, right - x, bottom - y], color)?;
    }
    Ok(())
}
struct TextPaint<'a> {
    surface: &'a mut RasterSurface,
    geometry: &'a mut easl_text::GlyphPainting,
}
impl TextPaint<'_> {
    fn text(
        &mut self,
        layout: &easl_native_text::parley::Layout<[u8; 4]>,
        origin: [f64; 2],
        clip: [f64; 4],
    ) -> Result<(), easl_text::Error> {
        self.geometry.paint(self.surface, layout, origin, clip)
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "Independent generic text paint inputs"
)]
fn paint_widget(
    painter: &mut TextPaint<'_>,
    editor: &easl_native_text::TextEditor,
    origin: [f64; 2],
    clip: [f64; 4],
    focused: bool,
    ink: [u8; 4],
    selection: [u8; 4],
    show_selection: bool,
) -> Result<(), easl_text::Error> {
    if show_selection {
        for (rect, _) in editor.inner().selection_geometry() {
            clipped_rect(
                painter.surface,
                [
                    origin[0] + rect.x0,
                    origin[1] + rect.y0,
                    rect.width(),
                    rect.height(),
                ],
                clip,
                selection,
            )?;
        }
    }
    if let Some(layout) = editor.inner().try_layout() {
        painter.text(layout, origin, clip)?;
    }
    if focused && let Some(rect) = editor.inner().cursor_geometry(1.3) {
        clipped_rect(
            painter.surface,
            [
                origin[0] + rect.x0,
                origin[1] + rect.y0,
                rect.width(),
                rect.height(),
            ],
            clip,
            ink,
        )?;
    }
    Ok(())
}

fn styled_label(
    system: &mut TextSystem,
    labels: &mut VecDeque<CachedLabel>,
    painter: &mut TextPaint<'_>,
    rect: Rect,
    style: &TextStyle,
    centered: bool,
    text: &str,
) -> Result<(), easl_text::Error> {
    if rect.0[2] <= 0. || rect.0[3] <= 0. {
        return Ok(());
    }
    let entry = shaped_label(system, labels, text, style, rect.0[2])?;
    let layout = entry.prepared.layout();
    let x = rect.0[0]
        + if centered {
            (rect.0[2] - layout.width()).max(0.) * 0.5
        } else {
            0.
        };
    let y = rect.0[1] + (rect.0[3] - layout.height()).max(0.) * 0.5;
    painter.text(layout, [f64::from(x), f64::from(y)], rect.0.map(f64::from))?;
    keep_label(labels, entry);
    Ok(())
}

fn label(
    system: &mut TextSystem,
    labels: &mut VecDeque<CachedLabel>,
    painter: &mut TextPaint<'_>,
    rect: Rect,
    rgb: [f32; 4],
    text: &str,
) -> Result<(), easl_text::Error> {
    let [x, y, width, size] = rect.0;
    if width <= 0. || size <= 0. {
        return Ok(());
    }
    let style = TextStyle {
        family: if [1., 3.].contains(&rgb[3]) {
            "Iowan Old Style, Palatino, Georgia, serif"
        } else {
            "Inter, ui-sans-serif, -apple-system, BlinkMacSystemFont, Segoe UI, sans-serif"
        }
        .into(),
        size,
        line_height: size
            * if [2., 3., 5.].contains(&rgb[3]) {
                1.
            } else {
                1.35
            },
        weight: if crate::interface::id(rgb[3]).unwrap_or_default() == 5 {
            800.
        } else if rgb[3] >= 2. {
            600.
        } else {
            400.
        },
        color: color([rgb[0], rgb[1], rgb[2], 1.]),
        ..TextStyle::default()
    };
    let entry = shaped_label(system, labels, text, &style, width)?;
    let offset = if [2., 3., 5.].contains(&rgb[3]) {
        (width - entry.prepared.layout().width()).max(0.) * 0.5
    } else {
        0.
    };
    painter.text(
        entry.prepared.layout(),
        [f64::from(x + offset), f64::from(y)],
        [
            f64::from(x),
            f64::from(y),
            f64::from(width),
            f64::from(size * 1.5),
        ],
    )?;
    keep_label(labels, entry);
    Ok(())
}

fn shaped_label(
    system: &mut TextSystem,
    labels: &mut VecDeque<CachedLabel>,
    text: &str,
    style: &TextStyle,
    width: f32,
) -> Result<CachedLabel, easl_native_text::Error> {
    let mut entry = if let Some(i) = labels
        .iter()
        .position(|e| e.text == text && e.style == *style)
    {
        labels.remove(i).ok_or(easl_native_text::Error::Raster)?
    } else {
        CachedLabel {
            text: text.into(),
            style: style.clone(),
            prepared: system.prepare(text, style, WhiteSpace::Preserve, &[], &[])?,
            width: -1.,
        }
    };
    if entry.width.to_bits() != width.to_bits() {
        entry.prepared.reflow(width, Alignment::Start)?;
        entry.width = width;
    }
    Ok(entry)
}

fn keep_label(labels: &mut VecDeque<CachedLabel>, entry: CachedLabel) {
    labels.push_back(entry);
    while labels.len() > 128 || labels.iter().map(|e| e.text.len()).sum::<usize>() > 1024 * 1024 {
        labels.pop_front();
    }
}

fn paragraph_decorations(
    system: &mut TextSystem,
    labels: &mut VecDeque<CachedLabel>,
    painter: &mut TextPaint<'_>,
    field: &crate::text_field::TextField,
    body: &TextStyle,
    origin: [f64; 2],
    clip: [f64; 4],
) -> Result<(), easl_text::Error> {
    let Some(layout) = field.editor.inner().try_layout() else {
        return Ok(());
    };
    let blocks = field.blocks();
    let mut index = 0;
    let [x, y] = origin;
    let quote = field.paragraph_role(31);
    let mut marker_style = body.clone();
    marker_style.wrap = false;
    for line in layout.lines() {
        let range = line.text_range();
        while blocks
            .get(index + 1)
            .is_some_and(|b| b.display.start <= range.start)
        {
            index += 1;
        }
        let Some(block) = blocks.get(index) else {
            continue;
        };
        let metrics = line.metrics();
        let top = y + f64::from(metrics.block_min_coord);
        let bottom = y + f64::from(metrics.block_max_coord);
        if bottom < clip[1] || top > clip[1] + clip[3] {
            continue;
        }
        let left = x + f64::from(metrics.inline_min_coord);
        let mut rule_x = left - f64::from(quote.inset_left * 0.6);
        for _ in 0..block.quote_depth {
            let mut color = body.color;
            color[3] = 80;
            clipped_rect(
                painter.surface,
                [rule_x, top, 1.5, bottom - top],
                clip,
                color,
            )?;
            rule_x -= f64::from(quote.inset_left);
        }
        if block.style == loom_markdown::BlockStyle::Rule {
            clipped_rect(
                painter.surface,
                [
                    left,
                    top + (bottom - top) * 0.5,
                    f64::from(metrics.inline_max_coord - metrics.inline_min_coord),
                    1.,
                ],
                clip,
                body.color,
            )?;
        }
        if range.start == block.display.start
            && let Some(marker) = &block.marker
        {
            let entry = shaped_label(system, labels, marker, &marker_style, 512.)?;
            let marker_layout = entry.prepared.layout();
            if let Some(first) = marker_layout.lines().next() {
                painter.text(
                    marker_layout,
                    [
                        left - f64::from(body.size * 0.35 + marker_layout.width()),
                        y + f64::from(metrics.baseline - first.metrics().baseline),
                    ],
                    clip,
                )?;
            }
            keep_label(labels, entry);
        }
    }
    Ok(())
}
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "Explicit rounded and clamped 8-bit color quantization"
)]
fn color(rgba: [f32; 4]) -> [u8; 4] {
    rgba.map(|v| (v.clamp(0., 1.) * 255.).round() as u8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interface::{Interface, SOURCE};
    use easl_native_text::EditCommand;

    #[test]
    fn two_native_panes_keep_independent_layouts_of_one_document() {
        let root = tempfile::tempdir().unwrap();
        let mut docs = Documents::open(root.path()).unwrap();
        docs.manuscript = crate::document::TextField::new(
            "A single document has a wide writing view and a narrower pane. Each view keeps the caret aligned with its own wrapped lines, while edits update the same source.",
            true,
        ).unwrap();
        let mut renderer = Renderer::new();
        let mut ui = Interface::compile(SOURCE).unwrap();
        let mut input = [0.; crate::interface::INPUT_COUNT];
        input[0..2].copy_from_slice(&[1280., 820.]);
        input[10] = 1.;
        input[12] = 1.;
        let mut scene = ui.step(input).unwrap();
        let mut pane = None;
        for draw in &mut scene.draws {
            if let Draw::Editor(rect, style, color) = draw {
                *rect = Rect([40., 60., 650., 700.]);
                let mut pane_style = *style;
                pane_style[2] = 3.;
                pane = Some(Draw::Editor(
                    Rect([800., 60., 180., 700.]),
                    pane_style,
                    *color,
                ));
            }
        }
        scene.draws.push(pane.unwrap());
        let mut pixels = vec![0; 1280 * 820];
        renderer
            .paint(&mut pixels, 1280, 820, 1., &scene, &mut docs, None)
            .unwrap();
        let wide_lines = docs
            .field(1, &mut renderer.text)
            .unwrap()
            .editor
            .inner()
            .try_layout()
            .unwrap()
            .lines()
            .count();
        let narrow_lines = docs
            .field(3, &mut renderer.text)
            .unwrap()
            .editor
            .inner()
            .try_layout()
            .unwrap()
            .lines()
            .count();
        assert!(narrow_lines > wide_lines);
        docs.field(3, &mut renderer.text)
            .unwrap()
            .command(&mut renderer.text, EditCommand::Insert("Shared. ".into()))
            .unwrap();
        renderer
            .paint(&mut pixels, 1280, 820, 1., &scene, &mut docs, None)
            .unwrap();
        assert!(
            docs.field(1, &mut renderer.text)
                .unwrap()
                .editor
                .text()
                .starts_with("Shared. ")
        );
        assert!(
            docs.field(3, &mut renderer.text)
                .unwrap()
                .editor
                .text()
                .starts_with("Shared. ")
        );
    }

    #[test]
    fn current_reference_scene_renders_in_both_appearances() {
        let root = tempfile::tempdir().unwrap();
        let mut docs = Documents::open(root.path()).unwrap();
        docs.manuscript = crate::document::TextField::new(
            "# A current page\n\nNative **text**, with *emphasis*.\n",
            false,
        )
        .unwrap();
        let mut renderer = Renderer::new();
        let mut ui = Interface::compile(SOURCE).unwrap();
        let mut input = [0.; crate::interface::INPUT_COUNT];
        input[0..2].copy_from_slice(&[1280., 820.]);
        input[10] = 1.;
        input[12] = 1.;
        input[21] = renderer.body_character_width().unwrap();
        let mut pixels = vec![0; 1280 * 820];
        for dark in [false, true] {
            input[19] = f32::from(dark);
            let scene = ui.step(input).unwrap();
            renderer
                .paint(&mut pixels, 1280, 820, 1., &scene, &mut docs, None)
                .expect("current reference assets and native text must render together");
            let painting = renderer.painting.as_ref().unwrap();
            let prepared = painting.preparations;
            let hits = painting.cache_hits;
            assert!(
                prepared > 0,
                "actual Loom draws must use the EASL paint policy"
            );
            let expected = pixels.clone();
            renderer
                .paint(&mut pixels, 1280, 820, 1., &scene, &mut docs, None)
                .unwrap();
            let painting = renderer.painting.as_ref().unwrap();
            assert_eq!(
                painting.preparations, prepared,
                "unchanged repaint reran the VM"
            );
            assert!(painting.cache_hits > hits);
            assert_eq!(pixels, expected);
        }
    }

    #[test]
    fn resizing_configured_panes_preserves_source_carets_scroll_and_history() {
        use crate::pane_divider::DividerId;
        use easl_native_text::Movement;
        let root = tempfile::tempdir().unwrap();
        let config = "[workspace.panes.reader]\nkind='editor'\nposition='right'\nvisible=true\ndocument='@document'\n[workspace.panes.proof]\nkind='editor'\nposition='bottom'\nvisible=true\ndocument='@document'\n";
        std::fs::write(root.path().join(".mine.toml"), config).unwrap();
        let mut docs = Documents::open(root.path()).unwrap();
        let source = "A native paragraph with **emphasis**, café and a steady rhythm. Its source remains intact as the pane changes size.\n\n".repeat(40);
        docs.manuscript = crate::document::TextField::new(&source, true).unwrap();
        let ids = [
            1,
            u32::from(docs.workspace.view_id(&docs.project, 1)),
            u32::from(docs.workspace.view_id(&docs.project, 2)),
        ];
        let mut renderer = Renderer::new();
        let mut ui = Interface::compile(SOURCE).unwrap();
        let mut input = [0.; crate::interface::INPUT_COUNT];
        input[0..2].copy_from_slice(&[1200., 800.]);
        input[10] = 1.;
        let mut selections = Vec::new();
        for id in ids {
            let field = docs.field(id, &mut renderer.text).unwrap();
            field
                .command(
                    &mut renderer.text,
                    EditCommand::Move(Movement::TextEnd, false),
                )
                .unwrap();
            field.editor.scroll = 120.;
            field.editor.reveal_caret = false;
            selections.push(field.editor.selection_bytes());
        }
        let revision = docs.manuscript.revision();
        let mut pixels = vec![0; 1200 * 800];
        for (right, bottom) in [(180., 100.), (650., 400.), (320., 240.)] {
            docs.workspace.resize(DividerId::Right, right);
            docs.workspace.resize(DividerId::Bottom, bottom);
            docs.workspace
                .write_inputs(&docs.project, docs.document_id(), true, &mut input);
            input[43] = 2.;
            let scene = ui.step(input).unwrap();
            renderer
                .paint(&mut pixels, 1200, 800, 1., &scene, &mut docs, None)
                .unwrap();
            for (id, selection) in ids.into_iter().zip(&selections) {
                let field = docs.field(id, &mut renderer.text).unwrap();
                assert_eq!(field.editor.selection_bytes(), *selection);
                assert!((field.editor.scroll - 120.).abs() < 0.01);
            }
            assert_eq!(docs.manuscript.text(), source);
            assert_eq!(docs.manuscript.revision(), revision);
            assert!(!docs.manuscript.dirty());
        }
        assert_eq!(
            std::fs::read_to_string(root.path().join(".mine.toml")).unwrap(),
            config
        );
    }

    fn write_review_pixels(output: &std::path::Path, name: &str, pixels: &[u32]) {
        use std::io::Write;
        let path = output.join(format!("{name}.ppm"));
        let mut file = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
        file.write_all(b"P6\n1280 820\n255\n").unwrap();
        for pixel in pixels {
            file.write_all(&pixel.to_be_bytes()[1..]).unwrap();
        }
        file.flush().unwrap();
        println!("Native review pixels: {}", path.display());
    }

    /// Review actual native pixels without reading a user's manuscript or window.
    #[test]
    #[ignore = "manual synthetic native appearance review"]
    #[allow(
        clippy::too_many_lines,
        reason = "Keep the ordered synthetic review states together"
    )]
    fn render_appearance_review() {
        let output = std::env::var_os("EASL_REVIEW_DIRECTORY")
            .map(std::path::PathBuf::from)
            .expect("set an isolated EASL_REVIEW_DIRECTORY");
        std::fs::create_dir_all(&output).unwrap();
        let root = output.join("fixture");
        std::fs::create_dir(&root).expect("use a fresh review directory");
        let source = "# The measure of a page\n\nBeautiful type makes space for thought. **A steady rhythm**, a generous measure, and *careful emphasis* help the eye settle into a paragraph. Café, Ελληνικά, العربية, 日本語.\n\n> A quotation has its own measure, while keeping the source ordinary and readable.\n\n1. Shape the paragraph with the same metrics as its caret.\n2. Keep the writing intact through every change of view.\n\nA final paragraph returns to the page’s natural rhythm.\n";
        std::fs::write(root.join("A current page.md"), source).unwrap();
        std::fs::write(
            root.join("Reading notes.md"),
            "# Reading notes\n\nA separate, synthetic document.\n",
        )
        .unwrap();
        std::fs::write(
            root.join(".mine.toml"),
            r#"
[workspace.panes.reader]
kind = "editor"
position = "right"
visible = true
 title = "Reader"
document = "@document"
[workspace.panes.reference]
kind = "editor"
position = "right"
visible = true
title = "Reference"
document = '@"Reading notes.md"'
[workspace.panes.proof]
kind = "editor"
position = "bottom"
visible = true
title = "Proof"
document = "@document"
"#,
        )
        .unwrap();
        let mut docs = Documents::open(&root).unwrap();
        assert_eq!(docs.manuscript.text(), source);
        let mut renderer = Renderer::new();
        let mut ui = Interface::compile(SOURCE).unwrap();
        let mut input = [0.; crate::interface::INPUT_COUNT];
        input[0..2].copy_from_slice(&[1280., 820.]);
        input[8..12].copy_from_slice(&[0., 0., 1., 19.]);
        input[12] = 1.;
        input[21] = renderer.body_character_width().unwrap();
        docs.workspace
            .write_inputs(&docs.project, docs.document_id(), true, &mut input);
        for (name, dark) in [("light", false), ("dark", true)] {
            input[22] = 0.;
            input[44] = 0.;
            input[45] = 0.;
            input[19] = f32::from(dark);
            input[20] = if dark { 2. } else { 1. };
            let scene = ui.step(input).unwrap();
            let mut pixels = vec![0; 1280 * 820];
            renderer
                .paint(&mut pixels, 1280, 820, 1., &scene, &mut docs, None)
                .unwrap();
            write_review_pixels(&output, name, &pixels);
            let field = docs.field(1, &mut renderer.text).unwrap();
            field
                .command(&mut renderer.text, easl_native_text::EditCommand::SelectAll)
                .unwrap();
            let mut palette = crate::formatting::Palette::open(1, field).unwrap();
            input[22] = 1.;
            input[24] = f32::from(palette.active_mask());
            input[46] = f32::from(palette.can_link());
            input[47] = f32::from(palette.can_unlink());
            let scene = ui.step(input).unwrap();
            let mut pixels = vec![0; 1280 * 820];
            renderer
                .paint(
                    &mut pixels,
                    1280,
                    820,
                    1.,
                    &scene,
                    &mut docs,
                    Some((crate::formatting::DESTINATION, &mut palette.destination)),
                )
                .unwrap();
            write_review_pixels(&output, &format!("{name}-format"), &pixels);
            palette
                .destination
                .set_value(
                    &mut renderer.text,
                    "https://example.com/review/a-long-destination/café?source=loom",
                )
                .unwrap();
            input[44] = crate::interface::small_number(crate::formatting::DESTINATION);
            input[45] = 1.;
            input[46] = f32::from(palette.can_link());
            let scene = ui.step(input).unwrap();
            let mut pixels = vec![0; 1280 * 820];
            renderer
                .paint(
                    &mut pixels,
                    1280,
                    820,
                    1.,
                    &scene,
                    &mut docs,
                    Some((crate::formatting::DESTINATION, &mut palette.destination)),
                )
                .unwrap();
            assert!(palette.destination.scroll_x > 0.);
            assert_eq!(docs.manuscript.text(), source);
            write_review_pixels(&output, &format!("{name}-link"), &pixels);
        }
    }

    /// Offscreen, synthetic prose only. Run with --ignored --nocapture.
    #[test]
    #[ignore = "manual native view performance measurement"]
    fn profile_native_view() {
        let root = tempfile::tempdir().unwrap();
        let mut docs = Documents::open(root.path()).unwrap();
        let mut renderer = Renderer::new();
        let mut ui = Interface::compile(SOURCE).unwrap();
        let mut input = [0.; crate::interface::INPUT_COUNT];
        input[0..2].copy_from_slice(&[1280., 820.]);
        input[8..12].copy_from_slice(&[1., 1., 1., 20.]);
        input[12] = 1.;
        let sample = "Beautiful type is native to the page. A line of prose carries rhythm, space, and meaning — café, Ελληνικά, العربية, 日本語, 👩🏽‍🚀.\n\n".repeat(80);
        for scale in [1_u16, 2] {
            for populated in [false, true] {
                docs.manuscript =
                    crate::document::TextField::new(if populated { &sample } else { "" }, true)
                        .unwrap();
                let width = usize::from(1280 * scale);
                let height = usize::from(820 * scale);
                let mut pixels = vec![0; width * height];
                for frame in 0..4 {
                    let start = Instant::now();
                    let scene = ui.step(input).unwrap();
                    let vm = start.elapsed();
                    renderer
                        .paint(
                            &mut pixels,
                            width,
                            height,
                            f32::from(scale),
                            &scene,
                            &mut docs,
                            None,
                        )
                        .unwrap();
                    let PaintTimings {
                        policy_compile,
                        encode,
                        raster,
                        copy,
                    } = renderer.timings;
                    println!(
                        "scale={scale} text={populated} frame={frame} vm={vm:?} policy_compile={policy_compile:?} encode={encode:?} raster={raster:?} copy={copy:?}"
                    );
                }
                let start = Instant::now();
                docs.manuscript
                    .command(&mut renderer.text, EditCommand::Insert("x".into()))
                    .unwrap();
                println!(
                    "scale={scale} text={populated} insert={:?}",
                    start.elapsed()
                );
            }
        }
    }
}
