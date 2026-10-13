//! Bounded MIME admission before eager body/tree construction.
//!
//! Header input is capped before using `mail-parser`'s header-only parser. The
//! same parser's delimiter primitives then walk declared boundaries without
//! decoding bodies. Boundary identity, rather than a byte alphabet or suffix
//! guess, distinguishes opening markers from closing markers.

use attachment_native_types::IssueClass;
use mail_parser::{GetHeader, Header, HeaderName, MessageParser, parsers::MessageStream};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MimePreflight {
    pub(crate) parts: u32,
    pub(crate) metadata_bytes: u64,
    pub(crate) potential_derived_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MimePreflightError {
    pub(crate) class: IssueClass,
    pub(crate) code: &'static str,
    pub(crate) message: &'static str,
}

struct Boundary {
    value: Vec<u8>,
    digest: bool,
    message_depth: u16,
}

pub(crate) fn preflight(
    bytes: &[u8],
    max_parts: u32,
    max_message_depth: u16,
    max_metadata_bytes: u64,
    max_potential_derived_bytes: u64,
) -> Result<MimePreflight, MimePreflightError> {
    let parser = MessageParser::default();
    let mut boundaries: Vec<Boundary> = Vec::new();
    let mut parts = 0_u32;
    let mut metadata_bytes = 0_u64;
    let mut cursor = 0_usize;
    let mut in_nested_message = false;
    let mut header_charged_by_delimiter = false;
    let mut message_depth = 0_u16;

    loop {
        let Some(headers) = read_headers(
            bytes,
            &mut cursor,
            &parser,
            &mut metadata_bytes,
            max_metadata_bytes,
        )?
        else {
            return finish(bytes, parts, metadata_bytes, max_potential_derived_bytes);
        };
        let content_type = headers
            .header_value(&HeaderName::ContentType)
            .and_then(|value| value.as_content_type());

        if let Some(content_type) = content_type.filter(|value| value.ctype() == "multipart") {
            if let Some(boundary) = content_type
                .attribute("boundary")
                .filter(|value| !value.is_empty())
            {
                let mut stream = MessageStream::new(&bytes[cursor..]);
                if stream.seek_next_part(boundary.as_bytes()) {
                    // The parser's first match enters a child unconditionally.
                    // It checks the closing suffix after that first child.
                    stream.skip_crlf();
                    cursor += stream.offset();
                    charge_part(&mut parts, max_parts)?;
                    boundaries.push(Boundary {
                        value: boundary.as_bytes().to_vec(),
                        digest: content_type.subtype() == Some("digest"),
                        message_depth,
                    });
                    in_nested_message = false;
                    header_charged_by_delimiter = true;
                    continue;
                }
            }
            // No, empty, or unmatched boundaries recover as one attachment.
            if !header_charged_by_delimiter {
                charge_part(&mut parts, max_parts)?;
            }
        }
        if content_type.is_some_and(|value| {
            value.ctype() == "message" && matches!(value.subtype(), Some("rfc822" | "global"))
        }) || content_type.is_none()
            && !in_nested_message
            && boundaries.last().is_some_and(|boundary| boundary.digest)
        {
            if headers
                .header_value(&HeaderName::ContentTransferEncoding)
                .and_then(|value| value.as_text())
                .is_some_and(|encoding| {
                    encoding.eq_ignore_ascii_case("base64")
                        || encoding.eq_ignore_ascii_case("quoted-printable")
                })
            {
                return Err(MimePreflightError {
                    class: IssueClass::Unsupported,
                    code: "mime_encoded_message_unsupported",
                    message: "A transfer-encoded nested email cannot be inspected before decoding its hidden MIME structure. Its original bytes were retained without parsing.",
                });
            }
            // A delimiter already charged this same MessagePart. Raw inner
            // headers start a new message and have no delimiter of their own.
            if !header_charged_by_delimiter {
                charge_part(&mut parts, max_parts)?;
            }
            message_depth = message_depth
                .checked_add(1)
                .filter(|depth| *depth <= max_message_depth)
                .ok_or_else(|| {
                    budget(
                        "mime_message_depth_exceeded",
                        "The email's nested message structure exceeds the remaining attachment depth. It was not parsed.",
                    )
                })?;
            in_nested_message = true;
            header_charged_by_delimiter = false;
            continue;
        } else {
            // Ancestor delimiters are searched after the current part closes,
            // as in the locked parser. Missing delimiters retain its existing
            // best-effort recovery once the observed structure is bounded.
            loop {
                let Some(boundary) = boundaries.last() else {
                    return finish(bytes, parts, metadata_bytes, max_potential_derived_bytes);
                };
                match next_boundary(bytes, &mut cursor, &boundary.value) {
                    Some(true) => {
                        boundaries.pop();
                    }
                    Some(false) => {
                        message_depth = boundary.message_depth;
                        charge_part(&mut parts, max_parts)?;
                        header_charged_by_delimiter = true;
                        break;
                    }
                    None => {
                        return finish(bytes, parts, metadata_bytes, max_potential_derived_bytes);
                    }
                }
            }
        }
        in_nested_message = false;
    }
}

fn read_headers<'a>(
    bytes: &'a [u8],
    cursor: &mut usize,
    parser: &MessageParser,
    metadata_bytes: &mut u64,
    max_metadata_bytes: u64,
) -> Result<Option<Vec<Header<'a>>>, MimePreflightError> {
    let remaining = max_metadata_bytes.saturating_sub(*metadata_bytes);
    let input = &bytes[*cursor..];
    // The sentinel detects a header crossing the cap. Even malformed field
    // parsers receive at most this bounded slice, never the remaining body.
    let window_len = usize::try_from(remaining)
        .unwrap_or(usize::MAX)
        .saturating_add(1)
        .min(input.len());
    let mut stream = MessageStream::new(&input[..window_len]);
    let mut headers = Vec::new();
    let complete = stream.parse_headers(parser, &mut headers);
    let consumed = u64::try_from(stream.offset()).map_err(|_| {
        budget(
            "mime_metadata_length_overflow",
            "MIME metadata length cannot be represented safely.",
        )
    })?;
    *metadata_bytes = metadata_bytes.checked_add(consumed).ok_or_else(|| {
        budget(
            "mime_metadata_length_overflow",
            "MIME metadata length cannot be represented safely.",
        )
    })?;
    if *metadata_bytes > max_metadata_bytes || !complete && window_len < input.len() {
        return Err(budget(
            "mime_metadata_limit_exceeded",
            "The email's MIME headers exceed the configured container-metadata limit. It was not parsed.",
        ));
    }
    *cursor += stream.offset();
    Ok(complete.then_some(headers))
}

