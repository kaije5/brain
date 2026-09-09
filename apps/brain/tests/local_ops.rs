use std::cell::RefCell;

use std::ffi::OsString;

use brain::{
    LocalOpError, SecretWriter, data_directory_for, default_database_path_for, import_secret,
    init_config,
};
use tempfile::TempDir;

struct RecordingStore {
    written: RefCell<Vec<(String, String, Vec<u8>)>>,
    fail: bool,
}

impl SecretWriter for RecordingStore {
    fn write(&self, service: &str, username: &str, secret: &[u8]) -> Result<(), LocalOpError> {
        if self.fail {
            return Err(LocalOpError::StoreUnavailable);
        }
        self.written
            .borrow_mut()
            .push((service.to_owned(), username.to_owned(), secret.to_vec()));
        Ok(())
    }
}

#[test]
fn init_config_writes_the_documented_template_once() {
    let directory = TempDir::new().expect("temporary directory");
    let database = directory.path().join("cortex.db");
    let written = init_config(&database).expect("first init succeeds");
    assert_eq!(written, directory.path().join("cortexd.toml"));
    let contents = std::fs::read_to_string(&written).expect("template written");
    assert!(contents.contains("default_profile"));
    assert!(!contents.to_lowercase().contains("api_key"));

    assert!(
        matches!(
            init_config(&database),
            Err(LocalOpError::ConfigAlreadyExists)
        ),
        "init must never overwrite an existing config"
    );
}

#[test]
fn import_secret_writes_keyring_entry_and_yields_secret_ref() {
    let store = RecordingStore {
        written: RefCell::new(Vec::new()),
        fail: false,
    };
    let reference = import_secret(&store, "nim", b"nvapi-test-value").expect("import succeeds");
    assert_eq!(reference, "keyring:cortexd/nim");
    let (service, username, secret) = &store.written.borrow()[0];
    assert_eq!((service.as_str(), username.as_str()), ("cortexd", "nim"));
    assert_eq!(secret.as_slice(), b"nvapi-test-value");
}

#[test]
fn import_secret_rejects_profiles_that_could_escape_the_keyring_target() {
    let store = RecordingStore {
        written: RefCell::new(Vec::new()),
        fail: false,
    };
    for profile in ["", "a/b", "has space", "tab\there"] {
        assert!(
            matches!(
                import_secret(&store, profile, b"secret"),
                Err(LocalOpError::InvalidProfile)
            ),
            "profile `{profile}` must be rejected"
        );
    }
    assert!(store.written.borrow().is_empty());
}

#[test]
fn import_secret_surfaces_store_failures_without_leaking_the_secret() {
    let store = RecordingStore {
        written: RefCell::new(Vec::new()),
        fail: true,
    };
    let error = import_secret(&store, "nim", b"nvapi-test-value").expect_err("store fails");
    assert!(
        !format!("{error:?}").contains("nvapi"),
        "errors must never embed the raw secret"
    );
}

#[test]
fn default_paths_need_no_override_and_follow_it_when_present() {
    let directory = TempDir::new().expect("temporary directory");
    let database = directory.path().join("custom.db");
    let overridden: Option<OsString> = Some(database.clone().into_os_string());

    assert!(default_database_path_for(None).ends_with("cortex.db"));
    assert!(data_directory_for(None).ends_with("cortex"));

    assert_eq!(data_directory_for(overridden.as_deref()), directory.path());
    assert_eq!(
        default_database_path_for(overridden.as_deref()),
        directory.path().join("custom.db")
    );
}
