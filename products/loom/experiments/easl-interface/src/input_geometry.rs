//! Backend-independent conversion into the native view's logical coordinates.
//! Keyboard line-wheel input is already logical; only pixel input uses DPI.

pub const SCROLL_LINE_PIXELS: f32 = 24.;

fn logical_scalar(value: f64, scale: f64) -> Option<f64> {
    if !value.is_finite() || !scale.is_normal() || scale <= 0. {
        return None;
    }
    let logical = value / scale;
    (logical.is_finite() && logical.abs() <= f64::from(f32::MAX)).then_some(logical)
}

pub fn logical_point(point: [f64; 2], scale: f64) -> Option<[f64; 2]> {
    Some([
        logical_scalar(point[0], scale)?,
        logical_scalar(point[1], scale)?,
    ])
}

pub fn pixel_scroll_steps(delta: f64, scale: f64) -> Option<f64> {
    logical_scalar(delta, scale).map(|logical| -logical / f64::from(SCROLL_LINE_PIXELS))
}

#[cfg(test)]
#[path = "input_geometry_tests.rs"]
mod tests;
