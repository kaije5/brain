#![allow(clippy::result_large_err)]
//! The first-party Markdown vault provider: root/scope confinement and the
//! confined read operations.
//!
//! This module owns the security boundary between Cortex and the configured
//! local vault (format spec; storage plan §9). Every read and mutation in
//! later subtasks obtains its absolute path through [`MarkdownVaultProvider::confine`],
//! which enforces — in one place — that paths are vault-relative and bounded,
//! that they stay inside the canonical root even across symlinks, and that
//! they honor the configured exclusions and resource scopes. Errors are
//! typed and value-free: local paths never cross this boundary outward.

use std::{
    collections::BTreeSet,
    num::NonZeroUsize,
    path::{Component, Path, PathBuf},
};

use cortex_application::{
    KnowledgeCreate, KnowledgeDelete, KnowledgeDocument, KnowledgeProvider, KnowledgeQuery,
    KnowledgeUpdate, ProviderError, ProviderFreshness, ProviderMutation, ProviderPage,
    ProviderRead, ProviderTask, ProviderTaskPriority, ProviderTaskStatus, TaskComplete, TaskCreate,
    TaskDelete, TaskProvider, TaskQuery, TaskSchedulingMetadata, TaskUpdate,
};
use cortex_domain::{
    ContentHash, ObservedRevision, ProviderId, ProviderProvenance, ProviderResourceId,
    ProviderResourceKind, ProviderResourceRef, WorkspaceId,
};
use cortex_vault::{parse_document, parse_task};
use sha2::{Digest, Sha256};

use crate::vault::{VaultConfigError, VaultProviderConfig};

const MAX_RELATIVE_BYTES: usize = 1024;
const MAX_ENUMERATED_FILES: usize = 10_000;
/// Documents are addressed by their vault-relative path: the format spec has
/// no on-disk stable document id, so document rename semantics are handled by
/// the derived index (SCRUM-92). Tasks use the rename-stable `brain_id`.
const DOCUMENT_ID_PREFIX: &str = "path:";

/// Typed, value-free confinement failures. Variants never carry paths.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum VaultPathError {
    /// The relative path is absent, malformed, or exceeds its bound.
    #[error("invalid vault path")]
    InvalidPath,
    /// The path attempts traversal outside the vault root.
    #[error("path escapes the vault root")]
    Traversal,
    /// The path resolves outside the canonical root through a symlink.
    #[error("symlink escape rejected")]
    SymlinkEscape,
    /// The path is inside a configured exclusion.
    #[error("path is excluded")]
    Excluded,
    /// The resource kind is not in the configured scopes.
    #[error("resource kind out of scope")]
    OutOfScope,
    /// The configured root could not be canonicalized.
    #[error("vault root is invalid")]
    InvalidRoot,
    /// A rename target already exists; renames never clobber another
    /// writer's file.
    #[error("rename target exists")]
    TargetExists,
}

impl From<VaultConfigError> for VaultPathError {
    fn from(error: VaultConfigError) -> Self {
        match error {
            VaultConfigError::RootInaccessible | VaultConfigError::Invalid { field: "root" } => {
                Self::InvalidRoot
            }
            VaultConfigError::Invalid { .. } => Self::InvalidPath,
        }
    }
}

/// A vault-relative path that passed confinement, paired with its absolute
/// location. Constructed only through [`MarkdownVaultProvider::confine`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfinedPath {
    relative: String,
    absolute: PathBuf,
}

impl ConfinedPath {
    /// The normalized vault-relative path (`/`-separated).
    #[must_use]
    pub fn relative(&self) -> &str {
        &self.relative
    }

    /// The confined absolute path. Callers must not reconstruct paths by
    /// string concatenation; every operation goes through confinement.
    #[must_use]
    pub fn absolute(&self) -> &Path {
        &self.absolute
    }
}

/// The first-party adapter over the configured local Markdown vault.
#[derive(Clone, Debug)]
pub struct MarkdownVaultProvider {
    config: VaultProviderConfig,
    workspace_id: WorkspaceId,
    provider_id: ProviderId,
    canonical_root: PathBuf,
}

impl MarkdownVaultProvider {
    /// Opens the vault: validates the configuration and canonicalizes the
    /// root so every later confinement check compares against one exact
    /// prefix. The daemon workspace scopes every constructed resource
    /// reference.
    ///
    /// # Errors
    /// Returns [`VaultPathError::InvalidRoot`] when the root is missing,
    /// not a directory, or cannot be canonicalized.
    pub fn open(
        config: VaultProviderConfig,
        workspace_id: WorkspaceId,
    ) -> Result<Self, VaultPathError> {
        config.validate_root_access()?;
        let canonical_root =
            std::fs::canonicalize(config.root()).map_err(|_| VaultPathError::InvalidRoot)?;
        let provider_id = ProviderId::new(config.provider_id().as_str())
            .map_err(|_| VaultPathError::InvalidPath)?;
        Ok(Self {
            config,
            workspace_id,
            provider_id,
            canonical_root,
        })
    }

