/// The deterministic outcome of an authorization policy evaluation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PolicyDecision {
    Allow,
    Deny(PolicyDeny),
}

/// A safe, stable reason for refusing a requested capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PolicyDeny {
    MissingGrant,
    TargetOutsideWorkspace,
}
