use std::collections::BTreeSet;
use std::num::NonZeroUsize;
use std::path::PathBuf;

use cortex_application::{
    KnowledgeCreate, KnowledgeProvider, KnowledgeQuery, ProviderError, TaskCreate, TaskProvider,
    TaskQuery,
};
use cortex_domain::{OperationId, ProviderResourceKind, TaskId};
use cortexd::{
    DaemonConfig, InMemoryVaultProvider, LocalSettings, SettingsError, VaultConfigError,
    VaultExclusion, VaultProviderConfig, VaultProviderMode, VaultScope,
};
use tempfile::TempDir;

fn valid_config(root: PathBuf) -> VaultProviderConfig {
    let mut scopes = BTreeSet::new();
    scopes.insert(VaultScope::new("knowledge").expect("knowledge scope"));
    scopes.insert(VaultScope::new("task").expect("task scope"));
    let mut exclusions = BTreeSet::new();
    exclusions.insert(VaultExclusion::new(".obsidian").expect("valid exclusion"));
    VaultProviderConfig::new(
        "markdown-vault",
        root,
        VaultProviderMode::ReadOnly,
        scopes,
        exclusions,
    )
    .expect("valid configuration")
}

fn one_scope() -> BTreeSet<VaultScope> {
    let mut scopes = BTreeSet::new();
    scopes.insert(VaultScope::new("knowledge").expect("knowledge scope"));
    scopes
}

fn write_settings(directory: &std::path::Path, contents: &str) -> PathBuf {
    let path = directory.join("cortexd.toml");
    std::fs::write(&path, contents).expect("settings file should be writable");
    path
}

#[test]
fn vault_config_validates_bounds_and_scopes() {
    let root = PathBuf::from("unused-root");
    assert!(matches!(
        VaultProviderConfig::new(
            "markdown-vault",
            root.clone(),
            VaultProviderMode::ReadWrite,
            BTreeSet::new(),
            BTreeSet::new()
        ),
        Err(VaultConfigError::Invalid { field: "scopes" })
    ));
    assert!(matches!(
        VaultProviderConfig::new(
            " ",
            root.clone(),
            VaultProviderMode::ReadWrite,
            one_scope(),
            BTreeSet::new()
        ),
        Err(VaultConfigError::Invalid {
            field: "provider_id"
        })
    ));
    assert!(matches!(
        VaultProviderConfig::new(
            "markdown-vault",
            PathBuf::new(),
            VaultProviderMode::ReadWrite,
            one_scope(),
            BTreeSet::new()
        ),
        Err(VaultConfigError::Invalid { field: "root" })
    ));
    assert!(matches!(
        VaultScope::new("calendar"),
        Err(VaultConfigError::Invalid { field: "scopes" })
    ));
}

#[test]
fn vault_exclusions_reject_escape_and_absolute_patterns() {
    assert!(matches!(
        VaultExclusion::new("../outside"),
        Err(VaultConfigError::Invalid {
            field: "exclusions"
        })
    ));
    assert!(matches!(
        VaultExclusion::new("C:\\\\vault"),
        Err(VaultConfigError::Invalid {
            field: "exclusions"
        })
    ));
    assert!(matches!(
        VaultExclusion::new("  "),
        Err(VaultConfigError::Invalid {
            field: "exclusions"
        })
    ));
    let exclusion = VaultExclusion::new("archive/old").expect("relative exclusion");
    assert_eq!(exclusion.as_str(), "archive/old");
}

#[test]
fn vault_config_debug_never_exposes_the_local_root() {
    let config = valid_config(PathBuf::from("C:\\Users\\secret-person\\PrivateVault"));
    let rendered = format!("{config:?}");
    assert!(!rendered.contains("secret-person"));
    assert!(!rendered.contains("PrivateVault"));
}

