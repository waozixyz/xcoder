//! Zi2c code-mode backend. The existing Rust actors retain approvals, transport,
//! output budgets and terminal-state ownership. Active cells are native Ziran;
//! no JavaScript engine or per-session worker is started.
mod event_channel;

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use codex_code_mode_protocol::{CodeModeToolKind, ExecuteRequest};
use codex_protocol::ToolName;
use futures::FutureExt;
use serde_json::{Value as JsonValue, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::TaskFailureHandler;
use crate::session_runtime::OutputItem;
pub(crate) use event_channel::{RuntimeEventReceiver, RuntimeEventSender, runtime_event_channel};

pub(crate) type RuntimeSender<T> = mpsc::UnboundedSender<T>;

#[derive(Debug)]
pub(crate) enum RuntimeCommand {
    ToolResponse { id: String, result: JsonValue },
    ToolError { id: String, error_text: String },
    ObservePendingFrontier,
    Terminate,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum PendingRuntimeMode {
    #[cfg(test)]
    Continue,
    PauseUntilResumed,
}
#[derive(Debug)]
pub(crate) enum RuntimeControlCommand {
    Continue,
    Resume,
    Terminate,
}

#[derive(Debug)]
pub(crate) enum RuntimeEvent {
    Started,
    Pending,
    ContentItem(OutputItem),
    YieldRequested,
    ToolCall {
        id: String,
        name: ToolName,
        kind: CodeModeToolKind,
        input: Option<JsonValue>,
    },
    Notify {
        call_id: String,
        text: String,
    },
    Result {
        stored_value_writes: HashMap<String, Arc<JsonValue>>,
        error_text: Option<String>,
    },
    ThreadPanicked,
}

#[derive(Clone)]
pub(crate) struct TerminationHandle(CancellationToken);
impl TerminationHandle {
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self(CancellationToken::new())
    }
    pub(crate) fn terminate_execution(&self) -> bool {
        self.0.cancel();
        true
    }
}

const MAX_FRAME: usize = 64 * 1024 * 1024;
const MAX_REQUESTS: usize = 128;
const MAX_BUFFERED_RPC: usize = 256 * 1024 * 1024;

#[derive(serde::Deserialize)]
struct NativeMessage {
    op: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    value: String,
}

struct RequestSocket {
    socket: UnixStream,
    _budget: OwnedSemaphorePermit,
}

struct BufferedReply {
    socket: UnixStream,
    bytes: Vec<u8>,
    _budget: OwnedSemaphorePermit,
}

pub(crate) fn spawn_runtime(
    stored_values: HashMap<String, Arc<JsonValue>>,
    request: ExecuteRequest,
    event_tx: impl Into<RuntimeEventSender>,
    pending_mode: PendingRuntimeMode,
    task_failure_handler: Option<TaskFailureHandler>,
    heap_limit: Option<usize>,
) -> Result<
    (
        RuntimeSender<RuntimeCommand>,
        RuntimeSender<RuntimeControlCommand>,
        TerminationHandle,
    ),
    String,
> {
    let toolchain = crate::native_sandbox::toolchain()?;
    if request.source.len() > 1024 * 1024 {
        return Err("Ziran cell source exceeds 1 MiB".into());
    }
    let (commands, command_rx) = mpsc::unbounded_channel();
    let (controls, control_rx) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();
    let termination = TerminationHandle(cancel.clone());
    let event_tx = event_tx.into();
    tokio::spawn(async move {
        let operation = AssertUnwindSafe(run(
            request,
            stored_values,
            event_tx.clone(),
            command_rx,
            control_rx,
            pending_mode,
            toolchain,
            heap_limit,
        ))
        .catch_unwind();
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {}
            result = operation => {
                let failure = match result {
                    Ok(Ok(())) => None,
                    Ok(Err(error)) => Some(error),
                    Err(_) => {
                        let message = "native cell supervisor panicked".to_owned();
                        if let Some(handler) = task_failure_handler { handler(message.clone()); }
                        let _ = event_tx.send(RuntimeEvent::ThreadPanicked).await;
                        Some(message)
                    }
                };
                if let Some(error) = failure {
                    tracing::debug!("native cell failed: {error}");
                    let _ = event_tx.send(RuntimeEvent::Result { stored_value_writes: HashMap::new(), error_text: Some(error) }).await;
                }
            }
        }
    });
    Ok((commands, controls, termination))
}