    /// The validated configuration this provider was opened with.
    #[must_use]
    pub const fn config(&self) -> &VaultProviderConfig {
        &self.config
    }

    /// The provider identity every constructed reference carries.
    #[must_use]
    pub const fn provider_reference_id(&self) -> &ProviderId {
        &self.provider_id
    }

    /// The workspace every constructed resource reference is scoped to.
    #[must_use]
    pub const fn workspace_id(&self) -> WorkspaceId {
        self.workspace_id
    }

    /// The canonical vault root (for internal composition only).
    #[must_use]
    pub fn canonical_root(&self) -> &Path {
        &self.canonical_root
    }

    /// Confines a vault-relative path for a resource kind. This is the single
    /// gate for all later read and mutation operations.
    ///
    /// # Errors
    /// Returns the matching typed [`VaultPathError`] for invalid paths,
    /// traversal, symlink escape, configured exclusions, and out-of-scope
    /// resource kinds.
    pub fn confine(
        &self,
        relative: &str,
        kind: ProviderResourceKind,
    ) -> Result<ConfinedPath, VaultPathError> {
        let normalized = validate_relative(relative)?;
        if !self.config.allows_kind(kind) {
            return Err(VaultPathError::OutOfScope);
        }
        if self.excluded(&normalized) {
            return Err(VaultPathError::Excluded);
        }
        let absolute = self.canonical_root.join(&normalized);
        self.reject_symlink_escape(&absolute)?;
        Ok(ConfinedPath {
            relative: normalized,
            absolute,
        })
    }

    /// Whether a normalized relative path falls inside a configured
    /// exclusion. An exclusion matches its exact path and anything beneath
    /// it, mirroring the vault-relative exclusion model in the format spec.
    fn excluded(&self, normalized: &str) -> bool {
        let mut prefixes = BTreeSet::new();
        let mut accumulated = String::new();
        for component in normalized.split('/') {
            if !accumulated.is_empty() {
                accumulated.push('/');
            }
            accumulated.push_str(component);
            prefixes.insert(accumulated.clone());
        }
        self.config.exclusions().iter().any(|exclusion| {
            let exclusion = exclusion.as_str().trim_end_matches('/');
            prefixes.contains(exclusion)
        })
    }

    /// Atomically renames a confined file to a new confined location within
    /// the vault (same filesystem, single rename). Task identity lives in
    /// `brain_id` and is unaffected; document identities follow their path
    /// (format spec §4; SCRUM-92 owns rename-stable document identity).
    ///
    /// # Errors
    /// Returns the matching typed [`VaultPathError`] when either endpoint
    /// fails confinement, or an IO error surfaces as [`VaultPathError::InvalidPath`]
    /// only when the source is missing; all other failures propagate.
    pub fn rename(
        &self,
        from_relative: &str,
        to_relative: &str,
        kind: ProviderResourceKind,
    ) -> Result<(), VaultPathError> {
        let from = self.confine(from_relative, kind)?;
        let to = self.confine(to_relative, kind)?;
        // Renames never clobber another writer's file.
        if to.absolute().exists() {
            return Err(VaultPathError::TargetExists);
        }
        std::fs::rename(from.absolute(), to.absolute()).map_err(|_| VaultPathError::InvalidPath)
    }

    /// Detects symlink escape: canonicalize the deepest existing ancestor of
    /// the path and require it to stay inside the canonical root. A symlink
    /// anywhere on the existing portion that points outside the root fails
    /// this check.
    fn reject_symlink_escape(&self, absolute: &Path) -> Result<(), VaultPathError> {
        let mut deepest = absolute.to_path_buf();
        loop {
            match std::fs::canonicalize(&deepest) {
                Ok(canonical) => {
                    if !canonical.starts_with(&self.canonical_root) {
                        return Err(VaultPathError::SymlinkEscape);
                    }
                    return Ok(());
                }
                Err(_) => {
                    // The tail does not exist yet (a create); drop the last
                    // component and retry against the existing ancestor.
                    if !deepest.pop() || deepest == self.canonical_root {
                        return Ok(());
                    }
                }
            }
        }
    }

    fn reference(&self, resource_id: &str, kind: ProviderResourceKind) -> ProviderResourceRef {
        let id = ProviderResourceId::new(resource_id)
            .unwrap_or_else(|_| ProviderResourceId::new("invalid-id").expect("static id"));
        ProviderResourceRef::new(self.workspace_id, self.provider_id.clone(), id, kind)
    }

