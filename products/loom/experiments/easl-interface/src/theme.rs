//! Projection of the three validated, project-owned theme tokens into the view.
//! Mixes and geometry remain authored in EASL, matching current app.css.

/// A strict, dependency-free conversion; this is not another settings parser.
pub fn rgb(value: &str) -> Option<[f32; 3]> {
    let bytes = value.strip_prefix('#')?.as_bytes();
    if bytes.len() != 6 || !bytes.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    let mut color = [0.; 3];
    for (channel, pair) in color.iter_mut().zip(bytes.chunks_exact(2)) {
        let high = char::from(pair[0]).to_digit(16)?;
        let low = char::from(pair[1]).to_digit(16)?;
        *channel = f32::from(u8::try_from(high * 16 + low).ok()?) / 255.;
    }
    Some(color)
}

/// Returns a presence mask and RGB triplets. Absence leaves the EASL light/dark
/// defaults in charge, including pure black (which must not mean "absent").
pub fn project(tokens: [Option<&str>; 3]) -> (u8, [f32; 9]) {
    let mut mask = 0_u8;
    let mut values = [0.; 9];
    for (index, token) in tokens.into_iter().enumerate() {
        if let Some(color) = token.and_then(rgb) {
            mask |= 1 << index;
            values[index * 3..index * 3 + 3].copy_from_slice(&color);
        }
    }
    (mask, values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn black_is_present_not_an_unset_token() {
        let (mask, values) = project([Some("#000000"), None, Some("#FFFFFF")]);
        assert_eq!(mask, 5);
        assert_eq!(values[..3], [0., 0., 0.]);
        assert_eq!(values[6..], [1., 1., 1.]);
    }

    #[test]
    fn conversion_is_case_insensitive_and_channel_exact() {
        assert_eq!(rgb("#aBcDeF"), rgb("#ABCDEF"));
        assert_eq!(rgb("#123456"), Some([18. / 255., 52. / 255., 86. / 255.]));
    }

    #[test]
    fn no_css_expressions_alpha_or_malformed_unicode_enter_numeric_inputs() {
        for token in [
            "red",
            "#123",
            "#12345678",
            "#000000;",
            "url(x)",
            "#éaaaa",
            "#gg0000",
        ] {
            assert_eq!(rgb(token), None, "{token}");
        }
    }

    #[test]
    fn optional_tokens_do_not_borrow_another_tokens_presence() {
        let (mask, values) = project([None, Some("#ff0000"), None]);
        assert_eq!(mask, 2);
        assert_eq!(values, [0., 0., 0., 1., 0., 0., 0., 0., 0.]);
        assert_eq!(project([None; 3]), (0, [0.; 9]));
    }
}
