CREATE TABLE workspace (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL CHECK (length(trim(name)) > 0),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
) STRICT;

CREATE TABLE principal (
    id TEXT PRIMARY KEY NOT NULL,
    workspace_id TEXT NOT NULL,
    name TEXT NOT NULL CHECK (length(trim(name)) > 0),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (workspace_id, id),
    FOREIGN KEY (workspace_id) REFERENCES workspace(id) ON DELETE RESTRICT
) STRICT;

CREATE TABLE capability_grant (
    workspace_id TEXT NOT NULL,
    principal_id TEXT NOT NULL,
    capability TEXT NOT NULL CHECK (length(trim(capability)) > 0),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (workspace_id, principal_id, capability),
    FOREIGN KEY (workspace_id, principal_id)
        REFERENCES principal(workspace_id, id) ON DELETE RESTRICT
) STRICT;

CREATE TABLE note (
    id TEXT PRIMARY KEY NOT NULL,
    workspace_id TEXT NOT NULL,
    title TEXT NOT NULL CHECK (length(trim(title)) > 0),
    content TEXT NOT NULL CHECK (length(trim(content)) > 0),
    revision INTEGER NOT NULL CHECK (revision > 0),
    lifecycle TEXT NOT NULL CHECK (lifecycle IN ('active', 'deleted')),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (workspace_id, id),
    FOREIGN KEY (workspace_id) REFERENCES workspace(id) ON DELETE RESTRICT
) STRICT;

CREATE TABLE task (
    id TEXT PRIMARY KEY NOT NULL,
    workspace_id TEXT NOT NULL,
    title TEXT NOT NULL CHECK (length(trim(title)) > 0),
    due_at TEXT,
    status TEXT NOT NULL CHECK (status IN ('open', 'completed')),
    revision INTEGER NOT NULL CHECK (revision > 0),
    lifecycle TEXT NOT NULL CHECK (lifecycle IN ('active', 'deleted')),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (workspace_id, id),
    FOREIGN KEY (workspace_id) REFERENCES workspace(id) ON DELETE RESTRICT
) STRICT;

CREATE TABLE source (
    id TEXT PRIMARY KEY NOT NULL,
    workspace_id TEXT NOT NULL,
    reference TEXT NOT NULL CHECK (length(trim(reference)) > 0),
    revision INTEGER NOT NULL CHECK (revision > 0),
    lifecycle TEXT NOT NULL CHECK (lifecycle IN ('active', 'deleted')),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (workspace_id, id),
    FOREIGN KEY (workspace_id) REFERENCES workspace(id) ON DELETE RESTRICT
) STRICT;

CREATE TABLE memory_assertion (
    id TEXT PRIMARY KEY NOT NULL,
    workspace_id TEXT NOT NULL,
    statement TEXT NOT NULL CHECK (length(trim(statement)) > 0),
    normalized_subject TEXT NOT NULL CHECK (length(trim(normalized_subject)) > 0),
    normalized_predicate TEXT NOT NULL CHECK (length(trim(normalized_predicate)) > 0),
    normalized_object TEXT NOT NULL CHECK (length(trim(normalized_object)) > 0),
    supersedes_id TEXT,
    status TEXT NOT NULL CHECK (status IN ('active', 'superseded', 'forgotten')),
    revision INTEGER NOT NULL CHECK (revision > 0),
    lifecycle TEXT NOT NULL CHECK (lifecycle IN ('active', 'deleted')),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (workspace_id, id),
    FOREIGN KEY (workspace_id) REFERENCES workspace(id) ON DELETE RESTRICT,
    FOREIGN KEY (workspace_id, supersedes_id)
        REFERENCES memory_assertion(workspace_id, id) ON DELETE RESTRICT,
    CHECK (supersedes_id IS NULL OR supersedes_id <> id)
) STRICT;

CREATE TABLE memory_source (
    workspace_id TEXT NOT NULL,
    memory_id TEXT NOT NULL,
    source_id TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (workspace_id, memory_id, source_id),
    FOREIGN KEY (workspace_id, memory_id)
        REFERENCES memory_assertion(workspace_id, id) ON DELETE CASCADE,
    FOREIGN KEY (workspace_id, source_id)
        REFERENCES source(workspace_id, id) ON DELETE RESTRICT
) STRICT;

