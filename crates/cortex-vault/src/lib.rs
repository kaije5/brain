//! Safe, bounded parsing and serialization of Brain vault Markdown
//! documents (format spec: `docs/vault/markdown-vault-format.md`).
//!
//! This crate is a pure format layer: no filesystem, no network, no
//! Obsidian, no sync transport. Parsing is total — every failure is a
//! typed, value-free [`VaultFormatError`] — and all format bounds from
//! spec §8 are enforced here. The concrete vault provider (SCRUM-91)
//! consumes this crate; domain and application contracts stay untouched.

#![forbid(unsafe_code)]

mod document;
mod error;
mod frontmatter;
mod task;

pub use document::{
    InlineLink, ParsedDocument, Wikilink, normalize_line_endings, parse_document,
    parse_document_bytes,
};
pub use error::VaultFormatError;
pub use frontmatter::{FrontmatterBlock, split_frontmatter};
pub use task::{ParsedTask, TASK_MANAGED_KEYS, parse_task, serialize_task_frontmatter};