    fn document_reference(
        &self,
        relative: &str,
        kind: ProviderResourceKind,
    ) -> ProviderResourceRef {
        self.reference(&format!("{DOCUMENT_ID_PREFIX}{relative}"), kind)
    }

    fn provenance(
        resource: &ProviderResourceRef,
        bytes: &[u8],
    ) -> Result<ProviderProvenance, ProviderError> {
        let digest = Sha256::digest(bytes);
        let revision = hex_revision(&digest)?;
        Ok(ProviderProvenance::new(
            resource.clone(),
            revision,
            ContentHash::new(digest.into()),
        ))
    }

    /// Bounded recursive enumeration of `.md` files under the root that pass
    /// confinement for the given kind. Oversized or unreadable paths are
    /// skipped, never fatal.
    fn enumerate_paths(&self, kind: ProviderResourceKind, limit: NonZeroUsize) -> Vec<String> {
        let mut relatives = Vec::new();
        let mut stack = vec![self.canonical_root.clone()];
        let mut visited = 0_usize;
        while let Some(directory) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&directory) else {
                continue;
            };
            for entry in entries.flatten() {
                visited += 1;
                if visited > MAX_ENUMERATED_FILES || relatives.len() >= limit.get() {
                    return relatives;
                }
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().is_none_or(|extension| extension != "md") {
                    continue;
                }
                // Leftovers of a crashed atomic write never enter the model.
                if path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.contains(".tmp-"))
                {
                    continue;
                }
                let Ok(relative) = path.strip_prefix(&self.canonical_root) else {
                    continue;
                };
                let Some(relative) = relative.to_str() else {
                    continue;
                };
                // The format uses `/` separators regardless of platform.
                let normalized = relative.replace('\\', "/");
                // `Tasks/` is the Brain-managed task directory; its files are
                // never knowledge documents.
                if kind == ProviderResourceKind::Knowledge && normalized.starts_with("Tasks/") {
                    continue;
                }
                if self.confine(&normalized, kind).is_ok() {
                    relatives.push(normalized);
                }
            }
        }
        relatives
    }

    /// Reads a confined file and observes its revision and content hash in
    /// one pass (storage plan §7 step 2).
    ///
    /// # Errors
    /// Returns a redacted [`ProviderError`]: `NotFound` for missing files,
    /// `Unavailable` for other IO faults.
    pub fn read_observed(
        &self,
        confined: &ConfinedPath,
        kind: ProviderResourceKind,
    ) -> Result<(ProviderResourceRef, Vec<u8>, ProviderProvenance), ProviderError> {
        let bytes = std::fs::read(confined.absolute()).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                ProviderError::NotFound {
                    resource: self.document_reference(confined.relative(), kind),
                }
            } else {
                ProviderError::Unavailable
            }
        })?;
        let resource = self.document_reference(confined.relative(), kind);
        let provenance = Self::provenance(&resource, &bytes)?;
        Ok((resource, bytes, provenance))
    }

    /// Observes the current revision of a confined file by re-reading it.
    ///
    /// # Errors
    /// Returns a redacted [`ProviderError`] when the file is missing or
    /// unreadable.
    pub fn current_revision(
        &self,
        confined: &ConfinedPath,
    ) -> Result<ObservedRevision, ProviderError> {
        let bytes = std::fs::read(confined.absolute()).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                ProviderError::NotFound {
                    resource: self
                        .document_reference(confined.relative(), ProviderResourceKind::Knowledge),
                }
            } else {
                ProviderError::Unavailable
            }
        })?;
        let digest = Sha256::digest(&bytes);
        hex_revision(&digest)
    }

    /// Optimistic concurrency gate (storage plan §7 steps 3-6): re-reads the
    /// file and compares against the caller's expected revision. On mismatch
    /// the error carries the freshly observed provenance so the caller can
    /// re-read and reconcile; a blind last-writer-wins overwrite is
    /// structurally impossible through this primitive.
    ///
    /// # Errors
    /// Returns [`ProviderError::Conflict`] with the current provenance on a
    /// revision mismatch, and redacted IO errors when the file disappeared.
    pub fn verify_revision(
        &self,
        confined: &ConfinedPath,
        expected: &ObservedRevision,
    ) -> Result<(), ProviderError> {
        let bytes = std::fs::read(confined.absolute()).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                ProviderError::NotFound {
                    resource: self
                        .document_reference(confined.relative(), ProviderResourceKind::Knowledge),
                }
            } else {
                ProviderError::Unavailable
            }
        })?;
        let digest = Sha256::digest(&bytes);
        let current = hex_revision(&digest)?;
        if current != *expected {
            let hash = ContentHash::new(digest.into());
            return Err(ProviderError::Conflict {
                current: ProviderProvenance::new(
                    self.document_reference(confined.relative(), ProviderResourceKind::Knowledge),
                    current,
                    hash,
                ),
            });
        }
        Ok(())
    }

    /// Enumerates Brain-managed task resource references (bounded).
    /// Enumerates Brain-managed task resource references (bounded).
    ///
    /// # Errors
    /// Returns a redacted [`ProviderError`] when the enumeration itself
    /// fails; individual unreadable or identity-less files are skipped.
    pub fn enumerate_tasks_public(
        &self,
        limit: NonZeroUsize,
    ) -> Result<Vec<ProviderResourceRef>, ProviderError> {
        let mut references = Vec::new();
        for relative in self.enumerate_paths(ProviderResourceKind::Task, limit) {
            let Ok(bytes) = std::fs::read(self.canonical_root.join(&relative)) else {
                continue;
            };
            let Ok(text) = std::str::from_utf8(&bytes) else {
                continue;
            };
            let Ok(parsed) = parse_document(text) else {
                continue;
            };
            let Ok(task) = parse_task(&parsed, "task") else {
                continue;
            };
            references.push(self.reference(task.brain_id(), ProviderResourceKind::Task));
        }
        Ok(references)
    }

    /// Enumerates knowledge documents as bounded resource references.
    ///
    /// # Errors
    /// Returns a redacted [`ProviderError`] when the enumeration itself
    /// fails; individual unreadable files are skipped.
    pub fn enumerate_knowledge(
        &self,
        limit: NonZeroUsize,
    ) -> Result<ProviderPage<ProviderResourceRef>, ProviderError> {
        let references = self
            .enumerate_paths(ProviderResourceKind::Knowledge, limit)
            .into_iter()
            .map(|relative| self.document_reference(&relative, ProviderResourceKind::Knowledge))
            .collect();
        ProviderPage::new(references, ProviderFreshness::Current)
    }
    /// Reads and parses one knowledge document by resource id.
    fn read_knowledge(
        &self,
        resource: &ProviderResourceRef,
    ) -> Result<ProviderRead<KnowledgeDocument>, ProviderError> {
        let relative = document_relative(resource)?;
        let confined = self
            .confine(relative, ProviderResourceKind::Knowledge)
            .map_err(|_| ProviderError::NotFound {
                resource: resource.clone(),
            })?;
        let bytes = std::fs::read(confined.absolute()).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                ProviderError::NotFound {
                    resource: resource.clone(),
                }
            } else {
                ProviderError::Unavailable
            }
        })?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| ProviderError::Validation { field: "encoding" })?;
        let parsed =
            parse_document(text).map_err(|_| ProviderError::Validation { field: "markdown" })?;
        let provenance = Self::provenance(resource, &bytes)?;
        // Mid-read change detection: if the file changed while it was being
        // parsed, re-read and reconcile once (storage plan §7.1).
        if self
            .verify_revision(&confined, provenance.observed_revision())
            .is_err()
        {
            let reread_bytes =
                std::fs::read(confined.absolute()).map_err(|_| ProviderError::Unavailable)?;
            let reread_text = std::str::from_utf8(&reread_bytes)
                .map_err(|_| ProviderError::Validation { field: "encoding" })?;
            let reread_parsed = parse_document(reread_text)
                .map_err(|_| ProviderError::Validation { field: "markdown" })?;
            let reread_provenance = Self::provenance(resource, &reread_bytes)?;
            let document = KnowledgeDocument::new(
                reread_provenance,
                reread_parsed.title(confined.relative()),
                reread_parsed.body(),
            )?;
            return Ok(ProviderRead::new(document, ProviderFreshness::Stale));
        }
        let document =
            KnowledgeDocument::new(provenance, parsed.title(confined.relative()), parsed.body())?;
        Ok(ProviderRead::new(document, ProviderFreshness::Current))
    }

    /// Reads and parses one Brain-managed task file by resource id.
    fn read_task(
        &self,
        resource: &ProviderResourceRef,
    ) -> Result<ProviderRead<ProviderTask>, ProviderError> {
        let brain_id = resource.resource_id().as_str();
        for relative in self.enumerate_paths(ProviderResourceKind::Task, MAX_ENUMERATED_LIMIT) {
            let Ok(bytes) = std::fs::read(self.canonical_root.join(&relative)) else {
                continue;
            };
            let Ok(text) = std::str::from_utf8(&bytes) else {
                continue;
            };
            let Ok(parsed) = parse_document(text) else {
                continue;
            };
            let stem = file_stem(&relative);
            let Ok(task) = parse_task(&parsed, &stem) else {
                continue;
            };
            if task.brain_id() == brain_id {
                let provenance = Self::provenance(resource, &bytes)?;
                let provider_task = ProviderTask::new(
                    provenance,
                    *task.task_id(),
                    task.title().to_owned(),
                    task.body().to_owned(),
                    task.status(),
                    task.priority(),
                    task.scheduling().clone(),
                )?;
                return Ok(ProviderRead::new(provider_task, ProviderFreshness::Current));
            }
        }
        Err(ProviderError::NotFound {
            resource: resource.clone(),
        })
    }
}

