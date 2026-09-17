//! Native raster surface with transparent RGBA output suitable for a sibling
//! native view, a compositor texture, or a standalone window.
use crate::Error;
use parley::{Layout, PositionedLayoutItem};
use std::sync::Arc;
use vello_cpu::{
    Glyph, Pixmap, RasterizerSettings, RenderContext, RenderMode, Resources,
    kurbo::{Affine, Cap, Join, Rect, RoundedRect, Shape, Stroke},
    peniko::Color,
};

/// Font-independent output of a view's glyph-placement policy. Coordinates are
/// logical pixels relative to the layout, before the host's origin/display scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PaintGlyph {
    pub id: u32,
    pub point: [f32; 2],
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PaintDecoration {
    /// Signed width follows the shaped advance, including overlapping tracking.
    pub bounds: [f32; 4],
    pub color: [u8; 4],
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GlyphPaint {
    pub glyphs: Vec<PaintGlyph>,
    pub decorations: Vec<PaintDecoration>,
}

impl GlyphPaint {
    fn validate(&self) -> Result<(), Error> {
        if self.glyphs.len() > crate::MAX_TEXT_BYTES || self.decorations.len() > 2 {
            return Err(Error::Limit);
        }
        if self.glyphs.iter().any(|glyph| {
            glyph.id > u32::from(u16::MAX)
                || glyph.point.iter().any(|v| !v.is_finite() || v.abs() > 1e9)
        }) {
            return Err(Error::InvalidGeometry);
        }
        for decoration in &self.decorations {
            if decoration
                .bounds
                .iter()
                .any(|v| !v.is_finite() || v.abs() > 1e9)
                || decoration.bounds[3] < 0.
            {
                return Err(Error::InvalidGeometry);
            }
        }
        Ok(())
    }
}

pub struct RasterSurface {
    context: RenderContext,
    resources: Resources,
    pixmap: Pixmap,
    scale: f64,
    commands: usize,
}
impl std::fmt::Debug for RasterSurface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RasterSurface")
            .field("width", &self.pixmap.width())
            .field("height", &self.pixmap.height())
            .finish_non_exhaustive()
    }
}
impl RasterSurface {
    pub fn new(width: u16, height: u16, scale: f64) -> Result<Self, Error> {
        validate(width, height, scale)?;
        Ok(Self {
            context: RenderContext::new(width, height),
            resources: Resources::new(),
            pixmap: Pixmap::new(width, height),
            scale,
            commands: 0,
        })
    }
    pub fn begin(&mut self, width: u16, height: u16, scale: f64) -> Result<(), Error> {
        validate(width, height, scale)?;
        self.scale = scale;
        self.commands = 0;
        self.context.reset_and_resize(width, height);
        if self.pixmap.width() != width || self.pixmap.height() != height {
            self.pixmap = Pixmap::new(width, height);
        }
        self.context.set_transform(Affine::scale(scale));
        Ok(())
    }
    pub fn rect(&mut self, rect: [f64; 4], rgba: [u8; 4]) -> Result<(), Error> {
        self.count()?;
        let rect = checked_rect(rect)?;
        self.context.set_transform(Affine::scale(self.scale));
        self.context.set_paint(color(rgba));
        self.context.fill_rect(&rect);
        Ok(())
    }
    pub fn rounded_rect(
        &mut self,
        rect: [f64; 4],
        radius: f64,
        rgba: [u8; 4],
    ) -> Result<(), Error> {
        if !radius.is_finite() || !(0. ..=4096.).contains(&radius) {
            return Err(Error::InvalidGeometry);
        }
        let rect = checked_rect(rect)?;
        self.count()?;
        self.context.set_transform(Affine::scale(self.scale));
        self.context.set_paint(color(rgba));
        self.context
            .fill_path(&RoundedRect::from_rect(rect, radius).to_path(0.01));
        Ok(())
    }
    pub fn icon(
        &mut self,
        icon: &crate::VectorIcon,
        rect: [f64; 4],
        rgba: [u8; 4],
        stroke: f64,
        filled: bool,
    ) -> Result<(), Error> {
        if !stroke.is_finite() || !(0. ..=32.).contains(&stroke) {
            return Err(Error::InvalidGeometry);
        }
        let rect = checked_rect(rect)?;
        let [x, y, w, h] = icon.viewbox;
        let scale = (rect.width() / w).min(rect.height() / h);
        self.context.set_transform(
            Affine::scale(self.scale)
                * Affine::translate((
                    rect.x0 + (rect.width() - w * scale) * 0.5,
                    rect.y0 + (rect.height() - h * scale) * 0.5,
                ))
                * Affine::scale(scale)
                * Affine::translate((-x, -y)),
        );
        self.context.set_paint(color(rgba));
        self.context.set_stroke(
            Stroke::new(stroke)
                .with_caps(Cap::Round)
                .with_join(Join::Round),
        );
        for path in &icon.paths {
            self.count()?;
            if filled {
                self.context.fill_path(path);
            } else {
                self.context.stroke_path(path);
            }
        }
        Ok(())
    }
    pub fn text(
        &mut self,
        layout: &Layout<[u8; 4]>,
        origin: [f64; 2],
        clip: [f64; 4],
    ) -> Result<(), Error> {
        self.text_with(layout, origin, clip, |glyph_run| {
            let run = glyph_run.run();
            let style = glyph_run.style();
            let mut paint = GlyphPaint {
                glyphs: glyph_run
                    .positioned_glyphs()
                    .map(|g| PaintGlyph {
                        id: g.id,
                        point: [g.x, g.y],
                    })
                    .collect(),
                decorations: Vec::new(),
            };
            for (decoration, offset, size) in [
                (
                    &style.underline,
                    run.metrics().underline_offset,
                    run.metrics().underline_size,
                ),
                (
                    &style.strikethrough,
                    run.metrics().strikethrough_offset,
                    run.metrics().strikethrough_size,
                ),
            ] {
                if let Some(d) = decoration {
                    paint.decorations.push(PaintDecoration {
                        bounds: [
                            glyph_run.offset(),
                            glyph_run.baseline() - d.offset.unwrap_or(offset),
                            glyph_run.advance(),
                            d.size.unwrap_or(size),
                        ],
                        color: d.brush,
                    });
                }
            }
            Ok(Arc::new(paint))
        })
    }

