//! Daemon-owned vault provider configuration and composition seam.
//!
//! The vault provider becomes the authoritative store for user-authored
//! documents and tasks (ADR-025/ADR-026). This module owns the validated,
//! non-secret daemon configuration for that provider: one local root, a
//! bounded set of allowed logical scopes, bounded exclusions, and an explicit
//! provider mode. Filesystem path types stay in cortexd and are converted
//! into provider construction inputs only — they never become domain
//! resources. Credentials and sync-transport state have no representable
//! field here.
//!
//! The concrete Markdown adapter and its path-confinement enforcement belong
//! to SCRUM-91; [`InMemoryVaultProvider`] is the composition test seam that
//! proves a fake provider starts and serves through the same boundary.

#![allow(clippy::result_large_err)]

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    path::{Path, PathBuf},
    sync::Mutex,
};

use cortex_application::{
    KnowledgeCreate, KnowledgeDelete, KnowledgeDocument, KnowledgeProvider, KnowledgeQuery,
    KnowledgeUpdate, ProviderError, ProviderFreshness, ProviderMutation, ProviderPage,
    ProviderRead, ProviderTask, ProviderTaskStatus, TaskComplete, TaskCreate, TaskDelete,
    TaskProvider, TaskQuery, TaskUpdate,
};
use cortex_domain::{
    ContentHash, ObservedRevision, ProviderId, ProviderProvenance, ProviderResourceId,
    ProviderResourceKind, ProviderResourceRef, WorkspaceId,
};
use sha2::{Digest, Sha256};

const MAX_ROOT_BYTES: usize = 1024;
const MAX_SCOPES: usize = 8;
const MAX_EXCLUSIONS: usize = 64;
const MAX_EXCLUSION_BYTES: usize = 256;

/// Coarse, value-free failure category for vault provider configuration.
/// Errors never embed the configured root path or any other value.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum VaultConfigError {
    /// A declared value is absent, malformed, out of bounds, or contradictory.
    #[error("invalid vault provider configuration")]
    Invalid {
        /// Coarse field name for diagnostics; never a value.
        field: &'static str,
    },
    /// The configured local root does not exist or is not a directory.
    #[error("vault root is inaccessible")]
    RootInaccessible,
}

/// Explicit mutation posture of the configured provider. Kept separate from
/// capability grants so a future read-only deployment can deny provider
/// writes in policy without touching daemon wiring.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum VaultProviderMode {
    /// The provider may serve reads and mutations (default).
    #[default]
    ReadWrite,
    /// The provider serves reads only; provider mutations are refused.
    ReadOnly,
}

/// One allowed logical scope, addressed by provider resource kind. The
/// daemon's durable workspace bounds the scope; no free-form scope strings
/// are accepted.
#[derive(Clone, Copy, Debug, Eq, PartialEq, PartialOrd, Ord)]
pub struct VaultScope(ProviderResourceKind);

impl VaultScope {
    /// Validates one configured logical scope name.
    ///
    /// # Errors
    /// Returns [`VaultConfigError::Invalid`] for names outside the typed
    /// scope set.
    pub fn new(name: &str) -> Result<Self, VaultConfigError> {
        let kind = match name {
            "knowledge" => ProviderResourceKind::Knowledge,
            "task" => ProviderResourceKind::Task,
            _ => {
                return Err(VaultConfigError::Invalid { field: "scopes" });
            }
        };
        Ok(Self(kind))
    }

    #[must_use]
    pub const fn resource_kind(&self) -> ProviderResourceKind {
        self.0
    }
}

/// One bounded, vault-relative exclusion pattern (for example a directory or
/// file name the provider must never index or mutate). Absolute paths and
/// parent traversal are structurally rejected.
#[derive(Clone, Eq, PartialEq, PartialOrd, Ord)]
pub struct VaultExclusion(String);

impl fmt::Debug for VaultExclusion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("VaultExclusion([redacted])")
    }
}