fn runtime_root() -> Result<PathBuf, String> {
    let root = std::env::var_os("XCODER_RUNTIME_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("XDG_RUNTIME_DIR").map(|value| PathBuf::from(value).join("xcoder"))
        })
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|value| PathBuf::from(value).join(".local/state/xcoder/runtime"))
        })
        .ok_or("set XCODER_RUNTIME_DIR or HOME")?;
    std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let info = std::fs::symlink_metadata(&root).map_err(|error| error.to_string())?;
    // SAFETY: geteuid has no pointer arguments or side effects.
    if !info.is_dir() || info.uid() != unsafe { libc::geteuid() } {
        return Err("native runtime directory must be a directory owned by this user".into());
    }
    if info.mode() & 0o077 != 0 {
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
        let info = std::fs::symlink_metadata(&root).map_err(|error| error.to_string())?;
        if !info.is_dir() || info.uid() != unsafe { libc::geteuid() } || info.mode() & 0o077 != 0 {
            return Err("native runtime directory must be private and owned by this user".into());
        }
    }
    Ok(root)
}

async fn bounded_read(reader: impl AsyncRead + Unpin, limit: usize) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    reader
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| error.to_string())?;
    if bytes.len() > limit {
        return Err("native compiler diagnostic limit exceeded".into());
    }
    Ok(bytes)
}

async fn read_request(
    mut socket: UnixStream,
    budget: Arc<Semaphore>,
) -> Result<(RequestSocket, NativeMessage), String> {
    let length = socket
        .read_u32_le()
        .await
        .map_err(|error| error.to_string())? as usize;
    if length > MAX_FRAME {
        return Err("native request exceeds 64 MiB".into());
    }
    // Reserve both the wire bytes and their decoded strings before allocating.
    // Merely sending many length headers cannot inflate the Rust supervisor.
    let permit = budget
        .try_acquire_many_owned((length * 2) as u32)
        .map_err(|_| "native RPC buffered-memory limit exceeded")?;
    let mut bytes = vec![0; length];
    socket
        .read_exact(&mut bytes)
        .await
        .map_err(|error| error.to_string())?;
    let request: NativeMessage = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid native request: {error}"))?;
    Ok((
        RequestSocket {
            socket,
            _budget: permit,
        },
        request,
    ))
}

