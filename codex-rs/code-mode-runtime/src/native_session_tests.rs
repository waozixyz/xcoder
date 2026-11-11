//! Backend contract tests use real compiled Ziran cells and the production actor.
use super::*;
use codex_code_mode_protocol::{CodeModeToolKind, NoopCodeModeSessionDelegate, ToolDefinition};
use codex_protocol::ToolName;
use serde_json::json;
use tokio::sync::mpsc;

fn request(body: &str) -> ExecuteRequest {
    ExecuteRequest {
        tool_call_id: "native-test".into(),
        enabled_tools: vec![ToolDefinition {
            name: "fixture".into(),
            tool_name: ToolName {
                namespace: None,
                name: "fixture".into(),
            },
            description: "test capability".into(),
            kind: CodeModeToolKind::Function,
            input_schema: None,
            input_schema_max_bytes: None,
            output_schema: None,
        }],
        source: format!(
            "using X :: #import \"xcoder/runtime\"\nmain :: () -> s32 {{\n{body}\nreturn 0\n}}\n"
        ),
        yield_time_ms: Some(30_000),
        max_output_tokens: None,
    }
}
fn text(response: RuntimeResponse) -> (String, Option<String>) {
    let RuntimeResponse::Result {
        content_items,
        error_text,
        ..
    } = response
    else {
        panic!("expected completed native cell")
    };
    (
        content_items
            .into_iter()
            .filter_map(|item| match item {
                FunctionCallOutputContentItem::InputText { text } => Some(text),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(""),
        error_text,
    )
}
async fn run(session: &InProcessCodeModeSession, body: &str) -> RuntimeResponse {
    session
        .execute(request(body), Arc::new(NoopCodeModeSessionDelegate), None)
        .await
        .unwrap()
        .initial_response()
        .await
        .unwrap()
}

#[tokio::test]
async fn compiled_output_stored_values_and_session_isolation() {
    let first = InProcessCodeModeSession::new();
    assert_eq!(
        text(run(&first, "Text(\"native\"); Store(\"key\", \"42\")").await),
        ("native".into(), None)
    );
    let loaded = text(
        run(
            &first,
            "r := Load(\"key\"); defer Release(*r); Text(Body(*r))",
        )
        .await,
    );
    assert_eq!(
        serde_json::from_str::<JsonValue>(&loaded.0).unwrap()["value"],
        json!(42)
    );
    let other = InProcessCodeModeSession::new();
    let loaded = text(
        run(
            &other,
            "r := Load(\"key\"); defer Release(*r); Text(Body(*r))",
        )
        .await,
    );
    assert_eq!(
        serde_json::from_str::<JsonValue>(&loaded.0).unwrap()["value"],
        JsonValue::Null
    );
    first.shutdown().await.unwrap();
    other.shutdown().await.unwrap();
}

struct Delegate {
    calls: mpsc::UnboundedSender<CodeModeNestedToolCall>,
    block: bool,
    barrier: Option<Arc<tokio::sync::Barrier>>,
}
impl CodeModeSessionDelegate for Delegate {
    fn invoke_tool<'a>(
        &'a self,
        invocation: CodeModeNestedToolCall,
        cancel: CancellationToken,
    ) -> codex_code_mode_protocol::ToolInvocationFuture<'a> {
        Box::pin(async move {
            self.calls.send(invocation).unwrap();
            if let Some(barrier) = &self.barrier {
                tokio::select! {
                    _ = barrier.wait() => {},
                    _ = cancel.cancelled() => return Err("cancelled".into()),
                }
            }
            if self.block {
                cancel.cancelled().await;
                return Err("cancelled".into());
            }
            Ok(json!({"proof":49}))
        })
    }
    fn notify<'a>(
        &'a self,
        _: String,
        _: CellId,
        _: String,
        _: CancellationToken,
    ) -> codex_code_mode_protocol::NotificationFuture<'a> {
        Box::pin(async { Ok(()) })
    }
    fn cell_closed(&self, _: &CellId) {}
}

