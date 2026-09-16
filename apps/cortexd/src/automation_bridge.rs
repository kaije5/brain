//! Bridge from watcher transport events to normalized automation triggers
//! (SCRUM-132).
//!
//! The filesystem watcher is just one transport. Automation only ever sees
//! the normalized [`VaultChangeTrigger`] shape, so rules stay decoupled from
//! the watching mechanism and can be replayed from any source.

use cortex_application::{VaultChangeKind, VaultChangeTrigger};
use cortex_domain::OperationId;

use crate::vault_watcher::VaultEvent;

/// Normalizes one watcher event into a transport-agnostic automation
/// trigger. `event_id` is the deduplication key for idempotent delivery.
///
/// # Errors
/// Returns a typed validation error when the event carries a malformed path.
pub fn normalized_trigger(
    provider_id: &str,
    event: &VaultEvent,
    event_id: OperationId,
) -> Result<VaultChangeTrigger, cortex_application::ApplicationError> {
    let (kind, resource_id) = match event {
        VaultEvent::Created { relative } => (VaultChangeKind::Created, relative),
        VaultEvent::Updated { relative } => (VaultChangeKind::Updated, relative),
        VaultEvent::Deleted { relative } => (VaultChangeKind::Deleted, relative),
        VaultEvent::Renamed { to, .. } => (VaultChangeKind::Renamed, to),
    };
    VaultChangeTrigger::new(event_id, kind, provider_id, format!("path:{resource_id}"))
}

#[cfg(test)]
mod tests {
    use super::normalized_trigger;
    use cortex_application::{VaultChangeKind, VaultChangeTrigger};
    use cortex_domain::OperationId;

    #[test]
    fn watcher_events_map_to_normalized_triggers() {
        let created = normalized_trigger(
            "markdown-vault",
            &crate::vault_watcher::VaultEvent::Created {
                relative: "notes/launch.md".to_owned(),
            },
            OperationId::new(),
        )
        .expect("created maps");
        assert_eq!(created.kind(), VaultChangeKind::Created);
        assert_eq!(created.provider_id(), "markdown-vault");
        assert_eq!(created.resource_id(), "path:notes/launch.md");

        let renamed = normalized_trigger(
            "markdown-vault",
            &crate::vault_watcher::VaultEvent::Renamed {
                from: "tasks/old.md".to_owned(),
                to: "tasks/new.md".to_owned(),
            },
            OperationId::new(),
        )
        .expect("renamed maps");
        assert_eq!(renamed.kind(), VaultChangeKind::Renamed);
        assert_eq!(renamed.resource_id(), "path:tasks/new.md");
        assert!(matches!(
            VaultChangeTrigger::new(OperationId::new(), VaultChangeKind::Deleted, "p", ""),
            Err(cortex_application::ApplicationError::Validation { .. })
        ));
    }
}
