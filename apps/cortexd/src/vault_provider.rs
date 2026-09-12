//! The first-party Markdown vault provider: root/scope confinement.
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
    path::{Component, Path, PathBuf},
};

use cortex_domain::ProviderResourceKind;

use crate::vault::{VaultConfigError, VaultProviderConfig};

const MAX_RELATIVE_BYTES: usize = 1024;

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
    canonical_root: PathBuf,
}

impl MarkdownVaultProvider {
    /// Opens the vault: validates the configuration and canonicalizes the
    /// root so every later confinement check compares against one exact
    /// prefix.
    ///
    /// # Errors
    /// Returns [`VaultPathError::InvalidRoot`] when the root is missing,
    /// not a directory, or cannot be canonicalized.
    pub fn open(config: VaultProviderConfig) -> Result<Self, VaultPathError> {
        config.validate_root_access()?;
        let canonical_root =
            std::fs::canonicalize(config.root()).map_err(|_| VaultPathError::InvalidRoot)?;
        Ok(Self {
            config,
            canonical_root,
        })
    }

    /// The validated configuration this provider was opened with.
    #[must_use]
    pub const fn config(&self) -> &VaultProviderConfig {
        &self.config
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
