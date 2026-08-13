use std::collections::BTreeSet;

use loom_types::BlobId;
use unicode_normalization::UnicodeNormalization as _;
use unicode_segmentation::UnicodeSegmentation as _;

pub(crate) const MAX_NORMALIZED_BYTES: usize = 32 * 1024 * 1024;
pub(crate) const MAX_NORMALIZED_WORDS: usize = 250_000;
pub(crate) const MAX_SHINGLES: usize = 250_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TextLimitError {
    NormalizedBytes,
    Words,
    Shingles,
}

pub(crate) fn normalize_words(text: &str) -> Result<Vec<String>, TextLimitError> {
    let mut normalized = String::new();
    for character in text.nfkc().flat_map(char::to_lowercase).nfkc() {
        if normalized.len().saturating_add(character.len_utf8()) > MAX_NORMALIZED_BYTES {
            return Err(TextLimitError::NormalizedBytes);
        }
        normalized.push(character);
    }

    let mut words = Vec::new();
    for word in normalized.unicode_words() {
        if words.len() == MAX_NORMALIZED_WORDS {
            return Err(TextLimitError::Words);
        }
        words.push(word.to_owned());
    }
    Ok(words)
}

pub(crate) fn normalize_tag(tag: &str, max_bytes: usize) -> Result<String, TextLimitError> {
    let mut normalized = String::new();
    for character in tag.trim().nfkc().flat_map(char::to_lowercase).nfkc() {
        if normalized.len().saturating_add(character.len_utf8()) > max_bytes {
            return Err(TextLimitError::NormalizedBytes);
        }
        normalized.push(character);
    }
    Ok(normalized)
}

pub(crate) fn shingle_hashes(
    words: &[String],
    width: usize,
) -> Result<BTreeSet<BlobId>, TextLimitError> {
    if width == 0 || words.len() < width {
        return Ok(BTreeSet::new());
    }
    let count = words.len() - width + 1;
    if count > MAX_SHINGLES {
        return Err(TextLimitError::Shingles);
    }

    let mut hashes = BTreeSet::new();
    let mut canonical = Vec::new();
    for window in words.windows(width) {
        canonical.clear();
        for word in window {
            let length = u64::try_from(word.len()).map_err(|_| TextLimitError::NormalizedBytes)?;
            canonical.extend_from_slice(&length.to_be_bytes());
            canonical.extend_from_slice(word.as_bytes());
        }
        hashes.insert(BlobId::digest(&canonical));
    }
    Ok(hashes)
}

pub(crate) fn intersection_and_union(
    left: &BTreeSet<BlobId>,
    right: &BTreeSet<BlobId>,
) -> (usize, usize) {
    let (smaller, larger) = if left.len() <= right.len() {
        (left, right)
    } else {
        (right, left)
    };
    let intersection = smaller.iter().filter(|item| larger.contains(item)).count();
    let union = left.len() + right.len() - intersection;
    (intersection, union)
}

pub(crate) fn contains_word_sequence(haystack: &[String], needle: &[String]) -> bool {
    if needle.is_empty() || needle.len() > haystack.len() {
        return false;
    }
    let prefix = prefix_table(needle);
    let mut matched = 0_usize;
    for word in haystack {
        while matched > 0 && word != &needle[matched] {
            matched = prefix[matched - 1];
        }
        if word == &needle[matched] {
            matched += 1;
            if matched == needle.len() {
                return true;
            }
        }
    }
    false
}

fn prefix_table(words: &[String]) -> Vec<usize> {
    let mut prefix = vec![0; words.len()];
    let mut matched = 0_usize;
    for index in 1..words.len() {
        while matched > 0 && words[index] != words[matched] {
            matched = prefix[matched - 1];
        }
        if words[index] == words[matched] {
            matched += 1;
            prefix[index] = matched;
        }
    }
    prefix
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_handles_composition_case_and_unicode_words() {
        let composed = normalize_words("CAFÉ 東京").unwrap();
        let decomposed = normalize_words("cafe\u{301} 東京").unwrap();
        assert_eq!(composed, decomposed);
        assert_eq!(composed, vec!["café", "東", "京"]);
    }

    #[test]
    fn sequence_search_is_linear_and_exact() {
        let haystack = ["a", "b", "a", "b", "c"]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let found = ["a", "b", "c"]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let absent = ["a", "c"]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        assert!(contains_word_sequence(&haystack, &found));
        assert!(!contains_word_sequence(&haystack, &absent));
    }
}
