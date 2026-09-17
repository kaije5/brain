-- SCRUM-177: backfill the cortex_agent_run grant for owner principals.
--
-- bootstrap_owner seeds capability grants only when the workspace row is
-- first created, so databases provisioned before cortex_agent_run joined
-- the capability catalog never receive it. Every agent run is then denied
-- with PolicyDeny::MissingGrant and the TUI chat degrades to
-- permission_denied. Owner grants are the complete catalog unless an owner
-- administration flow changes them, so inserting the missing grant matches
-- the intended production state without touching revocations of
-- pre-existing capabilities. ON CONFLICT DO NOTHING keeps already-granted
-- databases and remote principals untouched.
INSERT INTO capability_grant (workspace_id, principal_id, capability)
SELECT p.workspace_id, p.id, 'cortex_agent_run'
FROM principal p
WHERE p.name = 'owner'
ON CONFLICT DO NOTHING;