impl VaultExclusion {
    /// Validates one configured exclusion pattern.
    ///
    /// # Errors
    /// Returns [`VaultConfigError::Invalid`] when the pattern is blank,
    /// oversized, contains control characters, is absolute, or escapes the
    /// vault root with `..` components.
    pub fn new(pattern: &str) -> Result<Self, VaultConfigError> {
        if pattern.trim().is_empty()
            || pattern.len() > MAX_EXCLUSION_BYTES
            || pattern.chars().any(char::is_control)
        {
            return Err(VaultConfigError::Invalid {
                field: "exclusions",
            });
        }
        let path = Path::new(pattern);
        if path.is_absolute()
            || path.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir | std::path::Component::RootDir
                )
            })
        {
            return Err(VaultConfigError::Invalid {
                field: "exclusions",
            });
        }
        Ok(Self(pattern.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Validated, daemon-owned vault provider configuration.
///
/// `Debug` is fully redacted: the local root is infrastructure state that
/// must never reach logs or client-visible diagnostics.
#[derive(Clone, Eq, PartialEq)]
pub struct VaultProviderConfig {
    provider_id: ProviderId,
    root: PathBuf,
    mode: VaultProviderMode,
    scopes: BTreeSet<VaultScope>,
    exclusions: BTreeSet<VaultExclusion>,
}

impl fmt::Debug for VaultProviderConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VaultProviderConfig")
            .field("provider_id", &"[redacted]")
            .field("root", &"[redacted]")
            .field("mode", &self.mode)
            .field("scopes", &self.scopes)
            .field("exclusion_count", &self.exclusions.len())
            .finish()
    }
}

impl VaultProviderConfig {
    /// Validates and builds the vault provider configuration.
    ///
    /// # Errors
    /// Returns [`VaultConfigError::Invalid`] when the provider id, root,
    /// scopes, or exclusions fail their bounds, or when the configuration is
    /// contradictory (for example no allowed scopes).
    pub fn new(
        provider_id: &str,
        root: PathBuf,
        mode: VaultProviderMode,
        scopes: BTreeSet<VaultScope>,
        exclusions: BTreeSet<VaultExclusion>,
    ) -> Result<Self, VaultConfigError> {
        let provider_id = ProviderId::new(provider_id).map_err(|_| VaultConfigError::Invalid {
            field: "provider_id",
        })?;
        let root_text = root
            .to_str()
            .filter(|text| !text.trim().is_empty())
            .ok_or(VaultConfigError::Invalid { field: "root" })?;
        if root_text.len() > MAX_ROOT_BYTES {
            return Err(VaultConfigError::Invalid { field: "root" });
        }
        if root.as_os_str().is_empty() {
            return Err(VaultConfigError::Invalid { field: "root" });
        }
        if scopes.is_empty() || scopes.len() > MAX_SCOPES {
            return Err(VaultConfigError::Invalid { field: "scopes" });
        }
        if exclusions.len() > MAX_EXCLUSIONS {
            return Err(VaultConfigError::Invalid {
                field: "exclusions",
            });
        }
        Ok(Self {
            provider_id,
            root,
            mode,
            scopes,
            exclusions,
        })
    }

    /// Verifies the configured root exists and is a directory. Called at
    /// daemon composition so an inaccessible vault fails startup with a
    /// typed diagnostic instead of failing later mid-operation.
    ///
    /// # Errors
    /// Returns [`VaultConfigError::RootInaccessible`] when the root is
    /// missing or not a directory.
    pub fn validate_root_access(&self) -> Result<(), VaultConfigError> {
        if self.root.is_dir() {
            Ok(())
        } else {
            Err(VaultConfigError::RootInaccessible)
        }
    }

    #[must_use]
    pub const fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub const fn mode(&self) -> VaultProviderMode {
        self.mode
    }

    #[must_use]
    pub const fn scopes(&self) -> &BTreeSet<VaultScope> {
        &self.scopes
    }

    #[must_use]
    pub const fn exclusions(&self) -> &BTreeSet<VaultExclusion> {
        &self.exclusions
    }

    /// Returns whether the configured scopes allow provider mutations of the
    /// given resource kind at all. Policy remains authoritative; this is a
    /// composition-time sanity check for the adapter.
    #[must_use]
    pub fn allows_kind(&self, kind: ProviderResourceKind) -> bool {
        self.scopes
            .iter()
            .any(|scope| scope.resource_kind() == kind)
    }
}

/// In-memory fake implementing both knowledge and task provider ports.
///
/// This is the SCRUM-98 composition seam: daemon tests start against this
/// provider to prove the boundary works without a filesystem, Obsidian, or a
/// live sync process. It is deliberately not a Markdown implementation and
/// carries no path semantics.
pub struct InMemoryVaultProvider {
    state: Mutex<InMemoryState>,
}

#[derive(Default)]
struct InMemoryState {
    sequence: u64,
    knowledge: BTreeMap<ProviderResourceRef, KnowledgeDocument>,
    tasks: BTreeMap<ProviderResourceRef, ProviderTask>,
}

impl Default for InMemoryVaultProvider {
    fn default() -> Self {
        Self {
            state: Mutex::new(InMemoryState::default()),
        }
    }
}