#[test]
fn vault_root_accessibility_is_a_typed_diagnostic() {
    let missing = valid_config(PathBuf::from("Z:\\definitely\\not\\here"));
    assert!(matches!(
        missing.validate_root_access(),
        Err(VaultConfigError::RootInaccessible)
    ));

    let directory = TempDir::new().expect("temporary directory");
    let present = valid_config(directory.path().to_path_buf());
    assert!(present.validate_root_access().is_ok());
}

#[test]
fn settings_vault_section_parses_into_the_typed_config() {
    let directory = TempDir::new().expect("temporary directory");
    let root = directory.path().join("vault-root");
    std::fs::create_dir_all(&root).expect("vault root directory");
    let path = write_settings(
        directory.path(),
        &format!(
            r#"
[vault]
provider_id = "markdown-vault"
root = "{}"
mode = "read_only"
scopes = ["knowledge", "task"]
exclusions = [".obsidian", "archive"]
"#,
            root.to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/")
        ),
    );
    let settings = LocalSettings::load(&path)
        .expect("valid settings")
        .expect("file present");
    let config = settings
        .vault_config()
        .expect("valid vault configuration")
        .expect("vault section present");
    assert_eq!(config.provider_id().as_str(), "markdown-vault");
    assert_eq!(config.mode(), VaultProviderMode::ReadOnly);
    assert_eq!(config.scopes().len(), 2);
    assert_eq!(config.exclusions().len(), 2);
    assert!(config.allows_kind(ProviderResourceKind::Knowledge));
    assert!(config.allows_kind(ProviderResourceKind::Task));
}

#[test]
fn settings_vault_section_rejects_invalid_and_absent_values() {
    let directory = TempDir::new().expect("temporary directory");

    std::fs::create_dir_all(directory.path().join("a")).expect("subdirectory");
    let unknown_scope = write_settings(
        &directory.path().join("a"),
        r#"
[vault]
provider_id = "markdown-vault"
root = "unused"
mode = "read_write"
scopes = ["calendar"]
"#,
    );
    let settings = LocalSettings::load(&unknown_scope)
        .expect("file parses")
        .expect("file present");
    assert!(matches!(
        settings.vault_config(),
        Err(SettingsError::Invalid { field: "scopes" })
    ));

    // Unknown keys (including any credential-shaped field) fail startup.
    std::fs::create_dir_all(directory.path().join("b")).expect("subdirectory");
    let credential = write_settings(
        &directory.path().join("b"),
        r#"
[vault]
provider_id = "markdown-vault"
root = "unused"
mode = "read_write"
scopes = ["knowledge"]
password = "hunter2"
"#,
    );
    let loaded = LocalSettings::load(&credential);
    assert!(matches!(
        loaded,
        Err(SettingsError::Invalid { field: "file" })
    ));

    let absent = LocalSettings::load(&directory.path().join("c").join("cortexd.toml"))
        .expect("missing file defaults");
    assert!(absent.is_none());
}