const MAX_ENUMERATED_LIMIT: NonZeroUsize = NonZeroUsize::MAX;

fn document_relative(resource: &ProviderResourceRef) -> Result<&str, ProviderError> {
    let id = resource.resource_id().as_str();
    id.strip_prefix(DOCUMENT_ID_PREFIX)
        .ok_or(ProviderError::Validation {
            field: "resource_id",
        })
}

fn hex_revision(digest: &[u8]) -> Result<ObservedRevision, ProviderError> {
    let mut hex = String::with_capacity(32);
    for byte in digest.iter().take(16) {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    ObservedRevision::new(format!("rev-{hex}")).map_err(|_| ProviderError::Internal)
}

/// The file stem (name without `.md`) of a vault-relative path.
fn file_stem(relative: &str) -> String {
    relative
        .rsplit('/')
        .next()
        .unwrap_or(relative)
        .strip_suffix(".md")
        .unwrap_or_else(|| relative.rsplit('/').next().unwrap_or(relative))
        .to_owned()
}

/// Validates and normalizes a vault-relative path: `/`-separated, no
/// absolute or traversal components, bounded.
fn validate_relative(relative: &str) -> Result<String, VaultPathError> {
    if relative.trim().is_empty()
        || relative.len() > MAX_RELATIVE_BYTES
        || relative.chars().any(char::is_control)
        || relative.contains('\\')
    {
        return Err(VaultPathError::InvalidPath);
    }
    let path = Path::new(relative);
    if path.is_absolute() {
        return Err(VaultPathError::Traversal);
    }
    for component in path.components() {
        match component {
            Component::Normal(_) => {}
            _ => return Err(VaultPathError::Traversal),
        }
    }
    if relative.starts_with('/') {
        return Err(VaultPathError::Traversal);
    }
    Ok(relative.to_owned())
}

impl KnowledgeProvider for MarkdownVaultProvider {
    async fn get(
        &self,
        resource: &ProviderResourceRef,
    ) -> Result<Option<ProviderRead<KnowledgeDocument>>, ProviderError> {
        if resource.kind() != ProviderResourceKind::Knowledge
            || resource.workspace_id() != self.workspace_id
        {
            return Err(ProviderError::Validation {
                field: "resource_kind",
            });
        }
        match self.read_knowledge(resource) {
            Ok(read) => Ok(Some(read)),
            // Missing files are a normal read outcome, not an error.
            Err(ProviderError::NotFound { .. }) => Ok(None),
            Err(error) => Err(error),
        }
    }

    async fn search(
        &self,
        query: &KnowledgeQuery,
    ) -> Result<ProviderPage<KnowledgeDocument>, ProviderError> {
        if query.workspace_id() != self.workspace_id {
            return Err(ProviderError::Validation { field: "workspace" });
        }
        let text = query.text().map(str::to_lowercase);
        let mut documents = Vec::new();
        for reference in self.enumerate_knowledge(MAX_ENUMERATED_LIMIT)?.into_items() {
            if documents.len() >= query.limit().get() {
                break;
            }
            let read = self.read_knowledge(&reference)?;
            let include = text.as_ref().is_none_or(|text| {
                read.item().title().to_lowercase().contains(text)
                    || read.item().body().to_lowercase().contains(text)
            });
            if include {
                documents.push(read.into_item());
            }
        }
        ProviderPage::new(documents, ProviderFreshness::Current)
    }

    async fn create(&self, input: KnowledgeCreate) -> Result<ProviderMutation, ProviderError> {
        let slug = slug_from_title(input.title());
        // New documents live under `Documents/` (spec §3 allows any
        // non-task location; this keeps creates out of `Tasks/`).
        let relative = unique_relative(self, "Documents", &slug);
        let confined = self
            .confine(&relative, ProviderResourceKind::Knowledge)
            .map_err(|_| ProviderError::Validation { field: "path" })?;
        let mut content = String::new();
        content.push_str("---\ntitle: ");
        content.push_str(&quote_yaml(input.title()));
        content.push_str("\n---\n\n");
        content.push_str(input.body());
        if !content.ends_with('\n') {
            content.push('\n');
        }
        let bytes = content.into_bytes();
        write_atomic(&confined, &bytes).map_err(|_| ProviderError::Unavailable)?;
        let resource =
            self.document_reference(confined.relative(), ProviderResourceKind::Knowledge);
        let current = Self::provenance(&resource, &bytes)?;
        Ok(ProviderMutation::created(current))
    }

    async fn update(&self, input: KnowledgeUpdate) -> Result<ProviderMutation, ProviderError> {
        let relative = document_relative(input.resource())?;
        let confined = self
            .confine(relative, ProviderResourceKind::Knowledge)
            .map_err(|_| ProviderError::NotFound {
                resource: input.resource().clone(),
            })?;
        // Optimistic concurrency: the file must still observe the caller's
        // revision, otherwise the conflict carries the current provenance.
        self.verify_revision(&confined, input.expected_revision())?;
        let bytes = std::fs::read(confined.absolute()).map_err(|_| ProviderError::Unavailable)?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| ProviderError::Validation { field: "encoding" })?;
        let parsed =
            parse_document(text).map_err(|_| ProviderError::Validation { field: "markdown" })?;
        let previous = Self::provenance(input.resource(), &bytes)?;

        // Rewrite only the managed title; unknown properties are preserved.
        let frontmatter = match parsed.frontmatter() {
            Some(block) => block
                .with_managed_properties(&[("title", quote_yaml(input.title()))])
                .map_err(|_| ProviderError::Internal)?,
            None => format!("title: {}\n", quote_yaml(input.title())),
        };
        let mut content = String::from("---\n");
        content.push_str(&frontmatter);
        content.push_str("---\n");
        content.push_str(input.body());
        if !content.ends_with('\n') {
            content.push('\n');
        }
        let new_bytes = content.into_bytes();
        write_atomic(&confined, &new_bytes).map_err(|_| ProviderError::Unavailable)?;
        let current = Self::provenance(input.resource(), &new_bytes)?;
        ProviderMutation::updated(previous, current)
    }

    async fn delete(&self, input: KnowledgeDelete) -> Result<ProviderMutation, ProviderError> {
        let relative = document_relative(input.resource())?;
        let confined = self
            .confine(relative, ProviderResourceKind::Knowledge)
            .map_err(|_| ProviderError::NotFound {
                resource: input.resource().clone(),
            })?;
        let bytes = std::fs::read(confined.absolute()).map_err(|_| ProviderError::Unavailable)?;
        let previous = Self::provenance(input.resource(), &bytes)?;
        self.verify_revision(&confined, input.expected_revision())?;
        std::fs::remove_file(confined.absolute()).map_err(|_| ProviderError::Unavailable)?;
        Ok(ProviderMutation::deleted(previous))
    }
}