impl InMemoryVaultProvider {
    /// Creates an available fake provider with current observations.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn resource(
        workspace: WorkspaceId,
        sequence: u64,
        kind: ProviderResourceKind,
    ) -> Result<ProviderResourceRef, ProviderError> {
        Ok(ProviderResourceRef::new(
            workspace,
            ProviderId::new("in-memory-vault").map_err(|_| ProviderError::Internal)?,
            ProviderResourceId::new(format!("resource-{sequence}"))
                .map_err(|_| ProviderError::Internal)?,
            kind,
        ))
    }

    fn provenance(
        resource: ProviderResourceRef,
        revision: u64,
        title: &str,
        body: &str,
    ) -> Result<ProviderProvenance, ProviderError> {
        let mut hasher = Sha256::new();
        hasher.update(title.as_bytes());
        hasher.update([0]);
        hasher.update(body.as_bytes());
        let hash = hasher.finalize().into();
        Ok(ProviderProvenance::new(
            resource,
            ObservedRevision::new(format!("rev-{revision}"))
                .map_err(|_| ProviderError::Internal)?,
            ContentHash::new(hash),
        ))
    }

    fn revision(
        current: &ProviderProvenance,
        expected: &ObservedRevision,
    ) -> Result<u64, ProviderError> {
        if current.observed_revision() != expected {
            return Err(ProviderError::Conflict {
                current: current.clone(),
            });
        }
        let parsed = current
            .observed_revision()
            .as_str()
            .trim_start_matches("rev-")
            .parse::<u64>()
            .unwrap_or(0);
        Ok(parsed + 1)
    }
}

impl KnowledgeProvider for InMemoryVaultProvider {
    async fn get(
        &self,
        resource: &ProviderResourceRef,
    ) -> Result<Option<ProviderRead<KnowledgeDocument>>, ProviderError> {
        if resource.kind() != ProviderResourceKind::Knowledge {
            return Err(ProviderError::Validation {
                field: "resource_kind",
            });
        }
        Ok(self
            .state
            .lock()
            .map_err(|_| ProviderError::Internal)?
            .knowledge
            .get(resource)
            .cloned()
            .map(|item| ProviderRead::new(item, ProviderFreshness::Current)))
    }

    async fn search(
        &self,
        query: &KnowledgeQuery,
    ) -> Result<ProviderPage<KnowledgeDocument>, ProviderError> {
        let text = query.text().map(str::to_lowercase);
        let items = self
            .state
            .lock()
            .map_err(|_| ProviderError::Internal)?
            .knowledge
            .values()
            .filter(|item| item.provenance().resource().workspace_id() == query.workspace_id())
            .filter(|item| {
                text.as_ref().is_none_or(|text| {
                    item.title().to_lowercase().contains(text)
                        || item.body().to_lowercase().contains(text)
                })
            })
            .take(query.limit().get())
            .cloned()
            .collect();
        ProviderPage::new(items, ProviderFreshness::Current)
    }

    async fn create(&self, input: KnowledgeCreate) -> Result<ProviderMutation, ProviderError> {
        let mut state = self.state.lock().map_err(|_| ProviderError::Internal)?;
        state.sequence += 1;
        let resource = Self::resource(
            input.workspace_id(),
            state.sequence,
            ProviderResourceKind::Knowledge,
        )?;
        let provenance = Self::provenance(resource.clone(), 1, input.title(), input.body())?;
        let document = KnowledgeDocument::new(provenance.clone(), input.title(), input.body())?;
        state.knowledge.insert(resource, document);
        Ok(ProviderMutation::created(provenance))
    }

    async fn update(&self, input: KnowledgeUpdate) -> Result<ProviderMutation, ProviderError> {
        let mut state = self.state.lock().map_err(|_| ProviderError::Internal)?;
        let current = state
            .knowledge
            .get(input.resource())
            .cloned()
            .ok_or_else(|| ProviderError::NotFound {
                resource: input.resource().clone(),
            })?;
        let revision = Self::revision(current.provenance(), input.expected_revision())?;
        let previous = current.provenance().clone();
        let provenance = Self::provenance(
            input.resource().clone(),
            revision,
            input.title(),
            input.body(),
        )?;
        state.knowledge.insert(
            input.resource().clone(),
            KnowledgeDocument::new(provenance.clone(), input.title(), input.body())?,
        );
        ProviderMutation::updated(previous, provenance)
    }

    async fn delete(&self, input: KnowledgeDelete) -> Result<ProviderMutation, ProviderError> {
        let mut state = self.state.lock().map_err(|_| ProviderError::Internal)?;
        let current =
            state
                .knowledge
                .get(input.resource())
                .ok_or_else(|| ProviderError::NotFound {
                    resource: input.resource().clone(),
                })?;
        Self::revision(current.provenance(), input.expected_revision())?;
        let removed = state
            .knowledge
            .remove(input.resource())
            .ok_or(ProviderError::Internal)?;
        Ok(ProviderMutation::deleted(removed.provenance().clone()))
    }
}

