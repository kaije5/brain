use uuid::Uuid;

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
    };
}

domain_id!(WorkspaceId);
domain_id!(PrincipalId);
domain_id!(EntityId);
domain_id!(OperationId);
domain_id!(AuditEventId);
