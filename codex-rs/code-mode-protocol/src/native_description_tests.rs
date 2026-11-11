use super::*;
use codex_protocol::openai_models::{CodeModeToolMessages, ToolMessage};
use serde_json::json;

fn definition(name: &str) -> ToolDefinition {
    ToolDefinition {
        name: name.into(),
        tool_name: ToolName::plain(name),
        description: "Fixture capability".into(),
        kind: CodeModeToolKind::Function,
        input_schema: Some(
            json!({"type":"object","properties":{"cmd":{"type":"string","description":"command to run"}}}),
        ),
        input_schema_max_bytes: None,
        output_schema: None,
    }
}

#[test]
fn native_descriptions_keep_names_schemas_and_incremental_identity() {
    let tools = vec![
        definition("exec_command"),
        definition("mcp__fixture__lookup"),
    ];
    let (description, entries) = build_exec_tool_description_and_definitions(
        &tools,
        &[],
        &BTreeMap::new(),
        1234,
        true,
        ImageDetailVisibility::Visible,
        DeferredToolDiscovery::Catalog,
        true,
        None,
    );
    assert!(description.contains("native Ziran"));
    assert!(description.contains("1234 ms"));
    assert!(description.contains("command to run"));
    assert!(!description.contains("declare const tools"));
    assert!(!description.contains("fresh V8 isolate"));
    assert_eq!(entries.len(), 2);
    for entry in entries {
        assert!(description.contains(&entry.description));
    }
}

#[test]
fn server_javascript_templates_cannot_override_the_native_contract() {
    let messages = CodeModeToolMessages {
        exec: Some(ToolMessage {
            description: Some("Run JavaScript in V8".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let (description, _) = build_exec_tool_description_and_definitions(
        &[],
        &[],
        &BTreeMap::new(),
        10_000,
        false,
        ImageDetailVisibility::Hidden,
        DeferredToolDiscovery::RankedSearch,
        false,
        Some(&messages),
    );
    assert!(description.contains("xcoder/runtime"));
    assert!(!description.contains("Run JavaScript in V8"));
    assert!(description.contains("Call(\"tool_search\""));
}

#[test]
fn native_freeform_declarations_do_not_expect_json() {
    let mut tool = definition("apply_patch");
    tool.kind = CodeModeToolKind::Freeform;
    let tool = augment_tool_definition(tool);
    assert!(tool.description.contains("literal freeform text"));
    assert!(tool.description.contains("apply_patch"));
}

#[test]
fn native_source_and_observation_pragma_round_trip() {
    let source =
        "using X :: #import \"xcoder/runtime\"\nmain :: () -> s32 { Text(\"hi\"); return 0 }";
    let plain = parse_exec_source(source).unwrap();
    assert_eq!(plain.code, source);
    let with_pragma = parse_exec_source(&format!(
        "// @exec: {{\"yield_time_ms\":12,\"max_output_tokens\":42}}\n{source}"
    ))
    .unwrap();
    assert_eq!(with_pragma.code, source);
    assert_eq!(with_pragma.yield_time_ms, Some(12));
    assert_eq!(with_pragma.max_output_tokens, Some(42));
    for pragma in [
        "{\"unknown\":1}",
        "{\"yield_time_ms\":-1}",
        "[]",
        "not json",
    ] {
        assert!(parse_exec_source(&format!("// @exec: {pragma}\n{source}")).is_err());
    }
}

#[test]
fn native_schema_previews_respect_the_catalog_budget() {
    let mut tool = definition("huge");
    tool.input_schema = Some(json!({"description":"文字".repeat(100_000)}));
    tool.input_schema_max_bytes = Some(128);
    let result = augment_tool_definition(tool);
    assert!(result.description.len() < 512);
    assert!(result.description.contains("schema preview truncated"));
    assert!(result.description.contains("Catalog()"));
}
