use super::*;

fn request(id: &str) -> IndependentRawRequest {
    IndependentRawRequest {
        request_id: id.to_owned(),
        case_id: format!("case-{id}"),
        prompt: "An exact raw prompt".into(),
        sampling: SamplingConfig {
            seed: 1,
            max_tokens: 16,
            ..SamplingConfig::default()
        },
    }
}

#[test]
fn independently_owned_requests_keep_distinct_names_even_with_identical_prompts() {
    let requests = vec![request("first"), request("second")];
    validate_requests(&requests).expect("two independent requests");
    assert_ne!(requests[0].request_id, requests[1].request_id);
    assert_ne!(requests[0].case_id, requests[1].case_id);
    assert_eq!(requests[0].prompt, requests[1].prompt);
}

#[test]
fn invalid_batch_identity_and_resource_bounds_fail_before_native_acquisition() {
    assert!(validate_requests(&[]).is_err());
    assert!(validate_requests(&vec![request("same"); 2]).is_err());
    let mut cases = vec![request("first"), request("second")];
    cases[1].case_id = cases[0].case_id.clone();
    assert!(validate_requests(&cases).is_err());
    let mut invalid = request("bad");
    invalid.sampling.seed = u32::MAX;
    assert!(validate_requests(&[invalid]).is_err());
    let mut invalid = request("bad");
    invalid.prompt = "x".repeat(MAX_RAW_PROMPT_BYTES + 1);
    assert!(validate_requests(&[invalid]).is_err());
    let cases = (0..=MAX_INDEPENDENT_RAW_CASES)
        .map(|index| request(&index.to_string()))
        .collect::<Vec<_>>();
    assert!(validate_requests(&cases).is_err());
}