fn next_boundary(bytes: &[u8], cursor: &mut usize, boundary: &[u8]) -> Option<bool> {
    let mut stream = MessageStream::new(&bytes[*cursor..]);
    if !stream.seek_next_part(boundary) {
        return None;
    }
    let closing = stream.is_multipart_end();
    *cursor += stream.offset();
    Some(closing)
}

fn charge_part(parts: &mut u32, max_parts: u32) -> Result<(), MimePreflightError> {
    *parts = parts.checked_add(1).ok_or_else(|| {
        budget(
            "mime_part_count_overflow",
            "The MIME part count cannot be represented safely.",
        )
    })?;
    if *parts > max_parts {
        return Err(budget(
            "mime_part_limit_exceeded",
            "The email declares more potential MIME parts than the remaining global attachment budget. It was not parsed.",
        ));
    }
    Ok(())
}

fn finish(
    bytes: &[u8],
    parts: u32,
    metadata_bytes: u64,
    max_potential_derived_bytes: u64,
) -> Result<MimePreflight, MimePreflightError> {
    // Preserve the input-times-structural-parts work allowance. This is a
    // conservative admission policy, not an exact heap-size accounting claim.
    let potential_derived_bytes = u64::try_from(bytes.len())
        .ok()
        .and_then(|raw| raw.checked_mul(u64::from(parts.max(1))))
        .ok_or_else(|| {
            budget(
                "mime_derived_length_overflow",
                "Potential MIME decoded length cannot be represented safely.",
            )
        })?;
    if potential_derived_bytes > max_potential_derived_bytes {
        return Err(budget(
            "mime_derived_budget_exceeded",
            "The email's MIME structure can exceed the remaining cumulative decoded-byte budget. It was not parsed.",
        ));
    }
    Ok(MimePreflight {
        parts,
        metadata_bytes,
        potential_derived_bytes,
    })
}

fn budget(code: &'static str, message: &'static str) -> MimePreflightError {
    MimePreflightError {
        class: IssueClass::Budget,
        code,
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::preflight;

    #[test]
    fn counts_declared_parts_with_bounded_headers() {
        let message = b"MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=x\r\n\r\n--x\r\nContent-Type: text/plain\r\n\r\none\r\n--x\r\nContent-Type: text/plain\r\n\r\ntwo\r\n--x--\r\n";
        let summary = preflight(message, 2, 8, 1024, 1024).expect("bounded MIME");
        assert_eq!(summary.parts, 2);
        assert!(summary.metadata_bytes > 0);
        assert!(summary.potential_derived_bytes >= 10);
    }

    #[test]
    fn blocks_width_before_a_structured_parser_can_run() {
        let message = b"Content-Type: multipart/mixed; boundary=x\r\n\r\n--x\r\nContent-Disposition: attachment\r\n\r\na\r\n--x\r\nContent-Disposition: attachment\r\n\r\nb\r\n";
        let error = preflight(message, 1, 8, 1024, 1024).expect_err("too wide");
        assert_eq!(error.code, "mime_part_limit_exceeded");
    }
}
