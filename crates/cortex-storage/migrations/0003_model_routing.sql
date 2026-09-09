CREATE TABLE model_provider_profile (
    id TEXT PRIMARY KEY CHECK (length(trim(id)) > 0 AND length(id) <= 256),
    enabled INTEGER NOT NULL CHECK (enabled IN (0, 1)),
    secret_ref TEXT
) STRICT;

CREATE TABLE model_capability_evidence (
    profile_id TEXT NOT NULL REFERENCES model_provider_profile(id) ON DELETE CASCADE,
    model_id TEXT NOT NULL CHECK (length(trim(model_id)) > 0 AND length(model_id) <= 256),
    capability TEXT NOT NULL CHECK (capability IN ('tool_calling', 'structured_output')),
    observed_at TEXT NOT NULL,
    PRIMARY KEY (profile_id, model_id, capability)
) STRICT;

CREATE INDEX model_capability_evidence_profile_idx
    ON model_capability_evidence (profile_id);
