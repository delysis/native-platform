use crate::Error;
use parley::{
    FontFamily, FontFeatures, FontStyle, FontVariations, FontWeight, LineHeight, OverflowWrap,
    StyleProperty, TextWrapMode, WordBreak,
};
use serde::{Deserialize, Serialize};
use std::{borrow::Cow, ops::Range};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum WhiteSpace {
    Collapse,
    #[default]
    Preserve,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Alignment {
    #[default]
    Start,
    Center,
    End,
    Justify,
}
impl From<Alignment> for parley::Alignment {
    fn from(value: Alignment) -> Self {
        match value {
            Alignment::Start => Self::Start,
            Alignment::Center => Self::Center,
            Alignment::End => Self::End,
            Alignment::Justify => Self::Justify,
        }
    }
}
/// Values use logical pixels; the host applies display scale only when painting.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TextStyle {
    pub family: String,
    pub size: f32,
    pub line_height: f32,
    pub weight: f32,
    pub italic: bool,
    pub letter_spacing: f32,
    pub word_spacing: f32,
    pub features: String,
    pub variations: String,
    pub locale: Option<String>,
    pub color: [u8; 4],
    pub underline: bool,
    pub strike: bool,
    pub keep_all: bool,
    pub wrap: bool,
}
impl Default for TextStyle {
    fn default() -> Self {
        Self {
            family: "serif".into(),
            size: 20.,
            line_height: 33.,
            weight: 400.,
            italic: false,
            letter_spacing: 0.,
            word_spacing: 0.,
            features: "'kern' 1, 'liga' 1, 'clig' 1, 'calt' 1".into(),
            variations: String::new(),
            locale: None,
            color: [46, 54, 46, 255],
            underline: false,
            strike: false,
            keep_all: false,
            wrap: true,
        }
    }
}
impl TextStyle {
    pub fn validate(&self) -> Result<(), Error> {
        for (value, range) in [
            (self.size, 1. ..=512.),
            (self.line_height, 1. ..=2048.),
            (self.weight, 1. ..=1000.),
            (self.letter_spacing, -128. ..=512.),
            (self.word_spacing, -128. ..=512.),
        ] {
            if !value.is_finite() || !range.contains(&value) {
                return Err(Error::InvalidStyle);
            }
        }
        if self.family.len() > 1024
            || self.features.len() > 1024
            || self.variations.len() > 1024
            || self
                .locale
                .as_ref()
                .is_some_and(|s| s.len() > 64 || s.parse::<parley::Language>().is_err())
        {
            return Err(Error::InvalidStyle);
        }
        if parley::FontFeature::parse_css_list(&self.features).any(|v| v.is_err())
            || parley::FontVariation::parse_css_list(&self.variations)
                .any(|v| v.is_err() || v.is_ok_and(|v| !v.value.is_finite() || v.value.abs() > 1e6))
        {
            return Err(Error::InvalidStyle);
        }
        Ok(())
    }
    pub fn properties(&self) -> Vec<StyleProperty<'static, [u8; 4]>> {
        vec![
            StyleProperty::FontFamily(FontFamily::Source(Cow::Owned(self.family.clone()))),
            StyleProperty::FontSize(self.size),
            StyleProperty::LineHeight(LineHeight::Absolute(self.line_height)),
            StyleProperty::FontWeight(FontWeight::new(self.weight)),
            StyleProperty::FontStyle(if self.italic {
                FontStyle::Italic
            } else {
                FontStyle::Normal
            }),
            StyleProperty::LetterSpacing(self.letter_spacing),
            StyleProperty::WordSpacing(self.word_spacing),
            StyleProperty::FontFeatures(FontFeatures::Source(Cow::Owned(self.features.clone()))),
            StyleProperty::FontVariations(FontVariations::Source(Cow::Owned(
                self.variations.clone(),
            ))),
            StyleProperty::Locale(self.locale.as_ref().and_then(|s| s.parse().ok())),
            StyleProperty::Brush(self.color),
            StyleProperty::Underline(self.underline),
            StyleProperty::Strikethrough(self.strike),
            StyleProperty::WordBreak(if self.keep_all {
                WordBreak::KeepAll
            } else {
                WordBreak::Normal
            }),
            StyleProperty::OverflowWrap(OverflowWrap::Anywhere),
            StyleProperty::TextWrapMode(if self.wrap {
                TextWrapMode::Wrap
            } else {
                TextWrapMode::NoWrap
            }),
        ]
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct StyledSpan {
    pub range: Range<usize>,
    pub style: TextStyle,
}
/// An atomic native element participating in text flow (e.g. a chip or image).
#[derive(Clone, Debug)]
pub struct InlineBox {
    pub id: u64,
    pub index: usize,
    pub width: f32,
    pub height: f32,
}
