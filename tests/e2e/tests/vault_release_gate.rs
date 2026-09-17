//! SCRUM-136 release gate: end-to-end vault resilience through the real
//! daemon and brain CLI — adversarial Markdown ingestion, full index
//! rebuild, and doctor/status vault-health reporting during a sync outage.

mod support;

use support::Harness;

fn vault_root_of(harness: &Harness) -> std::path::PathBuf {
    harness
        .database_path()
        .parent()
        .expect("database has a parent")
        .join("vault")
}

#[tokio::test]
async fn vault_survives_adversarial_content_rebuild_and_reports_outage_health() {
    let harness = Harness::start().await;
    let vault_root = vault_root_of(&harness);

    // Adversarial Markdown: a malformed frontmatter document and a
    // pathological unicode body must never crash ingestion.
    std::fs::write(
        vault_root.join("broken.md"),
        "---\nnot: [closed\n---\nbody\n",
    )
    .expect("write malformed note");
    std::fs::write(
        vault_root.join("weird.md"),
        format!("# {}\n\nbody", "é".repeat(512)),
    )
    .expect("write weird note");

    // Any provider mutation refreshes the derived index; use one so the
    // health snapshot reflects the adversarial files written above.
    let created = harness
        .ipc_call(
            "cortex_note_create",
            serde_json::json!({ "title": "gate probe", "content": "probe body" }),
        )
        .await;
    created.assert_success();

    // The release-relevant doctor surface reports secret-free vault health.
    let doctor = harness.cli(&["brain", "doctor"]).await;
    doctor.assert_success();
    let vault = doctor
        .data()
        .expect("doctor data")
        .get("vault")
        .cloned()
        .expect("vault health block");
    assert_eq!(vault["configured"], serde_json::json!(true));
    assert_eq!(vault["root_accessible"], serde_json::json!(true));
    assert!(vault["index"]["skipped"].as_u64().expect("skipped") >= 1);
    assert_eq!(vault["fresh"], serde_json::json!(true));
    let rendered = vault.to_string().to_lowercase();
    assert!(!rendered.contains("secret") && !rendered.contains("bearer"));
    assert!(
        !rendered.contains("probe body"),
        "health output never carries vault content"
    );

    // A sync outage removes the configured root: local operation continues
    // and health reports explicit staleness instead of failing silently.
    std::fs::remove_dir_all(&vault_root).expect("simulate sync outage");
    harness.cli(&["brain", "status"]).await.assert_success();
    let status = harness.cli(&["brain", "status"]).await;
    let vault = status
        .data()
        .expect("status data")
        .get("vault")
        .cloned()
        .expect("vault health block");
    assert_eq!(
        vault["root_accessible"],
        serde_json::json!(false),
        "outage must be visible in diagnostics"
    );
    assert_eq!(
        vault["fresh"],
        serde_json::json!(false),
        "freshness must read stale/unknown while the root is gone"
    );

    // Recovery: the root returns and the next mutation restores full health.
    std::fs::create_dir_all(&vault_root).expect("recreate vault root");
    let recovered = harness
        .ipc_call(
            "cortex_note_create",
            serde_json::json!({ "title": "recovery probe", "content": "recovered" }),
        )
        .await;
    recovered.assert_success();
    let vault = harness
        .cli(&["brain", "doctor"])
        .await
        .data()
        .expect("doctor data")
        .get("vault")
        .cloned()
        .expect("vault health block");
    assert_eq!(vault["root_accessible"], serde_json::json!(true));
    assert_eq!(vault["fresh"], serde_json::json!(true));

    harness.shutdown().await;
}
