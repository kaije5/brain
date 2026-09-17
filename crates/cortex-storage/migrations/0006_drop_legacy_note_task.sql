-- SCRUM-93: one-way removal of legacy canonical note/task authority.
--
-- User-authored notes and tasks are owned exclusively by the Markdown vault
-- (MarkdownVaultProvider). These tables were already unused for writes since
-- the provider cutover; dropping them makes any legacy row recreation a hard
-- schema failure instead of a policy assumption. Retained state: memories,
-- sources, capability grants, operations/audit evidence, model routing,
-- remote enrollment and provider operations.
--
-- `search_document` is rebuilt to drop 'note'/'task' from the entity_kind
-- CHECK. Surviving memory/source rows are carried over verbatim, the FTS
-- index is rebuilt from the new table, and embeddings for carried rows are
-- preserved through the same copy.

DROP TABLE IF EXISTS embedding;
DROP TRIGGER IF EXISTS search_document_embedding_invalidate;
DROP TRIGGER IF EXISTS search_document_fts_insert;
DROP TRIGGER IF EXISTS search_document_fts_delete;
DROP TRIGGER IF EXISTS search_document_fts_update;
DROP TABLE IF EXISTS search_document_fts;

CREATE TABLE search_document_new (
    row_id INTEGER PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    entity_id TEXT NOT NULL,
    entity_kind TEXT NOT NULL CHECK (entity_kind IN ('memory', 'source')),
    snippet TEXT NOT NULL CHECK (length(trim(snippet)) > 0),
    content_hash BLOB NOT NULL CHECK (length(content_hash) = 32),
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (workspace_id, entity_id),
    FOREIGN KEY (workspace_id) REFERENCES workspace(id) ON DELETE CASCADE
) STRICT;

INSERT INTO search_document_new (row_id, workspace_id, entity_id, entity_kind, snippet, content_hash, updated_at)
SELECT row_id, workspace_id, entity_id, entity_kind, snippet, content_hash, updated_at
FROM search_document
WHERE entity_kind IN ('memory', 'source');

DROP TABLE search_document;
ALTER TABLE search_document_new RENAME TO search_document;

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

CREATE TRIGGER search_document_embedding_invalidate
AFTER UPDATE OF content_hash ON search_document
WHEN old.content_hash <> new.content_hash
BEGIN
    DELETE FROM embedding
    WHERE workspace_id = new.workspace_id AND entity_id = new.entity_id;
END;

CREATE TABLE embedding (
    workspace_id TEXT NOT NULL,
    entity_id TEXT NOT NULL,
    model_id TEXT NOT NULL CHECK (length(trim(model_id)) > 0),
    model_version TEXT NOT NULL CHECK (length(trim(model_version)) > 0),
    dimensions INTEGER NOT NULL CHECK (dimensions > 0),
    content_hash BLOB NOT NULL CHECK (length(content_hash) = 32),
    vector BLOB NOT NULL CHECK (length(vector) = dimensions * 4),
    index_state TEXT NOT NULL CHECK (index_state IN ('pending', 'ready', 'failed')),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (workspace_id, entity_id, model_id, model_version),
    FOREIGN KEY (workspace_id, entity_id)
        REFERENCES search_document(workspace_id, entity_id) ON DELETE CASCADE
) STRICT;

DROP TABLE IF EXISTS task;
DROP TABLE IF EXISTS note;

-- The wire capabilities behind note mutations are now the knowledge ones.
UPDATE capability_grant
SET capability = REPLACE(capability, 'cortex_note_', 'cortex_knowledge_')
WHERE capability LIKE 'cortex_note_%';
