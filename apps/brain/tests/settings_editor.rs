use brain::SecretWriter;
use brain::tui::{App, SettingsSummary, TextPurpose};
use tempfile::TempDir;

struct RecordingStore;

impl SecretWriter for RecordingStore {
    fn write(
        &self,
        _service: &str,
        _username: &str,
        _secret: &[u8],
    ) -> Result<(), brain::LocalOpError> {
        Ok(())
    }
}

fn app_with_settings() -> App {
    app_with_config_path("cortexd.toml")
}

fn app_with_config_path(config_path: &str) -> App {
    let mut app = App::new();
    app.select_tab(brain::tui::Tab::Settings);
    app.set_settings_summary(SettingsSummary {
        config_path: config_path.to_owned(),
        default_profile: Some("nim".to_owned()),
        profiles: vec![
            ("nim".to_owned(), "https://nim.example/v1/".to_owned(), true),
            (
                "local".to_owned(),
                "http://127.0.0.1:8000/v1/".to_owned(),
                false,
            ),
        ],
        model_status: "resolved by the daemon at startup".to_owned(),
    });
    app
}

#[test]
fn entering_edit_mode_drafts_the_current_settings() {
    let mut app = app_with_settings();
    app.start_settings_edit();
    let editor = app.settings_editor().expect("edit mode");
    assert_eq!(
        editor.default_profile().map(str::to_owned),
        Some("nim".to_owned())
    );
    let profiles = editor.profiles();
    assert_eq!(profiles.len(), 2);
    assert!(profiles.iter().any(|p| p.id == "nim" && p.enabled));
    assert!(profiles.iter().any(|p| p.id == "local" && !p.enabled));
    app.cancel_settings_edit();
    assert!(app.settings_editor().is_none());
}

#[test]
fn saving_refreshes_the_settings_summary_and_reopened_editor() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("cortexd.toml");
    let mut app = app_with_config_path(path.to_str().unwrap());
    app.start_settings_edit();
    app.begin_text(TextPurpose::DefaultProfile);
    for c in "local".chars() {
        app.editor_text_input(c);
    }
    app.confirm_text();
    app.save_settings().unwrap();
    app.cancel_settings_edit();
    assert_eq!(
        app.settings_summary().unwrap().default_profile.as_deref(),
        Some("local")
    );
    app.start_settings_edit();
    assert_eq!(
        app.settings_editor().unwrap().default_profile(),
        Some("local")
    );
}

#[test]
fn cursor_moves_through_rows() {
    let mut app = app_with_settings();
    app.start_settings_edit();
    assert_eq!(app.settings_editor().unwrap().cursor(), 0);
    app.editor_down();
    assert_eq!(app.settings_editor().unwrap().cursor(), 1);
    app.editor_up();
    assert_eq!(app.settings_editor().unwrap().cursor(), 0);
}

#[test]
fn default_profile_can_be_changed() {
    let mut app = app_with_settings();
    app.start_settings_edit();
    app.begin_text(TextPurpose::DefaultProfile);
    for character in "local".chars() {
        app.editor_text_input(character);
    }
    app.confirm_text();
    assert_eq!(
        app.settings_editor().unwrap().default_profile(),
        Some("local")
    );
}

#[test]
fn a_new_profile_can_be_added() {
    let mut app = app_with_settings();
    app.start_settings_edit();
    app.begin_text(TextPurpose::NewProfileId);
    for character in "gpu".chars() {
        app.editor_text_input(character);
    }
    app.confirm_text();
    app.begin_text(TextPurpose::BaseUrl("gpu".to_owned()));
    for character in "http://127.0.0.1:9000/v1/".chars() {
        app.editor_text_input(character);
    }
    app.confirm_text();
    let editor = app.settings_editor().unwrap();
    let gpu = editor
        .profiles()
        .iter()
        .find(|profile| profile.id == "gpu")
        .expect("new profile");
    assert_eq!(gpu.base_url, "http://127.0.0.1:9000/v1/");
    assert!(gpu.enabled);
}

