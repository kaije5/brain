//! Debounced, idempotent vault watching (SCRUM-113; storage plan §8).
//!
//! This module models filesystem watching deterministically: raw events are
//! pushed with caller-supplied millisecond timestamps and coalesced by a
//! debounce window, so bursts of noisy editor/sync traffic collapse into one
//! latest-state event per path. Applying a coalesced event to the derived
//! index is idempotent — re-applying changes nothing, because upserts are
//! hash-gated and deletes only remove what exists. The OS-specific watch
//! mechanism attaches to [`EventQueue::push`] later; the reconciliation scan
//! that recovers missed events is SCRUM-114.

use cortex_domain::{ProviderResourceId, ProviderResourceKind, ProviderResourceRef};
use cortex_search::{ChunkProvenance, DerivedVaultIndex};
use sha2::Sha256;
use std::{collections::BTreeSet, io::Read, num::NonZeroU64};

use crate::vault_provider::MarkdownVaultProvider;

/// Debounce window default: editors and sync engines emit bursts; events
/// within this window collapse into one event per path.
pub const DEFAULT_DEBOUNCE_MILLIS: u64 = 500;
/// Maximum watcher input accepted for an in-memory parse. Metadata is checked
/// before opening the file, so untrusted oversized hints cannot allocate it.
const MAX_WATCHED_FILE_BYTES: u64 = 1024 * 1024;

/// A raw filesystem transition on one vault-relative path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VaultEvent {
    Created { relative: String },
    Updated { relative: String },
    Deleted { relative: String },
    Renamed { from: String, to: String },
}

impl VaultEvent {
    /// The path whose content this event's latest state describes.
    #[must_use]
    pub fn target(&self) -> &str {
        match self {
            Self::Created { relative }
            | Self::Updated { relative }
            | Self::Deleted { relative } => relative,
            Self::Renamed { to, .. } => to,
        }
    }

    /// Every path the event touches (rename touches two).
    #[must_use]
    pub fn touched_paths(&self) -> Vec<&str> {
        match self {
            Self::Created { relative }
            | Self::Updated { relative }
            | Self::Deleted { relative } => vec![relative],
            Self::Renamed { from, to } => vec![from, to],
        }
    }
}

/// A coalesced event ready for idempotent application.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoalescedEvent {
    pub event: VaultEvent,
    /// Timestamp of the newest raw event folded into this coalesced event.
    pub observed_at: u64,
}

/// Bounded, debounced event queue. Events older than the debounce window
/// (relative to the `ready_at` watermark passed to [`EventQueue::drain`])
/// are folded per path into one latest-state event.
#[derive(Debug)]
pub struct EventQueue {
    debounce_millis: u64,
    pending: Vec<(VaultEvent, u64)>,
    capacity: usize,
}

impl EventQueue {
    #[must_use]
    pub fn new(debounce_millis: u64) -> Self {
        Self {
            debounce_millis,
            pending: Vec::new(),
            capacity: 4096,
        }
    }

    /// Pushes a raw event. Bounded: the oldest pending event is dropped when
    /// capacity is reached (a reconciliation scan recovers anything missed).
    pub fn push(&mut self, event: VaultEvent, at: u64) {
        if self.pending.len() >= self.capacity {
            self.pending.remove(0);
        }
        self.pending.push((event, at));
    }

