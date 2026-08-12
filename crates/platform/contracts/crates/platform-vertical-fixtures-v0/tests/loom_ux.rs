mod support;

use platform_vertical_fixtures_v0::{
    FactValueV0, ValidationError, VerticalIdV0, validate_observation,
};

#[test]
fn loom_projection_requires_the_quiet_caret_local_contract() {
    let observation = support::observation(VerticalIdV0::LoomSuggestionPromotion);
    validate_observation(&observation).expect("quiet Loom projection");

    let invalid_values = [
        ("visible_ghost_count", FactValueV0::Integer(3)),
        (
            "tab_without_exact_boundary",
            FactValueV0::Text("consume_tab".to_owned()),
        ),
        ("persistent_candidate_count", FactValueV0::Boolean(true)),
        ("skip_to_manuscript_control", FactValueV0::Boolean(true)),
        ("primary_use_this_control", FactValueV0::Boolean(true)),
        ("stale_suggestion_promoted", FactValueV0::Boolean(true)),
        (
            "ordinary_tab_action",
            FactValueV0::Text("tab_consumed_without_edit".to_owned()),
        ),
        (
            "additional_candidates",
            FactValueV0::Text("always_visible".to_owned()),
        ),
    ];
    for (name, invalid) in invalid_values {
        let mut changed = observation.clone();
        changed
            .projection
            .output_facts
            .insert(name.to_owned(), invalid);
        assert_eq!(
            validate_observation(&changed),
            Err(ValidationError::Invalid {
                field: "loom.output_facts"
            }),
            "Loom fact {name} must remain fixed"
        );
    }
}
