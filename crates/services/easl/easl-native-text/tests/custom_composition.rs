use easl_native_text::{Alignment, Error, TextStyle, TextSystem, WhiteSpace};

#[test]
fn rejected_language_choices_preserve_the_drawable_layout() {
    let source = "First paragraph has several words.\r\nSecond paragraph has its own words.";
    let mut system = TextSystem::new();
    let mut text = system
        .prepare(
            source,
            &TextStyle::default(),
            WhiteSpace::Preserve,
            &[],
            &[],
        )
        .unwrap();
    text.reflow(120., Alignment::Start).unwrap();
    let previous = format!("{:?}", text.lines().collect::<Vec<_>>());
    let reflows = text.reflows;
    for mode in 0..5 {
        let result = text.compose_with::<Error>(120., Alignment::Start, |points, _| {
            match mode {
                0 => Ok(Vec::new()),
                1 => Ok(vec![usize::MAX]),
                2 => Ok(vec![points[1].cluster, points[1].cluster]),
                // Skips the mandatory CRLF break even though the end is legal.
                3 => Ok(vec![points.last().unwrap().cluster]),
                _ => Err(Error::Limit),
            }
        });
        assert!(result.is_err());
        assert_eq!(format!("{:?}", text.lines().collect::<Vec<_>>()), previous);
        assert_eq!(text.source(), source);
        assert_eq!(text.reflows, reflows);
    }
    text.optimize(220., Alignment::Start).unwrap();
    assert_eq!(system.preparations, 1);
}
