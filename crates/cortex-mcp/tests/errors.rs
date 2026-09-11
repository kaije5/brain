use cortex_application::ApplicationError;
use cortex_mcp::map_application_error;

#[test]
fn storage_error_becomes_safe_mcp_internal_error() {
    let application_error =
        ApplicationError::Storage("sqlite failure at C:/private/cortex.db".to_owned());
    let error = map_application_error(&application_error);
    assert_eq!(error.code, "cortex_internal_error");
    assert!(!error.message.contains("sqlite"));
    assert!(!error.message.contains("C:/private"));
}

#[test]
fn unavailable_secret_store_becomes_safe_mcp_unavailable_error() {
    let error = map_application_error(&ApplicationError::SecretStoreUnavailable);
    assert_eq!(error.code, "cortex_unavailable");
    assert!(!error.message.contains("keyring"));
}