impl TaskProvider for MarkdownVaultProvider {
    async fn get(
        &self,
        resource: &ProviderResourceRef,
    ) -> Result<Option<ProviderRead<ProviderTask>>, ProviderError> {
        if resource.kind() != ProviderResourceKind::Task
            || resource.workspace_id() != self.workspace_id
        {
            return Err(ProviderError::Validation {
                field: "resource_kind",
            });
        }
        self.read_task(resource).map(Some)
    }

    async fn search(&self, query: &TaskQuery) -> Result<ProviderPage<ProviderTask>, ProviderError> {
        if query.workspace_id() != self.workspace_id {
            return Err(ProviderError::Validation { field: "workspace" });
        }
        let text = query.text().map(str::to_lowercase);
        let mut tasks = Vec::new();
        for relative in self.enumerate_paths(ProviderResourceKind::Task, MAX_ENUMERATED_LIMIT) {
            if tasks.len() >= query.limit().get() {
                break;
            }
            let Ok(bytes) = std::fs::read(self.canonical_root.join(&relative)) else {
                continue;
            };
            let Ok(parsed_text) = std::str::from_utf8(&bytes) else {
                continue;
            };
            let Ok(parsed) = parse_document(parsed_text) else {
                continue;
            };
            let stem = file_stem(&relative);
            let Ok(task) = parse_task(&parsed, &stem) else {
                continue;
            };
            let include = text.as_ref().is_none_or(|text| {
                task.title().to_lowercase().contains(text)
                    || task.body().to_lowercase().contains(text)
            });
            if !include {
                continue;
            }
            let resource = self.reference(task.brain_id(), ProviderResourceKind::Task);
            let provenance = Self::provenance(&resource, &bytes)?;
            tasks.push(ProviderTask::new(
                provenance,
                *task.task_id(),
                task.title().to_owned(),
                task.body().to_owned(),
                task.status(),
                task.priority(),
                task.scheduling().clone(),
            )?);
        }
        ProviderPage::new(tasks, ProviderFreshness::Current)
    }

