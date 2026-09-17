//! Immutable UTF-8 values for language-owned editors. No files, OS commands,
//! selection state or history policy. Handles belong to one evaluator and must
//! be released explicitly; zero is the permanent empty value.
#![forbid(unsafe_code)]

use icu_segmenter::LineSegmenter;
use std::{collections::HashMap, sync::Arc};
use unicode_segmentation::{GraphemeCursor, UnicodeSegmentation};

pub const MAX_TEXT_BYTES: usize = 1024 * 1024;
const MAX_RETAINED_BYTES: usize = 128 * MAX_TEXT_BYTES;
const MAX_HANDLES: usize = 16_384;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TextError {
  #[error(
    "Text exceeds the 1 MiB value or 128 MiB/16384-handle evaluator limit"
  )]
  Limit,
  #[error("Text range must be ordered UTF-8 character boundaries")]
  Range,
  #[error("Invalid or released text handle")]
  Handle,
  #[error("Text input is not valid UTF-8")]
  Utf8,
  #[error("Unknown text boundary mode")]
  BoundaryMode,
  #[error("Unicode boundary lookup failed")]
  UnicodeBoundary,
  #[error("Incorrect text operation arguments")]
  Arguments,
  #[error("Unsupported Unicode script code or property data")]
  ScriptCode,
}

/// Exact scalar/extended-grapheme ranges and Unicode Script_Extensions facts.
/// Each 16-word record contains start/end, grapheme start/end, scalar, ICU
/// UScriptCode, bracket kind (0/1 open/2 close), canonical opening bracket,
/// then a 256-bit set indexed by UScriptCode. These are properties, not runs.
/// Keep Common/Inherited/Unknown in the raw set: EASL owns their interpretation.
pub fn script_words(text: &str) -> Result<Vec<u32>, TextError> {
  use icu_properties::{
    CodePointMapData,
    props::{BidiMirroringGlyph, BidiPairedBracketType},
    script::ScriptWithExtensions,
  };
  if text.len() > MAX_TEXT_BYTES {
    return Err(TextError::Limit);
  }
  let scripts = ScriptWithExtensions::new();
  let brackets = CodePointMapData::<BidiMirroringGlyph>::new();
  let mut words = Vec::with_capacity(text.chars().count() * 16);
  for (cluster, grapheme) in text.grapheme_indices(true) {
    for (relative, scalar) in grapheme.char_indices() {
      let start = cluster + relative;
      let primary = u32::from(scripts.get_script_val(scalar).to_icu4c_value());
      if primary >= 256 {
        return Err(TextError::ScriptCode);
      }
      let mut extensions = [0u32; 8];
      for script in scripts.get_script_extensions_val(scalar).iter() {
        let code = usize::from(script.to_icu4c_value());
        let word =
          extensions.get_mut(code / 32).ok_or(TextError::ScriptCode)?;
        *word |= 1 << (code % 32);
      }
      let bracket = brackets.get(scalar);
      let (kind, pair) = match bracket.paired_bracket_type {
        BidiPairedBracketType::Open => (1, scalar as u32),
        BidiPairedBracketType::Close => (
          2,
          bracket.mirroring_glyph.ok_or(TextError::ScriptCode)? as u32,
        ),
        BidiPairedBracketType::None => (0, 0),
        _ => return Err(TextError::ScriptCode),
      };
      // U+2329 and U+3008 are canonically equivalent opening brackets.
      let pair = if pair == 0x2329 { 0x3008 } else { pair };
      words.extend([
        start as u32,
        (start + scalar.len_utf8()) as u32,
        cluster as u32,
        (cluster + grapheme.len()) as u32,
        scalar as u32,
        primary,
        kind,
        pair,
      ]);
      words.extend(extensions);
    }
  }
  Ok(words)
}

