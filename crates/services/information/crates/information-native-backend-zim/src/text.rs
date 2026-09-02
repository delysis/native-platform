use crate::{ZimError, ZimTextKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExtractedText {
    pub(crate) text: String,
    pub(crate) truncated: bool,
}

pub(crate) fn extract_inert_text(
    bytes: &[u8],
    kind: ZimTextKind,
    max_output_bytes: usize,
) -> Result<ExtractedText, ZimError> {
    let input = std::str::from_utf8(bytes).map_err(|_| ZimError::InvalidArticleUtf8)?;
    let mut output = TextCollector::new(max_output_bytes);
    match kind {
        ZimTextKind::PlainText => output.push_text(input),
        ZimTextKind::Html => extract_html(input, &mut output)?,
    }
    Ok(output.finish())
}

fn extract_html(input: &str, output: &mut TextCollector) -> Result<(), ZimError> {
    let mut cursor = 0_usize;
    let mut suppressed: Option<String> = None;

    while cursor < input.len() {
        let Some(relative_open) = input[cursor..].find('<') else {
            if suppressed.is_none() {
                output.push_text(&input[cursor..]);
            }
            break;
        };
        let open = cursor
            .checked_add(relative_open)
            .ok_or(ZimError::IntegerOverflow)?;
        if suppressed.is_none() {
            output.push_text(&input[cursor..open]);
        }

        if input[open..].starts_with("<!--") {
            let comment_body = open.checked_add(4).ok_or(ZimError::IntegerOverflow)?;
            let Some(relative_close) = input[comment_body..].find("-->") else {
                return Err(ZimError::MalformedHtml);
            };
            cursor = comment_body
                .checked_add(relative_close)
                .and_then(|value| value.checked_add(3))
                .ok_or(ZimError::IntegerOverflow)?;
            continue;
        }

        let Some(relative_close) = input[open..].find('>') else {
            return Err(ZimError::MalformedHtml);
        };
        let close = open
            .checked_add(relative_close)
            .ok_or(ZimError::IntegerOverflow)?;
        let tag_body = input[open + 1..close].trim();
        let closing = tag_body.starts_with('/');
        let tag_name = tag_body
            .trim_start_matches('/')
            .trim_start()
            .split(|character: char| character.is_ascii_whitespace() || character == '/')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();

        if let Some(name) = &suppressed {
            if closing && tag_name == *name {
                suppressed = None;
            }
        } else if !closing && is_suppressed_tag(&tag_name) {
            suppressed = Some(tag_name);
        } else if is_block_tag(&tag_name) || tag_name == "br" {
            output.push_break();
        }

        cursor = close.checked_add(1).ok_or(ZimError::IntegerOverflow)?;
    }
    Ok(())
}

fn is_suppressed_tag(name: &str) -> bool {
    matches!(name, "script" | "style" | "template" | "noscript")
}

fn is_block_tag(name: &str) -> bool {
    matches!(
        name,
        "address"
            | "article"
            | "aside"
            | "blockquote"
            | "dd"
            | "div"
            | "dl"
            | "dt"
            | "figcaption"
            | "figure"
            | "footer"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "header"
            | "hr"
            | "li"
            | "main"
            | "nav"
            | "ol"
            | "p"
            | "pre"
            | "section"
            | "table"
            | "td"
            | "th"
            | "tr"
            | "ul"
    )
}

struct TextCollector {
    output: String,
    max_bytes: usize,
    pending_space: bool,
    pending_break: bool,
    truncated: bool,
}

impl TextCollector {
    fn new(max_bytes: usize) -> Self {
        Self {
            output: String::new(),
            max_bytes,
            pending_space: false,
            pending_break: false,
            truncated: false,
        }
    }

    fn push_text(&mut self, value: &str) {
        let mut cursor = 0_usize;
        while cursor < value.len() {
            let rest = &value[cursor..];
            if let Some(entity) = decode_entity(rest) {
                self.push_char(entity.character);
                cursor = cursor.saturating_add(entity.consumed);
                continue;
            }
            let Some(character) = rest.chars().next() else {
                break;
            };
            cursor = cursor.saturating_add(character.len_utf8());
            self.push_char(character);
        }
    }

    fn push_char(&mut self, character: char) {
        if character.is_whitespace() {
            if character == '\n' || character == '\r' {
                self.pending_break = true;
                self.pending_space = false;
            } else if !self.pending_break {
                self.pending_space = true;
            }
            return;
        }
        if character.is_control() {
            return;
        }
        if self.pending_break && !self.output.is_empty() {
            self.push_raw('\n');
        } else if self.pending_space && !self.output.is_empty() {
            self.push_raw(' ');
        }
        self.pending_break = false;
        self.pending_space = false;
        self.push_raw(character);
    }

    fn push_break(&mut self) {
        self.pending_break = true;
        self.pending_space = false;
    }

    fn push_raw(&mut self, character: char) {
        let Some(next_len) = self.output.len().checked_add(character.len_utf8()) else {
            self.truncated = true;
            return;
        };
        if next_len > self.max_bytes {
            self.truncated = true;
            return;
        }
        self.output.push(character);
    }

    fn finish(mut self) -> ExtractedText {
        while self.output.ends_with(char::is_whitespace) {
            self.output.pop();
        }
        ExtractedText {
            text: self.output,
            truncated: self.truncated,
        }
    }
}

struct DecodedEntity {
    character: char,
    consumed: usize,
}

fn decode_entity(value: &str) -> Option<DecodedEntity> {
    if !value.starts_with('&') {
        return None;
    }
    let end = value
        .as_bytes()
        .iter()
        .take(16)
        .position(|byte| *byte == b';')?;
    let body = value.get(1..end)?;
    let character = match body {
        "amp" => '&',
        "apos" => '\'',
        "gt" => '>',
        "lt" => '<',
        "nbsp" => ' ',
        "quot" => '"',
        _ if body.starts_with("#x") || body.starts_with("#X") => {
            let value = u32::from_str_radix(&body[2..], 16).ok()?;
            char::from_u32(value)?
        }
        _ if body.starts_with('#') => {
            let value = body[1..].parse::<u32>().ok()?;
            char::from_u32(value)?
        }
        _ => return None,
    };
    Some(DecodedEntity {
        character,
        consumed: end.checked_add(1)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_is_inert_and_bounded() -> Result<(), ZimError> {
        let extracted = extract_inert_text(
            b"<h1>One &amp; Two</h1><script>alert(1)</script><p>Body</p>",
            ZimTextKind::Html,
            16,
        )?;
        assert_eq!(extracted.text, "One & Two\nBody");
        assert!(!extracted.text.contains('<'));
        assert!(!extracted.text.contains("alert"));
        assert!(!extracted.truncated);
        Ok(())
    }

    #[test]
    fn extraction_marks_utf8_boundary_truncation() -> Result<(), ZimError> {
        let extracted = extract_inert_text("abcdé".as_bytes(), ZimTextKind::PlainText, 5)?;
        assert_eq!(extracted.text, "abcd");
        assert!(extracted.truncated);
        Ok(())
    }
}
