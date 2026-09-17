use easl_native_text::{MAX_EDITOR_SPANS, StyledSpan, TextStyle};
use easl_text::{Error, STYLES_SOURCE, StyleInput, StyleProperties as P, StyleRule, Styling};

fn input(start: usize, end: usize, style: u32, flags: u32) -> StyleInput {
    StyleInput {
        range: start..end,
        style,
        flags,
    }
}

#[test]
fn every_rich_property_round_trips_and_masked_rules_preserve_other_fields() {
    let base = TextStyle::default();
    let changed = TextStyle {
        family: "Iowan Old Style, serif".into(),
        size: 27.,
        line_height: 45.,
        weight: 700.,
        italic: true,
        letter_spacing: -0.3,
        word_spacing: 2.5,
        features: "'liga' 0".into(),
        variations: "'wght' 700".into(),
        locale: Some("sr-Latn".into()),
        color: [1, 2, 3, 4],
        underline: true,
        strike: true,
        keep_all: true,
        wrap: false,
    };
    let fields = [
        P::FAMILY,
        P::SIZE,
        P::LINE_HEIGHT,
        P::WEIGHT,
        P::ITALIC,
        P::LETTER_SPACING,
        P::WORD_SPACING,
        P::FEATURES,
        P::VARIATIONS,
        P::LANGUAGE,
        P::COLOR,
        P::UNDERLINE,
        P::STRIKE,
        P::KEEP_ALL,
        P::WRAP,
    ];
    let rules: Vec<_> = fields
        .iter()
        .enumerate()
        .map(|(i, &properties)| StyleRule {
            flag: 1 << i,
            properties,
            style: 1,
        })
        .collect();
    let inputs: Vec<_> = (0..15).map(|i| input(i, i + 1, 0, 1 << i)).collect();
    let mut styling = Styling::new().unwrap();
    let spans = styling
        .resolve(
            "abcdefghijklmno",
            &[base.clone(), changed.clone()],
            &rules,
            &inputs,
        )
        .unwrap();
    assert_eq!(spans.len(), 15);
    for (i, span) in spans.iter().enumerate() {
        let mut expected = base.clone();
        match i {
            0 => expected.family.clone_from(&changed.family),
            1 => expected.size = changed.size,
            2 => expected.line_height = changed.line_height,
            3 => expected.weight = changed.weight,
            4 => expected.italic = changed.italic,
            5 => expected.letter_spacing = changed.letter_spacing,
            6 => expected.word_spacing = changed.word_spacing,
            7 => expected.features.clone_from(&changed.features),
            8 => expected.variations.clone_from(&changed.variations),
            9 => expected.locale.clone_from(&changed.locale),
            10 => expected.color = changed.color,
            11 => expected.underline = changed.underline,
            12 => expected.strike = changed.strike,
            13 => expected.keep_all = changed.keep_all,
            14 => expected.wrap = changed.wrap,
            _ => unreachable!(),
        }
        assert_eq!(
            span,
            &StyledSpan {
                range: i..i + 1,
                style: expected
            },
            "field {i}"
        );
    }
    let full = [StyleRule {
        flag: 1,
        properties: P::ALL,
        style: 1,
    }];
    assert_eq!(
        styling
            .resolve(
                "x",
                &[base.clone(), changed.clone()],
                &full,
                &[input(0, 1, 0, 1)]
            )
            .unwrap()[0]
            .style,
        changed
    );
    // Clearing booleans and optional language is as important as setting them.
    let full = [StyleRule {
        flag: 1,
        properties: P::ALL,
        style: 0,
    }];
    assert_eq!(
        styling
            .resolve("x", &[base.clone(), changed], &full, &[input(0, 1, 1, 1)])
            .unwrap()[0]
            .style,
        base
    );
}

