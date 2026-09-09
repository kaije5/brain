use std::path::Path;

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
