use brain::tui::{App, Tab};
use cortexd::{AuthenticatedIpcClient, DaemonRequest, PROTOCOL_VERSION};
use serde_json::json;
use support::Harness;
use uuid::Uuid;

mod support;

fn owner_client(database_path: &std::path::Path) -> AuthenticatedIpcClient {
    AuthenticatedIpcClient::from_database_path(database_path).expect("owner enrollment")
}

fn request(capability: &str) -> DaemonRequest {
    DaemonRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: Uuid::now_v7(),
        principal_id: Uuid::now_v7(),
        operation_id: Uuid::now_v7(),
        capability: capability.to_owned(),
        payload: json!({}),
    }
}

/// SCRUM-77 end to end: the daemon exposes the discovered catalog, the TUI
/// browser narrows it with a search query, and confirming pins the model
/// into `cortexd.toml` where the daemon's own settings loader reads it back.
#[tokio::test]
async fn model_selection_round_trips_from_catalog_to_config() {
    let harness = Harness::start().await;
    let client = owner_client(harness.database_path());

    let listed = client
        .request(&request("cortex_model_list"))
        .await
        .expect("model list through cortexd");
    let models: Vec<String> = match listed.result {
        cortexd::WireResult::Success { value } => value["models"]
            .as_array()
            .expect("models array")
            .iter()
            .filter_map(|model| model.as_str().map(str::to_owned))
            .collect(),
        cortexd::WireResult::Error { code } => panic!("model list failed: {code}"),
    };
    assert!(!models.is_empty(), "the harness model must be discovered");

    let mut app = App::new();
    app.select_tab(Tab::Settings);
    app.open_model_browser(models.clone());

    let needle = models[0].split('/').next_back().unwrap_or("nope");
    for character in needle.chars() {
        if let Some(browser) = app.model_browser_mut() {
            browser.push_query(character);
        }
    }
    assert!(
        app.model_browser()
            .unwrap()
            .matches()
            .contains(&models[0].as_str())
    );

    // The editor draft needs the loaded summary to exist before confirming.
    let config_path = harness
        .database_path()
        .parent()
        .unwrap()
        .join("cortexd.toml");
    app.set_settings_summary(brain::tui::SettingsSummary {
        config_path: config_path.display().to_string(),
        default_profile: Some("e2e".to_owned()),
        pinned_model: None,
        profiles: vec![("e2e".to_owned(), "http://127.0.0.1:9/".to_owned(), true)],
        model_status: "ready".to_owned(),
    });
    assert!(app.confirm_model_selection());

    app.save_settings().expect("pin persists");

    let settings = cortexd::LocalSettings::load(&config_path)
        .expect("parses")
        .expect("present");
    assert_eq!(settings.pinned_model(), Some(models[0].as_str()));
}
