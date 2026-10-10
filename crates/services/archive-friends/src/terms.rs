pub fn content_terms(text: &str, maximum: usize) -> Vec<String> {
    let mut terms = Vec::new();
    let mut token = String::new();
    let flush = |token: &mut String, terms: &mut Vec<String>| {
        let value = token
            .trim_matches(|character: char| character == '#' || character == '@')
            .to_lowercase();
        token.clear();
        if value.len() >= 3
            && !value.chars().all(|character| character.is_ascii_digit())
            && !is_stopword(&value)
            && !terms.contains(&value)
        {
            terms.push(value);
        }
    };
    for character in text.chars() {
        if character.is_alphanumeric() || matches!(character, '#' | '@' | '\'' | '-') {
            token.push(character);
        } else {
            flush(&mut token, &mut terms);
            if terms.len() >= maximum {
                break;
            }
        }
    }
    if terms.len() < maximum {
        flush(&mut token, &mut terms);
    }
    terms.truncate(maximum);
    terms
}

fn is_stopword(term: &str) -> bool {
    matches!(
        term,
        "about"
            | "after"
            | "again"
            | "against"
            | "also"
            | "among"
            | "and"
            | "are"
            | "because"
            | "been"
            | "before"
            | "being"
            | "between"
            | "both"
            | "but"
            | "can"
            | "could"
            | "does"
            | "from"
            | "good"
            | "have"
            | "high"
            | "how"
            | "into"
            | "its"
            | "make"
            | "makes"
            | "more"
            | "most"
            | "not"
            | "only"
            | "other"
            | "our"
            | "rather"
            | "should"
            | "some"
            | "such"
            | "than"
            | "that"
            | "the"
            | "their"
            | "them"
            | "then"
            | "there"
            | "these"
            | "they"
            | "this"
            | "those"
            | "through"
            | "toward"
            | "under"
            | "very"
            | "was"
            | "were"
            | "what"
            | "when"
            | "where"
            | "which"
            | "while"
            | "who"
            | "will"
            | "with"
            | "would"
            | "you"
            | "your"
    )
}
