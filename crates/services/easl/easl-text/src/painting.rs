//! Bulk native transport for EASL glyph placement. Font instances and source
//! remain with the caller; the VM returns only logical paint geometry.
use crate::{Error, runtime::Runtime};
use easl_native_text::{GlyphPaint, PaintDecoration, PaintGlyph, RasterSurface, parley};
use std::{collections::VecDeque, sync::Arc};

const HEADER: usize = 20;
const BATCH: usize = 1024;
const CACHE_BYTES: usize = 2 * 1024 * 1024;

struct CachedRun {
    input: Vec<u32>,
    paint: Arc<GlyphPaint>,
}
impl CachedRun {
    fn bytes(&self) -> usize {
        self.input.capacity() * size_of::<u32>()
            + self.paint.glyphs.capacity() * size_of::<PaintGlyph>()
            + self.paint.decorations.capacity() * size_of::<PaintDecoration>()
    }
}

pub struct GlyphPainting {
    runtime: Runtime,
    faulted: bool,
    cache: VecDeque<CachedRun>,
    cache_bytes: usize,
    pub preparations: u64,
    pub cache_hits: u64,
}
impl std::fmt::Debug for GlyphPainting {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GlyphPainting")
            .field("faulted", &self.faulted)
            .field("cached_runs", &self.cache.len())
            .field("cache_bytes", &self.cache_bytes)
            .field("preparations", &self.preparations)
            .field("cache_hits", &self.cache_hits)
            .finish_non_exhaustive()
    }
}
impl GlyphPainting {
    pub fn new() -> Result<Self, Error> {
        let runtime = std::panic::catch_unwind(|| {
            Runtime::compile(&[
                (
                    "native_painting.easl",
                    include_str!("../library/native_painting.easl"),
                ),
                ("painting.easl", include_str!("../library/painting.easl")),
                (
                    "decoration.easl",
                    include_str!("../library/decoration.easl"),
                ),
            ])
        })
        .map_err(|_| Error::Language("Paint compiler panicked".into()))??;
        Ok(Self {
            runtime,
            faulted: false,
            cache: VecDeque::new(),
            cache_bytes: 0,
            preparations: 0,
            cache_hits: 0,
        })
    }

    /// Use EASL placement for every visible run. The native surface retains font
    /// resources, rasterization, clipping and display scale. Scroll/host origin
    /// are outside the cache key; an unchanged run does not re-enter the VM.
    pub fn paint(
        &mut self,
        surface: &mut RasterSurface,
        layout: &parley::Layout<[u8; 4]>,
        origin: [f64; 2],
        clip: [f64; 4],
    ) -> Result<(), Error> {
        surface.text_with(layout, origin, clip, |run| self.prepare(run))
    }

