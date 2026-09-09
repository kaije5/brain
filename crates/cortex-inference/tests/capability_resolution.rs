use cortex_application::{
    Capability, CapabilityCatalog, CapabilityGrant, CommandContext, GrantPolicy,
};
use cortex_domain::{OperationId, PrincipalId, WorkspaceId};
use cortex_inference::AuthorizedCapabilities;
use uuid::Uuid;

fn context() -> CommandContext {
    CommandContext::from_authenticated(
        WorkspaceId::new(),
        PrincipalId::new(),
        OperationId::new(),
        Uuid::now_v7(),
    )
}

fn granted(
    context: &CommandContext,
    capabilities: impl IntoIterator<Item = Capability>,
) -> GrantPolicy {
    GrantPolicy::new(capabilities.into_iter().map(|capability| {
        CapabilityGrant::new(context.workspace_id, context.principal_id, capability)
    }))
}

#[test]
fn resolver_derives_the_exact_authorized_subset_from_policy() {
    let context = context();
    let policy = granted(&context, [Capability::NoteSearch, Capability::TaskList]);

    let authorized = AuthorizedCapabilities::resolve(
        &context,
        &policy,
        [
            Capability::NoteSearch,
            Capability::TaskList,
            Capability::NoteDelete,
            Capability::MemoryCreate,
        ],
    )
    .expect("resolution succeeds");

    // Only the granted subset is exposed; the tool schema list handed to the
    // model is derived from this set.
    let tools: Vec<String> = authorized
        .inference_tools()
        .into_iter()
        .map(|tool| tool.name)
        .collect();
    assert_eq!(tools.len(), 2);
    assert!(tools.contains(&"cortex_note_search".to_owned()));
    assert!(tools.contains(&"cortex_task_list".to_owned()));
}

#[test]
fn unauthorized_capabilities_are_absent_not_merely_rejected_at_selection() {
    let context = context();
    let policy = granted(&context, [Capability::NoteSearch]);

    let authorized = AuthorizedCapabilities::resolve(
        &context,
        &policy,
        CapabilityCatalog::all().iter().copied(),
    )
    .expect("resolution succeeds");

    let tools: Vec<String> = authorized
        .inference_tools()
        .into_iter()
        .map(|tool| tool.name)
        .collect();
    assert_eq!(tools, vec!["cortex_note_search".to_owned()]);
}

#[test]
fn all_denied_capability_requests_resolve_to_an_empty_subset() {
    let context = context();
    let policy = granted(&context, []);

    let authorized = AuthorizedCapabilities::resolve(
        &context,
        &policy,
        [Capability::NoteDelete, Capability::MemoryCreate],
    )
    .expect("an empty authorized subset is a valid degraded turn");

    assert_eq!(authorized.inference_tools().len(), 0);
}

#[test]
fn provider_content_cannot_expand_the_authorized_capability_set() {
    // Capability exposure is derived only from Cortex policy: even if a
    // candidate list is seeded from somewhere hostile, resolution still
    // consults the policy for every entry and grants nothing extra.
    let context = context();
    let policy = granted(&context, [Capability::TaskList]);
    let injected_candidates = [
        Capability::NoteDelete,
        Capability::MemoryDelete,
        Capability::TaskDelete,
        Capability::TaskList,
    ];

    let authorized = AuthorizedCapabilities::resolve(&context, &policy, injected_candidates)
        .expect("resolution succeeds");

    let tools: Vec<String> = authorized
        .inference_tools()
        .into_iter()
        .map(|tool| tool.name)
        .collect();
    assert_eq!(tools, vec!["cortex_task_list".to_owned()]);
}
