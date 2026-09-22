use crate::{Error, TwoFields};
use easl_native_text::RasterSurface;

impl TwoFields {
    /// Paint the same buffers used by editing. No fixture or synthetic preview path.
    pub fn paint(&mut self, surface: &mut RasterSurface) -> Result<(), Error> {
        let boxes = self.boxes.ok_or(Error::Geometry)?;
        for (index, rect) in boxes.into_iter().enumerate() {
            let focused = self.window_focused && index == self.active;
            surface.rounded_rect(
                rect.0.map(f64::from),
                4.,
                if focused {
                    [70, 70, 70, 255]
                } else {
                    [185, 185, 185, 255]
                },
            )?;
            let [x, y, w, h] = rect.0.map(f64::from);
            surface.rounded_rect([x + 1., y + 1., w - 2., h - 2.], 3., [255; 4])?;
            let clip = rect.content().0.map(f64::from);
            let field = &mut self.fields[index];
            let content = rect.content();
            field.ensure_layout(&mut self.system, &self.style, content.0[2], content.0[3])?;
            let origin = [clip[0], clip[1] - f64::from(field.scroll)];
            for (r, _) in field.inner().selection_geometry() {
                clipped(
                    surface,
                    [origin[0] + r.x0, origin[1] + r.y0, r.width(), r.height()],
                    clip,
                    if focused {
                        [190, 208, 230, 255]
                    } else {
                        [220, 220, 220, 255]
                    },
                )?;
            }
            if let Some(layout) = field.inner().try_layout() {
                surface.text(layout, origin, clip)?;
            }
            if focused && let Some(r) = field.inner().cursor_geometry(1.3) {
                clipped(
                    surface,
                    [origin[0] + r.x0, origin[1] + r.y0, r.width(), r.height()],
                    clip,
                    self.style.color,
                )?;
            }
        }
        Ok(())
    }
}
fn clipped(
    surface: &mut RasterSurface,
    rect: [f64; 4],
    clip: [f64; 4],
    color: [u8; 4],
) -> Result<(), Error> {
    let x = rect[0].max(clip[0]);
    let y = rect[1].max(clip[1]);
    let right = (rect[0] + rect[2]).min(clip[0] + clip[2]);
    let bottom = (rect[1] + rect[3]).min(clip[1] + clip[3]);
    if right > x && bottom > y {
        surface.rect([x, y, right - x, bottom - y], color)?;
    }
    Ok(())
}
