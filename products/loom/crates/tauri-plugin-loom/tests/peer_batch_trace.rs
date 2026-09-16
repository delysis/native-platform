#[path = "../examples/qualification/trace.rs"]
mod trace;

use trace::{find_shared, verify};

fn record(batch: &str, cases: &[&str], counts: &[usize]) -> String {
    serde_json::json!({
        "kind": "loom_native_decode_batch_v1",
        "request_id": batch,
        "case_ids": cases,
        "sequence_ids": cases.iter().map(|id| if *id == "a" { 0 } else { 1 }).collect::<Vec<_>>(),
        "generated_counts": counts,
    }).to_string()
}

#[test]
fn actual_shared_decode_then_survivor_decode_is_required() {
    let shared = record("batch", &["a", "b"], &[1, 1]);
    let survivor = record("batch", &["b"], &[3]);
    let input = format!("{shared}\n{survivor}\n");
    assert_eq!(find_shared(&input, "a", "b").unwrap().as_deref(), Some("batch"));
    let summary = verify(&input, "batch", "a", "b").unwrap();
    assert!(summary.survivor_decode_line > summary.shared_decode_line);
}

#[test]
fn independent_callbacks_or_different_batches_never_qualify() {
    for input in [
        String::new(),
        format!("{}\n{}\n", record("x", &["a"], &[1]), record("y", &["b"], &[3])),
        format!("{}\n{}\n", record("x", &["a", "b"], &[1, 1]), record("y", &["b"], &[3])),
        format!("{}\n", record("x", &["a", "b"], &[1, 1])),
    ] {
        assert!(verify(&input, "x", "a", "b").is_err());
    }
}

#[test]
fn duplicate_sequences_no_progress_and_resurrected_cases_never_qualify() {
    let shared = record("x", &["a", "b"], &[1, 1]);
    let survivor = record("x", &["b"], &[3]);
    for input in [
        format!("{}\n{survivor}\n", shared.replace("[0,1]", "[0,0]")),
        format!("{shared}\n{}\n", record("x", &["b"], &[1])),
        format!("{shared}\n{survivor}\n{shared}\n"),
    ] {
        assert!(verify(&input, "x", "a", "b").is_err());
    }
    assert!(find_shared("not JSON\n", "a", "b").is_err());
}
