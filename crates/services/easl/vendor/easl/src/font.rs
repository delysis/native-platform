//! Native font decoding, shaping and rasterization. Placement, line breaking
//! and editing policy belong to EASL libraries, not this module.
#![forbid(unsafe_code)]

use harfrust::{
  BufferFlags, Direction, Feature, Language, Script, ShapeLimits, ShapeOptions,
  ShaperData, ShaperInstance, Tag, UnicodeBuffer,
};
use read_fonts::TableProvider;
use std::{fmt, fs::File, io::Read, ops::Range, path::Path, sync::Arc};
use swash::{
  CacheKey, FontRef,
  scale::{Render, ScaleContext, Source},
};
use unicode_segmentation::{GraphemeCursor, UnicodeSegmentation};

const MAX_FONT_BYTES: usize = 64 * 1024 * 1024;
const ATLAS_WIDTH: u32 = 4096;
const MAX_ATLAS_HEIGHT: u32 = 4096;
pub const MAX_RUN_BYTES: usize = 1024 * 1024;
pub const MAX_FONT_SETTINGS: usize = 64;
const SHAPE_LIMITS: ShapeLimits = ShapeLimits {
  max_glyphs: MAX_RUN_BYTES,
  max_operations: 16 * 1024 * 1024,
};

#[derive(Debug, thiserror::Error)]
pub enum FontError {
  #[error("Font I/O: {0}")]
  Io(#[from] std::io::Error),
  #[error("Invalid outline font")]
  InvalidFont,
  #[error("Font resolution must be finite and between 1 and 256 pixels per em")]
  InvalidResolution,
  #[error(
    "Font exceeds 64 MiB or the requested glyph page exceeds 4096 by 4096 pixels"
  )]
  Limit,
  #[error("Glyph page requires at most 1048576 valid glyph IDs from this font")]
  InvalidGlyphs,
  #[error("Text must contain at most 1 MiB of valid UTF-8 bytes")]
  InvalidText,
  #[error("Unknown OpenType script tag")]
  InvalidScript,
  #[error("Shaping span must end on extended grapheme boundaries")]
  InvalidSpan,
  #[error("Font shaping exceeded its glyph or operation limit")]
  ShapingLimit,
  #[error("Font shaping produced invalid glyphs, geometry or cluster ranges")]
  InvalidShape,
  #[error(
    "Font settings require at most 64 unique printable OpenType tags and finite variation values within ±1000000"
  )]
  InvalidSettings,
  #[error(
    "Font language must be empty or a hyphen-separated ASCII language identifier of at most 64 bytes"
  )]
  InvalidLanguage,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FontVariation {
  pub tag: u32,
  pub value: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FontFeature {
  pub tag: u32,
  pub value: u32,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FontLoadOptions<'a> {
  pub face_index: u32,
  pub variations: &'a [FontVariation],
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FontShapeOptions<'a> {
  /// Features apply to this entire span; EASL owns style partitioning.
  pub features: &'a [FontFeature],
  /// Empty means unspecified. No process locale is consulted.
  pub language: &'a str,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AtlasGlyph {
  pub rect: [u32; 4],
  /// Baseline-relative ink origin in downward-positive pixel coordinates.
  pub offset: [f32; 2],
  pub advance: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShapedGlyph {
  pub id: u32,
  pub start: u32,
  pub end: u32,
  pub x: f32,
  pub y: f32,
  pub advance: f32,
  /// Cluster-boundary facts: 1 = unsafe to break, 2 = unsafe to concatenate.
  /// An unsafe boundary requires reshaping; it is not a forbidden line break.
  pub flags: u32,
}

struct FontFace {
  data: Vec<u8>,
  shaper_data: ShaperData,
  instance: ShaperInstance,
  face_index: u32,
  key: CacheKey,
  glyph_count: u16,
}

pub struct FontAtlas {
  face: Arc<FontFace>,
  pub metrics: [f32; 4],
  /// Baseline-relative offsets (upwards positive) and stroke sizes, in pixels:
  /// underline offset/size, strikeout offset/size, for this exact font instance.
  pub decorations: [f32; 4],
  pub glyphs: Vec<AtlasGlyph>,
  /// Sorted unique IDs requested for this immutable page, including empty ink.
  pub rasterized_glyphs: Vec<u32>,
  pub width: u32,
  pub height: u32,
}

impl fmt::Debug for FontAtlas {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("FontAtlas")
      .field("key", &self.face.key)
      .field("metrics", &self.metrics)
      .field("glyph_count", &self.glyphs.len())
      .field("width", &self.width)
      .field("height", &self.height)
      .finish_non_exhaustive()
  }
}

impl PartialEq for FontAtlas {
  fn eq(&self, other: &Self) -> bool {
    self.face.key == other.face.key
      && self.metrics == other.metrics
      && self.rasterized_glyphs == other.rasterized_glyphs
  }
}

impl FontAtlas {
  pub fn load(
    path: &Path,
    pixels_per_em: f32,
  ) -> Result<(Self, Vec<u8>), FontError> {
    Self::load_with(path, pixels_per_em, FontLoadOptions::default())
  }

  pub fn load_with(
    path: &Path,
    pixels_per_em: f32,
    options: FontLoadOptions<'_>,
  ) -> Result<(Self, Vec<u8>), FontError> {
    let font = Self::open_with(path, pixels_per_em, options)?;
    font.rasterize_all()
  }

  /// Open immutable font facts and shaping state without rasterizing glyphs.
  /// The empty 1x1 page may be replaced by pages chosen by EASL layout/cache policy.
  pub fn open(path: &Path, pixels_per_em: f32) -> Result<Self, FontError> {
    Self::open_with(path, pixels_per_em, FontLoadOptions::default())
  }

  pub fn open_with(
    path: &Path,
    pixels_per_em: f32,
    options: FontLoadOptions<'_>,
  ) -> Result<Self, FontError> {
    validate_load_options(pixels_per_em, options)?;
    let mut bytes = Vec::new();
    File::open(path)?
      .take((MAX_FONT_BYTES + 1) as u64)
      .read_to_end(&mut bytes)?;
    Self::open_bytes_with(bytes, pixels_per_em, options)
  }

  pub fn from_bytes(
    data: Vec<u8>,
    pixels_per_em: f32,
  ) -> Result<(Self, Vec<u8>), FontError> {
    Self::from_bytes_with(data, pixels_per_em, FontLoadOptions::default())
  }

  pub fn from_bytes_with(
    data: Vec<u8>,
    pixels_per_em: f32,
    options: FontLoadOptions<'_>,
  ) -> Result<(Self, Vec<u8>), FontError> {
    Self::open_bytes_with(data, pixels_per_em, options)?.rasterize_all()
  }

  pub fn open_bytes_with(
    data: Vec<u8>,
    pixels_per_em: f32,
    options: FontLoadOptions<'_>,
  ) -> Result<Self, FontError> {
    validate_load_options(pixels_per_em, options)?;
    if data.len() > MAX_FONT_BYTES {
      return Err(FontError::Limit);
    }
    let font = FontRef::from_index(&data, options.face_index as usize)
      .ok_or(FontError::InvalidFont)?;
    if font.variations().len() > MAX_FONT_SETTINGS {
      return Err(FontError::InvalidSettings);
    }
    let key = font.key;
    let glyph_count = font.metrics(&[]).glyph_count;
    let shaper_font = harfrust::FontRef::from_index(&data, options.face_index)
      .map_err(|_| FontError::InvalidFont)?;
    let shaper_data = ShaperData::new(&shaper_font);
    let instance = ShaperInstance::from_variations(
      &shaper_font,
      options
        .variations
        .iter()
        .map(|v| (Tag::new(&v.tag.to_be_bytes()), v.value)),
    );
    Self::empty(
      Arc::new(FontFace {
        data,
        shaper_data,
        instance,
        face_index: options.face_index,
        key,
        glyph_count,
      }),
      pixels_per_em,
    )
  }

  fn empty(face: Arc<FontFace>, pixels_per_em: f32) -> Result<Self, FontError> {
    validate_load_options(pixels_per_em, FontLoadOptions::default())?;
    let font = FontRef::from_index(&face.data, face.face_index as usize)
      .ok_or(FontError::InvalidFont)?;
    let shaper_font =
      harfrust::FontRef::from_index(&face.data, face.face_index)
        .map_err(|_| FontError::InvalidFont)?;
    let coords: Vec<i16> =
      face.instance.coords().iter().map(|v| v.to_bits()).collect();
    let metrics = font.metrics(&coords).scale(pixels_per_em);
    if metrics.glyph_count == 0 || metrics.units_per_em == 0 {
      return Err(FontError::InvalidFont);
    }
    let decorations = decoration_metrics(
      &shaper_font,
      face.instance.coords(),
      pixels_per_em / f32::from(metrics.units_per_em),
    )?;
    let advances = font.glyph_metrics(&coords).scale(pixels_per_em);
    let mut context = ScaleContext::new();
    if !context
      .builder(font)
      .size(pixels_per_em)
      .normalized_coords(&coords)
      .build()
      .has_outlines()
    {
      return Err(FontError::InvalidFont);
    }
    let glyphs = (0..metrics.glyph_count)
      .map(|id| AtlasGlyph {
        advance: advances.advance_width(id),
        ..Default::default()
      })
      .collect();
    Ok(Self {
      face,
      metrics: [
        metrics.ascent,
        metrics.descent,
        metrics.leading,
        pixels_per_em,
      ],
      decorations,
      glyphs,
      rasterized_glyphs: Vec::new(),
      width: 1,
      height: 1,
    })
  }

  fn rasterize_all(&self) -> Result<(Self, Vec<u8>), FontError> {
    let ids: Vec<u32> = (0..u32::from(self.face.glyph_count)).collect();
    self.rasterize(&ids, self.metrics[3])
  }

  /// Build a new immutable page at the requested raster resolution. Font bytes,
  /// collection face, normalized variation instance and shaping data are shared.
  /// Glyph selection, page residency/eviction and placement remain EASL policy.
  /// Validate the entire request before rasterization; failure leaves every
  /// existing page and its UV coordinates valid. Duplicates are accepted.
  pub fn rasterize(
    &self,
    ids: &[u32],
    pixels_per_em: f32,
  ) -> Result<(Self, Vec<u8>), FontError> {
    if ids.len() > MAX_RUN_BYTES
      || ids.iter().any(|&id| id >= u32::from(self.face.glyph_count))
    {
      return Err(FontError::InvalidGlyphs);
    }
    let mut page = Self::empty(self.face.clone(), pixels_per_em)?;
    let mut selected = vec![false; usize::from(self.face.glyph_count)];
    for &id in ids {
      selected[id as usize] = true;
    }
    page.rasterized_glyphs = selected
      .iter()
      .enumerate()
      .filter_map(|(id, &yes)| yes.then_some(id as u32))
      .collect();
    if page.rasterized_glyphs.is_empty() {
      return Ok((page, vec![0; 4]));
    }
    let mut font =
      FontRef::from_index(&self.face.data, self.face.face_index as usize)
        .ok_or(FontError::InvalidFont)?;
    font.key = self.face.key;
    let coords: Vec<i16> = self
      .face
      .instance
      .coords()
      .iter()
      .map(|v| v.to_bits())
      .collect();
    let mut context = ScaleContext::new();
    let mut scaler = context
      .builder(font)
      .size(pixels_per_em)
      .normalized_coords(&coords)
      .hint(true)
      .build();
    let render = Render::new(&[Source::Outline]);
    let mut pixels = Vec::new();
    let (mut x, mut y, mut row_height) = (1u32, 1u32, 0u32);
    for &id in &page.rasterized_glyphs {
      let mut glyph = page.glyphs[id as usize];
      let id = id as u16;
      // Inspect the outline before asking the rasterizer to allocate its mask.
      if let Some(outline) = scaler.scale_outline(id) {
        if outline.points().iter().any(|p| {
          !p.x.is_finite()
            || !p.y.is_finite()
            || p.x.abs() > 2044.
            || p.y.abs() > 2044.
        }) {
          return Err(FontError::Limit);
        }
        let bounds = outline.bounds();
        if bounds.width() > 2044. || bounds.height() > 2044. {
          return Err(FontError::Limit);
        }
      }
      if let Some(image) = render.render(&mut scaler, id) {
        let p = image.placement;
        if p.width > ATLAS_WIDTH - 2 || p.height > MAX_ATLAS_HEIGHT - 2 {
          return Err(FontError::Limit);
        }
        if p.width != 0 && p.height != 0 {
          if x + p.width + 1 > ATLAS_WIDTH {
            x = 1;
            y += row_height + 2;
            row_height = 0;
          }
          if y + p.height + 1 > MAX_ATLAS_HEIGHT {
            return Err(FontError::Limit);
          }
          row_height = row_height.max(p.height);
          pixels.resize(((y + row_height + 1) * ATLAS_WIDTH * 4) as usize, 0);
          for row in 0..p.height {
            for column in 0..p.width {
              let alpha = image.data[(row * p.width + column) as usize];
              let at = (((y + row) * ATLAS_WIDTH + x + column) * 4) as usize;
              pixels[at..at + 4].copy_from_slice(&[255, 255, 255, alpha]);
            }
          }
          glyph.rect = [x, y, p.width, p.height];
          glyph.offset = [p.left as f32, -p.top as f32];
          x += p.width + 2;
        }
      }
      page.glyphs[id as usize] = glyph;
    }
    let height = y + row_height + 1;
    pixels.resize((height * ATLAS_WIDTH * 4) as usize, 0);
    page.width = ATLAS_WIDTH;
    page.height = height;
    Ok((page, pixels))
  }

  /// Shape one script/direction run. This is not bidi paragraph resolution,
  /// fallback selection or line layout. Source ranges remain exact UTF-8 bytes.
  pub fn shape(
    &self,
    utf8: &[u32],
    script: u32,
    rtl: bool,
  ) -> Result<Vec<ShapedGlyph>, FontError> {
    if utf8.len() > MAX_RUN_BYTES {
      return Err(FontError::InvalidText);
    }
    let bytes: Vec<u8> = utf8
      .iter()
      .map(|&b| u8::try_from(b).map_err(|_| FontError::InvalidText))
      .collect::<Result<_, _>>()?;
    let text =
      std::str::from_utf8(&bytes).map_err(|_| FontError::InvalidText)?;
    self.shape_span(text, 0..text.len(), script, rtl)
  }

  /// Shape a whole-grapheme span with the surrounding source as shaping context.
  /// Returned byte ranges are relative to the span. To shape at a true text
  /// boundary, pass that bounded source (for example the final line), not a
  /// larger paragraph whose adjoining text would influence contextual forms.
  pub fn shape_span(
    &self,
    source: &str,
    span: Range<usize>,
    script: u32,
    rtl: bool,
  ) -> Result<Vec<ShapedGlyph>, FontError> {
    self.shape_span_with(source, span, script, rtl, FontShapeOptions::default())
  }

  pub fn shape_span_with(
    &self,
    source: &str,
    span: Range<usize>,
    script: u32,
    rtl: bool,
    options: FontShapeOptions<'_>,
  ) -> Result<Vec<ShapedGlyph>, FontError> {
    validate_tags(options.features.iter().map(|f| f.tag))?;
    let language = validate_language(options.language)?;
    let features: Vec<Feature> = options
      .features
      .iter()
      .map(|f| Feature::new(Tag::new(&f.tag.to_be_bytes()), f.value, ..))
      .collect();
    if source.len() > MAX_RUN_BYTES {
      return Err(FontError::InvalidText);
    }
    let text = source.get(span.clone()).ok_or(FontError::InvalidSpan)?;
    for offset in [span.start, span.end] {
      if GraphemeCursor::new(offset, source.len(), true).is_boundary(source, 0)
        != Ok(true)
      {
        return Err(FontError::InvalidSpan);
      }
    }
    let script = shaping_script(script)?;
    let font =
      harfrust::FontRef::from_index(&self.face.data, self.face.face_index)
        .map_err(|_| FontError::InvalidFont)?;
    let shaper = self
      .face
      .shaper_data
      .shaper(&font)
      .instance(Some(&self.face.instance))
      .build();
    let mut buffer = UnicodeBuffer::new();
    let mut boundaries = Vec::new();
    // Use the same Unicode extended-grapheme boundaries as the editor. HarfRust
    // may merge these clusters for ligatures, but must never split one of them.
    for (start, grapheme) in text.grapheme_indices(true) {
      boundaries.push(start as u32);
      for scalar in grapheme.chars() {
        buffer.add(scalar, start as u32);
      }
    }
    boundaries.push(text.len() as u32);
    buffer.set_script(script);
    if let Some(language) = language {
      buffer.set_language(language);
    }
    buffer.set_direction(if rtl {
      Direction::RightToLeft
    } else {
      Direction::LeftToRight
    });
    buffer.set_pre_context(&source[..span.start]);
    buffer.set_post_context(&source[span.end..]);
    let mut flags = BufferFlags::PRODUCE_UNSAFE_TO_CONCAT;
    if span.start == 0 {
      flags |= BufferFlags::BEGINNING_OF_TEXT;
    }
    if span.end == source.len() {
      flags |= BufferFlags::END_OF_TEXT;
    }
    buffer.set_flags(flags);
    let shaped = shaper
      .shape_bounded(
        buffer,
        ShapeOptions::new().features(&features),
        SHAPE_LIMITS,
      )
      .map_err(|_| FontError::ShapingLimit)?;
    let scale = self.metrics[3] / shaper.units_per_em() as f32;
    let mut output = Vec::with_capacity(shaped.len());
    for (info, position) in
      shaped.glyph_infos().iter().zip(shaped.glyph_positions())
    {
      let glyph = ShapedGlyph {
        id: info.glyph_id,
        start: info.cluster,
        end: 0,
        x: position.x_offset as f32 * scale,
        y: -(position.y_offset as f32) * scale,
        advance: position.x_advance as f32 * scale,
        flags: info.flags().to_bits() & 3,
      };
      if glyph.id >= u32::from(self.face.glyph_count)
        || !glyph.x.is_finite()
        || !glyph.y.is_finite()
        || !glyph.advance.is_finite()
        || position.y_advance != 0
      {
        return Err(FontError::InvalidShape);
      }
      output.push(glyph);
    }
    // HarfRust emits visual order. EASL places RTL clusters itself, so restore
    // logical cluster order without changing the order of marks within a cluster.
    if rtl {
      output.reverse();
      for group in output.chunk_by_mut(|a, b| a.start == b.start) {
        group.reverse();
      }
    }
    if !text.is_empty() && output.first().map(|g| g.start) != Some(0) {
      return Err(FontError::InvalidShape);
    }
    let mut boundary = 0;
    let mut index = 0;
    while index < output.len() {
      let start = output[index].start;
      while boundary < boundaries.len() && boundaries[boundary] < start {
        boundary += 1;
      }
      if boundaries.get(boundary) != Some(&start) {
        return Err(FontError::InvalidShape);
      }
      let begin = index;
      while index < output.len() && output[index].start == start {
        index += 1;
      }
      let end = output.get(index).map_or(text.len() as u32, |g| g.start);
      if end <= start || end > text.len() as u32 {
        return Err(FontError::InvalidShape);
      }
      for glyph in &mut output[begin..index] {
        glyph.end = end;
      }
    }
    Ok(output)
  }
}

fn validate_tags(tags: impl Iterator<Item = u32>) -> Result<(), FontError> {
  let mut seen = [0u32; MAX_FONT_SETTINGS];
  for (i, tag) in tags.enumerate() {
    if i == MAX_FONT_SETTINGS
      || !tag.to_be_bytes().iter().all(|b| (32..=126).contains(b))
      || seen[..i].contains(&tag)
    {
      return Err(FontError::InvalidSettings);
    }
    seen[i] = tag;
  }
  Ok(())
}

fn decoration_metrics(
  font: &harfrust::FontRef<'_>,
  coords: &[harfrust::NormalizedCoord],
  scale: f32,
) -> Result<[f32; 4], FontError> {
  // Swash exposes one stroke_size for both decorations. Preserve the distinct
  // post/OS2 values and their MVAR deltas instead of silently conflating them.
  let post = font.post().ok();
  let os2 = font.os2().ok();
  let mut values = [
    post
      .as_ref()
      .map_or(0., |p| f32::from(p.underline_position().to_i16())),
    post
      .as_ref()
      .map_or(0., |p| f32::from(p.underline_thickness().to_i16())),
    os2
      .as_ref()
      .map_or(0., |o| f32::from(o.y_strikeout_position())),
    os2.as_ref().map_or(0., |o| f32::from(o.y_strikeout_size())),
  ];
  let mvar = font.mvar().ok();
  for (value, tag) in
    values.iter_mut().zip([b"undo", b"unds", b"stro", b"strs"])
  {
    if let Some(mvar) = &mvar {
      *value += mvar
        .metric_delta(Tag::new(tag), coords)
        .map_err(|_| FontError::InvalidFont)?
        .to_f32();
    }
    *value *= scale;
    if !value.is_finite() || value.abs() > 16384. {
      return Err(FontError::InvalidFont);
    }
  }
  if values[1] < 0. || values[3] < 0. {
    return Err(FontError::InvalidFont);
  }
  Ok(values)
}

fn validate_load_options(
  size: f32,
  options: FontLoadOptions<'_>,
) -> Result<(), FontError> {
  if !size.is_finite() || !(1. ..=256.).contains(&size) {
    return Err(FontError::InvalidResolution);
  }
  validate_tags(options.variations.iter().map(|v| v.tag))?;
  if options
    .variations
    .iter()
    .any(|v| !v.value.is_finite() || v.value.abs() > 1_000_000.)
  {
    return Err(FontError::InvalidSettings);
  }
  Ok(())
}

fn validate_language(language: &str) -> Result<Option<Language>, FontError> {
  if language.is_empty() {
    return Ok(None);
  }
  if language.len() > 64
    || language.split('-').enumerate().any(|(i, part)| {
      part.is_empty()
        || part.len() > 8
        || !part.bytes().all(|b| {
          if i == 0 {
            b.is_ascii_alphabetic()
          } else {
            b.is_ascii_alphanumeric()
          }
        })
    })
  {
    return Err(FontError::InvalidLanguage);
  }
  Ok(Language::new(language))
}

fn shaping_script(tag: u32) -> Result<Script, FontError> {
  use icu_properties::{PropertyParser, props::Script as UnicodeScript};
  let mut name = match &tag.to_be_bytes() {
    b"lao " => *b"Laoo",
    b"nko " => *b"Nkoo",
    b"vai " => *b"Vaii",
    b"yi  " => *b"Yiii",
    other => *other,
  };
  name[0].make_ascii_uppercase();
  let code = PropertyParser::<UnicodeScript>::new()
    .get_strict_u16_utf8(&name)
    .ok_or(FontError::InvalidScript)?;
  // A four-letter tag alone is not evidence of a known concrete script.
  if crate::text::script_tag(u32::from(code)).ok() != Some(tag) {
    return Err(FontError::InvalidScript);
  }
  Script::from_iso15924_tag(harfrust::Tag::new(&name))
    .ok_or(FontError::InvalidScript)
}