    pub fn prepare(
        &mut self,
        run: &parley::GlyphRun<'_, [u8; 4]>,
    ) -> Result<Arc<GlyphPaint>, Error> {
        if self.faulted {
            return Err(Error::Faulted);
        }
        let input = encode(run)?;
        if let Some(index) = self.cache.iter().position(|item| item.input == input) {
            let cached = self.cache.remove(index).ok_or(Error::Invalid)?;
            let paint = cached.paint.clone();
            self.cache.push_back(cached);
            self.cache_hits = self.cache_hits.saturating_add(1);
            return Ok(paint);
        }
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.prepare_inner(&input)));
        let paint = match result {
            Ok(Ok(paint)) => Arc::new(paint),
            Ok(Err(error @ Error::Language(_))) => {
                self.faulted = true;
                return Err(error);
            }
            Ok(Err(error)) => return Err(error),
            Err(_) => {
                self.faulted = true;
                return Err(Error::Language("Paint VM panicked".into()));
            }
        };
        self.preparations = self.preparations.saturating_add(1);
        let cached = CachedRun {
            input,
            paint: paint.clone(),
        };
        if cached.bytes() <= CACHE_BYTES {
            self.cache_bytes += cached.bytes();
            self.cache.push_back(cached);
            while self.cache.len() > 256 || self.cache_bytes > CACHE_BYTES {
                let removed = self.cache.pop_front().ok_or(Error::Invalid)?;
                self.cache_bytes -= removed.bytes();
            }
        }
        Ok(paint)
    }

    fn prepare_inner(&mut self, input: &[u32]) -> Result<GlyphPaint, Error> {
        let mut paint = GlyphPaint::default();
        let mut origin = [input[0], input[1]];
        for batch in input[HEADER..].chunks(BATCH * 4) {
            let count = batch.len() / 4;
            self.runtime.write("paint-input", batch)?;
            self.runtime.write("paint-output", &vec![0; count * 3])?;
            self.runtime.write("paint-origin", &origin)?;
            self.runtime.run("text-paint-native")?;
            let result = self.runtime.read("paint-result")?;
            if result.len() != 3 {
                return Err(Error::Invalid);
            }
            status(result[0])?;
            if result[1] as usize != count {
                return Err(Error::Invalid);
            }
            let output = self.runtime.read("paint-output")?;
            if output.len() != count * 3 {
                return Err(Error::Invalid);
            }
            for (placed, shaped) in output.chunks_exact(3).zip(batch.chunks_exact(4)) {
                let point = [f32::from_bits(placed[1]), f32::from_bits(placed[2])];
                if placed[0] != shaped[0] || point.iter().any(|v| !finite(*v)) {
                    return Err(Error::Invalid);
                }
                paint.glyphs.push(PaintGlyph {
                    id: placed[0],
                    point,
                });
            }
            if !finite(f32::from_bits(result[2])) {
                return Err(Error::Invalid);
            }
            origin[0] = result[2];
        }
        self.runtime.write("paint-origin", &input[..2])?;
        self.runtime.write("paint-advance", &input[2..3])?;
        self.runtime.write("paint-metrics", &input[3..7])?;
        self.runtime.write("paint-overrides", &input[7..11])?;
        self.runtime.write("paint-options", &input[11..12])?;
        self.runtime.write("paint-colors", &input[12..20])?;
        self.runtime.write("paint-decorations", &[0; 16])?;
        self.runtime.run("text-decorations-native")?;
        let result = self.runtime.read("paint-decorated")?;
        if result.len() != 3 {
            return Err(Error::Invalid);
        }
        status(result[0])?;
        let count = (input[11] & 9).count_ones() as usize;
        if result[1] as usize != count {
            return Err(Error::Invalid);
        }
        let decorations = self.runtime.read("paint-decorations")?;
        if decorations.len() != 16 {
            return Err(Error::Invalid);
        }
        for record in decorations.chunks_exact(8).take(count) {
            let bounds = std::array::from_fn(|i| f32::from_bits(record[i]));
            if bounds.iter().any(|v| !finite(*v)) || bounds[3] < 0. {
                return Err(Error::Invalid);
            }
            let mut color = [0; 4];
            for (channel, value) in color.iter_mut().zip(&record[4..]) {
                *channel = u8::try_from(*value).map_err(|_| Error::Invalid)?;
            }
            paint.decorations.push(PaintDecoration { bounds, color });
        }
        Ok(paint)
    }
}

fn status(value: u32) -> Result<(), Error> {
    match value {
        0 => Ok(()),
        1 => Err(Error::Invalid),
        2 => Err(Error::Limit),
        _ => Err(Error::Invalid),
    }
}
fn finite(value: f32) -> bool {
    value.is_finite() && value.abs() <= 1e9
}

fn encode(glyph_run: &parley::GlyphRun<'_, [u8; 4]>) -> Result<Vec<u32>, Error> {
    let metrics = glyph_run.run().metrics();
    let style = glyph_run.style();
    let mut input = vec![0; HEADER];
    input[..7].copy_from_slice(
        &[
            glyph_run.offset(),
            glyph_run.baseline(),
            glyph_run.advance(),
            metrics.underline_offset,
            metrics.underline_size,
            metrics.strikethrough_offset,
            metrics.strikethrough_size,
        ]
        .map(f32::to_bits),
    );
    for (i, decoration) in [&style.underline, &style.strikethrough]
        .into_iter()
        .enumerate()
    {
        if let Some(d) = decoration {
            let mut flags = 1u32;
            if let Some(offset) = d.offset {
                flags |= 2;
                input[7 + i * 2] = offset.to_bits();
            }
            if let Some(size) = d.size {
                flags |= 4;
                input[8 + i * 2] = size.to_bits();
            }
            input[11] |= flags << (i * 3);
            input[12 + i * 4..16 + i * 4].copy_from_slice(&d.brush.map(u32::from));
        }
    }
    for g in glyph_run.glyphs() {
        if input.len() >= HEADER + easl_native_text::MAX_TEXT_BYTES * 4 {
            return Err(Error::Limit);
        }
        input.extend([g.id, g.x.to_bits(), g.y.to_bits(), g.advance.to_bits()]);
    }
    Ok(input)
}
