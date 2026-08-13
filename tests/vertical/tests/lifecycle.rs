#[test]
fn imported_contract_runs_the_shared_reference_lifecycle_suite() {
    platform_contract_testkit::lifecycle_suite::run_reference_suite();
}

#[test]
fn root_workspace_marker_is_linked() {
    assert_eq!(integration_vertical::GRAPH_KIND, "single-root-workspace");
}
