#[test]
fn exact_contract_revision_runs_the_shared_reference_lifecycle_suite() {
    platform_contract_testkit_current::lifecycle_suite::run_reference_suite();
}

#[test]
fn current_lightweight_product_crates_compile_in_one_graph() {
    assert_eq!(
        integration_current::GRAPH_KIND,
        "exact-revision-current-compatibility"
    );
}