#[test]
fn rules_coalesce_equal_styles_keep_gaps_and_preserve_empty_insertion_styles() {
    let base = TextStyle::default();
    let bold = TextStyle {
        weight: 700.,
        ..base.clone()
    };
    let italic = TextStyle {
        italic: true,
        ..base.clone()
    };
    let styles = [base.clone(), bold.clone(), italic];
    let rules = [
        StyleRule {
            flag: 1,
            properties: P::WEIGHT,
            style: 1,
        },
        StyleRule {
            flag: 2,
            properties: P::ITALIC,
            style: 2,
        },
        StyleRule {
            flag: 4,
            properties: P::WEIGHT,
            style: 0,
        },
    ];
    let spans = [
        input(0, 3, 0, 1),
        input(3, 4, 1, 0),
        input(5, 6, 0, 3),
        input(6, 7, 0, 7),
        input(7, 7, 1, 0),
    ];
    let mut styling = Styling::new().unwrap();
    let output = styling.resolve("áb cde", &styles, &rules, &spans).unwrap();
    assert_eq!(output.len(), 4);
    assert_eq!(
        output[0],
        StyledSpan {
            range: 0..4,
            style: bold.clone()
        }
    );
    assert_eq!(output[1].range, 5..6);
    assert!(output[1].style.italic);
    assert_eq!(output[1].style.weight, 700.);
    assert!(output[2].style.italic);
    assert_eq!(output[2].style.weight, 400.);
    assert_eq!(
        output[3],
        StyledSpan {
            range: 7..7,
            style: bold
        }
    );
    assert_eq!(styling.resolutions, 1);
    assert_eq!(styling.resolve("", &[], &[], &[]).unwrap(), []);
}

#[test]
fn invalid_ranges_styles_and_work_leave_the_resolver_usable() {
    let style = TextStyle::default();
    let mut styling = Styling::new().unwrap();
    for spans in [
        vec![input(1, 2, 0, 0)],
        vec![input(0, 3, 0, 0), input(0, 1, 0, 0)],
        vec![input(0, 3, 1, 0)],
        vec![input(0, 3, 0, 1)],
        vec![input(3, 0, 0, 0)],
    ] {
        assert!(
            styling
                .resolve("éx", std::slice::from_ref(&style), &[], &spans)
                .is_err()
        );
    }
    let mut invalid = style.clone();
    invalid.size = f32::NAN;
    assert!(
        styling
            .resolve("x", &[invalid], &[], &[input(0, 1, 0, 0)])
            .is_err()
    );
    let source = "a".repeat(MAX_EDITOR_SPANS);
    let spans: Vec<_> = (0..MAX_EDITOR_SPANS)
        .map(|i| input(i, i + 1, 0, 63))
        .collect();
    let rules: Vec<_> = (0..32)
        .map(|i| StyleRule {
            flag: 1 << i,
            properties: P::WEIGHT,
            style: 0,
        })
        .collect();
    assert!(matches!(
        styling.resolve(&source, std::slice::from_ref(&style), &rules, &spans),
        Err(Error::Limit)
    ));
    assert_eq!(styling.resolutions, 0);
    // The admitted editor span bound is useful with Loom's six rules.
    let output = styling
        .resolve(&source, std::slice::from_ref(&style), &rules[..6], &spans)
        .unwrap();
    assert_eq!(
        output,
        [StyledSpan {
            range: 0..source.len(),
            style
        }]
    );
}

