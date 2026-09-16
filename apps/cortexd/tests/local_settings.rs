use std::path::Path;

use cortex_inference::{ApiMode, AuthStrategy};
use cortexd::{LocalSettings, SettingsError};
use tempfile::TempDir;

fn write_settings(directory: &Path, contents: &str) -> std::path::PathBuf {
    let path = directory.join("cortexd.toml");
    std::fs::write(&path, contents).expect("settings file should be writable");
    path
}

#[test]
fn missing_settings_file_yields_documented_defaults() {
    let directory = TempDir::new().expect("temporary directory should be available");
    let loaded =
        LocalSettings::load(&directory.path().join("cortexd.toml")).expect("missing file is valid");
    assert!(
        loaded.is_none(),
        "absent config must load as documented defaults"
    );
}

#[test]
fn settings_file_parses_non_secret_keys() {
    let directory = TempDir::new().expect("temporary directory should be available");
    let path = write_settings(
        directory.path(),
        r#"
[daemon]
endpoint = "cortexd-lab"

[models]
default_profile = "nim"

[models.profiles.nim]
base_url = "https://integrate.api.nvidia.com/v1"
secret_ref = "keyring:cortexd/nim"

[models.profiles.local]
base_url = "http://127.0.0.1:8000/v1/"
enabled = false
"#,
    );
    let settings = LocalSettings::load(&path)
        .expect("valid settings")
        .expect("file present");
    assert_eq!(settings.endpoint_override(), Some("cortexd-lab"));
    assert_eq!(settings.default_profile_id(), Some("nim"));
    let profiles = settings.provider_profiles().expect("valid profiles");
    assert_eq!(profiles.len(), 2);
    let nim = profiles
        .iter()
        .find(|profile| profile.id().as_str() == "nim")
        .expect("nim profile");
    assert!(nim.enabled());
    assert_eq!(
        nim.secret_reference().expect("secret reference").as_str(),
        "keyring:cortexd/nim"
    );
    let local = profiles
        .iter()
        .find(|profile| profile.id().as_str() == "local")
        .expect("local profile");
    assert!(!local.enabled());
}

#[test]
fn raw_secret_keys_are_rejected_in_the_config_file() {
    let directory = TempDir::new().expect("temporary directory should be available");
    let path = write_settings(
        directory.path(),
        r#"
[models]
default_profile = "nim"

[models.profiles.nim]
base_url = "https://integrate.api.nvidia.com/v1"
api_key = "nvapi-raw-secret"
"#,
    );
    let result = LocalSettings::load(&path);
    assert!(
        matches!(result, Err(SettingsError::Invalid { .. })),
        "raw credentials must never be accepted in the config file"
    );
}

#[test]
fn invalid_toml_is_a_settings_error_not_a_crash() {
    let directory = TempDir::new().expect("temporary directory should be available");
    let path = write_settings(directory.path(), "[daemon\nendpoint =");
    assert!(matches!(
        LocalSettings::load(&path),
        Err(SettingsError::Invalid { .. })
    ));
}

#[test]
fn committed_template_documents_every_supported_key_and_parses() {
    let directory = TempDir::new().expect("temporary directory should be available");
    let path = write_settings(directory.path(), LocalSettings::template());
    let settings = LocalSettings::load(&path)
        .expect("template must parse")
        .expect("template is present");
    for key in [
        "database",
        "endpoint",
        "default_profile",
        "base_url",
        "secret_ref",
        "enabled",
    ] {
        assert!(
            LocalSettings::template().contains(key),
            "template must document `{key}`"
        );
    }
    assert!(
        settings.provider_profiles().is_ok(),
        "template profiles must build"
    );
}

#[test]
fn fully_populated_profile_parses_all_typed_fields() {
    let directory = TempDir::new().expect("temporary directory should be available");
    let path = write_settings(
        directory.path(),
        r#"
[models]
default_profile = "nim"

[models.profiles.nim]
base_url = "https://integrate.api.nvidia.com/v1"
secret_ref = "keyring:cortexd/nim"
api_mode = "openai_completions"
auth_type = "secret_ref"
connect_timeout_ms = 2000
request_timeout_ms = 9000
stale_stream_timeout_ms = 15000
models = ["meta/llama-3.1-70b-instruct"]

[models.profiles.nim.quirks]
omit_tool_choice = true
"#,
    );
    let settings = LocalSettings::load(&path)
        .expect("valid settings")
        .expect("file present");
    let profiles = settings.provider_profiles().expect("valid profiles");
    let nim = profiles
        .iter()
        .find(|profile| profile.id().as_str() == "nim")
        .expect("nim profile");
    assert_eq!(nim.api_mode(), ApiMode::OpenAiCompletions);
    assert_eq!(nim.auth_strategy(), AuthStrategy::SecretRef);
    assert_eq!(nim.timeouts().connect().as_millis(), 2_000);
    assert_eq!(nim.timeouts().request().as_millis(), 9_000);
    assert_eq!(nim.timeouts().stale_stream().as_millis(), 15_000);
    assert_eq!(nim.declared_models().len(), 1);
    assert!(nim.quirks().omit_tool_choice());
}

#[test]
fn unsupported_api_mode_is_rejected_at_parse_time() {
    let directory = TempDir::new().expect("temporary directory should be available");
    let path = write_settings(
        directory.path(),
        r#"
[models.profiles.nim]
base_url = "https://integrate.api.nvidia.com/v1"
api_mode = "anthropic_messages"
"#,
    );
    assert!(matches!(
        LocalSettings::load(&path),
        Err(SettingsError::Invalid { .. })
    ));
}