/// Convert a concrete Unicode UScriptCode to the legacy OpenType script tag
/// accepted by shape-font-run. A property value does not promise font coverage
/// or shaper support. Implicit and unrecognized values fail explicitly.
pub fn script_tag(code: u32) -> Result<u32, TextError> {
  use icu_properties::{PropertyNamesShort, props::Script};
  if code >= 256 || matches!(code, 0 | 1 | 103) {
    return Err(TextError::ScriptCode);
  }
  let names = PropertyNamesShort::<Script>::new();
  let name = names
    .get(Script::from_icu4c_value(code as u16))
    .ok_or(TextError::ScriptCode)?;
  let tag = match name {
    "Hira" | "Kana" => *b"kana",
    "Laoo" => *b"lao ",
    "Nkoo" => *b"nko ",
    "Vaii" => *b"vai ",
    "Yiii" => *b"yi  ",
    _ => {
      let mut tag: [u8; 4] = name
        .as_bytes()
        .try_into()
        .map_err(|_| TextError::ScriptCode)?;
      tag.make_ascii_lowercase();
      tag
    }
  };
  Ok(u32::from_be_bytes(tag))
}

#[derive(Clone, Copy, Debug)]
pub enum TextOp {
  Length,
  Retain,
  Release,
  Slice,
  Replace,
  Boundary,
  Equal,
}

pub fn operation(name: &str) -> Option<TextOp> {
  Some(match name {
    "text-length" => TextOp::Length,
    "text-retain" => TextOp::Retain,
    "text-release" => TextOp::Release,
    "text-slice" => TextOp::Slice,
    "text-replace" => TextOp::Replace,
    "text-boundary" => TextOp::Boundary,
    "text-equal" => TextOp::Equal,
    _ => return None,
  })
}

#[derive(Default)]
pub struct TextValues {
  values: HashMap<u32, Arc<str>>,
  next: u32,
  retained_bytes: usize,
}

impl TextValues {
  pub fn get(&self, key: u32) -> Result<&str, TextError> {
    if key == 0 {
      return Ok("");
    }
    self
      .values
      .get(&key)
      .map(AsRef::as_ref)
      .ok_or(TextError::Handle)
  }

  pub fn insert(&mut self, text: &str) -> Result<u32, TextError> {
    if text.is_empty() {
      return Ok(0);
    }
    self.reserve(text.len())?;
    self.store(Arc::from(text))
  }

  pub fn insert_utf8(
    &mut self,
    words: &[u32],
    start: u32,
    end: u32,
  ) -> Result<u32, TextError> {
    if words.len() > MAX_TEXT_BYTES {
      return Err(TextError::Limit);
    }
    let words = words
      .get(start as usize..end as usize)
      .ok_or(TextError::Range)?;
    if words.is_empty() {
      return Ok(0);
    }
    self.reserve(words.len())?;
    let bytes = words
      .iter()
      .map(|&b| u8::try_from(b).map_err(|_| TextError::Utf8))
      .collect::<Result<Vec<_>, _>>()?;
    self.insert(std::str::from_utf8(&bytes).map_err(|_| TextError::Utf8)?)
  }

  fn reserve(&self, length: usize) -> Result<(), TextError> {
    if length == 0 {
      return Ok(());
    }
    if length > MAX_TEXT_BYTES
      || length > MAX_RETAINED_BYTES - self.retained_bytes
      || self.values.len() >= MAX_HANDLES
      || self.next == u32::MAX
    {
      return Err(TextError::Limit);
    }
    Ok(())
  }

  fn store(&mut self, text: Arc<str>) -> Result<u32, TextError> {
    if text.is_empty() {
      return Ok(0);
    }
    self.reserve(text.len())?;
    self.next += 1; // never reuse IDs: stale handles cannot alias a newer value
    self.retained_bytes += text.len();
    self.values.insert(self.next, text);
    Ok(self.next)
  }

