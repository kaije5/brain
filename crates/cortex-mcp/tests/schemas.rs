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

#[test]
fn memory_schema_requires_structured_source_references_and_task_limit_is_an_integer() {
    let memory = tool_schema("cortex_memory_create").expect("memory create schema");
    assert_eq!(
        memory.input_schema["required"],
        serde_json::json!([
            "statement",
            "normalized_subject",
            "normalized_predicate",
            "normalized_object",
            "sources"
        ])
    );
    assert_eq!(
        memory.input_schema["properties"]["sources"]["type"],
        "array"
    );
    assert!(memory.input_schema["properties"]["sources"]["items"]["$ref"].is_string());

    let tasks = tool_schema("cortex_task_list").expect("task list schema");
    assert!(
        tasks.input_schema["properties"]["limit"]["type"]
            .as_array()
            .is_some_and(|types| types.iter().any(|kind| kind == "integer"))
    );
}

#[test]
fn schemas_publish_wire_types_formats_bounds_and_closed_objects() {
    for schema in tool_schemas() {
        assert_eq!(schema.input_schema["type"], "object", "{}", schema.name);
        assert_eq!(
            schema.input_schema["additionalProperties"], false,
            "{}",
            schema.name
        );
        assert!(
            schema.input_schema["properties"]
                .get("principal_id")
                .is_none(),
            "{}",
            schema.name
        );
        assert!(
            schema.input_schema["properties"]
                .get("workspace_id")
                .is_none(),
            "{}",
            schema.name
        );
    }

    let entity = tool_schema("cortex_note_delete").expect("entity command schema");
    assert_eq!(
        entity.input_schema["properties"]["entity_id"]["format"],
        "uuid"
    );
    assert_eq!(
        entity.input_schema["properties"]["expected_revision"]["minimum"],
        1
    );

    let task = tool_schema("cortex_task_create").expect("task create schema");
    assert_eq!(
        task.input_schema["properties"]["due_at"]["format"],
        "date-time"
    );

    let search = tool_schema("cortex_knowledge_search").expect("knowledge schema");
    assert_eq!(search.input_schema["properties"]["limit"]["minimum"], 1);
    assert_eq!(search.input_schema["properties"]["limit"]["maximum"], 100);

    let memory = tool_schema("cortex_memory_create").expect("memory create schema");
    assert_eq!(
        memory.input_schema["$defs"]["Source"]["properties"]["source_id"]["format"],
        "uuid"
    );
}
