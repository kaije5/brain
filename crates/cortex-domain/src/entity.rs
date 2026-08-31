use crate::DomainError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Lifecycle {
    Active,
    Deleted,
}

impl Lifecycle {
    /// # Errors
    ///
    /// Returns [`DomainError::AlreadyDeleted`] when the lifecycle is already deleted.
    pub fn delete(self) -> Result<Self, DomainError> {
        match self {
            Self::Active => Ok(Self::Deleted),
            Self::Deleted => Err(DomainError::AlreadyDeleted),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Revision(u64);

impl Revision {
    #[must_use]
    pub const fn initial() -> Self {
        Self(1)
    }

    /// # Errors
    ///
    /// Returns [`DomainError::RevisionOverflow`] when the revision cannot be incremented.
    pub fn next(self) -> Result<Self, DomainError> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or(DomainError::RevisionOverflow)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}