    /// Folds all raw events at or before `ready_at - debounce` into one
    /// latest-state event per path (first-touched order) and clears them.
    /// Younger events stay pending for the next drain.
    #[must_use]
    pub fn drain(&mut self, ready_at: u64) -> Vec<CoalescedEvent> {
        let cutoff = ready_at.saturating_sub(self.debounce_millis);
        // A younger hint keeps every older transition touching that path in
        // the queue. Without this gate an old update could drain before the
        // final delete/rename in the same burst.
        let blocked_paths: Vec<String> = self
            .pending
            .iter()
            .filter(|(_, at)| *at >= cutoff)
            .flat_map(|(event, _)| event.touched_paths().into_iter().map(str::to_owned))
            .collect();
        let mut ready: Vec<(VaultEvent, u64)> = Vec::new();
        let mut remaining = Vec::new();
        for (event, at) in self.pending.drain(..) {
            let blocked = event.touched_paths().iter().any(|path| {
                blocked_paths
                    .iter()
                    .any(|blocked| path == &blocked.as_str())
            });
            if at < cutoff && !blocked {
                ready.push((event, at));
            } else {
                remaining.push((event, at));
            }
        }
        self.pending = remaining;

        // First-touched order: stable bucket per target path.
        let mut buckets: Vec<(String, VaultEvent, u64)> = Vec::new();
        for (event, at) in ready {
            let target = event.target().to_owned();
            if let Some(bucket) = buckets.iter_mut().find(|(path, _, _)| path == &target) {
                // Latest state wins; keep the newest timestamp.
                if at >= bucket.2 {
                    if !matches!(bucket.1, VaultEvent::Renamed { .. }) {
                        bucket.1 = event;
                    }
                    bucket.2 = at;
                }
            } else {
                buckets.push((target, event, at));
            }
        }
        buckets
            .into_iter()
            .map(|(_, event, observed_at)| CoalescedEvent { event, observed_at })
            .collect()
    }

    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
}

/// Outcome of applying one coalesced event — for diagnostics and tests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AppliedEvent {
    /// Chunks were indexed or refreshed.
    Indexed,
    /// The content hash matched the indexed provenance: skipped.
    Unchanged,
    /// Chunks were removed.
    Removed,
    /// The path is outside the configured scopes/exclusions, or the file is
    /// oversized/unparseable — skipped without error.
    Skipped,
}

/// Counts the convergent effects of one vault reconciliation or rebuild.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReconciliationReport {
    pub indexed: usize,
    pub unchanged: usize,
    pub removed: usize,
    pub skipped: usize,
}

/// Payload-free reconciliation failures. Existing derived state is retained
/// whenever a complete, unambiguous authoritative scan cannot be proven.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ReconciliationError {
    #[error("vault scan incomplete")]
    ScanIncomplete,
    #[error("duplicate vault resource identity")]
    DuplicateResource,
}

struct ScanSnapshot {
    candidates: Vec<(String, ProviderResourceRef)>,
    live: BTreeSet<ProviderResourceRef>,
    skipped: usize,
}

/// Scans the authoritative vault and converges provider-owned derived state.
/// Live paths are applied in deterministic lexical order; indexed resources
/// absent from the scan are removed. State owned by another workspace or
/// provider is left untouched.
///
/// # Errors
///
/// Returns [`ReconciliationError::ScanIncomplete`] if the vault cannot be
/// read completely, or [`ReconciliationError::DuplicateResource`] if two
/// paths claim the same stable resource identity. The index is not mutated
/// unless preflight succeeds.
pub fn reconcile_vault(
    provider: &MarkdownVaultProvider,
    index: &mut DerivedVaultIndex,
) -> Result<ReconciliationReport, ReconciliationError> {
    let mut report = ReconciliationReport::default();
    let ScanSnapshot {
        candidates,
        live,
        skipped,
    } = scan_indexable_resources(provider)?;
    report.skipped = skipped;

    for (relative, _) in candidates {
        let outcome = apply_event(
            provider,
            index,
            &CoalescedEvent {
                event: VaultEvent::Updated { relative },
                observed_at: 0,
            },
        );
        match outcome {
            AppliedEvent::Indexed => report.indexed += 1,
            AppliedEvent::Unchanged => report.unchanged += 1,
            AppliedEvent::Removed => report.removed += 1,
            AppliedEvent::Skipped => report.skipped += 1,
        }
    }

    let indexed: BTreeSet<_> = index
        .documents()
        .cloned()
        .chain(
            index
                .chunks()
                .map(|(reference, _)| reference.resource.clone()),
        )
        .collect();
    for resource in indexed {
        let owned = resource.workspace_id() == provider.workspace_id()
            && resource.provider_id() == provider.provider_reference_id();
        if owned && !live.contains(&resource) {
            index.remove_resource(&resource);
            report.removed += 1;
        }
    }
    Ok(report)
}

/// Deterministic periodic reconciliation coordinator. The caller owns the
/// clock and invokes this from its runtime loop; no sleeping or background
/// thread is hidden inside the index boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VaultReconciler {
    interval_millis: NonZeroU64,
    last_run_at: Option<u64>,
}