impl TaskProvider for InMemoryVaultProvider {
    async fn get(
        &self,
        resource: &ProviderResourceRef,
    ) -> Result<Option<ProviderRead<ProviderTask>>, ProviderError> {
        if resource.kind() != ProviderResourceKind::Task {
            return Err(ProviderError::Validation {
                field: "resource_kind",
            });
        }
        Ok(self
            .state
            .lock()
            .map_err(|_| ProviderError::Internal)?
            .tasks
            .get(resource)
            .cloned()
            .map(|item| ProviderRead::new(item, ProviderFreshness::Current)))
    }

    async fn search(&self, query: &TaskQuery) -> Result<ProviderPage<ProviderTask>, ProviderError> {
        let text = query.text().map(str::to_lowercase);
        let items = self
            .state
            .lock()
            .map_err(|_| ProviderError::Internal)?
            .tasks
            .values()
            .filter(|item| item.provenance().resource().workspace_id() == query.workspace_id())
            .filter(|item| {
                text.as_ref().is_none_or(|text| {
                    item.title().to_lowercase().contains(text)
                        || item.body().to_lowercase().contains(text)
                })
            })
            .take(query.limit().get())
            .cloned()
            .collect();
        ProviderPage::new(items, ProviderFreshness::Current)
    }

    async fn create(&self, input: TaskCreate) -> Result<ProviderMutation, ProviderError> {
        let mut state = self.state.lock().map_err(|_| ProviderError::Internal)?;
        state.sequence += 1;
        let resource = Self::resource(
            input.workspace_id(),
            state.sequence,
            ProviderResourceKind::Task,
        )?;
        let provenance = Self::provenance(resource.clone(), 1, input.title(), input.body())?;
        let task = ProviderTask::new(
            provenance.clone(),
            input.task_id(),
            input.title(),
            input.body(),
            ProviderTaskStatus::Todo,
            input.priority(),
            input.scheduling().clone(),
        )?;
        state.tasks.insert(resource, task);
        Ok(ProviderMutation::created(provenance))
    }

    async fn update(&self, input: TaskUpdate) -> Result<ProviderMutation, ProviderError> {
        let mut state = self.state.lock().map_err(|_| ProviderError::Internal)?;
        let current =
            state
                .tasks
                .get(input.resource())
                .cloned()
                .ok_or_else(|| ProviderError::NotFound {
                    resource: input.resource().clone(),
                })?;
        let revision = Self::revision(current.provenance(), input.expected_revision())?;
        let previous = current.provenance().clone();
        let provenance = Self::provenance(
            input.resource().clone(),
            revision,
            input.title(),
            input.body(),
        )?;
        let updated = ProviderTask::new(
            provenance.clone(),
            current.task_id(),
            input.title(),
            input.body(),
            input.status(),
            input.priority(),
            input.scheduling().clone(),
        )?;
        state.tasks.insert(input.resource().clone(), updated);
        ProviderMutation::updated(previous, provenance)
    }

    async fn complete(&self, input: TaskComplete) -> Result<ProviderMutation, ProviderError> {
        let mut state = self.state.lock().map_err(|_| ProviderError::Internal)?;
        let current =
            state
                .tasks
                .get(input.resource())
                .cloned()
                .ok_or_else(|| ProviderError::NotFound {
                    resource: input.resource().clone(),
                })?;
        let revision = Self::revision(current.provenance(), input.expected_revision())?;
        let previous = current.provenance().clone();
        let provenance = Self::provenance(
            input.resource().clone(),
            revision,
            current.title(),
            current.body(),
        )?;
        let completed = ProviderTask::new(
            provenance.clone(),
            current.task_id(),
            current.title(),
            current.body(),
            ProviderTaskStatus::Completed,
            current.priority(),
            current.scheduling().clone(),
        )?;
        state.tasks.insert(input.resource().clone(), completed);
        ProviderMutation::updated(previous, provenance)
    }

    async fn delete(&self, input: TaskDelete) -> Result<ProviderMutation, ProviderError> {
        let mut state = self.state.lock().map_err(|_| ProviderError::Internal)?;
        let current = state
            .tasks
            .get(input.resource())
            .ok_or_else(|| ProviderError::NotFound {
                resource: input.resource().clone(),
            })?;
        Self::revision(current.provenance(), input.expected_revision())?;
        let removed = state
            .tasks
            .remove(input.resource())
            .ok_or(ProviderError::Internal)?;
        Ok(ProviderMutation::deleted(removed.provenance().clone()))
    }
}
