-- Resolve duplicate grants before the immutable migration 6 renames them.
DELETE FROM capability_grant AS legacy
WHERE substr(legacy.capability, 1, 12) = 'cortex_note_'
  AND EXISTS (
    SELECT 1 FROM capability_grant AS current
    WHERE current.workspace_id = legacy.workspace_id
      AND current.principal_id = legacy.principal_id
      AND current.capability = 'cortex_knowledge_' || substr(legacy.capability, 13)
  );