#[tokio::test]
async fn real_tool_input_output_and_unknown_tool_rejection() {
    let session = InProcessCodeModeSession::new();
    let (calls, mut received) = mpsc::unbounded_channel();
    let started = session
        .execute(
            request(r#"r := Call("fixture", "{\"value\":7}"); defer Release(*r); Text(Body(*r))"#),
            Arc::new(Delegate {
                calls,
                block: false,
                barrier: None,
            }),
            None,
        )
        .await
        .unwrap();
    let call = received.recv().await.unwrap();
    assert_eq!(call.tool_name.name, "fixture");
    assert_eq!(call.input, Some(json!({"value":7})));
    let (output, error) = text(started.initial_response().await.unwrap());
    assert_eq!(error, None);
    assert_eq!(
        serde_json::from_str::<JsonValue>(&output).unwrap()["value"]["proof"],
        json!(49)
    );
    let (output, error) = text(
        run(
            &session,
            "r := Call(\"not_enabled\"); defer Release(*r); Text(Body(*r))",
        )
        .await,
    );
    assert_eq!(error, None);
    assert_eq!(
        serde_json::from_str::<JsonValue>(&output).unwrap()["ok"],
        json!(false)
    );
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn terminate_pending_tool_cancels_callback_and_prevents_store() {
    let session = InProcessCodeModeSession::new();
    let (calls, mut received) = mpsc::unbounded_channel();
    let started = session
        .execute(
            request("r := Call(\"fixture\"); Release(*r); Store(\"forbidden\", \"1\")"),
            Arc::new(Delegate {
                calls,
                block: true,
                barrier: None,
            }),
            None,
        )
        .await
        .unwrap();
    received.recv().await.unwrap();
    let outcome = session
        .terminate(started.cell_id.clone().into())
        .await
        .unwrap();
    assert!(matches!(
        outcome,
        WaitOutcome::LiveCell(RuntimeResponse::Terminated { .. })
    ));
    let initial = started.initial_response().await.unwrap();
    assert!(matches!(initial, RuntimeResponse::Terminated { .. }));
    let (output, error) = text(
        run(
            &session,
            "r := Load(\"forbidden\"); defer Release(*r); Text(Body(*r))",
        )
        .await,
    );
    assert_eq!(error, None);
    assert_eq!(
        serde_json::from_str::<JsonValue>(&output).unwrap()["value"],
        JsonValue::Null
    );
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn explicit_yield_resumes_once_and_consumes_completion() {
    let session = InProcessCodeModeSession::new();
    let started = session
        .execute(
            request("Text(\"before\"); Yield(); Text(\"after\")"),
            Arc::new(NoopCodeModeSessionDelegate),
            None,
        )
        .await
        .unwrap();
    let id = started.cell_id.clone();
    let first = started.initial_response().await.unwrap();
    assert!(matches!(first, RuntimeResponse::Yielded { .. }));
    let wait = || WaitRequest {
        cell_id: id.clone(),
        yield_time_ms: 30_000,
        max_output_tokens: None,
    };
    let outcome = session.wait(wait(), None).await.unwrap();
    let WaitOutcome::LiveCell(result) = outcome else {
        panic!("missing yielded cell")
    };
    assert_eq!(text(result), ("after".into(), None));
    assert!(matches!(
        session.wait(wait(), None).await.unwrap(),
        WaitOutcome::MissingCell(_)
    ));
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn compilation_failure_is_cell_local_and_no_javascript_fallback() {
    let session = InProcessCodeModeSession::new();
    let mut invalid = request("");
    invalid.source = "text('javascript');".into();
    let result = session
        .execute(invalid, Arc::new(NoopCodeModeSessionDelegate), None)
        .await
        .unwrap()
        .initial_response()
        .await
        .unwrap();
    assert!(text(result).1.unwrap().contains("compilation failed"));
    assert_eq!(
        text(run(&session, "Text(\"still healthy\")").await),
        ("still healthy".into(), None)
    );
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn native_parallel_calls_keep_replies_and_ids_separate() {
    let session = InProcessCodeModeSession::new();
    let (calls, mut received) = mpsc::unbounded_channel();
    let started = session
        .execute(
            request(
                r#"calls: [4]BatchCall
    for i: 0..3 { calls[i].name = "fixture"; calls[i].input = "{\"value\":7}" }
    CallBatch(calls[:])
    for i: 0..3 { Text(Body(*calls[i].reply)); Release(*calls[i].reply) }"#,
            ),
            Arc::new(Delegate {
                calls,
                block: false,
                barrier: Some(Arc::new(tokio::sync::Barrier::new(4))),
            }),
            None,
        )
        .await
        .unwrap();
    let RuntimeResponse::Result {
        content_items,
        error_text,
        ..
    } = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        started.initial_response(),
    )
    .await
    .unwrap()
    .unwrap()
    else {
        panic!("parallel cell did not complete")
    };
    assert_eq!(error_text, None);
    assert_eq!(content_items.len(), 4);
    let mut ids = std::collections::HashSet::new();
    for _ in 0..4 {
        ids.insert(received.recv().await.unwrap().runtime_tool_call_id);
    }
    assert_eq!(ids.len(), 4);
    for item in content_items {
        let FunctionCallOutputContentItem::InputText { text } = item else {
            panic!("unexpected parallel output")
        };
        assert_eq!(
            serde_json::from_str::<JsonValue>(&text).unwrap()["value"]["proof"],
            json!(49)
        );
    }
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn pending_callbacks_leave_other_cells_responsive() {
    let session = InProcessCodeModeSession::new();
    let (calls, mut received) = mpsc::unbounded_channel();
    let started = session
        .execute(
            request(
                r#"calls: [2]BatchCall
    for i: 0..1 { calls[i].name = "fixture"; calls[i].input = "{}" }
    CallBatch(calls[:])
    for i: 0..1 { Release(*calls[i].reply) }"#,
            ),
            Arc::new(Delegate {
                calls,
                block: true,
                barrier: None,
            }),
            None,
        )
        .await
        .unwrap();
    for _ in 0..2 {
        tokio::time::timeout(std::time::Duration::from_secs(10), received.recv())
            .await
            .expect("both callbacks must reach the delegate before either finishes")
            .unwrap();
    }
    let independent = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        run(&session, r#"Text("independent cell completed")"#),
    )
    .await
    .expect("pending callbacks must not stall a text-only cell");
    assert_eq!(text(independent), ("independent cell completed".into(), None));
    session.terminate(started.cell_id.clone().into()).await.unwrap();
    assert!(matches!(
        started.initial_response().await.unwrap(),
        RuntimeResponse::Terminated { .. }
    ));
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn native_cells_cannot_fork_and_can_still_call_tools() {
    let session = InProcessCodeModeSession::new();
    let mut input = request("");
    input.source = r#"using X :: #import "xcoder/runtime"
c :: #system_library "libc";
Fork :: () -> s32 #foreign c "fork";
main :: () -> s32 { if Fork() >= 0 { return 8 }; Text("blocked"); return 0 }
"#
    .into();
    let result = session
        .execute(input, Arc::new(NoopCodeModeSessionDelegate), None)
        .await
        .unwrap()
        .initial_response()
        .await
        .unwrap();
    assert_eq!(text(result), ("blocked".into(), None));
    session.shutdown().await.unwrap();
}

#[tokio::test]
async fn session_memory_limit_reaches_the_native_allocator() {
    let session = InProcessCodeModeSession::with_limits(CodeModeSessionCellExecutionLimits {
        max_heap_size_bytes: Some(64 * 1024 * 1024),
        max_yield_time_ms: None,
    });
    let mut input = request("");
    input.source = r#"using X :: #import "xcoder/runtime"
c :: #system_library "libc";
Malloc :: (size: usize) -> *void #foreign c "malloc";
Free :: (value: *void) #foreign c "free";
main :: () -> s32 { memory := Malloc(cast(usize)134217728); if memory != null { Free(memory); return 5 }; Text("bounded"); return 0 }
"#.into();
    let result = session
        .execute(input, Arc::new(NoopCodeModeSessionDelegate), None)
        .await
        .unwrap()
        .initial_response()
        .await
        .unwrap();
    assert_eq!(text(result), ("bounded".into(), None));
    session.shutdown().await.unwrap();
}
