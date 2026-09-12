//! Provider-referenced derived index records (SCRUM-111).
//!
//! Derived index state is keyed by [`ProviderResourceRef`] + chunk number —
//! never by a canonical note/task table row — and carries the content hash
//! and observed revision observed at index time so hash checks can avoid
//! unnecessary re-indexing and detect external changes. All of this is
//! rebuildable derived state, never the authority for document content
//! (storage plan §5.2).

use std::{collections::BTreeMap, num::NonZeroUsize};

use cortex_domain::{ContentHash, ObservedRevision, ProviderResourceRef};

/// Maximum characters per indexed chunk.
pub const MAX_CHUNK_CHARS: usize = 1200;
/// Maximum chunks indexed per document.
pub const MAX_CHUNKS_PER_DOCUMENT: usize = 512;
/// Maximum indexed documents per index.
pub const MAX_DOCUMENTS: usize = 10_000;
/// Maximum stored text bytes per chunk.
const MAX_CHUNK_TEXT_BYTES: usize = MAX_CHUNK_CHARS * 4;

/// A stable, bounded reference to one chunk of one provider resource.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct ChunkReference {
    pub resource: ProviderResourceRef,
    /// One-based chunk ordinal within the resource.
    pub chunk: NonZeroUsize,
}

impl ChunkReference {
    #[must_use]
    pub fn new(resource: ProviderResourceRef, chunk: NonZeroUsize) -> Self {
        Self { resource, chunk }
    }
}

/// The file state observed when the chunk was last indexed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChunkProvenance {
    pub content_hash: ContentHash,
    pub observed_revision: ObservedRevision,
}

impl ChunkProvenance {
    #[must_use]
    pub const fn new(content_hash: ContentHash, observed_revision: ObservedRevision) -> Self {
        Self {
            content_hash,
            observed_revision,
        }
    }
}

/// One indexed chunk: bounded text plus the provenance of the file it came
/// from. The embedding stays optional so lexical retrieval works when
/// embeddings are unavailable (explicit degradation is upstream).
#[derive(Clone, Debug, PartialEq)]
pub struct IndexedChunk {
    pub reference: ChunkReference,
    pub text: String,
    pub provenance: ChunkProvenance,
    pub embedding: Option<crate::Embedding>,
}

impl IndexedChunk {
    /// The chunk reference.
    #[must_use]
    pub const fn reference(&self) -> &ChunkReference {
        &self.reference
    }

    /// Builds a chunk, rejecting oversized text.
    ///
    /// # Errors
    /// Returns a redacted validation error when the text exceeds the chunk
    /// byte bound.
    pub fn new(
        reference: ChunkReference,
        text: String,
        provenance: ChunkProvenance,
    ) -> Result<Self, cortex_domain::DomainError> {
        if text.is_empty() || text.len() > MAX_CHUNK_TEXT_BYTES {
            return Err(cortex_domain::DomainError::validation(
                "chunk_text",
                "chunk text must be non-empty and within bounds",
            ));
        }
        Ok(Self {
            reference,
            text,
            provenance,
            embedding: None,
        })
    }

    /// Whether this chunk was indexed from exactly the given file state.
    #[must_use]
    pub fn is_fresh(&self, provenance: &ChunkProvenance) -> bool {
        &self.provenance == provenance
    }
}

/// Document-level derived metadata keyed purely by provider resource.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentIndexEntry {
    pub resource: ProviderResourceRef,
    pub title: String,
    pub tags: Vec<String>,
    pub headings: Vec<String>,
    pub links: Vec<String>,
    /// Task metadata when the resource is a task (spec §6).
    pub task_status: Option<String>,
    pub task_priority: Option<String>,
}

/// In-memory derived vault index: chunks keyed by [`ChunkReference`], plus
/// document-level entries keyed by resource. Fully rebuildable.
#[derive(Clone, Debug, Default)]
pub struct DerivedVaultIndex {
    chunks: BTreeMap<ChunkReference, IndexedChunk>,
    documents: BTreeMap<ProviderResourceRef, DocumentIndexEntry>,
}

impl DerivedVaultIndex {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts or replaces a chunk. Hash-gated: if an identical chunk with
    /// identical provenance is already present, the update is a no-op
    /// (`false`) so callers avoid unnecessary re-embedding.
    ///
    /// # Errors
    /// Returns a redacted validation error when the chunk text is empty or
    /// oversized.
    pub fn upsert_chunk(
        &mut self,
        chunk: IndexedChunk,
    ) -> Result<bool, cortex_domain::DomainError> {
        if self.chunks.len() >= MAX_CHUNKS_PER_DOCUMENT * MAX_DOCUMENTS
            && !self.chunks.contains_key(&chunk.reference)
        {
            return Err(cortex_domain::DomainError::validation(
                "index_size",
                "derived index exceeds its bound",
            ));
        }
        if let Some(existing) = self.chunks.get(&chunk.reference)
            && existing.text == chunk.text
            && existing.is_fresh(&chunk.provenance)
            && existing.embedding == chunk.embedding
        {
            return Ok(false);
        }
        self.chunks.insert(chunk.reference.clone(), chunk);
        Ok(true)
    }