    /// Rasterize view-owned glyph/decorative geometry with the layout's retained
    /// font instances. The callback runs before any drawing or clipping state is
    /// changed; one invalid late run cannot publish a partial text draw.
    pub fn text_with<E: From<Error>>(
        &mut self,
        layout: &Layout<[u8; 4]>,
        origin: [f64; 2],
        clip: [f64; 4],
        mut geometry: impl FnMut(&parley::GlyphRun<'_, [u8; 4]>) -> Result<Arc<GlyphPaint>, E>,
    ) -> Result<(), E> {
        let clip = checked_rect(clip)?;
        if origin.iter().any(|v| !v.is_finite() || v.abs() > 1e9) {
            return Err(Error::InvalidGeometry.into());
        }
        let mut staged = Vec::new();
        let mut glyphs = 0usize;
        for line in layout.lines() {
            let metrics = line.metrics();
            if f64::from(metrics.block_max_coord) + origin[1] < clip.y0
                || f64::from(metrics.block_min_coord) + origin[1] > clip.y1
            {
                continue;
            }
            for item in line.items() {
                if let PositionedLayoutItem::GlyphRun(run) = item {
                    let paint = geometry(&run)?;
                    paint.validate()?;
                    glyphs = glyphs.checked_add(paint.glyphs.len()).ok_or(Error::Limit)?;
                    if glyphs > crate::MAX_TEXT_BYTES || staged.len() >= 65_535 {
                        return Err(Error::Limit.into());
                    }
                    staged.push((run, paint));
                }
            }
        }
        self.count()?;
        self.context.set_transform(Affine::scale(self.scale));
        self.context.push_clip_layer(&clip.to_path(0.1));
        self.context
            .set_transform(Affine::scale(self.scale) * Affine::translate((origin[0], origin[1])));
        for (glyph_run, paint) in staged {
            let run = glyph_run.run();
            self.context.set_paint(color(glyph_run.style().brush));
            self.context
                .glyph_run(&mut self.resources, run.font())
                .font_size(run.font_size())
                .normalized_coords(run.normalized_coords())
                .glyph_transform(run.synthesis().skew().map_or(Affine::IDENTITY, |angle| {
                    Affine::skew(f64::from(angle.to_radians().tan()), 0.)
                }))
                .hint(true)
                .atlas_cache(true)
                .fill_glyphs(paint.glyphs.iter().map(|g| Glyph {
                    id: g.id,
                    x: g.point[0],
                    y: g.point[1],
                }));
            for d in &paint.decorations {
                self.context.set_paint(color(d.color));
                let [x, y, w, h] = d.bounds;
                self.context.fill_rect(&Rect::new(
                    f64::from(x),
                    f64::from(y),
                    f64::from(x + w),
                    f64::from(y + h),
                ));
            }
        }
        self.context.pop_layer();
        self.context.set_transform(Affine::scale(self.scale));
        Ok(())
    }
    pub fn finish(&mut self) -> &[u8] {
        self.context.render_with(
            &mut self.pixmap,
            &mut self.resources,
            RasterizerSettings {
                render_mode: RenderMode::OptimizeQuality,
                ..Default::default()
            },
        );
        self.pixmap.data_as_u8_slice()
    }
    pub fn png(&self) -> Result<Vec<u8>, Error> {
        self.pixmap.clone().into_png().map_err(|_| Error::Raster)
    }
    fn count(&mut self) -> Result<(), Error> {
        self.commands += 1;
        if self.commands > 4096 {
            Err(Error::Limit)
        } else {
            Ok(())
        }
    }
}
fn validate(width: u16, height: u16, scale: f64) -> Result<(), Error> {
    if width == 0
        || height == 0
        || u64::from(width) * u64::from(height) > 16 * 1024 * 1024
        || !scale.is_finite()
        || !(0.25..=8.).contains(&scale)
    {
        Err(Error::InvalidGeometry)
    } else {
        Ok(())
    }
}
fn checked_rect(v: [f64; 4]) -> Result<Rect, Error> {
    if v.iter().any(|v| !v.is_finite() || v.abs() > 1e9) || v[2] < 0. || v[3] < 0. {
        Err(Error::InvalidGeometry)
    } else {
        Ok(Rect::new(v[0], v[1], v[0] + v[2], v[1] + v[3]))
    }
}
fn color(c: [u8; 4]) -> Color {
    Color::from_rgba8(c[0], c[1], c[2], c[3])
}
