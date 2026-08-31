#[test]
fn domain_crate_exposes_the_workspace_contract() {
    assert_eq!(cortex_domain::WORKSPACE_ARCHITECTURE, "modular-monolith");
    assert!(cortex_domain::UNSAFE_CODE_FORBIDDEN);
}