#[test]
fn easl_library_preflights_capacity_and_malformed_late_input_in_both_evaluators() {
    let source = r#"
      (var s-values: [TextRunStyle]) (var s-rules: [TextStyleRule]) (var s-spans: [TextStyleInput])
      (var s-output: [TextStyledSpan]) (var s-writes: u32) (var s-checks: u32)
      (defn s-expect [ok: bool] (+= s-checks 1u) (when (not ok) (print s-checks)))
      (defn s-emit [i: u32 span: TextStyledSpan] (+= s-writes 1u) (= (s-output i) span))
      (defn s-resolve [count: u32 capacity: u32 budget: u32]: TextStyleResult
        (text-resolve-styles 3u (array-length s-values) (fn [i] (s-values i))
          (array-length s-rules) (fn [i] (s-rules i)) count (fn [i] (s-spans i)) capacity s-emit budget))
      @cpu (defn main []
        (= s-values (zeroed-array 2u)) (= s-rules (zeroed-array 1u))
        (= s-spans (zeroed-array 3u)) (= s-output (zeroed-array 3u))
        (= (s-values 0u) (TextRunStyle 0u 20. 33. 400. 0. 0. 0u 0u 4294967295u (vec4u 20u) 16u))
        (= (s-values 1u) (s-values 0u)) (= (.weight (s-values 1u)) 700.)
        (= (s-rules 0u) (TextStyleRule 1u 8u 1u))
        (= (s-spans 0u) (TextStyleInput 0u 1u 0u 0u))
        (= (s-spans 1u) (TextStyleInput 1u 2u 0u 1u))
        (= (s-spans 2u) (TextStyleInput 2u 3u 1u 0u))
        (s-expect (== (.status (s-resolve 3u 1u 100u)) 2u)) (s-expect (== s-writes 0u))
        (s-expect (== (.status (s-resolve 3u 3u 1u)) 2u)) (s-expect (== s-writes 0u))
        (= (.end (s-spans 2u)) 4u)
        (s-expect (== (.status (s-resolve 3u 3u 100u)) 1u)) (s-expect (== s-writes 0u))
        (= (.end (s-spans 2u)) 3u) (= (.flags (s-spans 2u)) 2u)
        (s-expect (== (.status (s-resolve 3u 3u 100u)) 1u)) (s-expect (== s-writes 0u))
        (= (.flags (s-spans 2u)) 0u)
        (let [result (s-resolve 3u 3u 100u)]
          (s-expect (== result.status 0u)) (s-expect (== result.count 2u))
          (s-expect (== (.weight (.style (s-output 0u))) 400.))
          (s-expect (== (.weight (.style (s-output 1u))) 700.))
          (s-expect (== (.start (s-output 1u)) 1u)) (s-expect (== (.end (s-output 1u)) 3u))
          (= s-writes 0u)
          (s-expect (== (.status (s-resolve 3u 3u (- result.work 1u))) 2u)) (s-expect (== s-writes 0u)))
        (= (.size (s-values 1u)) (/ 0. 0.))
        (s-expect (== (.status (s-resolve 3u 3u 100u)) 1u)) (s-expect (== s-writes 0u))
        (= (.size (s-values 1u)) 20.) (= (.flag (s-rules 0u)) 3u)
        (s-expect (== (.status (s-resolve 3u 3u 100u)) 1u)) (s-expect (== s-writes 0u))
        (= (.flag (s-rules 0u)) 1u) (= (.properties (s-rules 0u)) 32768u)
        (s-expect (== (.status (s-resolve 3u 3u 100u)) 1u)) (s-expect (== s-writes 0u))
        (print "done"))
    "#;
    check_library(source);
}

fn check_library(source: &str) {
    use easl::{
        CompilerTarget,
        compiler::{builtins::built_in_macros, program::Program},
        interpreter::{
            CpuRuntime, IOEvent, StringIO, run_program_entry_with_io_and_runtime_from_path,
        },
        parse::{EaslMultiDocument, parse_easl_without_comments},
    };
    let mut docs = EaslMultiDocument::from_singular_document(
        parse_easl_without_comments(source),
        "styles-test.easl".into(),
        source.into(),
    );
    docs.add_document(
        parse_easl_without_comments(STYLES_SOURCE),
        "styles.easl".into(),
        STYLES_SOURCE.into(),
    );
    let (mut program, errors) = Program::from_easl_documents(&docs, built_in_macros());
    assert!(errors.is_empty(), "{errors:?}");
    let errors = program.validate_raw_program(CompilerTarget::WGSL);
    assert!(errors.is_empty(), "{errors:?}");
    for runtime in [CpuRuntime::TreeWalking, CpuRuntime::BytecodeVm] {
        let io = run_program_entry_with_io_and_runtime_from_path(
            program.clone(),
            Some("main"),
            StringIO::new(),
            std::path::Path::new("styles-test.easl"),
            runtime,
        )
        .unwrap()
        .0;
        assert_eq!(io.events, [IOEvent::Print("done".into())], "{runtime:?}");
    }
}

