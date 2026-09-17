//! Typed transport for EASL rich-style resolution. Strings stay in host-owned
//! tables; neither manuscript bytes nor document/history authority enter the VM.
use crate::{Error, runtime::Runtime};
use easl_native_text::{MAX_EDITOR_SPANS, MAX_TEXT_BYTES, StyledSpan, TextStyle};
use std::{
    collections::HashMap,
    ops::{BitOr, Range},
};

pub const STYLES_SOURCE: &str = include_str!("../library/styles.easl");
const STYLE_WORDS: usize = 14;
const SPAN_WORDS: usize = STYLE_WORDS + 2;

/// Fields replaced by a selected rule. All other properties survive unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StyleProperties(u32);
impl StyleProperties {
    pub const FAMILY: Self = Self(1);
    pub const SIZE: Self = Self(2);
    pub const LINE_HEIGHT: Self = Self(4);
    pub const WEIGHT: Self = Self(8);
    pub const ITALIC: Self = Self(16);
    pub const LETTER_SPACING: Self = Self(32);
    pub const WORD_SPACING: Self = Self(64);
    pub const FEATURES: Self = Self(128);
    pub const VARIATIONS: Self = Self(256);
    pub const LANGUAGE: Self = Self(512);
    pub const COLOR: Self = Self(1024);
    pub const UNDERLINE: Self = Self(2048);
    pub const STRIKE: Self = Self(4096);
    pub const KEEP_ALL: Self = Self(8192);
    pub const WRAP: Self = Self(16384);
    pub const ALL: Self = Self(32767);
}
impl BitOr for StyleProperties {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// A unique single-bit selector, replacement fields and index into the style
/// table. Selected rules apply in order; later rules win only their own fields.
#[derive(Clone, Copy, Debug)]
pub struct StyleRule {
    pub flag: u32,
    pub properties: StyleProperties,
    pub style: u32,
}

/// One ordered source/display range, base style index, and selected rule bits.
#[derive(Clone, Debug)]
pub struct StyleInput {
    pub range: Range<usize>,
    pub style: u32,
    pub flags: u32,
}

pub struct Styling {
    runtime: Runtime,
    faulted: bool,
    pub resolutions: u64,
}
impl std::fmt::Debug for Styling {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Styling")
            .field("faulted", &self.faulted)
            .field("resolutions", &self.resolutions)
            .finish_non_exhaustive()
    }
}
impl Styling {
    pub fn new() -> Result<Self, Error> {
        let runtime = std::panic::catch_unwind(|| {
            Runtime::compile(&[
                (
                    "native_styles.easl",
                    include_str!("../library/native_styles.easl"),
                ),
                ("styles.easl", STYLES_SOURCE),
            ])
        })
        .map_err(|_| Error::Language("Style compiler panicked".into()))??;
        Ok(Self {
            runtime,
            faulted: false,
            resolutions: 0,
        })
    }

    /// Resolve and coalesce rich styles without editing source. At most 4,096
    /// styles, 32 rules and MAX_EDITOR_SPANS nonoverlapping ranges are admitted.
    /// Gaps retain the consumer's default style; empty ranges preserve insertion
    /// styles. UTF-8 scalar boundaries are checked here; grapheme shaping is a
    /// later step. Failed input/VM work returns no replacement style list.
    pub fn resolve(
        &mut self,
        source: &str,
        styles: &[TextStyle],
        rules: &[StyleRule],
        spans: &[StyleInput],
    ) -> Result<Vec<StyledSpan>, Error> {
        if self.faulted {
            return Err(Error::Faulted);
        }
        if source.len() > MAX_TEXT_BYTES
            || styles.len() > 4096
            || rules.len() > 32
            || spans.len() > MAX_EDITOR_SPANS
        {
            return Err(Error::Limit);
        }
        for style in styles {
            style.validate()?;
        }
        for span in spans {
            if !source.is_char_boundary(span.range.start)
                || !source.is_char_boundary(span.range.end)
            {
                return Err(Error::Invalid);
            }
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.resolve_inner(source, styles, rules, spans)
        }));
        match result {
            Ok(Ok(output)) => {
                self.resolutions = self.resolutions.saturating_add(1);
                Ok(output)
            }
            Ok(Err(error @ Error::Language(_))) => {
                self.faulted = true;
                Err(error)
            }
            Ok(Err(error)) => Err(error),
            Err(_) => {
                self.faulted = true;
                Err(Error::Language("Style VM panicked".into()))
            }
        }
    }

    fn resolve_inner(
        &mut self,
        source: &str,
        styles: &[TextStyle],
        rules: &[StyleRule],
        spans: &[StyleInput],
    ) -> Result<Vec<StyledSpan>, Error> {
        let mut strings = Strings::default();
        let mut values = Vec::with_capacity(styles.len() * STYLE_WORDS);
        for style in styles {
            values.extend(encode(style, &mut strings)?);
        }
        let mut inputs = Vec::with_capacity(spans.len() * 4);
        for span in spans {
            inputs.extend([
                u32::try_from(span.range.start).map_err(|_| Error::Limit)?,
                u32::try_from(span.range.end).map_err(|_| Error::Limit)?,
                span.style,
                span.flags,
            ]);
        }
        let rules: Vec<_> = rules
            .iter()
            .flat_map(|r| [r.flag, r.properties.0, r.style])
            .collect();
        self.runtime.write("style-values", &values)?;
        self.runtime.write("style-rules", &rules)?;
        self.runtime.write("style-inputs", &inputs)?;
        self.runtime
            .write("style-output", &vec![0; spans.len() * SPAN_WORDS])?;
        self.runtime.write(
            "style-bytes",
            &[u32::try_from(source.len()).map_err(|_| Error::Limit)?],
        )?;
        self.runtime.write("style-result", &[u32::MAX, 0, 0])?;
        self.runtime.run("resolve-native-styles")?;
        let result = self.runtime.read("style-result")?;
        let [status, count, _work] = result.as_slice() else {
            return Err(Error::Invalid);
        };
        match status {
            0 => {}
            1 => return Err(Error::Invalid),
            2 => return Err(Error::Limit),
            _ => return Err(Error::Invalid),
        }
        if *count as usize > spans.len() {
            return Err(Error::Invalid);
        }
        let words = self.runtime.read("style-output")?;
        if words.len() != spans.len() * SPAN_WORDS {
            return Err(Error::Invalid);
        }
        let mut output = Vec::with_capacity(*count as usize);
        let mut previous = 0;
        for words in words.chunks_exact(SPAN_WORDS).take(*count as usize) {
            let range = words[0] as usize..words[1] as usize;
            if range.start < previous
                || range.start > range.end
                || !source.is_char_boundary(range.start)
                || !source.is_char_boundary(range.end)
            {
                return Err(Error::Invalid);
            }
            previous = range.end;
            output.push(StyledSpan {
                range,
                style: decode(&words[2..], &strings)?,
            });
        }
        Ok(output)
    }
}

