//! Ordered OS input facts for EASL editor libraries. A frame is immutable while
//! EASL evaluates it; text and preedit offsets always count UTF-8 bytes.
#![forbid(unsafe_code)]

const MAX_EVENTS: usize = 1024;
const MAX_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum InputKind {
  Text = 0,
  KeyDown = 1,
  KeyUp = 2,
  Preedit = 3,
  ImeEnabled = 4,
  ImeDisabled = 5,
  Blur = 6,
}

/// Shift=1, control=2, alt=4, super/command=8, repeat=16.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputEvent {
  pub kind: InputKind,
  pub key: u32,
  pub modifiers: u32,
  pub text_start: u32,
  pub text_end: u32,
  pub cursor_start: u32,
  pub cursor_end: u32,
}

#[derive(Clone, Debug, Default)]
pub struct InputFrame {
  events: Vec<InputEvent>,
  text: Vec<u32>,
  failed: bool,
}

impl InputFrame {
  pub fn clear(&mut self) {
    self.events.clear();
    self.text.clear();
    self.failed = false;
  }

  /// Reject the whole frame on overflow/invalid preedit. Never silently apply
  /// a prefix of a composition or an incomplete UTF-8 sequence.
  pub fn push(
    &mut self,
    kind: InputKind,
    key: u32,
    modifiers: u32,
    text: &str,
    cursor: Option<(usize, usize)>,
  ) {
    if self.failed {
      return;
    }
    if self.events.len() == MAX_EVENTS
      || text.len() > MAX_BYTES.saturating_sub(self.text.len())
      || cursor.is_some_and(|(a, b)| {
        a > b || !text.is_char_boundary(a) || !text.is_char_boundary(b)
      })
    {
      self.failed = true;
      return;
    }
    let start = self.text.len() as u32;
    self.text.extend(text.bytes().map(u32::from));
    self.events.push(InputEvent {
      kind,
      key,
      modifiers,
      text_start: start,
      text_end: self.text.len() as u32,
      cursor_start: cursor.map_or(u32::MAX, |(a, _)| a as u32),
      cursor_end: cursor.map_or(u32::MAX, |(_, b)| b as u32),
    });
  }

  /// Check bounds without copying a potentially large text payload.
  pub fn validate(&self) -> Result<(), &'static str> {
    if self.failed {
      Err(
        "Text input frame exceeded its bounds or contained invalid preedit offsets",
      )
    } else {
      Ok(())
    }
  }

  pub fn event_count(&self) -> usize {
    self.events.len()
  }

  pub fn text_byte_len(&self) -> usize {
    self.text.len()
  }

  pub fn words(&self, text: bool) -> Result<Vec<u32>, &'static str> {
    self.validate()?;
    Ok(if text {
      self.text.clone()
    } else {
      self
        .events
        .iter()
        .flat_map(|e| {
          [
            e.kind as u32,
            e.key,
            e.modifiers,
            e.text_start,
            e.text_end,
            e.cursor_start,
            e.cursor_end,
          ]
        })
        .collect()
    })
  }
}

/// Editor command keys. Character insertion comes from committed text, never
/// from these key codes. Unknown keys are retained as key code zero.
pub fn editing_key(name: &str) -> u32 {
  match name {
    "backspace" => 1,
    "delete" => 2,
    "arrowleft" => 3,
    "arrowright" => 4,
    "arrowup" => 5,
    "arrowdown" => 6,
    "home" => 7,
    "end" => 8,
    "pageup" => 9,
    "pagedown" => 10,
    "enter" => 11,
    "tab" => 12,
    "escape" => 13,
    "a" => 14,
    "c" => 15,
    "v" => 16,
    "x" => 17,
    "z" => 18,
    "y" => 19,
    _ => 0,
  }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InputArea {
  pub enabled: bool,
  pub rect: [f32; 4],
}
impl InputArea {
  pub fn new(enabled: bool, rect: [f32; 4]) -> Result<Self, &'static str> {
    if rect.iter().any(|v| !v.is_finite() || v.abs() > 1_000_000.)
      || rect[2] < 0.
      || rect[3] < 0.
    {
      return Err(
        "Text input area must have finite coordinates and nonnegative dimensions",
      );
    }
    Ok(Self { enabled, rect })
  }
}