#[test]
fn profiles_can_be_toggled_and_deleted() {
    let mut app = app_with_settings();
    app.start_settings_edit();
    app.editor_down(); // cursor on "nim"
    app.toggle_enabled();
    assert!(!app.settings_editor().unwrap().profiles()[0].enabled);
    app.toggle_enabled();
    assert!(app.settings_editor().unwrap().profiles()[0].enabled);

    app.begin_delete();
    app.confirm_text(); // confirm
    assert!(
        !app.settings_editor()
            .unwrap()
            .profiles()
            .iter()
            .any(|profile| profile.id == "nim")
    );
}

#[test]
fn secrets_are_imported_through_the_keyring_flow_and_never_shown() {
    let mut app = app_with_settings();
    app.start_settings_edit();
    app.begin_text(TextPurpose::Secret("local".to_owned()));
    for character in "nvapi-test-key".chars() {
        app.editor_text_input(character);
    }
    let screen = brain::tui::render_to_string(&app, 100, 30);
    assert!(
        !screen.contains("nvapi-test-key"),
        "secret input must be masked"
    );
    app.confirm_text_with_store(&RecordingStore)
        .expect("import ok");
    let local = &app.settings_editor().unwrap().profiles()[1];
    assert_eq!(local.secret_ref.as_deref(), Some("keyring:cortexd/local"));
    assert!(
        !format!("{local:?}").contains("nvapi"),
        "the raw key never enters the draft"
    );
}

#[test]
fn saved_settings_serialize_validate_and_persist() {
    let directory = TempDir::new().expect("temp dir");
    let path = directory.path().join("cortexd.toml");
    std::fs::write(&path, local_settings_fixture::text()).expect("seed config");

    let mut app = app_with_config_path(path.to_str().expect("utf8 path"));
    app.start_settings_edit();
    app.begin_text(TextPurpose::NewProfileId);
    for character in "gpu".chars() {
        app.editor_text_input(character);
    }
    app.confirm_text();
    app.begin_text(TextPurpose::BaseUrl("gpu".to_owned()));
    for character in "http://127.0.0.1:9000/v1/".chars() {
        app.editor_text_input(character);
    }
    app.confirm_text();

    app.save_settings().expect("valid draft saves");
    let contents = std::fs::read_to_string(&path).expect("config written");
    assert!(contents.contains("[models.profiles.gpu]"));
    assert!(contents.contains("default_profile"));

    let settings = cortexd::LocalSettings::load(&path)
        .expect("parses")
        .expect("present");
    assert!(settings.provider_profiles().is_ok());
    assert_eq!(settings.default_profile_id(), Some("nim"));
}

#[test]
fn invalid_profile_values_are_rejected_without_writing() {
    let directory = TempDir::new().expect("temp dir");
    let path = directory.path().join("cortexd.toml");
    std::fs::write(&path, local_settings_fixture::text()).expect("seed config");

    let mut app = app_with_config_path(path.to_str().expect("utf8 path"));
    app.start_settings_edit();
    app.begin_text(TextPurpose::NewProfileId);
    for character in "bad/id".chars() {
        app.editor_text_input(character);
    }
    app.confirm_text();
    let editor = app.settings_editor().expect("edit mode");
    assert!(
        editor.error().is_some(),
        "the invalid id must be rejected inline"
    );
    assert!(
        !editor
            .profiles()
            .iter()
            .any(|profile| profile.id == "bad/id")
    );

    // A draft missing a required field fails at save time and never writes.
    app.begin_text(TextPurpose::NewProfileId);
    for character in "gpu".chars() {
        app.editor_text_input(character);
    }
    app.confirm_text();
    let error = app
        .save_settings()
        .expect_err("a profile without a base_url must fail to save");
    assert!(!error.is_empty());
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        local_settings_fixture::text(),
        "a rejected draft must never touch the file"
    );
}

mod local_settings_fixture {
    pub fn text() -> String {
        "[models]\ndefault_profile = \"nim\"\n\n[models.profiles.nim]\nbase_url = \"https://nim.example/v1/\"\n".to_owned()
    }
}