  pub fn run(&mut self, op: TextOp, args: &[u32]) -> Result<u32, TextError> {
    let arity = match op {
      TextOp::Length | TextOp::Retain | TextOp::Release => 1,
      TextOp::Equal => 2,
      TextOp::Slice | TextOp::Boundary => 3,
      TextOp::Replace => 4,
    };
    if args.len() != arity {
      return Err(TextError::Arguments);
    }
    let text = self.get(args[0])?;
    match op {
      TextOp::Length => Ok(text.len() as u32),
      TextOp::Equal => Ok(u32::from(text == self.get(args[1])?)),
      TextOp::Retain => {
        let text = self.values.get(&args[0]).cloned();
        text.map_or(Ok(0), |text| self.store(text))
      }
      TextOp::Release => {
        if let Some(text) = self.values.remove(&args[0]) {
          self.retained_bytes -= text.len();
        }
        Ok(0)
      }
      TextOp::Slice => {
        let slice = text
          .get(args[1] as usize..args[2] as usize)
          .ok_or(TextError::Range)?;
        self.reserve(slice.len())?;
        let slice: Arc<str> = Arc::from(slice);
        self.store(slice)
      }
      TextOp::Replace => {
        let (start, end) = (args[1] as usize, args[2] as usize);
        text.get(start..end).ok_or(TextError::Range)?;
        let insert = self.get(args[3])?;
        let length = text.len() - (end - start) + insert.len();
        self.reserve(length)?;
        let mut result = String::with_capacity(length);
        result.push_str(&text[..start]);
        result.push_str(insert);
        result.push_str(&text[end..]);
        self.store(Arc::from(result))
      }
      TextOp::Boundary => {
        boundary(text, args[1] as usize, args[2]).map(|i| i as u32)
      }
    }
  }

  pub fn live_values(&self) -> usize {
    self.values.len()
  }
  pub fn retained_bytes(&self) -> usize {
    self.retained_bytes
  }
}

/// Bulk Unicode facts, in logical source order: start/end UTF-8 bytes, the
/// first scalar, and break-after (0 prohibited, 1 opportunity, 2 hard break).
/// EOF is an opportunity, not a hard break. CRLF is one extended grapheme.
/// Layout, whitespace treatment and emergency breaking remain language policy.
/// The input comes from a bounded TextBuffer; no retained handles are allocated.
pub fn grapheme_words(text: &str) -> Vec<u32> {
  let segmenter = LineSegmenter::new_auto(Default::default());
  let mut breaks = segmenter.segment_str(text).peekable();
  let mut words = Vec::new();
  for (start, grapheme) in text.grapheme_indices(true) {
    let end = start + grapheme.len();
    while breaks.peek().is_some_and(|&i| i < end) {
      breaks.next();
    }
    let scalar = grapheme.chars().next().expect("nonempty grapheme");
    let kind = if hard_break(scalar) {
      2
    } else {
      u32::from(breaks.peek() == Some(&end))
    };
    words.extend([start as u32, end as u32, scalar as u32, kind]);
  }
  words
}

/// Paragraph-resolved Unicode facts, in original scalar/UTF-8 order. Native
/// analysis does not select lines, reorder text, resolve script runs or shape.
/// `direction`: 0 automatic, 1 LTR, 2 RTL. Record words are start/end/scalar,
/// resolved level, paragraph start/base, L1 reset class, raw OpenType script.
/// Reset classes: 0 ordinary, 1 whitespace/isolate, 2 separator, 3 retained X9.
/// Script zero denotes Common/Inherited/Unknown; consumers resolve context.
pub fn directional_words(
  text: &str,
  direction: u32,
) -> Result<Vec<u32>, TextError> {
  use icu_properties::{props::Script, script::ScriptWithExtensions};
  use unicode_bidi::{BidiClass, BidiInfo, Level};
  if text.len() > MAX_TEXT_BYTES {
    return Err(TextError::Limit);
  }
  let base = match direction {
    0 => None,
    1 => Some(Level::ltr()),
    2 => Some(Level::rtl()),
    _ => return Err(TextError::Arguments),
  };
  let bidi = BidiInfo::new(text, base);
  let scripts = ScriptWithExtensions::new();
  let mut words = Vec::with_capacity(text.chars().count() * 8);
  for paragraph in &bidi.paragraphs {
    for (relative, scalar) in text[paragraph.range.clone()].char_indices() {
      let start = paragraph.range.start + relative;
      let reset = match bidi.original_classes[start] {
        BidiClass::B | BidiClass::S => 2,
        BidiClass::WS
        | BidiClass::FSI
        | BidiClass::LRI
        | BidiClass::RLI
        | BidiClass::PDI => 1,
        BidiClass::RLE
        | BidiClass::LRE
        | BidiClass::RLO
        | BidiClass::LRO
        | BidiClass::PDF
        | BidiClass::BN => 3,
        _ => 0,
      };
      let script = match scripts.get_script_val(scalar) {
        Script::Common | Script::Inherited | Script::Unknown => 0,
        script => script_tag(u32::from(script.to_icu4c_value()))?,
      };
      words.extend([
        start as u32,
        (start + scalar.len_utf8()) as u32,
        scalar as u32,
        u32::from(bidi.levels[start].number()),
        paragraph.range.start as u32,
        u32::from(paragraph.level.number()),
        reset,
        script,
      ]);
    }
  }
  Ok(words)
}