#[tokio::test]
async fn daemon_composition_mounts_and_validates_the_vault_provider() {
    let directory = TempDir::new().expect("temporary directory");
    let root = directory.path().join("vault-root");
    std::fs::create_dir_all(&root).expect("vault root directory");

    let mounted = DaemonConfig::for_test(directory.path())
        .with_vault_provider(valid_config(root.clone()))
        .expect("accessible vault root");
    let mounted_config = mounted.vault_provider().expect("vault provider mounted");
    assert_eq!(mounted_config.provider_id().as_str(), "markdown-vault");

    let inaccessible = DaemonConfig::for_test(directory.path())
        .with_vault_provider(valid_config(directory.path().join("missing-root")));
    assert!(inaccessible.is_err());

    // From local settings: a declared vault mounts; a missing root fails
    // startup with a typed configuration error.
    let settings_dir = TempDir::new().expect("temporary directory");
    let settings_root = settings_dir.path().join("vault");
    std::fs::create_dir_all(&settings_root).expect("vault root directory");
    let path = write_settings(
        settings_dir.path(),
        &format!(
            r#"
[vault]
provider_id = "markdown-vault"
root = "{}"
mode = "read_only"
scopes = ["knowledge"]
"#,
            settings_root
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/")
        ),
    );
    let settings = LocalSettings::load(&path)
        .expect("valid settings")
        .expect("file present");
    let composed =
        DaemonConfig::from_local_settings(settings_dir.path().join("cortex.db"), Some(&settings))
            .expect("valid composition");
    assert!(composed.vault_provider().is_some());

    let dangling = write_settings(
        settings_dir.path(),
        r#"
[vault]
provider_id = "markdown-vault"
root = "Z:\\missing\\vault"
mode = "read_only"
scopes = ["knowledge"]
"#,
    );
    let settings = LocalSettings::load(&dangling)
        .expect("valid settings")
        .expect("file present");
    let failed =
        DaemonConfig::from_local_settings(settings_dir.path().join("cortex.db"), Some(&settings));
    assert!(failed.is_err());
}

#[tokio::test]
async fn fake_provider_starts_and_serves_through_the_composition_boundary() {
    // The in-memory provider is the SCRUM-98 seam: a daemon test composes
    // knowledge/task providers without a filesystem, Obsidian, or a sync
    // process, through the same provider ports SCRUM-91's Markdown adapter
    // must implement.
    let directory = TempDir::new().expect("temporary directory");
    let config = DaemonConfig::for_test(directory.path())
        .with_vault_provider(valid_config(directory.path().to_path_buf()))
        .expect("accessible root");
    let workspace_id = config.workspace_id();

    let provider = InMemoryVaultProvider::new();
    let created = KnowledgeProvider::create(
        &provider,
        KnowledgeCreate::new(
            workspace_id,
            OperationId::new(),
            "Composed",
            "fake provider body",
        )
        .expect("valid create input"),
    )
    .await
    .expect("create succeeds");
    let resource = created.resource();

    let read = KnowledgeProvider::get(&provider, resource)
        .await
        .expect("get succeeds")
        .expect("created document found");
    assert_eq!(read.item().title(), "Composed");

    let limit = NonZeroUsize::new(10).expect("non-zero limit");
    let listed = KnowledgeProvider::search(
        &provider,
        &KnowledgeQuery::list(workspace_id, limit).expect("valid query"),
    )
    .await
    .expect("search succeeds");
    assert_eq!(listed.items().len(), 1);

    let task = TaskProvider::create(
        &provider,
        TaskCreate::new(
            workspace_id,
            OperationId::new(),
            TaskId::new(),
            "Composed task",
            "task body",
            cortex_application::ProviderTaskPriority::Normal,
            cortex_application::TaskSchedulingMetadata::new(
                None,
                None,
                None,
                None,
                None,
                Option::<String>::None,
                Option::<String>::None,
            )
            .expect("empty scheduling is valid"),
        )
        .expect("valid task create input"),
    )
    .await
    .expect("task create succeeds");
    let listed_tasks = TaskProvider::search(
        &provider,
        &TaskQuery::new(workspace_id, Option::<String>::None, limit).expect("valid query"),
    )
    .await
    .expect("task search succeeds");
    assert_eq!(listed_tasks.items().len(), 1);
    assert_eq!(
        listed_tasks.items()[0].provenance().resource(),
        task.resource()
    );
    // A knowledge read against a task-kind resource is a typed validation
    // error, never a silent cross-kind hit.
    let wrong_kind = cortex_domain::ProviderResourceRef::new(
        workspace_id,
        task.resource().provider_id().clone(),
        task.resource().resource_id().clone(),
        ProviderResourceKind::Task,
    );
    assert!(matches!(
        KnowledgeProvider::get(&provider, &wrong_kind).await,
        Err(ProviderError::Validation {
            field: "resource_kind"
        })
    ));
}