#[test]
fn auth_type_must_agree_with_the_secret_locator() {
    let directory = TempDir::new().expect("temporary directory should be available");
    let secret_ref_without_auth = write_settings(
        directory.path(),
        r#"
[models.profiles.a]
base_url = "https://integrate.api.nvidia.com/v1"
auth_type = "secret_ref"
"#,
    );
    assert!(
        matches!(
            LocalSettings::load(&secret_ref_without_auth)
                .expect("parses")
                .expect("present")
                .provider_profiles(),
            Err(SettingsError::Invalid { field: "auth_type" })
        ),
        "secret_ref auth without a locator must fail validation"
    );

    let keyless_with_locator = write_settings(
        directory.path(),
        r#"
[models.profiles.a]
base_url = "http://127.0.0.1:8000/v1"
secret_ref = "keyring:cortexd/local"
auth_type = "none"
"#,
    );
    assert!(
        matches!(
            LocalSettings::load(&keyless_with_locator)
                .expect("parses")
                .expect("present")
                .provider_profiles(),
            Err(SettingsError::Invalid { field: "auth_type" })
        ),
        "keyless auth with a locator must fail validation"
    );
}

#[test]
fn zero_timeouts_are_rejected_at_startup() {
    let directory = TempDir::new().expect("temporary directory should be available");
    let path = write_settings(
        directory.path(),
        r#"
[models.profiles.a]
base_url = "http://127.0.0.1:8000/v1"
request_timeout_ms = 0
"#,
    );
    assert!(matches!(
        LocalSettings::load(&path)
            .expect("parses")
            .expect("present")
            .provider_profiles(),
        Err(SettingsError::Invalid { field: "timeouts" })
    ));
}

#[test]
fn legacy_profiles_keep_working_without_explicit_auth_type() {
    let directory = TempDir::new().expect("temporary directory should be available");
    let path = write_settings(
        directory.path(),
        r#"
[models]
default_profile = "nim"

[models.profiles.nim]
base_url = "https://integrate.api.nvidia.com/v1"
secret_ref = "keyring:cortexd/nim"

[models.profiles.keyless]
base_url = "http://127.0.0.1:8000/v1"
"#,
    );
    let settings = LocalSettings::load(&path)
        .expect("valid settings")
        .expect("file present");
    let profiles = settings.provider_profiles().expect("valid profiles");
    let nim = profiles
        .iter()
        .find(|profile| profile.id().as_str() == "nim")
        .expect("nim profile");
    let keyless = profiles
        .iter()
        .find(|profile| profile.id().as_str() == "keyless")
        .expect("keyless profile");
    assert_eq!(nim.auth_strategy(), AuthStrategy::SecretRef);
    assert_eq!(keyless.auth_strategy(), AuthStrategy::None);
    // Historical default timeout is preserved for legacy configurations.
    assert_eq!(nim.timeouts().request().as_secs(), 5);
}

#[test]
fn brain_prompt_inline_and_file_sources_are_parsed() {
    let directory = TempDir::new().expect("temporary directory should be available");
    let path = write_settings(
        directory.path(),
        r#"
[brain]
prompt = "Always answer concisely."
"#,
    );
    let settings = LocalSettings::load(&path)
        .expect("loads")
        .expect("settings present");
    let source = settings.brain_prompt().expect("valid brain prompt");
    match source {
        Some(cortexd::BrainPromptSource::Inline(prompt)) => {
            assert_eq!(prompt, "Always answer concisely.");
        }
        other => panic!("expected inline source, got {other:?}"),
    }

    let path = write_settings(
        directory.path(),
        r#"
[brain]
prompt_file = "brain-prompt.md"
"#,
    );
    let settings = LocalSettings::load(&path)
        .expect("loads")
        .expect("settings present");
    assert!(matches!(
        settings.brain_prompt().expect("valid"),
        Some(cortexd::BrainPromptSource::File(_))
    ));
}

#[test]
fn brain_prompt_rejects_inline_and_file_declared_together() {
    let directory = TempDir::new().expect("temporary directory should be available");
    let path = write_settings(
        directory.path(),
        r#"
[brain]
prompt = "inline"
prompt_file = "brain-prompt.md"
"#,
    );
    let settings = LocalSettings::load(&path)
        .expect("loads")
        .expect("settings present");
    assert!(settings.brain_prompt().is_err(), "both sources declared");
}

#[test]
fn profile_prompts_are_parsed_per_profile_id() {
    let directory = TempDir::new().expect("temporary directory should be available");
    let path = write_settings(
        directory.path(),
        r#"
[models]
default_profile = "nim"

[models.profiles.nim]
base_url = "https://nim.example/v1/"
prompt = "Prefer concise technical answers."
"#,
    );
    let settings = LocalSettings::load(&path)
        .expect("loads")
        .expect("settings present");
    assert_eq!(
        settings.profile_prompt("nim"),
        Some("Prefer concise technical answers.")
    );
    assert_eq!(settings.profile_prompt("other"), None);
    let config = settings.prompt_config().expect("prompt config");
    assert!(config.global_inline.is_none());
    assert_eq!(
        config.profiles.get("nim").map(String::as_str),
        Some("Prefer concise technical answers.")
    );
}
