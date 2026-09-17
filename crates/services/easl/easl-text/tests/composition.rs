use easl_native_text::{Alignment, BreakOpportunity, TextStyle, TextSystem, WhiteSpace};
use easl_text::{Composer, Error, Policy};

fn word_points(words: &[u32]) -> Vec<BreakOpportunity> {
    let mut points = vec![BreakOpportunity {
        cluster: 0,
        advance: 0.,
        trimmed: 0.,
        hyphen: 0.,
        mandatory: true,
    }];
    let mut advance = 0.;
    for (i, word) in words.iter().enumerate() {
        let trimmed = advance + f64::from(*word);
        let mandatory = i + 1 == words.len();
        advance = trimmed + if mandatory { 0. } else { 1. };
        points.push(BreakOpportunity {
            cluster: i + 1,
            advance,
            trimmed,
            hyphen: 0.,
            mandatory,
        });
    }
    points
}

// Enumerates all partitions rather than repeating the dynamic-programming
// implementation. Small integer fixtures avoid ambiguous floating-point ties.
fn cost(
    points: &[BreakOpportunity],
    breaks: &[usize],
    width: f64,
    last_weight: f64,
) -> Option<f64> {
    let mut start = 0;
    let mut total = 0.;
    for &end in breaks {
        let occupied = points[end].trimmed - points[start].advance;
        if occupied > width {
            return None;
        }
        let slack = (width - occupied).max(0.) / width;
        total += slack
            * slack
            * if points[end].mandatory {
                last_weight
            } else {
                1.
            };
        start = end;
    }
    Some(total)
}

#[test]
fn easl_choices_match_exhaustive_partitions_and_policy_changes() {
    let mut composer = Composer::new().unwrap();
    let mut different = false;
    for seed in 0..48u32 {
        let words = (0..6)
            .map(|i| 1 + ((seed / (i + 1) + i * 7) % 7))
            .collect::<Vec<_>>();
        let points = word_points(&words);
        let width = 9.;
        let mut choices = Vec::new();
        for last_line_weight in [0., 1.] {
            let chosen = composer
                .choose_breaks(
                    &points,
                    width,
                    Policy {
                        last_line_weight,
                        ..Policy::default()
                    },
                )
                .unwrap();
            let chosen_cost = cost(
                &points,
                &chosen,
                f64::from(width),
                f64::from(last_line_weight),
            )
            .unwrap();
            let best = (0..(1 << (words.len() - 1)))
                .filter_map(|mask| {
                    let mut candidate = (1..words.len())
                        .filter(|i| mask & (1 << (i - 1)) != 0)
                        .collect::<Vec<_>>();
                    candidate.push(words.len());
                    cost(
                        &points,
                        &candidate,
                        f64::from(width),
                        f64::from(last_line_weight),
                    )
                })
                .fold(f64::INFINITY, f64::min);
            assert!(
                (best - chosen_cost).abs() < 1e-5,
                "words={words:?}, chosen={chosen:?}, best={best}, actual={chosen_cost}"
            );
            choices.push(chosen);
        }
        different |= choices[0] != choices[1];
    }
    assert!(
        different,
        "Changing policy must change at least one actual layout"
    );
}

#[test]
fn shaped_multilingual_source_reflows_without_changing_bytes_or_repreparing() {
    let mut composer = Composer::new().unwrap();
    let mut system = TextSystem::new();
    let sources = [
        "A page has a generous measure, careful spacing, and room for thought. Words become lines and lines become paragraphs.",
        "Café e\u{301}lan — Καλημέρα κόσμε. 日本語の文章。 مرحبا بالعالم. 👩🏽‍🚀 A soft hy\u{ad}phen stays in its source.",
        "First paragraph keeps its CRLF.\r\nSecond paragraph keeps its own words.\r\n",
    ];
    for source in sources {
        let style = TextStyle {
            size: 18.,
            line_height: 28.,
            ..TextStyle::default()
        };
        let mut prepared = system
            .prepare(source, &style, WhiteSpace::Preserve, &[], &[])
            .unwrap();
        let preparations = system.preparations;
        for width in [220., 350., 510.] {
            composer
                .compose(&mut prepared, width, Alignment::Justify, Policy::default())
                .unwrap();
            let mut end = 0;
            for line in prepared.lines() {
                assert_eq!(line.source.start, end);
                assert!(source.is_char_boundary(line.source.end));
                end = line.source.end;
            }
            assert_eq!(end, source.len());
            assert_eq!(prepared.source(), source);
            for line in prepared.layout().lines() {
                let metrics = line.metrics();
                assert!(metrics.advance - metrics.trailing_whitespace <= width + 0.1);
            }
        }
        assert_eq!(system.preparations, preparations);
        let previous = format!("{:?}", prepared.lines().collect::<Vec<_>>());
        let reflows = prepared.reflows;
        assert!(matches!(
            composer.compose(
                &mut prepared,
                220.,
                Alignment::Start,
                Policy {
                    edge_limit: 0,
                    ..Policy::default()
                }
            ),
            Err(Error::Limit)
        ));
        assert_eq!(
            format!("{:?}", prepared.lines().collect::<Vec<_>>()),
            previous
        );
        assert_eq!(prepared.reflows, reflows);
        composer
            .compose(&mut prepared, 350., Alignment::Start, Policy::default())
            .unwrap();
    }
}

#[test]
fn invalid_input_and_work_limits_do_not_poison_the_composer() {
    let mut composer = Composer::new().unwrap();
    let points = word_points(&[3, 2, 2, 5]);
    for width in [f32::NAN, f32::INFINITY, 0., -1.] {
        assert!(
            composer
                .choose_breaks(&points, width, Policy::default())
                .is_err()
        );
    }
    assert!(matches!(
        composer.choose_breaks(&points, 1., Policy::default()),
        Err(Error::Unbreakable)
    ));
    assert!(matches!(
        composer.choose_breaks(
            &points,
            6.,
            Policy {
                edge_limit: 0,
                ..Policy::default()
            }
        ),
        Err(Error::Limit)
    ));
    assert_eq!(
        composer
            .choose_breaks(&points, 6., Policy::default())
            .unwrap(),
        vec![1, 3, 4]
    );
}

#[test]
fn long_paragraph_offsets_keep_small_line_measurements() {
    let mut composer = Composer::new().unwrap();
    let mut points = word_points(&[3, 2, 2, 5]);
    // A preceding whitespace-only mandatory line has a large accumulated
    // advance. The following short lines must not lose their small differences
    // when the native f64 measurements cross EASL's f32 boundary.
    for point in &mut points {
        point.cluster += 1;
        point.advance += 1_000_000_000_000.;
        point.trimmed += 1_000_000_000_000.;
    }
    points[0].trimmed = 0.;
    points.insert(
        0,
        BreakOpportunity {
            cluster: 0,
            advance: 0.,
            trimmed: 0.,
            hyphen: 0.,
            mandatory: true,
        },
    );
    assert_eq!(
        composer
            .choose_breaks(&points, 6., Policy::default())
            .unwrap(),
        vec![1, 2, 4, 5]
    );
}