#[derive(Default)]
struct Strings<'a> {
    values: Vec<&'a str>,
    indices: HashMap<&'a str, u32>,
}
impl<'a> Strings<'a> {
    fn intern(&mut self, value: &'a str) -> Result<u32, Error> {
        if let Some(&index) = self.indices.get(value) {
            return Ok(index);
        }
        let index = u32::try_from(self.values.len()).map_err(|_| Error::Limit)?;
        self.values.push(value);
        self.indices.insert(value, index);
        Ok(index)
    }
    fn get(&self, index: u32) -> Result<String, Error> {
        self.values
            .get(index as usize)
            .map(|s| (*s).to_owned())
            .ok_or(Error::Invalid)
    }
}
fn encode<'a>(s: &'a TextStyle, strings: &mut Strings<'a>) -> Result<[u32; STYLE_WORDS], Error> {
    Ok([
        strings.intern(&s.family)?,
        s.size.to_bits(),
        s.line_height.to_bits(),
        s.weight.to_bits(),
        s.letter_spacing.to_bits(),
        s.word_spacing.to_bits(),
        strings.intern(&s.features)?,
        strings.intern(&s.variations)?,
        s.locale
            .as_deref()
            .map(|s| strings.intern(s))
            .transpose()?
            .unwrap_or(u32::MAX),
        u32::from(s.color[0]),
        u32::from(s.color[1]),
        u32::from(s.color[2]),
        u32::from(s.color[3]),
        u32::from(s.italic)
            | (u32::from(s.underline) << 1)
            | (u32::from(s.strike) << 2)
            | (u32::from(s.keep_all) << 3)
            | (u32::from(s.wrap) << 4),
    ])
}
fn decode(words: &[u32], strings: &Strings<'_>) -> Result<TextStyle, Error> {
    let [
        family,
        size,
        height,
        weight,
        letter,
        word,
        features,
        variations,
        language,
        r,
        g,
        b,
        a,
        flags,
    ] = words
    else {
        return Err(Error::Invalid);
    };
    if *flags > 31 {
        return Err(Error::Invalid);
    }
    let mut color = [0; 4];
    for (component, value) in color.iter_mut().zip([*r, *g, *b, *a]) {
        *component = u8::try_from(value).map_err(|_| Error::Invalid)?;
    }
    let style = TextStyle {
        family: strings.get(*family)?,
        size: f32::from_bits(*size),
        line_height: f32::from_bits(*height),
        weight: f32::from_bits(*weight),
        letter_spacing: f32::from_bits(*letter),
        word_spacing: f32::from_bits(*word),
        features: strings.get(*features)?,
        variations: strings.get(*variations)?,
        locale: if *language == u32::MAX {
            None
        } else {
            Some(strings.get(*language)?)
        },
        color,
        italic: flags & 1 != 0,
        underline: flags & 2 != 0,
        strike: flags & 4 != 0,
        keep_all: flags & 8 != 0,
        wrap: flags & 16 != 0,
    };
    style.validate()?;
    Ok(style)
}
