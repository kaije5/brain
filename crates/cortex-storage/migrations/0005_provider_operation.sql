CREATE TABLE provider_operation (
    workspace_id TEXT NOT NULL,
    operation_id TEXT NOT NULL,
    outcome_json TEXT NOT NULL,
    PRIMARY KEY (workspace_id, operation_id)
);