struct FrameWriter(Vec<u8>);
impl std::io::Write for FrameWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > MAX_FRAME.saturating_sub(self.0.len()) {
            return Err(std::io::Error::other("native reply exceeds 64 MiB"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn buffer_reply(
    socket: RequestSocket,
    value: JsonValue,
    budget: &Arc<Semaphore>,
) -> Result<BufferedReply, String> {
    let RequestSocket {
        socket,
        _budget: request_budget,
    } = socket;
    drop(request_budget);
    let mut writer = FrameWriter(Vec::new());
    serde_json::to_writer(&mut writer, &value).map_err(|error| error.to_string())?;
    let permit = budget
        .clone()
        .try_acquire_many_owned(writer.0.len() as u32)
        .map_err(|_| "native RPC buffered-memory limit exceeded")?;
    Ok(BufferedReply {
        socket,
        bytes: writer.0,
        _budget: permit,
    })
}

fn write_reply(tasks: &mut JoinSet<Result<(), String>>, reply: BufferedReply) {
    tasks.spawn(async move {
        let BufferedReply {
            mut socket,
            bytes,
            _budget,
        } = reply;
        socket
            .write_u32_le(bytes.len() as u32)
            .await
            .map_err(|error| error.to_string())?;
        socket
            .write_all(&bytes)
            .await
            .map_err(|error| error.to_string())?;
        Ok(())
    });
}

fn reply(
    tasks: &mut JoinSet<Result<(), String>>,
    socket: RequestSocket,
    value: JsonValue,
    budget: &Arc<Semaphore>,
) -> Result<(), String> {
    write_reply(tasks, buffer_reply(socket, value, budget)?);
    Ok(())
}

#[cfg(test)]
mod native_transport_tests {
    use super::*;

    #[tokio::test]
    async fn oversized_length_header_is_rejected_before_payload_allocation() {
        let (mut sender, socket) = UnixStream::pair().unwrap();
        sender.write_u32_le(1024).await.unwrap();
        let budget = Arc::new(Semaphore::new(1024));
        let result =
            tokio::time::timeout(Duration::from_secs(1), read_request(socket, budget.clone()))
                .await
                .unwrap();
        assert!(matches!(result, Err(error) if error.contains("buffered-memory limit")));
        assert_eq!(budget.available_permits(), 1024);
    }

    #[tokio::test]
    async fn callback_and_reply_budgets_are_released_after_writing() {
        let (mut sender, socket) = UnixStream::pair().unwrap();
        let wire = br#"{"op":"call","name":"fixture","value":"{}"}"#;
        sender.write_u32_le(wire.len() as u32).await.unwrap();
        sender.write_all(wire).await.unwrap();
        let budget = Arc::new(Semaphore::new(1024));
        let (socket, message) = read_request(socket, budget.clone()).await.unwrap();
        assert_eq!(message.name, "fixture");
        assert_eq!(budget.available_permits(), 1024 - wire.len() * 2);
        let mut writes = JoinSet::new();
        reply(&mut writes, socket, json!({"ok":true,"value":49}), &budget).unwrap();
        assert!(budget.available_permits() < 1024);
        let length = sender.read_u32_le().await.unwrap();
        let mut response = vec![0; length as usize];
        sender.read_exact(&mut response).await.unwrap();
        assert_eq!(
            serde_json::from_slice::<JsonValue>(&response).unwrap()["value"],
            json!(49)
        );
        writes.join_next().await.unwrap().unwrap().unwrap();
        assert_eq!(budget.available_permits(), 1024);
    }
}

async fn forward_output(
    mut reader: impl AsyncRead + Unpin,
    events: RuntimeEventSender,
) -> Result<(), String> {
    let mut buffer = [0u8; 4096];
    let mut pending = Vec::new();
    loop {
        let size = reader
            .read(&mut buffer)
            .await
            .map_err(|error| error.to_string())?;
        if size == 0 {
            if !pending.is_empty() {
                events
                    .send(RuntimeEvent::ContentItem(OutputItem::InputText {
                        text: String::from_utf8_lossy(&pending).into_owned(),
                    }))
                    .await
                    .map_err(|_| "cell output closed")?;
            }
            return Ok(());
        }
        pending.extend_from_slice(&buffer[..size]);
        let end = match std::str::from_utf8(&pending) {
            Ok(_) => pending.len(),
            Err(error) if error.error_len().is_none() => error.valid_up_to(),
            Err(_) => pending.len(),
        };
        if end > 0 {
            let text = String::from_utf8_lossy(&pending[..end]).into_owned();
            events
                .send(RuntimeEvent::ContentItem(OutputItem::InputText { text }))
                .await
                .map_err(|_| "cell output closed")?;
            pending.drain(..end);
        }
    }
}

async fn run(
    request: ExecuteRequest,
    mut values: HashMap<String, Arc<JsonValue>>,
    events: RuntimeEventSender,
    mut commands: mpsc::UnboundedReceiver<RuntimeCommand>,
    mut controls: mpsc::UnboundedReceiver<RuntimeControlCommand>,
    pending_mode: PendingRuntimeMode,
    toolchain: PathBuf,
    heap_limit: Option<usize>,
) -> Result<(), String> {
    let directory = tempfile::Builder::new()
        .prefix("cell-")
        .tempdir_in(runtime_root()?)
        .map_err(|error| error.to_string())?;
    let path = directory.path();
    std::fs::create_dir(path.join("api")).map_err(|error| error.to_string())?;
    std::fs::write(
        path.join("api/runtime.zi"),
        include_str!("../../../../native/runtime.zi"),
    )
    .map_err(|error| error.to_string())?;
    std::fs::write(path.join("main.zi"), &request.source).map_err(|error| error.to_string())?;
    let mut compiler = crate::native_sandbox::compiler(path, &toolchain)
        .spawn()
        .map_err(|error| format!("start Zi2c sandbox: {error}"))?;
    let stdout = compiler
        .stdout
        .take()
        .ok_or("compiler stdout unavailable")?;
    let stderr = compiler
        .stderr
        .take()
        .ok_or("compiler stderr unavailable")?;
    let compilation = async {
        let (status, out, err) = tokio::join!(
            compiler.wait(),
            bounded_read(stdout, 64 * 1024),
            bounded_read(stderr, 64 * 1024)
        );
        Ok::<_, String>((status.map_err(|error| error.to_string())?, out?, err?))
    };
    let (status, out, err) = tokio::time::timeout(Duration::from_secs(30), compilation)
        .await
        .map_err(|_| "Zi2c compilation exceeded 30 seconds")??;
    if !status.success() {
        return Err(format!(
            "Zi2c compilation failed: {}{}",
            String::from_utf8_lossy(&out),
            String::from_utf8_lossy(&err)
        ));
    }
    let listener = UnixListener::bind(path.join("rpc")).map_err(|error| error.to_string())?;
    let mut child = crate::native_sandbox::cell(path, heap_limit)?;
    events
        .send(RuntimeEvent::Started)
        .await
        .map_err(|_| "cell actor closed")?;
    let mut outputs = JoinSet::new();
    outputs.spawn(forward_output(
        child.stdout.take().ok_or("native stdout unavailable")?,
        events.clone(),
    ));
    outputs.spawn(forward_output(
        child.stderr.take().ok_or("native stderr unavailable")?,
        events.clone(),
    ));
    let mut reads = JoinSet::new();
    let mut writes = JoinSet::new();
    let mut pending = HashMap::<String, RequestSocket>::new();
    let budget = Arc::new(Semaphore::new(MAX_BUFFERED_RPC));
    let mut paused_replies = Vec::new();
    let mut paused = false;
    let mut sequence = 0u64;
    let mut stored_value_writes = HashMap::new();
    let catalog = crate::native_dispatch::Catalog::new(&request.enabled_tools);
    let status = loop {
        tokio::select! {
            biased;
            command = commands.recv() => match command {
                Some(RuntimeCommand::ToolResponse { id, result }) => {
                    if let Some(socket) = pending.remove(&id) {
                        let value = json!({"ok": true, "value": result});
                        let buffered = buffer_reply(socket, value, &budget)?;
                        if paused { paused_replies.push(buffered); } else { write_reply(&mut writes, buffered); }
                    }
                }
                Some(RuntimeCommand::ToolError { id, error_text }) => {
                    if let Some(socket) = pending.remove(&id) {
                        let value = json!({"ok": false, "error": error_text});
                        let buffered = buffer_reply(socket, value, &budget)?;
                        if paused { paused_replies.push(buffered); } else { write_reply(&mut writes, buffered); }
                    }
                }
                Some(RuntimeCommand::ObservePendingFrontier) => {
                    if !paused && !pending.is_empty() { paused = true; events.send(RuntimeEvent::Pending).await.map_err(|_| "cell actor closed")?; }
                }
                Some(RuntimeCommand::Terminate) | None => return Ok(()),
            },
            control = controls.recv() => match control {
                Some(RuntimeControlCommand::Continue | RuntimeControlCommand::Resume) => {
                    paused = false;
                    for buffered in paused_replies.drain(..) { write_reply(&mut writes, buffered); }
                }
                Some(RuntimeControlCommand::Terminate) | None => return Ok(()),
            },
            result = child.wait() => break result.map_err(|error| error.to_string())?,
            accepted = listener.accept(), if reads.len() + pending.len() + paused_replies.len() + writes.len() < MAX_REQUESTS => {
                let (socket, _) = accepted.map_err(|error| error.to_string())?;
                reads.spawn(read_request(socket, budget.clone()));
            }
            completed = reads.join_next(), if !reads.is_empty() => {
                let (socket, message) = completed.ok_or("native request task disappeared")?.map_err(|error| error.to_string())??;
                let op = message.op.as_str();
                let name = message.name.as_str();
                let value = message.value.as_str();
                match op {
                    "call" => {
                        let Some(tool) = catalog.resolve(name) else {
                            reply(&mut writes, socket, json!({"ok":false,"error":"unknown or ambiguous enabled tool"}), &budget)?; continue;
                        };
                        let input = match tool.kind {
                            CodeModeToolKind::Function => match serde_json::from_str(value) {
                                Ok(value) => value,
                                Err(error) => { reply(&mut writes, socket, json!({"ok":false,"error":error.to_string()}), &budget)?; continue; }
                            },
                            CodeModeToolKind::Freeform => JsonValue::String(value.to_owned()),
                        };
                        sequence = sequence.checked_add(1).ok_or("native tool ID exhausted")?;
                        let id = format!("{}_{}", request.tool_call_id, sequence);
                        pending.insert(id.clone(), socket);
                        events.send(RuntimeEvent::ToolCall { id, name: tool.tool_name.clone(), kind: tool.kind, input: Some(input) }).await.map_err(|_| "cell actor closed")?;
                        if !paused && pending_mode == PendingRuntimeMode::PauseUntilResumed {
                            paused = true; events.send(RuntimeEvent::Pending).await.map_err(|_| "cell actor closed")?;
                        }
                    }
                    "text" => {
                        events.send(RuntimeEvent::ContentItem(OutputItem::InputText { text: value.into() })).await.map_err(|_| "cell actor closed")?;
                        reply(&mut writes, socket, json!({"ok":true}), &budget)?;
                    }
                    "image" | "audio" => {
                        let item: OutputItem = serde_json::from_str(value).map_err(|error| error.to_string())?;
                        let valid = match &item {
                            OutputItem::InputImage { image_url, .. } => op == "image" && image_url.starts_with("data:"),
                            OutputItem::InputAudio { audio_url } => op == "audio" && audio_url.starts_with("data:"),
                            _ => false,
                        };
                        if !valid { return Err("native media requires a matching inline data URL".into()); }
                        events.send(RuntimeEvent::ContentItem(item)).await.map_err(|_| "cell actor closed")?;
                        reply(&mut writes, socket, json!({"ok":true}), &budget)?;
                    }
                    "notify" => {
                        events.send(RuntimeEvent::Notify { call_id: request.tool_call_id.clone(), text: value.into() }).await.map_err(|_| "cell actor closed")?;
                        reply(&mut writes, socket, json!({"ok":true}), &budget)?;
                    }
                    "store" => {
                        let value: JsonValue = serde_json::from_str(value).map_err(|error| error.to_string())?;
                        let value = Arc::new(value); values.insert(name.into(), value.clone()); stored_value_writes.insert(name.into(), value);
                        reply(&mut writes, socket, json!({"ok":true}), &budget)?;
                    }
                    "load" => reply(&mut writes, socket, json!({"ok":true,"value":values.get(name).map(AsRef::as_ref)}), &budget)?,
                    "catalog" => reply(&mut writes, socket, json!({"ok":true,"value":request.enabled_tools}), &budget)?,
                    "yield" => {
                        paused = true;
                        paused_replies.push(buffer_reply(socket, json!({"ok":true}), &budget)?);
                        events.send(RuntimeEvent::YieldRequested).await.map_err(|_| "cell actor closed")?;
                    }
                    _ => reply(&mut writes, socket, json!({"ok":false,"error":"unknown native operation"}), &budget)?,
                }
            }
            result = writes.join_next(), if !writes.is_empty() => { result.ok_or("native reply task disappeared")?.map_err(|error| error.to_string())??; }
        }
    };
    reads.abort_all();
    writes.abort_all();
    while let Some(result) = outputs.join_next().await {
        result.map_err(|error| error.to_string())??;
    }
    events
        .send(RuntimeEvent::Result {
            stored_value_writes,
            error_text: if status.success() {
                None
            } else {
                Some(format!("native cell exited with {status}"))
            },
        })
        .await
        .map_err(|_| "cell actor closed")?;
    Ok(())
}
