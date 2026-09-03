use std::path::{Path, PathBuf};

use cortex_application::SecretRef;
use cortex_domain::{PrincipalId, WorkspaceId};

/// Configuration owned locally by the daemon process, never by an IPC caller.
#[derive(Clone, Debug)]
pub struct DaemonConfig {
    pub(crate) database_path: PathBuf,
    pub(crate) endpoint_name: String,
    pub(crate) workspace_id: WorkspaceId,
    pub(crate) principal_id: PrincipalId,
    pub(crate) inference_secret: Option<SecretRef>,
}

impl DaemonConfig {
    /// Creates deterministic test-only local configuration with fresh ownership IDs.
    #[must_use]
    pub fn for_test(directory: &Path) -> Self {
        Self {
            database_path: directory.join("cortex.db"),
            endpoint_name: format!("cortexd-test-{}", uuid::Uuid::now_v7()),
            workspace_id: WorkspaceId::new(),
            principal_id: PrincipalId::new(),
            inference_secret: None,
        }
    }

    /// Loads a daemon configuration without accepting any identity from a client.
    ///
    /// # Errors
    /// Returns a safe configuration error when the database path is absent.
    pub fn from_database_path(database_path: PathBuf) -> Result<Self, crate::DaemonError> {
        if database_path.as_os_str().is_empty() {
            return Err(crate::DaemonError::InvalidConfiguration);
        }
        Ok(Self {
            database_path,
            endpoint_name: format!("cortexd-{}", uuid::Uuid::now_v7()),
            workspace_id: WorkspaceId::new(),
            principal_id: PrincipalId::new(),
            inference_secret: None,
        })
    }

    /// Adds an opaque inference credential locator that must be resolved by the composition root.
    #[must_use]
    pub fn with_inference_secret(mut self, reference: SecretRef) -> Self {
        self.inference_secret = Some(reference);
        self
    }
}
