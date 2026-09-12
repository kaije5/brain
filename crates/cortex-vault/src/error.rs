//! Typed, value-free failure categories for vault format parsing (format
//! spec §8). Errors never carry file contents, local paths, or provider
//! diagnostics.

/// A bounded, redacted vault format failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum VaultFormatError {
    /// The frontmatter block is not valid for the supported YAML subset.
    #[error("malformed frontmatter")]
    MalformedFrontmatter,
    /// The file is not valid UTF-8 and is never lossily repaired.
    #[error("invalid encoding")]
    InvalidEncoding,
    /// A Brain-managed property appears more than once.
    #[error("duplicate property")]
    DuplicateProperty {
        /// Coarse property name for diagnostics; never a value.
        field: &'static str,
    },
    /// A required property is absent (task files; SCRUM-95).
    #[error("missing property")]
    MissingProperty {
        /// Coarse property name for diagnostics; never a value.
        field: &'static str,
    },
    /// A property is present but fails its type or bounds.
    #[error("invalid property")]
    InvalidProperty {
        /// Coarse property name for diagnostics; never a value.
        field: &'static str,
    },
    /// The file or a collection exceeds its format bound.
    #[error("file exceeds format bounds")]
    TooLarge,
    /// A rewrite would replace one stable task identity with another; Brain
    /// never overwrites one task's `brain_id` with a different task's.
    #[error("task identity conflict")]
    IdentityConflict,
    /// Two task files claim the same stable `brain_id`.
    #[error("duplicate task identity")]
    DuplicateIdentity,
}
