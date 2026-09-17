use easl::{
    CompilerTarget,
    compiler::{builtins::built_in_macros, program::Program},
    interpreter::{CpuRuntime, IOEvent, StringIO, run_program_entry_with_io_and_runtime_from_path},
    parse::{EaslMultiDocument, parse_easl_without_comments},
};
use std::path::Path;

fn check(body: &str) {
    let source = format!(
        r#"
(var requested: [u32]) (var resident: [u32]) (var output: [u32])
(var test-scratch: TextPageScratch)
(var assertion: u32) (var writes: u32)
(defn expect [ok: bool] (+= assertion 1u) (when (not ok) (print assertion)))
(defn test-emit [i: u32 id: u32] (+= writes 1u) (= (output i) id))
(defn plan [limit: u32 capacity: u32 budget: u32]: TextPagePlan
  (text-page-plan test-scratch (array-length requested) (fn [i] (requested i))
    (array-length resident) (fn [i] (resident i)) limit capacity test-emit budget))
@cpu (defn main [] (= output (zeroed-array 4096u)) {body} (print "done"))
"#
    );
    let mut docs = EaslMultiDocument::from_singular_document(
        parse_easl_without_comments(&source),
        "page-policy.easl".into(),
        source,
    );
    let library = include_str!("../library/pages.easl");
    let parsed = parse_easl_without_comments(library);
    assert!(parsed.parsing_failures.is_empty());
    docs.add_document(parsed, "pages.easl".into(), library.into());
    let (mut program, errors) = Program::from_easl_documents(&docs, built_in_macros());
    assert!(errors.is_empty(), "{errors:?}");
    let errors = program.validate_raw_program(CompilerTarget::WGSL);
    assert!(errors.is_empty(), "{errors:?}");
    for runtime in [CpuRuntime::TreeWalking, CpuRuntime::BytecodeVm] {
        let io = run_program_entry_with_io_and_runtime_from_path(
            program.clone(),
            Some("main"),
            StringIO::new(),
            Path::new("page-policy.easl"),
            runtime,
        )
        .unwrap()
        .0;
        assert_eq!(io.events, [IOEvent::Print("done".into())], "{runtime:?}");
    }
}

#[test]
fn page_selection_matches_required_sets_and_reuses_covering_residency() {
    let mut body = String::new();
    for seed in 0..24_u32 {
        let requested: Vec<_> = (0..24).map(|i| (i * 107 + seed * 13) % 29).collect();
        let mut unique = Vec::new();
        for &id in &requested {
            if !unique.contains(&id) {
                unique.push(id);
            }
        }
        let residents: Vec<_> = (0..29).filter(|i| (i + seed) % 3 != 0).collect();
        let missing = requested.iter().any(|id| !residents.contains(id));
        let array = |items: &[u32]| {
            items
                .iter()
                .map(|id| format!("{id}u"))
                .collect::<Vec<_>>()
                .join(" ")
        };
        body += &format!(
            "(= requested (into-dynamic-array [{}]))\n\
             (= resident (into-dynamic-array [{}])) (= writes 0u)\n\
             (let [r (plan 29u 4096u 1000000u)]\n\
               (expect (== r.status 0u)) (expect (== r.count {}u))\n\
               (expect (== r.rebuild {})) (expect (== writes {}u)))\n",
            array(&requested),
            array(&residents),
            unique.len(),
            missing,
            if missing { unique.len() } else { 0 },
        );
        if missing {
            for (i, id) in unique.iter().enumerate() {
                body += &format!("(expect (== (output {i}u) {id}u))\n");
            }
        }
        unique.sort_unstable();
        body += &format!(
            "(= resident (into-dynamic-array [{}])) (= writes 0u)\n\
             (let [r (plan 29u 0u 1000000u)]\n\
               (expect (== r.status 0u)) (expect (not r.rebuild)) (expect (== writes 0u)))\n",
            array(&unique),
        );
    }
    check(&body);
}

#[test]
fn invalid_late_input_capacity_and_work_do_not_publish_requests() {
    check(
        r#"
(= requested (into-dynamic-array [65535u 0u 65535u 9u]))
(= resident (zeroed-array 0u))
(let [r (plan 65536u 3u 1000000u)]
  (expect (== r.status 0u)) (expect (== r.count 3u)) (expect r.rebuild)
  (expect (== (output 0u) 65535u)) (expect (== (output 1u) 0u)) (expect (== (output 2u) 9u)))
(= writes 0u)
(expect (== (.status (plan 65536u 2u 1000000u)) 2u)) (expect (== writes 0u))
(let [r (plan 65536u 3u 1000000u)]
  (= writes 0u)
  (expect (== (.status (plan 65536u 3u (- r.work 1u))) 2u)) (expect (== writes 0u))
  (expect (== (.status (plan 65536u 3u r.work)) 0u)))
(= writes 0u) (= (requested 3u) 65536u)
(expect (== (.status (plan 65536u 4096u 1000000u)) 1u)) (expect (== writes 0u))
(= (requested 3u) 4294967295u)
(expect (== (.status (plan 65536u 4096u 1000000u)) 1u)) (expect (== writes 0u))
(= (requested 3u) 9u) (= resident (into-dynamic-array [0u 9u 9u]))
(expect (== (.status (plan 65536u 4096u 1000000u)) 1u)) (expect (== writes 0u))
(= resident (into-dynamic-array [9u 0u]))
(expect (== (.status (plan 65536u 4096u 1000000u)) 1u)) (expect (== writes 0u))
(= resident (into-dynamic-array [65536u]))
(expect (== (.status (plan 65536u 4096u 1000000u)) 1u)) (expect (== writes 0u))
(expect (== (.status (plan 0u 4096u 1000000u)) 1u))
(expect (== (.status (plan 65537u 4096u 1000000u)) 1u))
(= resident (zeroed-array 0u)) (= requested (zeroed-array 0u))
(let [r (plan 65536u 0u 0u)]
  (expect (== r.status 0u)) (expect (== r.count 0u)) (expect (not r.rebuild)))
(expect (== writes 0u))
"#,
    );
}

#[test]
fn page_selection_bounds_thousands_of_unique_glyphs_without_losing_zero() {
    check(
        r#"
(= requested (zeroed-array 4097u)) (= resident (zeroed-array 0u))
(for [i 4097u] (= (requested i) i))
(expect (== (.status (plan 65536u 4096u 1000000u)) 2u)) (expect (== writes 0u))
(= (requested 4096u) 0u)
(let [r (plan 65536u 4096u 1000000u)]
  (expect (== r.status 0u)) (expect (== r.count 4096u)) (expect (== writes 4096u))
  (for [i 4096u] (expect (== (output i) i))))
"#,
    );
}
