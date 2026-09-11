#![allow(clippy::missing_errors_doc, clippy::result_large_err)]

use cortex_domain::{ProviderProvenance, ProviderResourceKind, ProviderResourceRef};

pub const MAX_PROVIDER_RESULTS: usize = 100;
pub(crate) const MAX_PROVIDER_TEXT_BYTES: usize = 1_048_576;
pub(crate) const MAX_PROVIDER_QUERY_BYTES: usize = 4_096;
pub(crate) const MAX_PROVIDER_LABEL_BYTES: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderFreshness {
    Current,
    Stale,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderRead<T> {
    item: T,
    freshness: ProviderFreshness,
}

impl<T> ProviderRead<T> {
    #[must_use]
    pub const fn new(item: T, freshness: ProviderFreshness) -> Self {
        Self { item, freshness }
    }

    #[must_use]
    pub const fn item(&self) -> &T {
        &self.item
    }

    #[must_use]
    pub fn into_item(self) -> T {
        self.item
    }

    #[must_use]
    pub const fn freshness(&self) -> ProviderFreshness {
        self.freshness
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderPage<T> {
    items: Vec<T>,
    freshness: ProviderFreshness,
}

impl<T> ProviderPage<T> {
    pub fn new(items: Vec<T>, freshness: ProviderFreshness) -> Result<Self, ProviderError> {
        if items.len() > MAX_PROVIDER_RESULTS {
            return Err(ProviderError::Validation {
                field: "provider_results",
            });
        }
        Ok(Self { items, freshness })
    }

    #[must_use]
    pub fn items(&self) -> &[T] {
        &self.items
    }

    #[must_use]
    pub fn into_items(self) -> Vec<T> {
        self.items
    }

    #[must_use]
    pub const fn freshness(&self) -> ProviderFreshness {
        self.freshness
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderMutation {
    resource: ProviderResourceRef,
    previous: Option<ProviderProvenance>,
    current: Option<ProviderProvenance>,
}

impl ProviderMutation {
    #[must_use]
    pub fn created(current: ProviderProvenance) -> Self {
        Self {
            resource: current.resource().clone(),
            previous: None,
            current: Some(current),
        }
    }

    pub fn updated(
        previous: ProviderProvenance,
        current: ProviderProvenance,
    ) -> Result<Self, ProviderError> {
        if previous.resource() != current.resource() {
            return Err(ProviderError::Validation {
                field: "provider_mutation_resource",
            });
        }
        Ok(Self {
            resource: current.resource().clone(),
            previous: Some(previous),
            current: Some(current),
        })
    }

    #[must_use]
    pub fn deleted(previous: ProviderProvenance) -> Self {
        Self {
            resource: previous.resource().clone(),
            previous: Some(previous),
            current: None,
        }
    }

    #[must_use]
    pub const fn resource(&self) -> &ProviderResourceRef {
        &self.resource
    }

    #[must_use]
    pub const fn previous(&self) -> Option<&ProviderProvenance> {
        self.previous.as_ref()
    }

    #[must_use]
    pub const fn current(&self) -> Option<&ProviderProvenance> {
        self.current.as_ref()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProviderError {
    Validation { field: &'static str },
    NotFound { resource: ProviderResourceRef },
    Conflict { current: ProviderProvenance },
    Unavailable,
    Internal,
}

pub(crate) fn validate_limit(limit: std::num::NonZeroUsize) -> Result<(), ProviderError> {
    if limit.get() > MAX_PROVIDER_RESULTS {
        return Err(ProviderError::Validation {
            field: "provider_results",
        });
    }
    Ok(())
}

pub(crate) fn validate_resource_kind(
    resource: &ProviderResourceRef,
    expected: ProviderResourceKind,
) -> Result<(), ProviderError> {
    if resource.kind() != expected {
        return Err(ProviderError::Validation {
            field: "resource_kind",
        });
    }
    Ok(())
}

pub(crate) fn validate_required_text(
    value: &str,
    field: &'static str,
    max_bytes: usize,
) -> Result<(), ProviderError> {
    if value.trim().is_empty() || value.len() > max_bytes || value.chars().any(char::is_control) {
        return Err(ProviderError::Validation { field });
    }
    Ok(())
}

pub(crate) fn validate_body(value: &str) -> Result<(), ProviderError> {
    if value.len() > MAX_PROVIDER_TEXT_BYTES {
        return Err(ProviderError::Validation { field: "body" });
    }
    Ok(())
}