impl VaultReconciler {
    #[must_use]
    pub const fn new(interval_millis: NonZeroU64) -> Self {
        Self {
            interval_millis,
            last_run_at: None,
        }
    }

    /// Reconciles immediately on the first call and thereafter only when the
    /// configured interval has elapsed from the last completed invocation.
    ///
    /// # Errors
    ///
    /// Returns a [`ReconciliationError`] when a due reconciliation cannot
    /// prove a complete, unambiguous vault snapshot. A failed run does not
    /// advance the periodic watermark.
    pub fn reconcile_if_due(
        &mut self,
        provider: &MarkdownVaultProvider,
        index: &mut DerivedVaultIndex,
        now_millis: u64,
    ) -> Result<Option<ReconciliationReport>, ReconciliationError> {
        let due = self.last_run_at.is_none_or(|last_run_at| {
            now_millis.saturating_sub(last_run_at) >= self.interval_millis.get()
        });
        if !due {
            return Ok(None);
        }
        let report = reconcile_vault(provider, index)?;
        self.last_run_at = Some(now_millis);
        Ok(Some(report))
    }
}

/// Reconstructs all derived vault state from authoritative Markdown and
/// replaces the caller-owned index only after the complete scan finishes.
///
/// # Errors
///
/// Returns a [`ReconciliationError`] if the replacement cannot be built from
/// a complete, unambiguous vault snapshot. The existing index is retained.
pub fn rebuild_vault(
    provider: &MarkdownVaultProvider,
    index: &mut DerivedVaultIndex,
) -> Result<ReconciliationReport, ReconciliationError> {
    let mut replacement = DerivedVaultIndex::new();
    let report = reconcile_vault(provider, &mut replacement)?;
    *index = replacement;
    Ok(report)
}

/// Applies one coalesced event to the derived index idempotently: upserts
/// are hash-gated (unchanged content → `Unchanged`), deletes only remove
/// what exists, and only confined paths within the resource scopes are ever
/// touched.
pub fn apply_event(
    provider: &MarkdownVaultProvider,
    index: &mut DerivedVaultIndex,
    coalesced: &CoalescedEvent,
) -> AppliedEvent {
    match &coalesced.event {
        VaultEvent::Deleted { relative } => apply_deleted(provider, index, relative),
        VaultEvent::Renamed { from, to } => {
            apply_removed(provider, index, from);
            apply_created_or_updated(provider, index, to)
        }
        VaultEvent::Created { relative } | VaultEvent::Updated { relative } => {
            apply_created_or_updated(provider, index, relative)
        }
    }
}

fn apply_created_or_updated(
    provider: &MarkdownVaultProvider,
    index: &mut DerivedVaultIndex,
    relative: &str,
) -> AppliedEvent {
    apply_index_current(provider, index, relative)
}

fn apply_index_current(
    provider: &MarkdownVaultProvider,
    index: &mut DerivedVaultIndex,
    relative: &str,
) -> AppliedEvent {
    let kind = kind_for(relative);
    let Ok(confined) = provider.confine(relative, kind) else {
        return AppliedEvent::Skipped;
    };
    let bytes = match read_bounded(confined.absolute()) {
        Ok(BoundedRead::Bytes(bytes)) => bytes,
        Ok(BoundedRead::Missing) => return apply_removed(provider, index, relative),
        Ok(BoundedRead::Oversized) | Err(_) => return AppliedEvent::Skipped,
    };
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return AppliedEvent::Skipped;
    };
    let Some(resource) = resource_from_text(provider, confined.relative(), text) else {
        return AppliedEvent::Skipped;
    };
    let provenance = ChunkProvenance::new(content_hash_of(&bytes), revision_of(&bytes));

    // Idempotency gate: if every indexed chunk for this resource is fresh
    // against the current provenance, the event is a no-op.
    let existing = index.chunks_for_resource(&resource);
    if !existing.is_empty() && existing.iter().all(|chunk| chunk.is_fresh(&provenance)) {
        return AppliedEvent::Unchanged;
    }

    // Build the replacement first. A malformed successor must not destroy a
    // still-valid indexed resource.
    let mut scoped = DerivedVaultIndex::new();
    let Ok(indexed_count) =
        cortex_search::index_document(&mut scoped, &resource, text, &provenance)
    else {
        return AppliedEvent::Skipped;
    };
    // Replace the complete live resource only after extraction succeeded, so
    // shortened documents cannot retain trailing chunks.
    let removed = index.remove_resource(&resource);
    if indexed_count == 0 {
        return if removed > 0 {
            AppliedEvent::Removed
        } else {
            AppliedEvent::Unchanged
        };
    }
    if let Some(entry) = scoped.document(&resource) {
        let _ = index.upsert_document(entry.clone());
    }
    for chunk in scoped.chunks_for_resource(&resource) {
        let _ = index.upsert_chunk(chunk.clone());
    }
    AppliedEvent::Indexed
}