    async fn create(&self, input: TaskCreate) -> Result<ProviderMutation, ProviderError> {
        let brain_id = uuid::Uuid::from(input.task_id()).hyphenated().to_string();
        let slug = slug_from_title(input.title());
        let relative = format!("Tasks/{brain_id}-{slug}.md");
        let confined = self
            .confine(&relative, ProviderResourceKind::Task)
            .map_err(|_| ProviderError::Validation { field: "path" })?;
        if confined.absolute().exists() {
            return Err(ProviderError::Conflict {
                current: Self::provenance(
                    &self.reference(&brain_id, ProviderResourceKind::Task),
                    &[],
                )?,
            });
        }
        let scheduling = input.scheduling().clone();
        let managed = task_managed_values(
            &brain_id,
            ProviderTaskStatus::Todo,
            input.priority(),
            &scheduling,
        );
        let mut content = String::from("---\n");
        content.push_str(&managed);
        content.push_str("---\n\n");
        content.push_str(input.body());
        if !content.ends_with('\n') {
            content.push('\n');
        }
        let bytes = content.into_bytes();
        write_atomic(&confined, &bytes).map_err(|_| ProviderError::Unavailable)?;
        let resource = self.reference(&brain_id, ProviderResourceKind::Task);
        let current = Self::provenance(&resource, &bytes)?;
        Ok(ProviderMutation::created(current))
    }

