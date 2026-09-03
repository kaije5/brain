use std::{
    fs,
    path::{Path, PathBuf},
};

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
    pub(crate) pairing_proof: uuid::Uuid,
    pub(crate) discovery_path: PathBuf,
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
            pairing_proof: uuid::Uuid::now_v7(),
            discovery_path: directory.join("cortexd-discovery.json"),
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
        let discovery_path = database_path.with_extension("cortexd-discovery.json");
        if discovery_path.exists() {
            let bytes =
                fs::read(&discovery_path).map_err(|_| crate::DaemonError::InvalidConfiguration)?;
            let discovery: Discovery = serde_json::from_slice(&bytes)
                .map_err(|_| crate::DaemonError::InvalidConfiguration)?;
            return Ok(Self {
                database_path,
                endpoint_name: discovery.endpoint_name,
                workspace_id: WorkspaceId::try_from(discovery.workspace_id)
                    .map_err(|_| crate::DaemonError::InvalidConfiguration)?,
                principal_id: PrincipalId::try_from(discovery.principal_id)
                    .map_err(|_| crate::DaemonError::InvalidConfiguration)?,
                inference_secret: None,
                pairing_proof: uuid::Uuid::now_v7(),
                discovery_path,
            });
        }
        let config = Self {
            database_path,
            endpoint_name: format!("cortexd-{}", uuid::Uuid::now_v7()),
            workspace_id: WorkspaceId::new(),
            principal_id: PrincipalId::new(),
            inference_secret: None,
            pairing_proof: uuid::Uuid::now_v7(),
            discovery_path,
        };
        config.write_discovery()?;
        Ok(config)
    }

    /// Adds an opaque inference credential locator that must be resolved by the composition root.
    #[must_use]
    pub fn with_inference_secret(mut self, reference: SecretRef) -> Self {
        self.inference_secret = Some(reference);
        self
    }

    pub(crate) fn write_discovery(&self) -> Result<(), crate::DaemonError> {
        let discovery = Discovery {
            endpoint_name: self.endpoint_name.clone(),
            workspace_id: self.workspace_id.into(),
            principal_id: self.principal_id.into(),
        };
        let bytes =
            serde_json::to_vec(&discovery).map_err(|_| crate::DaemonError::InvalidConfiguration)?;
        fs::write(&self.discovery_path, bytes).map_err(|_| crate::DaemonError::InvalidConfiguration)
    }
}

#[derive(serde::Deserialize, serde::Serialize)]
struct Discovery {
    endpoint_name: String,
    workspace_id: uuid::Uuid,
    principal_id: uuid::Uuid,
}
