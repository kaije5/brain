-- SCRUM-82: durable routing decision. A resolved route names both the model
-- and the provider profile it was selected on; credential values never reach
-- storage. Single-row table: the latest deterministic selection per daemon.
CREATE TABLE model_route_decision (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    profile_id TEXT NOT NULL REFERENCES model_provider_profile(id) ON DELETE CASCADE,
    model_id TEXT NOT NULL CHECK (length(trim(model_id)) > 0 AND length(model_id) <= 256),
    routed_at TEXT NOT NULL
) STRICT;