    /// Records or replaces document-level metadata.
    ///
    /// # Errors
    /// Returns a redacted validation error when the index exceeds its
    /// document bound.
    pub fn upsert_document(
        &mut self,
        entry: DocumentIndexEntry,
    ) -> Result<bool, cortex_domain::DomainError> {
        if self.documents.len() >= MAX_DOCUMENTS && !self.documents.contains_key(&entry.resource) {
            return Err(cortex_domain::DomainError::validation(
                "index_size",
                "derived index exceeds its document bound",
            ));
        }
        let inserted = !self.documents.contains_key(&entry.resource);
        self.documents.insert(entry.resource.clone(), entry);
        Ok(inserted)
    }

    /// Removes every chunk and the document entry for one resource. Returns
    /// the number of chunks removed.
    pub fn remove_resource(&mut self, resource: &ProviderResourceRef) -> usize {
        let stale: Vec<ChunkReference> = self
            .chunks
            .keys()
            .filter(|reference| &reference.resource == resource)
            .cloned()
            .collect();
        for reference in &stale {
            self.chunks.remove(reference);
        }
        self.documents.remove(resource);
        stale.len()
    }

    /// The chunk for a reference, if indexed.
    #[must_use]
    pub fn chunk(&self, reference: &ChunkReference) -> Option<&IndexedChunk> {
        self.chunks.get(reference)
    }

    /// All chunks belonging to one resource, ordered by chunk number.
    #[must_use]
    pub fn chunks_for_resource(&self, resource: &ProviderResourceRef) -> Vec<&IndexedChunk> {
        self.chunks
            .values()
            .filter(|chunk| &chunk.reference.resource == resource)
            .collect()
    }

    /// The document entry for a resource, if indexed.
    #[must_use]
    pub fn document(&self, resource: &ProviderResourceRef) -> Option<&DocumentIndexEntry> {
        self.documents.get(resource)
    }

    /// Every indexed document resource.
    pub fn documents(&self) -> impl Iterator<Item = &ProviderResourceRef> {
        self.documents.keys()
    }

    #[must_use]
    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    #[must_use]
    pub fn document_count(&self) -> usize {
        self.documents.len()
    }
}

/// Deterministic paragraph-boundary chunker: accumulates paragraphs until
/// [`MAX_CHUNK_CHARS`] is reached, never splitting a paragraph, and emits
/// one-based chunk ordinals bounded by [`MAX_CHUNKS_PER_DOCUMENT`].
#[must_use]
pub fn chunk_body(body: &str) -> Vec<(NonZeroUsize, String)> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut ordinal: usize = 1;
    let flush =
        |current: &mut String, chunks: &mut Vec<(NonZeroUsize, String)>, ordinal: &mut usize| {
            let trimmed = current.trim().to_owned();
            current.clear();
            if trimmed.is_empty() {
                return;
            }
            if let Ok(ordinal_value) = NonZeroUsize::try_from(*ordinal) {
                chunks.push((ordinal_value, trimmed));
            }
            *ordinal += 1;
        };
    for paragraph in body.split("\n\n") {
        let paragraph = paragraph.trim();
        if paragraph.is_empty() {
            continue;
        }
        // A single paragraph longer than the bound becomes its own chunk,
        // hard-truncated to the bound (never silently dropped).
        if paragraph.len() > MAX_CHUNK_CHARS {
            flush(&mut current, &mut chunks, &mut ordinal);
            let truncated: String = paragraph.chars().take(MAX_CHUNK_CHARS).collect();
            if let Ok(ordinal_value) = NonZeroUsize::try_from(ordinal) {
                chunks.push((ordinal_value, truncated));
            }
            ordinal += 1;
            continue;
        }
        if current.len() + paragraph.len() + 2 > MAX_CHUNK_CHARS && !current.is_empty() {
            flush(&mut current, &mut chunks, &mut ordinal);
        }
        if !current.is_empty() {
            current.push_str("\n\n");
        }
        current.push_str(paragraph);
        if chunks.len() >= MAX_CHUNKS_PER_DOCUMENT {
            break;
        }
    }
    flush(&mut current, &mut chunks, &mut ordinal);
    chunks
}
