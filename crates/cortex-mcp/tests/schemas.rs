use cortex_mcp::{tool_schema, tool_schemas};

#[test]
fn delete_tool_is_annotated_destructive_and_excludes_workspace_id() {
    let schema = tool_schema("memory.delete").expect("memory delete schema");
    assert!(schema.destructive);
    assert!(!schema.input_properties.contains_key("workspace_id"));
}

#[test]
fn schema_catalog_exposes_only_the_normalized_v2_tools() {
    let schemas = tool_schemas();
    let names: Vec<_> = schemas.iter().map(|schema| schema.name.as_str()).collect();
    // Normalized knowledge.* and task.* tools replace the legacy
    // knowledge/task contracts.
    for name in [
        "knowledge.create",
        "knowledge.update",
        "knowledge.delete",
        "knowledge.retrieve",
        "task.create",
        "task.update",
        "task.complete",
        "task.delete",
        "task.restore",
        "task.list",
    ] {
        assert!(names.contains(&name), "missing normalized tool {name}");
    }
    assert!(names.contains(&"memory.search"));
    // The agent loop is not an externally callable tool.
    assert!(!names.contains(&"agent.run"));
    // Legacy SQLite-entity tool names are gone.
    for legacy in [
        "cortex_knowledge_create",
        "cortex_task_list",
        "cortex_knowledge_search",
    ] {
        assert!(!names.contains(&legacy), "legacy tool {legacy} removed");
    }
}

#[test]
fn memory_schema_requires_structured_source_references_and_task_limit_is_an_integer() {
    let memory = tool_schema("memory.create").expect("memory create schema");
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

    let tasks = tool_schema("task.list").expect("task list schema");
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

    // Resource-scoped mutations address opaque provider resource ids and
    // observed revisions, not SQLite entity identity.
    let resource = tool_schema("knowledge.delete").expect("resource command schema");
    assert!(resource.input_schema["properties"]["resource_id"].is_object());
    assert!(resource.input_schema["properties"]["expected_revision"].is_object());
    assert!(
        resource.input_schema["properties"]
            .get("entity_id")
            .is_none()
    );

    let task = tool_schema("task.create").expect("task create schema");
    assert_eq!(
        task.input_schema["properties"]["due_at"]["format"],
        "date-time"
    );

    let search = tool_schema("knowledge.retrieve").expect("knowledge schema");
    assert_eq!(search.input_schema["properties"]["limit"]["minimum"], 1);
    assert_eq!(search.input_schema["properties"]["limit"]["maximum"], 100);

    let memory = tool_schema("memory.create").expect("memory create schema");
    assert_eq!(
        memory.input_schema["$defs"]["Source"]["properties"]["source_id"]["format"],
        "uuid"
    );
}

#[test]
fn memory_delete_arguments_decode() {
    let decoded = cortex_mcp::decode_arguments(
        "memory.delete",
        serde_json::json!({"entity_id": uuid::Uuid::now_v7(), "expected_revision": 1}),
    )
    .expect("memory.delete decodes");
    assert!(decoded["entity_id"].is_string());
}
