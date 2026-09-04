use cortex_mcp::{tool_schema, tool_schemas};

#[test]
fn delete_tool_is_annotated_destructive_and_excludes_workspace_id() {
    let schema = tool_schema("cortex_memory_delete").expect("memory delete schema");
    assert!(schema.destructive);
    assert!(!schema.input_properties.contains_key("workspace_id"));
}

#[test]
fn schema_catalog_exposes_only_supported_v0_1_tools() {
    let schemas = tool_schemas();
    let names: Vec<_> = schemas.iter().map(|schema| schema.name.as_str()).collect();
    assert!(names.contains(&"cortex_note_create"));
    assert!(names.contains(&"cortex_task_list"));
    assert!(names.contains(&"cortex_memory_search"));
    assert!(names.contains(&"cortex_knowledge_search"));
    assert!(!names.contains(&"cortex_agent_run"));
}
