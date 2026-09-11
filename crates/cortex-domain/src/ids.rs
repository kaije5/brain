use uuid::{Uuid, Version};

use crate::DomainError;

macro_rules! domain_id {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(Uuid);

        impl $name {
            #[must_use]
            #[allow(clippy::new_without_default)]
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }
        }

        impl TryFrom<Uuid> for $name {
            type Error = DomainError;

            fn try_from(value: Uuid) -> Result<Self, Self::Error> {
                if value.is_nil() || value.get_version() != Some(Version::SortRand) {
                    return Err(DomainError::validation("id", "must be a UUIDv7 value"));
                }
                Ok(Self(value))
            }
        }

        impl From<$name> for Uuid {
            fn from(value: $name) -> Self {
                value.0
            }
        }
    };
}

domain_id!(WorkspaceId);
domain_id!(PrincipalId);
domain_id!(EntityId);
domain_id!(OperationId);
domain_id!(AuditEventId);
domain_id!(TaskId);