CREATE TABLE search_document (
    row_id INTEGER PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    entity_id TEXT NOT NULL,
    entity_kind TEXT NOT NULL CHECK (entity_kind IN ('note', 'task', 'memory', 'source')),
    snippet TEXT NOT NULL CHECK (length(trim(snippet)) > 0),
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (workspace_id, entity_id),
    FOREIGN KEY (workspace_id) REFERENCES workspace(id) ON DELETE CASCADE
) STRICT;

CREATE VIRTUAL TABLE search_document_fts USING fts5(
    snippet,
    content = 'search_document',
    content_rowid = 'row_id',
    tokenize = 'unicode61'
);

CREATE TRIGGER search_document_fts_insert
AFTER INSERT ON search_document
BEGIN
    INSERT INTO search_document_fts(rowid, snippet) VALUES (new.row_id, new.snippet);
END;

CREATE TRIGGER search_document_fts_delete
AFTER DELETE ON search_document
BEGIN
    INSERT INTO search_document_fts(search_document_fts, rowid, snippet)
    VALUES ('delete', old.row_id, old.snippet);
END;

CREATE TRIGGER search_document_fts_update
AFTER UPDATE ON search_document
BEGIN
    INSERT INTO search_document_fts(search_document_fts, rowid, snippet)
    VALUES ('delete', old.row_id, old.snippet);
    INSERT INTO search_document_fts(rowid, snippet) VALUES (new.row_id, new.snippet);
END;

CREATE TABLE embedding (
    workspace_id TEXT NOT NULL,
    entity_id TEXT NOT NULL,
    model_id TEXT NOT NULL CHECK (length(trim(model_id)) > 0),
    model_version TEXT NOT NULL CHECK (length(trim(model_version)) > 0),
    dimensions INTEGER NOT NULL CHECK (dimensions > 0),
    content_hash BLOB NOT NULL,
    vector BLOB NOT NULL CHECK (length(vector) = dimensions * 4),
    index_state TEXT NOT NULL CHECK (index_state IN ('pending', 'ready', 'failed')),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (workspace_id, entity_id, model_id, model_version),
    FOREIGN KEY (workspace_id, entity_id)
        REFERENCES search_document(workspace_id, entity_id) ON DELETE CASCADE
) STRICT;

CREATE TABLE operation (
    workspace_id TEXT NOT NULL,
    operation_id TEXT NOT NULL,
    outcome_json TEXT NOT NULL CHECK (json_valid(outcome_json)),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (workspace_id, operation_id),
    FOREIGN KEY (workspace_id) REFERENCES workspace(id) ON DELETE RESTRICT
) STRICT;

CREATE TABLE audit_event (
    id TEXT PRIMARY KEY NOT NULL,
    workspace_id TEXT NOT NULL,
    principal_id TEXT NOT NULL,
    operation_id TEXT NOT NULL,
    correlation_id TEXT NOT NULL,
    capability TEXT NOT NULL CHECK (length(trim(capability)) > 0),
    target_id TEXT,
    policy_decision TEXT NOT NULL CHECK (policy_decision IN ('allow', 'deny_missing_grant')),
    result TEXT NOT NULL CHECK (result IN ('succeeded', 'rejected', 'failed')),
    redacted_metadata TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(redacted_metadata)),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (workspace_id, operation_id),
    FOREIGN KEY (workspace_id, principal_id)
        REFERENCES principal(workspace_id, id) ON DELETE RESTRICT
) STRICT;

CREATE TRIGGER audit_event_append_only_update
BEFORE UPDATE ON audit_event
BEGIN
    SELECT RAISE(ABORT, 'audit events are append-only');
END;

CREATE TRIGGER audit_event_append_only_delete
BEFORE DELETE ON audit_event
BEGIN
    SELECT RAISE(ABORT, 'audit events are append-only');
END;