fn apply_deleted(
    provider: &MarkdownVaultProvider,
    index: &mut DerivedVaultIndex,
    relative: &str,
) -> AppliedEvent {
    let kind = kind_for(relative);
    let Ok(confined) = provider.confine(relative, kind) else {
        return AppliedEvent::Skipped;
    };
    if confined.absolute().is_file() {
        return apply_index_current(provider, index, relative);
    }
    apply_removed(provider, index, relative)
}

fn apply_removed(
    provider: &MarkdownVaultProvider,
    index: &mut DerivedVaultIndex,
    relative: &str,
) -> AppliedEvent {
    let kind = kind_for(relative);
    let Ok(confined) = provider.confine(relative, kind) else {
        return AppliedEvent::Skipped;
    };
    if kind == ProviderResourceKind::Task {
        return remove_stale_task_resources(provider, index);
    }
    let Some(resource) = resource_for_relative(provider, confined.relative()) else {
        return AppliedEvent::Skipped;
    };
    // Deleting something never indexed is a no-op (idempotent).
    if index.document(&resource).is_none() && index.chunks_for_resource(&resource).is_empty() {
        return AppliedEvent::Unchanged;
    }
    let removed = index.remove_resource(&resource);
    if removed > 0 || index.document(&resource).is_none() {
        AppliedEvent::Removed
    } else {
        AppliedEvent::Unchanged
    }
}

fn kind_for(relative: &str) -> ProviderResourceKind {
    if relative.starts_with("Tasks/") {
        ProviderResourceKind::Task
    } else {
        ProviderResourceKind::Knowledge
    }
}

fn document_resource_id(relative: &str) -> Option<ProviderResourceId> {
    ProviderResourceId::new(format!("path:{relative}")).ok()
}

fn remove_stale_task_resources(
    provider: &MarkdownVaultProvider,
    index: &mut DerivedVaultIndex,
) -> AppliedEvent {
    let Ok(ScanSnapshot { live, .. }) = scan_indexable_resources(provider) else {
        return AppliedEvent::Skipped;
    };
    let stale: Vec<_> = index
        .documents()
        .filter(|resource| {
            resource.workspace_id() == provider.workspace_id()
                && resource.provider_id() == provider.provider_reference_id()
                && resource.kind() == ProviderResourceKind::Task
                && !live.contains(*resource)
        })
        .cloned()
        .collect();
    if stale.is_empty() {
        return AppliedEvent::Unchanged;
    }
    for resource in stale {
        index.remove_resource(&resource);
    }
    AppliedEvent::Removed
}

fn resource_for_relative(
    provider: &MarkdownVaultProvider,
    relative: &str,
) -> Option<cortex_domain::ProviderResourceRef> {
    Some(cortex_domain::ProviderResourceRef::new(
        provider.workspace_id(),
        provider.provider_reference_id().clone(),
        document_resource_id(relative)?,
        kind_for(relative),
    ))
}

fn read_indexable_resource(
    provider: &MarkdownVaultProvider,
    relative: &str,
) -> Result<Option<ProviderResourceRef>, ReconciliationError> {
    let kind = kind_for(relative);
    let confined = provider
        .confine(relative, kind)
        .map_err(|_| ReconciliationError::ScanIncomplete)?;
    let bytes = match read_bounded(confined.absolute()) {
        Ok(BoundedRead::Bytes(bytes)) => bytes,
        Ok(BoundedRead::Oversized) => return Ok(None),
        Ok(BoundedRead::Missing) | Err(_) => return Err(ReconciliationError::ScanIncomplete),
    };
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return Ok(None);
    };
    Ok(resource_from_text(provider, confined.relative(), text))
}