fn hard_break(c: char) -> bool {
  matches!(
    c,
    '\r' | '\n' | '\u{b}' | '\u{c}' | '\u{85}' | '\u{2028}' | '\u{2029}'
  )
}

/// Modes: previous/next/floor/ceil grapheme, previous/next UAX29 word boundary,
/// logical line start/end, floor UTF-8 character. These return Unicode facts,
/// never mutate a selection.
fn boundary(
  text: &str,
  position: usize,
  mode: u32,
) -> Result<usize, TextError> {
  if position > text.len() {
    return Err(TextError::Range);
  }
  Ok(match mode {
    0..=3 => grapheme_boundary(text, position, mode)?,
    4 => {
      let word = text
        .split_word_bound_indices()
        .map(|(i, _)| i)
        .take_while(|&i| i < position)
        .last()
        .unwrap_or(0);
      grapheme_boundary(text, word, 3)?
    }
    5 => {
      let word = text
        .split_word_bound_indices()
        .map(|(i, _)| i)
        .find(|&i| i > position)
        .unwrap_or(text.len());
      grapheme_boundary(text, word, 2)?
    }
    6 | 7 => {
      if !text.is_char_boundary(position) {
        return Err(TextError::Range);
      }
      let newline = hard_break;
      if mode == 6 {
        text[..position]
          .rmatch_indices(newline)
          .next()
          .map_or(0, |(i, s)| i + s.len())
      } else {
        text[position..]
          .find(newline)
          .map_or(text.len(), |i| position + i)
      }
    }
    8 => floor_character(text, position),
    _ => return Err(TextError::BoundaryMode),
  })
}

fn floor_character(text: &str, mut position: usize) -> usize {
  while !text.is_char_boundary(position) {
    position -= 1;
  }
  position
}

// A full immutable UTF-8 chunk supplies all lookbehind context. Ordinary caret
// queries examine the adjacent cluster instead of scanning from byte zero.
fn grapheme_boundary(
  text: &str,
  position: usize,
  mode: u32,
) -> Result<usize, TextError> {
  let aligned = floor_character(text, position);
  let mut cursor = GraphemeCursor::new(aligned, text.len(), true);
  let at_boundary = cursor
    .is_boundary(text, 0)
    .map_err(|_| TextError::UnicodeBoundary)?;
  if at_boundary
    && (mode == 2
      || (mode == 3 && aligned == position)
      || (mode == 0 && aligned < position))
  {
    return Ok(aligned);
  }
  if mode == 0 || mode == 2 {
    cursor
      .prev_boundary(text, 0)
      .map(|i| i.unwrap_or(0))
      .map_err(|_| TextError::UnicodeBoundary)
  } else {
    cursor
      .next_boundary(text, 0)
      .map(|i| i.unwrap_or(text.len()))
      .map_err(|_| TextError::UnicodeBoundary)
  }
}