#[test]
fn paragraph_style_projection_preserves_gaps_insertion_and_atomic_failure() {
    check_library(
        r#"
      (var p-input: [TextStyledSpan]) (var p-output: [TextStyledSpan])
      (var p-base: TextRunStyle) (var p-writes: u32) (var p-checks: u32)
      (defn p-expect [ok: bool] (+= p-checks 1u) (when (not ok) (print p-checks)))
      (defn p-emit [i: u32 s: TextStyledSpan] (+= p-writes 1u) (= (p-output i) s))
      (defn p-project [start: u32 end: u32 capacity: u32 budget: u32]: TextStyleResult
        (text-project-styles 10u 2u (fn [i] (p-input i)) start end p-base capacity p-emit budget))
      @cpu (defn main []
        (= p-base (TextRunStyle 0u 20. 30. 400. 0. 0. 0u 0u 4294967295u (vec4u 0u) 16u))
        (= p-input (zeroed-array 2u)) (= p-output (zeroed-array 5u))
        (= (p-input 0u) (TextStyledSpan 2u 4u p-base)) (= (.size (.style (p-input 0u))) 40.)
        (= (p-input 1u) (TextStyledSpan 6u 8u p-base)) (= (.size (.style (p-input 1u))) 60.)
        (let [r (p-project 3u 7u 3u 6u)]
          (p-expect (== r.status 0u)) (p-expect (== r.count 3u)) (p-expect (== p-writes 3u))
          (p-expect (== (.size (.style (p-output 0u))) 40.))
          (p-expect (== (.size (.style (p-output 1u))) 20.))
          (p-expect (== (.size (.style (p-output 2u))) 60.))
          (p-expect (== (.end (p-output 0u)) 1u))
          (p-expect (== (.start (p-output 1u)) 1u)) (p-expect (== (.end (p-output 1u)) 3u))
          (p-expect (== (.start (p-output 2u)) 3u)) (p-expect (== (.end (p-output 2u)) 4u)))
        (for [i 11u]
          (= p-writes 0u)
          (let [r (p-project i i 1u 6u)]
            (p-expect (== r.status 0u)) (p-expect (== r.count 1u))
            (p-expect (== (.start (p-output 0u)) 0u)) (p-expect (== (.end (p-output 0u)) 0u))
            (p-expect (== (.size (.style (p-output 0u)))
              (if (and (>= i 2u) (<= i 4u)) 40. (if (and (>= i 6u) (<= i 8u)) 60. 20.))))))
        (= p-writes 0u)
        (p-expect (== (.status (p-project 0u 10u 4u 6u)) 2u)) (p-expect (== p-writes 0u))
        (p-expect (== (.status (p-project 0u 10u 5u 5u)) 2u)) (p-expect (== p-writes 0u))
        ; Invalid input outside the requested slice cannot partially publish it.
        (= (.end (p-input 1u)) 11u)
        (p-expect (== (.status (p-project 0u 3u 5u 6u)) 1u)) (p-expect (== p-writes 0u))
        (= (.end (p-input 1u)) 8u) (= (.start (p-input 1u)) 3u)
        (p-expect (== (.status (p-project 0u 3u 5u 6u)) 1u)) (p-expect (== p-writes 0u))
        ; Explicit insertion style and a right-hand span win at the boundary.
        (= (.start (p-input 1u)) 4u) (= (.end (p-input 1u)) 4u)
        (p-expect (== (.status (p-project 4u 4u 1u 6u)) 0u))
        (p-expect (== (.size (.style (p-output 0u))) 60.))
        (= (.end (p-input 1u)) 8u)
        (p-expect (== (.status (p-project 4u 4u 1u 6u)) 0u))
        (p-expect (== (.size (.style (p-output 0u))) 60.))
        (print "done"))
    "#,
    );
}