fn scan_indexable_resources(
    provider: &MarkdownVaultProvider,
) -> Result<ScanSnapshot, ReconciliationError> {
    let mut live = BTreeSet::new();
    let mut candidates = Vec::new();
    let mut skipped = 0;
    let paths = provider
        .indexable_paths()
        .map_err(|_| ReconciliationError::ScanIncomplete)?;
    for relative in paths {
        let Some(resource) = read_indexable_resource(provider, &relative)? else {
            skipped += 1;
            continue;
        };
        if !live.insert(resource.clone()) {
            return Err(ReconciliationError::DuplicateResource);
        }
        candidates.push((relative, resource));
    }
    Ok(ScanSnapshot {
        candidates,
        live,
        skipped,
    })
}

fn resource_from_text(
    provider: &MarkdownVaultProvider,
    relative: &str,
    text: &str,
) -> Option<ProviderResourceRef> {
    let kind = kind_for(relative);
    let parsed = cortex_vault::parse_document(text).ok()?;
    let resource_id = if kind == ProviderResourceKind::Task {
        let stem = std::path::Path::new(relative)
            .file_stem()
            .and_then(|stem| stem.to_str())?;
        let task = cortex_vault::parse_task(&parsed, stem).ok()?;
        task.resource_id().clone()
    } else {
        document_resource_id(relative)?
    };
    Some(ProviderResourceRef::new(
        provider.workspace_id(),
        provider.provider_reference_id().clone(),
        resource_id,
        kind,
    ))
}

enum BoundedRead {
    Missing,
    Oversized,
    Bytes(Vec<u8>),
}

fn read_bounded(path: &std::path::Path) -> Result<BoundedRead, std::io::Error> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if is_not_found(&error) => {
            return Ok(BoundedRead::Missing);
        }
        Err(error) => return Err(error),
    };
    if !metadata.is_file() || metadata.len() > MAX_WATCHED_FILE_BYTES {
        return Ok(BoundedRead::Oversized);
    }
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(BoundedRead::Missing);
        }
        Err(error) => return Err(error),
    };
    match read_to_limit(file)? {
        Some(bytes) => Ok(BoundedRead::Bytes(bytes)),
        None => Ok(BoundedRead::Oversized),
    }
}

fn is_not_found(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::NotFound
}

fn read_to_limit(mut reader: impl Read) -> Result<Option<Vec<u8>>, std::io::Error> {
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take(MAX_WATCHED_FILE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    Ok(
        (bytes.len() <= usize::try_from(MAX_WATCHED_FILE_BYTES).expect("static bound"))
            .then_some(bytes),
    )
}

fn content_hash_of(bytes: &[u8]) -> cortex_domain::ContentHash {
    use sha2::Digest;
    cortex_domain::ContentHash::new(Sha256::digest(bytes).into())
}

fn revision_of(bytes: &[u8]) -> cortex_domain::ObservedRevision {
    use sha2::Digest;
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(32);
    for byte in digest.iter().take(16) {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    cortex_domain::ObservedRevision::new(format!("rev-{hex}"))
        .unwrap_or_else(|_| cortex_domain::ObservedRevision::new("rev-0").expect("static revision"))
}

#[cfg(test)]
mod tests {
    use super::{MAX_WATCHED_FILE_BYTES, is_not_found, read_to_limit};

    #[test]
    fn read_limit_rejects_growth_after_metadata_check() {
        let length = usize::try_from(MAX_WATCHED_FILE_BYTES + 1).expect("static bound");
        let reader = std::io::Cursor::new(vec![b'x'; length]);
        assert!(read_to_limit(reader).expect("read").is_none());
    }

    #[test]
    fn not_found_during_open_is_classified_as_missing() {
        assert!(is_not_found(&std::io::Error::from(
            std::io::ErrorKind::NotFound
        )));
    }
}
