//! Composes Code Mode descriptions, retaining runtime-owned tool declarations.
//! Exec templates substitute only the documented literal placeholders.

use codex_protocol::ToolName;
use codex_protocol::openai_models::CodeModeToolMessages;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;
use std::collections::BTreeSet;

use crate::PUBLIC_TOOL_NAME;

const MAX_JS_SAFE_INTEGER: u64 = (1_u64 << 53) - 1;
const DEFERRED_NESTED_TOOLS_GUIDANCE: &str = "Some nested tools are deferred. Use Catalog() to inspect enabled tool names and schemas; names can change between cells.";
pub const TOOL_SEARCH_GUIDANCE: &str = r#"Discover deferred tools with Call("tool_search", "{\"query\":\"...\",\"limit\":8}"). Inspect the returned tool names and schemas before calling them."#;
const EXEC_DESCRIPTION_TEMPLATE: &str = r#"Run native Ziran code to orchestrate tool calls. Xcoder compiles cells with Zi2c; JavaScript is not supported.
Supply a complete Ziran program, starting with `using X :: #import "xcoder/runtime"`, and `main :: () -> s32 { ...; return 0 }`.
Example:
using X :: #import "xcoder/runtime"
main :: () -> s32 {
    reply := Call("exec_command", "{\"cmd\":\"pwd\"}")
    defer Release(*reply)
    Text(Body(*reply))
    return 0
}
`Call(name, input)` invokes an enabled tool. For function tools input is JSON text; for freeform tools it is literal source text. The returned Reply owns its buffer: `defer Release(*reply)` releases it. `Body(*reply)` borrows JSON `{ok: true, value: ...}` or `{ok: false, error: ...}`; transport success is `reply.ok`. Tool permission and approval decisions remain with the host.
`Text(string)` emits text. `Image(json)` and `Audio(json)` emit input_image/input_audio content with inline data URLs. `Notify(string)` sends a notification.
`Store(key, json)` saves a JSON value; `Load(key)` returns an owned Reply. `Catalog()` returns enabled tool metadata and schemas. Inspect results with Text and release each Reply.
`Yield()` yields output and suspends the cell until wait resumes it. For concurrent tool calls, fill an array of `BatchCall` with `name` and `input`, then call `CallBatch(calls[:])`. It uses up to four threads and joins before returning. Read `Body(*calls[i].reply)` and `Release(*calls[i].reply)` for each result. Tool calls are effects; Ziran's pure `#parallel` regions reject them. Cells share only explicitly stored JSON values within their session.
Cells have isolated filesystems and no external network or desktop access. Call tools for host operations. Do not import operating-system modules to bypass tools.
An optional first-line `// @exec: {"yield_time_ms": 10000, "max_output_tokens": 1000}` sets observation limits. Default foreground yield: {{ default_exec_yield_time_ms }} ms.
"#;
const WAIT_DESCRIPTION_TEMPLATE: &str = r#"- Use `wait` only after `exec` returns `Script running with cell ID ...`.
- `cell_id` identifies the running `exec` cell to resume.
- `yield_time_ms` controls how long to wait for more output before yielding again. Defaults to 10000 ms.
- `max_tokens` limits how much new output this wait call returns. Defaults to 10000 tokens.
- `terminate: true` stops the running cell; false or omitted waits for output.
- `wait` returns only the new output since the last yield, or the final completion or termination result for that cell.
- If the cell is still running, `wait` may yield again with the same `cell_id`.
- If the cell has already finished, `wait` returns the completed result and closes the cell."#;
pub const CODE_MODE_PRAGMA_PREFIX: &str = "// @exec:";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeModeToolKind {
    Function,
    Freeform,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ToolDefinition {
    pub name: String,
    pub tool_name: ToolName,
    pub description: String,
    pub kind: CodeModeToolKind,
    pub input_schema: Option<JsonValue>,
    /// Internal budget used while rendering the input declaration, before sending it to the host.
    #[serde(skip)]
    pub input_schema_max_bytes: Option<usize>,
    pub output_schema: Option<JsonValue>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolNamespaceDescription {
    pub name: String,
    pub description: String,
}

/// One complete tool definition or namespace instruction and its identity in an update.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecDefinition {
    /// Stable identity (`Tool: tools.<name>` or `Namespace instructions: <name>`) used for diffing.
    pub key: String,
    /// Complete rendered entry, including its heading, used to replace an earlier definition.
    pub description: String,
}

#[derive(Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct CodeModeExecPragma {
    #[serde(default)]
    yield_time_ms: Option<u64>,
    #[serde(default)]
    max_output_tokens: Option<usize>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ParsedExecSource {
    pub code: String,
    pub yield_time_ms: Option<u64>,
    pub max_output_tokens: Option<usize>,
}

pub fn parse_exec_source(input: &str) -> Result<ParsedExecSource, String> {
    if input.trim().is_empty() {
        return Err(
            "exec expects a complete non-empty Ziran program, optionally with first-line `// @exec: {\"yield_time_ms\": 10000, \"max_output_tokens\": 1000}`.".to_string(),
        );
    }

    let mut args = ParsedExecSource {
        code: input.to_string(),
        yield_time_ms: None,
        max_output_tokens: None,
    };

    let mut lines = input.splitn(2, '\n');
    let first_line = lines.next().unwrap_or_default();
    let rest = lines.next().unwrap_or_default();
    let trimmed = first_line.trim_start();
    let Some(pragma) = trimmed.strip_prefix(CODE_MODE_PRAGMA_PREFIX) else {
        return Ok(args);
    };

    if rest.trim().is_empty() {
        return Err("exec pragma must be followed by Ziran source on subsequent lines".to_string());
    }

    let directive = pragma.trim();
    if directive.is_empty() {
        return Err(
            "exec pragma must be a JSON object with supported fields `yield_time_ms` and `max_output_tokens`"
                .to_string(),
        );
    }

    let value: serde_json::Value = serde_json::from_str(directive).map_err(|err| {
        format!(
            "exec pragma must be valid JSON with supported fields `yield_time_ms` and `max_output_tokens`: {err}"
        )
    })?;
    let object = value.as_object().ok_or_else(|| {
        "exec pragma must be a JSON object with supported fields `yield_time_ms` and `max_output_tokens`"
            .to_string()
    })?;
    for key in object.keys() {
        match key.as_str() {
            "yield_time_ms" | "max_output_tokens" => {}
            _ => {
                return Err(format!(
                    "exec pragma only supports `yield_time_ms` and `max_output_tokens`; got `{key}`"
                ));
            }
        }
    }

    let pragma: CodeModeExecPragma = serde_json::from_value(value).map_err(|err| {
        format!(
            "exec pragma fields `yield_time_ms` and `max_output_tokens` must be non-negative safe integers: {err}"
        )
    })?;
    if pragma
        .yield_time_ms
        .is_some_and(|yield_time_ms| yield_time_ms > MAX_JS_SAFE_INTEGER)
    {
        return Err(
            "exec pragma field `yield_time_ms` must be a non-negative safe integer".to_string(),
        );
    }
    if pragma.max_output_tokens.is_some_and(|max_output_tokens| {
        u64::try_from(max_output_tokens)
            .map(|max_output_tokens| max_output_tokens > MAX_JS_SAFE_INTEGER)
            .unwrap_or(true)
    }) {
        return Err(
            "exec pragma field `max_output_tokens` must be a non-negative safe integer".to_string(),
        );
    }

    args.code = rest.to_string();
    args.yield_time_ms = pragma.yield_time_ms;
    args.max_output_tokens = pragma.max_output_tokens;
    Ok(args)
}

pub fn is_code_mode_nested_tool(tool_name: &str) -> bool {
    tool_name != crate::PUBLIC_TOOL_NAME && tool_name != crate::WAIT_TOOL_NAME
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImageDetailVisibility {
    Visible,
    Hidden,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeferredToolDiscovery {
    Catalog,
    RankedSearch,
}

/// Builds the full description and optionally collects stable entries in one rendering pass.
#[allow(clippy::too_many_arguments)]
pub fn build_exec_tool_description_and_definitions(
    enabled_tools: &[ToolDefinition],
    _deferred_tools: &[ToolDefinition],
    namespace_descriptions: &BTreeMap<String, ToolNamespaceDescription>,
    default_exec_yield_time_ms: u64,
    code_mode_only: bool,
    _image_detail_visibility: ImageDetailVisibility,
    deferred_tool_discovery: DeferredToolDiscovery,
    incremental_exec_enabled: bool,
    _messages: Option<&CodeModeToolMessages>,
) -> (String, Vec<ExecDefinition>) {
    let mut sections = Vec::new();
    let description = EXEC_DESCRIPTION_TEMPLATE.replace(
        "{{ default_exec_yield_time_ms }}",
        &default_exec_yield_time_ms.to_string(),
    );
    if !description.is_empty() {
        sections.push(description);
    }
    let default_guidance = match deferred_tool_discovery {
        // In Code Mode Only, the enabled search tool's description is rendered below.
        DeferredToolDiscovery::RankedSearch if !code_mode_only => {
            format!("{DEFERRED_NESTED_TOOLS_GUIDANCE}\n\n{TOOL_SEARCH_GUIDANCE}")
        }
        DeferredToolDiscovery::Catalog | DeferredToolDiscovery::RankedSearch => {
            DEFERRED_NESTED_TOOLS_GUIDANCE.to_string()
        }
    };
    let guidance = default_guidance.as_str();
    if !guidance.is_empty() {
        sections.push(guidance.to_string());
    }
    if !code_mode_only {
        return (sections.join("\n\n"), Vec::new());
    }

    let mut entries = Vec::new();
    if !enabled_tools.is_empty() {
        let mut current_namespace: Option<&str> = None;
        let mut seen_namespaces = BTreeSet::new();
        let mut nested_tool_sections = Vec::with_capacity(enabled_tools.len());

        for tool in enabled_tools {
            let name = tool.name.as_str();
            let nested_description = render_code_mode_sample_for_definition(tool);
            let namespace_description = tool
                .tool_name
                .namespace
                .as_ref()
                .and_then(|namespace| namespace_descriptions.get(namespace));
            let next_namespace = namespace_description
                .map(|namespace_description| namespace_description.name.as_str());
            if let Some(namespace_description) = namespace_description {
                let namespace_description_text = namespace_description.description.trim();
                if !namespace_description_text.is_empty() {
                    let new_entry = incremental_exec_enabled
                        && seen_namespaces.insert(&namespace_description.name);
                    let new_group = next_namespace != current_namespace;
                    if new_entry || new_group {
                        let description = format!(
                            "## {}\n{namespace_description_text}",
                            namespace_description.name
                        );
                        if new_entry {
                            entries.push(ExecDefinition {
                                key: format!(
                                    "Namespace instructions: {}",
                                    namespace_description.name
                                ),
                                description: description.clone(),
                            });
                        }
                        if new_group {
                            nested_tool_sections.push(description);
                        }
                    }
                }
            }
            current_namespace = next_namespace;

            let global_name = normalize_code_mode_identifier(name);
            let nested_description = nested_description.trim();
            let description = if nested_description.is_empty() {
                render_tool_heading(&global_name, name)
            } else {
                format!(
                    "{}\n{nested_description}",
                    render_tool_heading(&global_name, name)
                )
            };
            if incremental_exec_enabled {
                entries.push(ExecDefinition {
                    key: format!("Tool: tools.{global_name}"),
                    description: description.clone(),
                });
            }
            nested_tool_sections.push(description);
        }

        sections.push(nested_tool_sections.join("\n\n"));
    }

    (sections.join("\n\n"), entries)
}

pub fn build_wait_tool_description() -> &'static str {
    WAIT_DESCRIPTION_TEMPLATE
}

pub fn normalize_code_mode_identifier(tool_key: &str) -> String {
    let mut identifier = String::new();

    for (index, ch) in tool_key.chars().enumerate() {
        let is_valid = if index == 0 {
            ch == '_' || ch == '$' || ch.is_ascii_alphabetic()
        } else {
            ch == '_' || ch == '$' || ch.is_ascii_alphanumeric()
        };

        if is_valid {
            identifier.push(ch);
        } else {
            identifier.push('_');
        }
    }

    if identifier.is_empty() {
        "_".to_string()
    } else {
        identifier
    }
}

pub fn augment_tool_definition(mut definition: ToolDefinition) -> ToolDefinition {
    if definition.name != PUBLIC_TOOL_NAME {
        definition.description = render_code_mode_sample_for_definition(&definition);
    }
    definition
}

pub fn enabled_tool_metadata(definition: &ToolDefinition) -> EnabledToolMetadata {
    EnabledToolMetadata {
        tool_name: definition.tool_name.clone(),
        global_name: normalize_code_mode_identifier(&definition.name),
        description: definition.description.clone(),
        kind: definition.kind,
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct EnabledToolMetadata {
    pub tool_name: ToolName,
    pub global_name: String,
    pub description: String,
    pub kind: CodeModeToolKind,
}

pub fn render_code_mode_sample(
    description: &str,
    tool_name: &str,
    input_name: &str,
    input_type: String,
    output_type: String,
) -> String {
    format!(
        "{description}\n\nNative tool name: {tool_name}\nInput {input_name}: {input_type}\nOutput: {output_type}\nUse Call(name, input) from xcoder/runtime."
    )
}

fn render_code_mode_sample_for_definition(definition: &ToolDefinition) -> String {
    let input = match definition.kind {
        CodeModeToolKind::Function => format!(
            "JSON input schema: {}",
            schema_preview(
                definition.input_schema.as_ref().unwrap_or(&JsonValue::Null),
                definition
                    .input_schema_max_bytes
                    .unwrap_or(crate::DEFAULT_INPUT_SCHEMA_MAX_BYTES)
            )
        ),
        CodeModeToolKind::Freeform => "Input: literal freeform text".to_owned(),
    };
    format!(
        "{}\n\nNative tool name: {}\n{}\nUse Call(name, input) from xcoder/runtime; release the returned Reply.",
        definition.description, definition.name, input
    )
}

// Stop serialization at the existing catalog budget instead of first allocating
// an unbounded JSON schema string. Full metadata remains available through the
// native Catalog helper when a cell needs more detail.
fn schema_preview(schema: &JsonValue, limit: usize) -> String {
    struct Preview {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl std::io::Write for Preview {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let remaining = self.limit.saturating_sub(self.bytes.len());
            self.bytes
                .extend_from_slice(&bytes[..bytes.len().min(remaining)]);
            if bytes.len() > remaining {
                return Err(std::io::Error::other("schema preview budget"));
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut preview = Preview {
        bytes: Vec::new(),
        limit,
    };
    let truncated = serde_json::to_writer(&mut preview, schema).is_err();
    let mut text = String::from_utf8_lossy(&preview.bytes).into_owned();
    if truncated {
        text.push_str(" … [schema preview truncated; use Catalog() for metadata]");
    }
    text
}

fn render_tool_heading(global_name: &str, raw_name: &str) -> String {
    if global_name == raw_name {
        format!("### `{global_name}`")
    } else {
        format!("### `{global_name}` (`{raw_name}`)")
    }
}

#[cfg(test)]
#[path = "native_description_tests.rs"]
mod tests;
