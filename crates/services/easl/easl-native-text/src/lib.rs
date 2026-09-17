#![forbid(unsafe_code)]
//! Native text preparation, layout, editing and rasterization for EASL hosts.
//! Layouts retain shaped glyphs across width changes and own all font references.
mod editor;
mod paragraph;
pub use paragraph::BreakOpportunity;
mod raster;
mod vector;
pub use editor::*;
pub use raster::*;
pub use vector::VectorIcon;
mod prepare;
mod style;
pub use parley;
pub use prepare::*;
pub use style::*;
pub use vello_cpu;
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Raster encoding failed")]
    Raster,
    #[error("Text, styles or layout exceeded its documented bound")]
    Limit,
    #[error("Invalid typography value")]
    InvalidStyle,
    #[error("Range must contain valid UTF-8 boundaries")]
    InvalidRange,
    #[error("Invalid or nonfinite layout geometry")]
    InvalidGeometry,
    #[error("Unsupported line-break yield")]
    UnsupportedFlow,
    #[error("Finish or cancel the active composition before applying an external edit")]
    CompositionActive,
}
