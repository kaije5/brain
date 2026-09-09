CREATE TABLE remote_enrollment (
    workspace_id TEXT NOT NULL,
    subject TEXT NOT NULL CHECK (length(trim(subject)) > 0),
    principal_id TEXT NOT NULL,
    pairing_verifier BLOB NOT NULL CHECK (length(pairing_verifier) = 32),
    grants_json TEXT NOT NULL CHECK (json_valid(grants_json)),
    correlation_id TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (workspace_id, subject),
    UNIQUE (workspace_id, principal_id),
    FOREIGN KEY (workspace_id, principal_id)
        REFERENCES principal(workspace_id, id) ON DELETE RESTRICT
) STRICT;
