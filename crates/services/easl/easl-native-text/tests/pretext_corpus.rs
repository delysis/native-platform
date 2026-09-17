//! Replay pinned upstream inputs against native invariants. This is deliberately
//! separate from browser differential accuracy: font fallback and bidi differ.
use easl_native_text::*;
use serde_json::Value;
use std::sync::Arc;
use unicode_segmentation::UnicodeSegmentation;

#[test]
fn pretext_retained_native_contracts() {
    let full = std::env::var_os("EASL_FULL_CORPUS").is_some();
    let data: Value = serde_json::from_str(include_str!("pretext/retained.json")).unwrap();
    let mut system = TextSystem::new();
    for bytes in [
        include_bytes!("fonts/shantell-sans-regular.ttf").as_slice(),
        include_bytes!("fonts/amiri.ttf").as_slice(),
    ] {
        system
            .fonts
            .collection
            .register_fonts(vello_cpu::peniko::Blob::new(Arc::new(bytes)), None);
    }
    let mut preparations = 0;
    let mut layouts = 0;
    for (group_index, group) in data["groups"].as_array().unwrap().iter().enumerate() {
        let samples = group["samples"].as_array().unwrap();
        for (sample_index, sample) in samples.iter().enumerate() {
            if !full && sample_index != group_index % samples.len() {
                continue;
            }
            let joined = sample["text"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    sample["parts"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|p| p.as_str().unwrap())
                        .collect()
                });
            let codes = group["codePoints"].as_array().map_or_else(
                || vec![None],
                |codes| {
                    codes
                        .iter()
                        .map(|c| {
                            Some(
                                char::from_u32(u32::try_from(c.as_u64().unwrap()).unwrap())
                                    .unwrap_or(char::REPLACEMENT_CHARACTER),
                            )
                        })
                        .collect()
                },
            );
            for code in codes {
                let text = code.map_or_else(
                    || joined.clone(),
                    |code| joined.replace('\u{fff0}', &code.to_string()),
                );
                for (option_index, option) in
                    group["options"].as_array().unwrap().iter().enumerate()
                {
                    if !full && option_index > 0 {
                        continue;
                    }
                    let font = option["font"].as_str().unwrap();
                    let (size, family) =
                        font.trim_start_matches("bold ").split_once("px ").unwrap();
                    let style = TextStyle {
                        family: family.replace("ProbeShantell", "Shantell Sans"),
                        size: size.parse().unwrap(),
                        weight: if font.starts_with("bold ") {
                            700.
                        } else {
                            400.
                        },
                        line_height: option["lineHeight"].as_f64().unwrap() as f32,
                        letter_spacing: option["letterSpacing"].as_f64().unwrap_or(0.) as f32,
                        keep_all: option["wordBreak"] == "keep-all",
                        locale: option["locale"].as_str().map(str::to_owned),
                        ..Default::default()
                    };
                    let whitespace = if option["whiteSpace"] == "normal" {
                        WhiteSpace::Collapse
                    } else {
                        WhiteSpace::Preserve
                    };
                    let context = format!(
                        "group={group_index} family={} sample={sample_index} option={option_index} text={text:?}",
                        group["family"]
                    );
                    let mut prepared =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            system.prepare(&text, &style, whitespace, &[], &[])
                        }))
                        .unwrap_or_else(|_| panic!("native panic: {context}"))
                        .unwrap_or_else(|e| panic!("{context}: {e}"));
                    let boundaries = prepared
                        .normalized_text()
                        .grapheme_indices(true)
                        .map(|(i, _)| i)
                        .chain([prepared.normalized_text().len()])
                        .collect::<Vec<_>>();
                    preparations += 1;
                    for span in sample["widths"].as_array().unwrap() {
                        let (start, step, count) = span.as_f64().map_or_else(
                            || {
                                (
                                    span["start"].as_f64().unwrap(),
                                    span["step"].as_f64().unwrap(),
                                    span["count"].as_u64().unwrap(),
                                )
                            },
                            |w| (w, 0., 1),
                        );
                        for i in 0..count {
                            if !full && i != 0 && i != count - 1 {
                                continue;
                            }
                            let width = (start + step * i as f64) as f32;
                            prepared
                                .reflow(width, Alignment::Start)
                                .unwrap_or_else(|e| panic!("{context} width={width}: {e}"));
                            let mut end = 0;
                            for line in prepared.lines() {
                                assert_eq!(line.normalized.start, end, "{context} width={width}");
                                assert!(
                                    boundaries.binary_search(&line.normalized.end).is_ok(),
                                    "split grapheme: {context} width={width} {line:?}"
                                );
                                assert!(
                                    text.is_char_boundary(line.source.start)
                                        && text.is_char_boundary(line.source.end),
                                    "{context}"
                                );
                                assert!(
                                    line.width.is_finite() && line.baseline.is_finite(),
                                    "{context}"
                                );
                                end = line.normalized.end;
                            }
                            assert_eq!(
                                end,
                                prepared.normalized_text().len(),
                                "{context} width={width}"
                            );
                            layouts += 1;
                        }
                    }
                }
            }
        }
    }
    assert_eq!(system.preparations, preparations);
    eprintln!(
        "Pretext retained native replay: {preparations} preparations, {layouts} layouts; full={full}; browser geometry not asserted"
    );
}