    async fn update(&self, input: TaskUpdate) -> Result<ProviderMutation, ProviderError> {
        let confined = self.find_task_confined(input.resource())?;
        let bytes = std::fs::read(confined.absolute()).map_err(|_| ProviderError::Unavailable)?;
        let previous = Self::provenance(input.resource(), &bytes)?;
        self.verify_revision(&confined, input.expected_revision())?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| ProviderError::Validation { field: "encoding" })?;
        let parsed =
            parse_document(text).map_err(|_| ProviderError::Validation { field: "markdown" })?;
        let parsed_task =
            parse_task(&parsed, "task").map_err(|_| ProviderError::Validation { field: "task" })?;
        let brain_id = parsed_task.brain_id().to_owned();
        let frontmatter = task_managed_values(
            &brain_id,
            input.status(),
            input.priority(),
            input.scheduling(),
        );
        let mut content = String::from("---\n");
        content.push_str(&frontmatter);
        content.push_str("---\n");
        content.push_str(parsed.body());
        if !content.ends_with('\n') {
            content.push('\n');
        }
        let new_bytes = content.into_bytes();
        write_atomic(&confined, &new_bytes).map_err(|_| ProviderError::Unavailable)?;
        let current = Self::provenance(input.resource(), &new_bytes)?;
        ProviderMutation::updated(previous, current)
    }

    async fn complete(&self, input: TaskComplete) -> Result<ProviderMutation, ProviderError> {
        let confined = self.find_task_confined(input.resource())?;
        let bytes = std::fs::read(confined.absolute()).map_err(|_| ProviderError::Unavailable)?;
        let previous = Self::provenance(input.resource(), &bytes)?;
        self.verify_revision(&confined, input.expected_revision())?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| ProviderError::Validation { field: "encoding" })?;
        let parsed =
            parse_document(text).map_err(|_| ProviderError::Validation { field: "markdown" })?;
        let parsed_task =
            parse_task(&parsed, "task").map_err(|_| ProviderError::Validation { field: "task" })?;
        let brain_id = parsed_task.brain_id().to_owned();
        let managed = task_managed_values(
            &brain_id,
            ProviderTaskStatus::Completed,
            parsed_task.priority(),
            parsed_task.scheduling(),
        );
        let frontmatter = managed;
        let mut content = String::from("---\n");
        content.push_str(&frontmatter);
        content.push_str("---\n");
        content.push_str(parsed.body());
        if !content.ends_with('\n') {
            content.push('\n');
        }
        let new_bytes = content.into_bytes();
        write_atomic(&confined, &new_bytes).map_err(|_| ProviderError::Unavailable)?;
        let current = Self::provenance(input.resource(), &new_bytes)?;
        ProviderMutation::updated(previous, current)
    }

    async fn delete(&self, input: TaskDelete) -> Result<ProviderMutation, ProviderError> {
        let confined = self.find_task_confined(input.resource())?;
        let bytes = std::fs::read(confined.absolute()).map_err(|_| ProviderError::Unavailable)?;
        let previous = Self::provenance(input.resource(), &bytes)?;
        self.verify_revision(&confined, input.expected_revision())?;
        std::fs::remove_file(confined.absolute()).map_err(|_| ProviderError::Unavailable)?;
        Ok(ProviderMutation::deleted(previous))
    }
}

/// Locates the confined task file owning a resource identity.
impl MarkdownVaultProvider {
    fn find_task_confined(
        &self,
        resource: &ProviderResourceRef,
    ) -> Result<ConfinedPath, ProviderError> {
        let brain_id = resource.resource_id().as_str();
        for relative in self.enumerate_paths(ProviderResourceKind::Task, MAX_ENUMERATED_LIMIT) {
            let Ok(confined) = self.confine(&relative, ProviderResourceKind::Task) else {
                continue;
            };
            let Ok(bytes) = std::fs::read(confined.absolute()) else {
                continue;
            };
            let Ok(text) = std::str::from_utf8(&bytes) else {
                continue;
            };
            let Ok(parsed) = parse_document(text) else {
                continue;
            };
            let stem = file_stem(&relative);
            let Ok(task) = parse_task(&parsed, &stem) else {
                continue;
            };
            if task.brain_id() == brain_id {
                return Ok(confined);
            }
        }
        Err(ProviderError::NotFound {
            resource: resource.clone(),
        })
    }
}

/// Serializes the managed task properties in canonical §6.1 order.
fn task_managed_values(
    brain_id: &str,
    status: ProviderTaskStatus,
    priority: ProviderTaskPriority,
    scheduling: &TaskSchedulingMetadata,
) -> String {
    let mut managed: Vec<(&str, String)> = vec![
        ("type", "task".to_owned()),
        ("brain_id", brain_id.to_owned()),
        (
            "status",
            match status {
                ProviderTaskStatus::Todo => "todo",
                ProviderTaskStatus::InProgress => "in_progress",
                ProviderTaskStatus::Completed => "done",
                ProviderTaskStatus::Cancelled => "cancelled",
            }
            .to_owned(),
        ),
        (
            "priority",
            match priority {
                ProviderTaskPriority::Low => "low",
                ProviderTaskPriority::Normal => "normal",
                ProviderTaskPriority::High => "high",
                ProviderTaskPriority::Urgent => "urgent",
            }
            .to_owned(),
        ),
    ];
    if let Some(due) = scheduling.due_at() {
        managed.push(("due", format_instant(due)));
    }
    if let Some(deadline) = scheduling.deadline_at() {
        managed.push(("deadline", format_instant(deadline)));
    }
    if let Some(duration) = scheduling.duration_minutes() {
        managed.push(("duration_minutes", duration.get().to_string()));
    }
    if let Some(start) = scheduling.earliest_start() {
        managed.push(("earliest_start", format_instant(start)));
    }
    if let Some(split) = scheduling.split() {
        managed.push(("split", if split { "true" } else { "false" }.to_owned()));
    }
    if let Some(project) = scheduling.project() {
        managed.push(("project", quote_yaml(project)));
    }
    if let Some(context) = scheduling.context() {
        managed.push(("context", quote_yaml(context)));
    }
    let mut output = String::new();
    for (key, value) in managed {
        output.push_str(key);
        output.push_str(": ");
        output.push_str(&value);
        output.push('\n');
    }
    output
}

/// Date-only output when the instant is midnight UTC, RFC 3339 otherwise.
fn format_instant(instant: chrono::DateTime<chrono::Utc>) -> String {
    if instant.time() == chrono::NaiveTime::MIN {
        instant.format("%Y-%m-%d").to_string()
    } else {
        instant.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    }
}

/// Derives a bounded file slug from a title.
fn slug_from_title(title: &str) -> String {
    let mut slug = String::new();
    for character in title.chars() {
        if character.is_alphanumeric() {
            slug.extend(character.to_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
        if slug.len() >= 48 {
            break;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        slug.push_str("untitled");
    }
    slug
}

/// Quotes a scalar when it could otherwise be misread as another YAML type.
fn quote_yaml(value: &str) -> String {
    let needs_quotes = value.parse::<f64>().is_ok()
        || matches!(value, "true" | "false" | "null" | "~")
        || value.starts_with(['[', '{', '\'', '"', '#', ' '])
        || value.ends_with(' ');
    if needs_quotes {
        format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        value.to_owned()
    }
}

/// Finds a non-existing `directory/slug(–N).md` path, bounded attempts.
fn unique_relative(provider: &MarkdownVaultProvider, directory: &str, slug: &str) -> String {
    for index in 1..100 {
        let candidate = if index == 1 {
            format!("{directory}/{slug}.md")
        } else {
            format!("{directory}/{slug}-{index}.md")
        };
        if provider
            .confine(&candidate, ProviderResourceKind::Knowledge)
            .is_ok()
            && !provider.canonical_root().join(&candidate).exists()
        {
            return candidate;
        }
    }
    format!("{directory}/{slug}-unnumbered.md")
}

/// Writes bytes through a same-filesystem temporary file plus atomic rename,
/// so faults cannot leave a partially written authoritative file.
fn write_atomic(confined: &ConfinedPath, bytes: &[u8]) -> Result<(), std::io::Error> {
    use std::io::Write;
    let target = confined.absolute();
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = target.with_extension(format!(
        "{}.tmp-{}",
        target
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("md"),
        uuid::Uuid::now_v7().simple()
    ));
    {
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    match std::fs::rename(&temporary, target) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = std::fs::remove_file(&temporary);
            Err(error)
        }
    }
}
