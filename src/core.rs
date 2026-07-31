// Conversation state, LLM round-trips, tool execution.
// No ratatui/crossterm imports — tui-facing rendering/input state
// lives in the tui module instead.

mod credentials;
mod diff;
mod mistral;
mod ollama;
mod sandbox_path;
mod settings;
mod tools;

pub use credentials::{CredentialSource, credential_log_message, lookup_mistral_api_key};
pub use diff::{DiffLine, DiffLineText};
pub use mistral::MistralClient;
pub use ollama::OllamaClient;

pub(crate) use diff::generate_diff;
pub(crate) use mistral::MISTRAL_MODEL;
pub(crate) use ollama::OLLAMA_MODEL;
pub(crate) use sandbox_path::{SandboxError, SandboxPath};
pub(crate) use settings::{FileAccessSecurity, Settings};
use tools::{dispatch, tool_definitions, write_file_with_confirmation};

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;

// Bounds worst-case cost of one user message: a running count of
// individual tool calls across the whole agent loop, not rounds — a
// round-based cap wouldn't actually bound the risk (a model batching
// many calls into one round would sail through it) and would also cut
// off legitimate work (e.g. reading a new project's ~15-20 files).
const MAX_TOOL_CALLS: usize = 40;

#[derive(Debug, PartialEq)]
pub enum CoreEvent {
    AssistantChunk(String),
    ToolCall {
        id: String,
        name: String,
        arguments: String,
        // Ok/Err, not a flattened String: lets the TUI show only "ok"
        // on success (a read_file result could be an entire file's
        // contents — useful to the model, not something the chat log
        // should echo back at the user) while still showing an error's
        // actual (short, useful) message in full.
        result: Result<String, String>,
    },
    // A write_file call awaiting user confirmation — the agent loop's
    // background thread blocks right after sending this, until
    // Core::respond_to_confirmation is called. No correlating id: the
    // loop processes tool calls one at a time, so only one of these can
    // ever be outstanding at once.
    WriteProposed {
        path: String,
        diff: Vec<DiffLine>,
    },
    Error(String),
}

// The user's answer to a WriteProposed prompt. An enum, not a bool —
// matches every other enum-of-kinds decision in this codebase, and
// reads clearly at the call site (respond_to_confirmation(Apply), not
// respond_to_confirmation(true)).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConfirmationChoice {
    Apply,
    Decline,
}

// One entry in the conversation history. An enum, not a flat struct with
// optional fields, so an invalid combination (e.g. a tool result with no
// correlating id) isn't representable at all — matches every other enum
// decision in this codebase (ChatError, CredentialSource, ProviderLabel).
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    User {
        content: String,
    },
    Assistant {
        content: String,
    },
    // The assistant's turn requesting one or more tool invocations. One
    // entry per individual call, not one entry per LLM turn (which could
    // batch several) — a deliberate simplification with a real, unverified
    // assumption about whether the provider tolerates several separate
    // single-call turns as well as one multi-call turn; see
    // ARCHITECTURE.md's "Conversation history" section.
    ToolCalls {
        calls: Vec<ToolCall>,
    },
    // One tool's result, correlated back to its request via tool_call_id.
    // Also carries the tool's own name, self-contained rather than
    // requiring a provider to scan back through history for the
    // matching ToolCalls entry — Ollama's wire format correlates a
    // result to its request by name, not id, so it needs this directly.
    ToolResult {
        tool_call_id: String,
        name: String,
        content: String,
    },
}

// Describes one tool the model may call. `parameters` is a JSON schema
// (Mistral's own tool-schema shape — see core::tools::tool_definitions
// and core::mistral::to_mistral_tools), kept as serde_json::Value rather
// than a typed struct since its shape varies per tool.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

// One requested tool invocation. `arguments` stays a raw JSON string —
// parsing it into typed arguments is each tool's own job, not something
// LlmResponse itself should assume the shape of.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LlmResponse {
    Text(String),
    ToolCalls(Vec<ToolCall>),
}

// Hand-written, not `thiserror` — per this project's Dependency
// Discipline (parent CLAUDE.md), a handful of variants isn't worth a
// dependency.
#[derive(Debug, Clone, PartialEq)]
pub enum ChatError {
    Connection(String),
    MalformedResponse(String),
    Auth(String),
}

impl fmt::Display for ChatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChatError::Connection(message) => write!(f, "connection error: {message}"),
            ChatError::MalformedResponse(message) => {
                write!(f, "malformed response: {message}")
            }
            ChatError::Auth(message) => write!(f, "authentication error: {message}"),
        }
    }
}

impl std::error::Error for ChatError {}

// Implemented by each provider's client; Core talks to whichever one is
// active only through this, never through a provider-specific type.
pub trait LlmClient {
    fn send(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> Result<LlmResponse, ChatError>;
}

// The agent loop: send, and either get a final answer (done) or one or
// more tool calls (dispatch each, append results, send again) — capped
// at MAX_TOOL_CALLS total individual calls. Runs entirely on the
// spawned background thread; only ever communicates back to Core via
// tx, never touches Core's own history directly — see poll_events for
// where Core's persistent history actually gets updated, which mirrors
// (deliberately kept in sync with) the shape pushed onto `history` here.
fn run_agent_loop(
    client: &Arc<dyn LlmClient + Send + Sync>,
    root: &Path,
    file_access_security: FileAccessSecurity,
    mut history: Vec<Message>,
    tx: &mpsc::Sender<CoreEvent>,
    confirm_rx: &mpsc::Receiver<ConfirmationChoice>,
) {
    let tool_defs = tool_definitions();
    let mut tool_call_count = 0usize;

    loop {
        match client.send(&history, &tool_defs) {
            Ok(LlmResponse::Text(text)) => {
                let _ = tx.send(CoreEvent::AssistantChunk(text));
                return;
            }
            Ok(LlmResponse::ToolCalls(calls)) => {
                if tool_call_count + calls.len() > MAX_TOOL_CALLS {
                    let _ = tx.send(CoreEvent::Error(
                        "agent loop exceeded the maximum number of tool calls".to_string(),
                    ));
                    return;
                }
                tool_call_count += calls.len();

                for call in calls {
                    history.push(Message::ToolCalls {
                        calls: vec![call.clone()],
                    });
                    // The model needs the full content either way (what
                    // was read, or why it failed) — only the CoreEvent
                    // sent to the TUI distinguishes Ok from Err.
                    // write_file needs the confirmation channel dispatch
                    // doesn't have, so it's routed separately rather
                    // than folded into dispatch's uniform signature.
                    let dispatch_result = if call.name == "write_file" {
                        write_file_with_confirmation(
                            root,
                            file_access_security,
                            &call,
                            tx,
                            confirm_rx,
                        )
                    } else {
                        dispatch(root, file_access_security, &call)
                    };
                    let content_for_history = match &dispatch_result {
                        Ok(output) => output.clone(),
                        Err(e) => e.to_string(),
                    };
                    history.push(Message::ToolResult {
                        tool_call_id: call.id.clone(),
                        name: call.name.clone(),
                        content: content_for_history,
                    });
                    let _ = tx.send(CoreEvent::ToolCall {
                        id: call.id,
                        name: call.name,
                        arguments: call.arguments,
                        result: dispatch_result.map_err(|e| e.to_string()),
                    });
                }
                // Loop again with the updated history.
            }
            Err(e) => {
                let _ = tx.send(CoreEvent::Error(e.to_string()));
                return;
            }
        }
    }
}

pub struct Core {
    client: Arc<dyn LlmClient + Send + Sync>,
    root: PathBuf,
    // Its file_access_security field is threaded through to dispatch()
    // on every tool call (see run_agent_loop/submit_user_message) — see
    // settings.rs and SECURITY.md for what it currently blocks.
    settings: Settings,
    history: Vec<Message>,
    tx: mpsc::Sender<CoreEvent>,
    rx: mpsc::Receiver<CoreEvent>,
    // A fresh channel is created per submit_user_message call (mirrors
    // the fresh thread spawned each time) — Core keeps the Sender half
    // so respond_to_confirmation can reach whichever loop is currently
    // running; the Receiver half moves into that loop's thread. None
    // until the first message is submitted.
    confirm_tx: Option<mpsc::Sender<ConfirmationChoice>>,
}

impl Default for Core {
    fn default() -> Self {
        Self::new()
    }
}

impl Core {
    pub fn new() -> Self {
        Self::with_client(Arc::new(OllamaClient::new(OLLAMA_MODEL.to_string())))
    }

    pub fn with_client(client: Arc<dyn LlmClient + Send + Sync>) -> Self {
        let (tx, rx) = mpsc::channel();
        let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let settings = Settings::load();
        Core {
            client,
            root,
            settings,
            history: Vec::new(),
            tx,
            rx,
            confirm_tx: None,
        }
    }

    pub fn submit_user_message(&mut self, text: String) {
        self.history.push(Message::User { content: text });

        let (confirm_tx, confirm_rx) = mpsc::channel();
        self.confirm_tx = Some(confirm_tx);

        let tx = self.tx.clone();
        let client = Arc::clone(&self.client);
        let history = self.history.clone();
        let root = self.root.clone();
        let file_access_security = self.settings.file_access_security;
        thread::spawn(move || {
            run_agent_loop(
                &client,
                &root,
                file_access_security,
                history,
                &tx,
                &confirm_rx,
            );
        });
    }

    // Answers a pending WriteProposed prompt. A no-op if nothing is
    // actually waiting (confirm_tx unset, or its receiving thread
    // already gone) — sending into a channel nobody's listening to just
    // errors silently, which is fine here.
    pub fn respond_to_confirmation(&mut self, choice: ConfirmationChoice) {
        if let Some(tx) = &self.confirm_tx {
            let _ = tx.send(choice);
        }
    }

    pub fn poll_events(&mut self) -> Vec<CoreEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.rx.try_recv() {
            match &event {
                CoreEvent::AssistantChunk(text) => {
                    self.history.push(Message::Assistant {
                        content: text.clone(),
                    });
                }
                CoreEvent::ToolCall {
                    id,
                    name,
                    arguments,
                    result,
                } => {
                    self.history.push(Message::ToolCalls {
                        calls: vec![ToolCall {
                            id: id.clone(),
                            name: name.clone(),
                            arguments: arguments.clone(),
                        }],
                    });
                    let content = match result {
                        Ok(output) => output.clone(),
                        Err(error) => error.clone(),
                    };
                    self.history.push(Message::ToolResult {
                        tool_call_id: id.clone(),
                        name: name.clone(),
                        content,
                    });
                }
                // Not itself a tool result — history only gains a
                // ToolCalls/ToolResult pair once the eventual
                // CoreEvent::ToolCall (outcome: applied or declined)
                // arrives, same as every other tool call.
                CoreEvent::WriteProposed { .. } => {}
                CoreEvent::Error(_) => {}
            }
            events.push(event);
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    // Records every call it receives so tests can assert on exactly what
    // history Core threaded through, without any real network I/O.
    struct RecordingClient {
        calls: Mutex<Vec<Vec<Message>>>,
    }

    impl RecordingClient {
        fn new() -> Self {
            RecordingClient {
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    impl LlmClient for RecordingClient {
        fn send(
            &self,
            messages: &[Message],
            _tools: &[ToolDefinition],
        ) -> Result<LlmResponse, ChatError> {
            let mut calls = self.calls.lock().unwrap();
            calls.push(messages.to_vec());
            let reply_number = calls.len();
            Ok(LlmResponse::Text(format!("reply {reply_number}")))
        }
    }

    // poll_events() is non-blocking, so it can race a reply that hasn't
    // arrived yet from the spawned thread. Same pattern as the
    // tests/real_*_smoke_test.rs integration tests.
    fn poll_until_nonempty(core: &mut Core, timeout: Duration) -> Vec<CoreEvent> {
        let deadline = Instant::now() + timeout;
        loop {
            let events = core.poll_events();
            if !events.is_empty() {
                return events;
            }
            if Instant::now() >= deadline {
                panic!("timed out waiting for a CoreEvent");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn history_accumulates_across_multiple_submit_user_message_calls() {
        let recorder = Arc::new(RecordingClient::new());
        let client: Arc<dyn LlmClient + Send + Sync> = recorder.clone();
        let mut core = Core::with_client(client);

        core.submit_user_message("first".to_string());
        poll_until_nonempty(&mut core, Duration::from_secs(1));

        core.submit_user_message("second".to_string());
        poll_until_nonempty(&mut core, Duration::from_secs(1));

        let calls = recorder.calls.lock().unwrap();
        assert_eq!(
            calls[0],
            vec![Message::User {
                content: "first".to_string()
            }]
        );
        assert_eq!(
            calls[1],
            vec![
                Message::User {
                    content: "first".to_string()
                },
                Message::Assistant {
                    content: "reply 1".to_string()
                },
                Message::User {
                    content: "second".to_string()
                },
            ]
        );
    }

    // Hand-rolled instead of a tempfile dev-dependency — same reasoning
    // as sandbox_path.rs's/tools.rs's identical fixture, duplicated
    // rather than shared per this codebase's existing convention.
    struct TempDir(PathBuf);

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    impl TempDir {
        fn new() -> Self {
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let mut path = std::env::temp_dir();
            path.push(format!("emed-code-core-test-{}-{id}", std::process::id()));
            std::fs::create_dir(&path).unwrap();
            TempDir(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    // Core's real constructors (new/with_client) always default root to
    // the real current_dir() — tests need a specific tempdir instead, so
    // this builds one directly via the struct literal (same module tree,
    // so Core's private fields are reachable here).
    fn core_with_root(client: Arc<dyn LlmClient + Send + Sync>, root: PathBuf) -> Core {
        let (tx, rx) = mpsc::channel();
        Core {
            client,
            root,
            settings: Settings::default(),
            history: Vec::new(),
            tx,
            rx,
            confirm_tx: None,
        }
    }

    // Replays a fixed, finite sequence of scripted responses — once
    // exhausted, keeps repeating the last one (so a client scripted with
    // a single ToolCalls response can simulate "never stops calling
    // tools" for the cap test below).
    struct ScriptedClient {
        responses: Vec<Result<LlmResponse, ChatError>>,
        call_count: AtomicUsize,
        // Records every call's history, same purpose as RecordingClient's
        // identical field — lets tests assert on exactly what history a
        // scripted tool-call round-trip produced, not just its replies.
        calls: Mutex<Vec<Vec<Message>>>,
    }

    impl ScriptedClient {
        fn new(responses: Vec<Result<LlmResponse, ChatError>>) -> Self {
            ScriptedClient {
                responses,
                call_count: AtomicUsize::new(0),
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    impl LlmClient for ScriptedClient {
        fn send(
            &self,
            messages: &[Message],
            _tools: &[ToolDefinition],
        ) -> Result<LlmResponse, ChatError> {
            self.calls.lock().unwrap().push(messages.to_vec());
            let index = self.call_count.fetch_add(1, Ordering::SeqCst);
            let index = index.min(self.responses.len() - 1);
            self.responses[index].clone()
        }
    }

    // Unlike poll_until_nonempty, waits for at least `count` events
    // across possibly multiple poll_events() calls — the agent loop can
    // emit several CoreEvents from one submit_user_message call, and a
    // single poll might race ahead of all of them landing.
    fn poll_until_at_least(core: &mut Core, count: usize, timeout: Duration) -> Vec<CoreEvent> {
        let deadline = Instant::now() + timeout;
        let mut events = Vec::new();
        while events.len() < count {
            events.extend(core.poll_events());
            if events.len() >= count {
                break;
            }
            if Instant::now() >= deadline {
                panic!(
                    "timed out waiting for {count} CoreEvents, only got {}",
                    events.len()
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        events
    }

    #[test]
    fn agent_loop_dispatches_a_tool_call_then_returns_the_final_answer() {
        let root = TempDir::new();
        std::fs::write(root.path().join("notes.txt"), "hello").unwrap();

        let tool_call = ToolCall {
            id: "call_1".to_string(),
            name: "read_file".to_string(),
            arguments: r#"{"path": "notes.txt"}"#.to_string(),
        };

        let client = Arc::new(ScriptedClient::new(vec![
            Ok(LlmResponse::ToolCalls(vec![tool_call])),
            Ok(LlmResponse::Text("done reading".to_string())),
        ]));

        let mut core = core_with_root(client, root.path().to_path_buf());
        core.submit_user_message("read notes.txt".to_string());

        let events = poll_until_at_least(&mut core, 2, Duration::from_secs(1));

        assert_eq!(
            events[0],
            CoreEvent::ToolCall {
                id: "call_1".to_string(),
                name: "read_file".to_string(),
                arguments: r#"{"path": "notes.txt"}"#.to_string(),
                result: Ok("hello".to_string()),
            }
        );
        assert_eq!(
            events[1],
            CoreEvent::AssistantChunk("done reading".to_string())
        );
    }

    // A tool result must carry its own tool's name — not just the
    // tool_call_id — so a provider whose wire format correlates results
    // by name rather than id (Ollama) has what it needs without scanning
    // back through history to find the matching ToolCalls entry.
    #[test]
    fn agent_loop_tags_a_tool_result_with_the_tool_name() {
        let root = TempDir::new();
        std::fs::write(root.path().join("notes.txt"), "hello").unwrap();

        let tool_call = ToolCall {
            id: "call_1".to_string(),
            name: "read_file".to_string(),
            arguments: r#"{"path": "notes.txt"}"#.to_string(),
        };

        let scripted = Arc::new(ScriptedClient::new(vec![
            Ok(LlmResponse::ToolCalls(vec![tool_call])),
            Ok(LlmResponse::Text("done reading".to_string())),
        ]));
        let client: Arc<dyn LlmClient + Send + Sync> = scripted.clone();

        let mut core = core_with_root(client, root.path().to_path_buf());
        core.submit_user_message("read notes.txt".to_string());

        poll_until_at_least(&mut core, 2, Duration::from_secs(1));

        let calls = scripted.calls.lock().unwrap();
        let second_call_history = &calls[1];
        assert!(
            second_call_history.contains(&Message::ToolResult {
                tool_call_id: "call_1".to_string(),
                name: "read_file".to_string(),
                content: "hello".to_string(),
            }),
            "expected the second LLM call's history to include a ToolResult tagged with the tool's name, got {second_call_history:?}"
        );
    }

    // The point of this fix: a failed dispatch (e.g. a missing file)
    // must surface as Err in the CoreEvent, not get silently flattened
    // into a success-shaped string — that's what let the TUI tell
    // "it worked" apart from "it didn't" without needing the payload.
    #[test]
    fn agent_loop_reports_a_failed_tool_call_as_an_error_result() {
        let root = TempDir::new();
        let tool_call = ToolCall {
            id: "call_1".to_string(),
            name: "read_file".to_string(),
            arguments: r#"{"path": "does-not-exist.txt"}"#.to_string(),
        };

        let client = Arc::new(ScriptedClient::new(vec![
            Ok(LlmResponse::ToolCalls(vec![tool_call])),
            Ok(LlmResponse::Text("done".to_string())),
        ]));

        let mut core = core_with_root(client, root.path().to_path_buf());
        core.submit_user_message("read a missing file".to_string());

        let events = poll_until_at_least(&mut core, 2, Duration::from_secs(1));

        match &events[0] {
            CoreEvent::ToolCall { result, .. } => {
                assert!(result.is_err(), "expected an error result, got {result:?}");
            }
            other => panic!("expected a ToolCall event, got {other:?}"),
        }
    }

    #[test]
    fn agent_loop_stops_at_the_tool_call_cap_with_a_clear_error() {
        let root = TempDir::new();
        let tool_call = ToolCall {
            id: "call_x".to_string(),
            name: "list_files".to_string(),
            arguments: r#"{"path": "."}"#.to_string(),
        };
        // A single scripted response, always ToolCalls, never Text —
        // proves the loop terminates via the cap instead of hanging.
        let client = Arc::new(ScriptedClient::new(vec![Ok(LlmResponse::ToolCalls(vec![
            tool_call,
        ]))]));

        let mut core = core_with_root(client, root.path().to_path_buf());
        core.submit_user_message("loop forever".to_string());

        let events = poll_until_at_least(&mut core, 41, Duration::from_secs(5));

        let tool_call_events = events
            .iter()
            .filter(|event| matches!(event, CoreEvent::ToolCall { .. }))
            .count();
        assert_eq!(tool_call_events, 40);
        assert!(matches!(events.last(), Some(CoreEvent::Error(_))));
    }

    // Direct coverage of the loop's plain-text path, replacing what
    // to_core_event's own test used to check before that function was
    // removed (its ToolCalls-is-unsupported branch became factually
    // wrong once the loop actually dispatches tool calls).
    #[test]
    fn agent_loop_returns_assistant_chunk_for_a_plain_text_reply() {
        let root = TempDir::new();
        let client = Arc::new(ScriptedClient::new(vec![Ok(LlmResponse::Text(
            "hi there".to_string(),
        ))]));

        let mut core = core_with_root(client, root.path().to_path_buf());
        core.submit_user_message("hello".to_string());

        let events = poll_until_at_least(&mut core, 1, Duration::from_secs(1));

        assert_eq!(events[0], CoreEvent::AssistantChunk("hi there".to_string()));
    }

    #[test]
    fn agent_loop_returns_an_error_event_when_send_fails() {
        let root = TempDir::new();
        let client = Arc::new(ScriptedClient::new(vec![Err(ChatError::Connection(
            "connection refused".to_string(),
        ))]));

        let mut core = core_with_root(client, root.path().to_path_buf());
        core.submit_user_message("hello".to_string());

        let events = poll_until_at_least(&mut core, 1, Duration::from_secs(1));

        assert!(matches!(events[0], CoreEvent::Error(_)));
    }

    // The real cross-thread blocking. Deliberately observes the file
    // *doesn't* exist yet right after WriteProposed arrives — proving
    // the background thread is still paused, not just that
    // respond_to_confirmation eventually produces the right
    // outcome regardless of timing.
    #[test]
    fn agent_loop_blocks_on_write_confirmation_then_applies_it() {
        let root = TempDir::new();
        let tool_call = ToolCall {
            id: "call_1".to_string(),
            name: "write_file".to_string(),
            arguments: r#"{"path": "new.txt", "content": "hello"}"#.to_string(),
        };
        let client = Arc::new(ScriptedClient::new(vec![
            Ok(LlmResponse::ToolCalls(vec![tool_call])),
            Ok(LlmResponse::Text("wrote it".to_string())),
        ]));

        let mut core = core_with_root(client, root.path().to_path_buf());
        core.submit_user_message("write a file".to_string());

        let events = poll_until_at_least(&mut core, 1, Duration::from_secs(1));
        match &events[0] {
            CoreEvent::WriteProposed { path, .. } => assert_eq!(path, "new.txt"),
            other => panic!("expected WriteProposed, got {other:?}"),
        }
        assert!(!root.path().join("new.txt").exists());

        core.respond_to_confirmation(ConfirmationChoice::Apply);

        let events = poll_until_at_least(&mut core, 2, Duration::from_secs(1));
        assert!(matches!(
            events[0],
            CoreEvent::ToolCall { result: Ok(_), .. }
        ));
        assert_eq!(events[1], CoreEvent::AssistantChunk("wrote it".to_string()));
        assert_eq!(
            std::fs::read_to_string(root.path().join("new.txt")).unwrap(),
            "hello"
        );
    }

    #[test]
    fn agent_loop_produces_a_declined_tool_result_when_write_is_declined() {
        let root = TempDir::new();
        let tool_call = ToolCall {
            id: "call_1".to_string(),
            name: "write_file".to_string(),
            arguments: r#"{"path": "new.txt", "content": "hello"}"#.to_string(),
        };
        let client = Arc::new(ScriptedClient::new(vec![
            Ok(LlmResponse::ToolCalls(vec![tool_call])),
            Ok(LlmResponse::Text("ok, not writing".to_string())),
        ]));

        let mut core = core_with_root(client, root.path().to_path_buf());
        core.submit_user_message("write a file".to_string());

        poll_until_at_least(&mut core, 1, Duration::from_secs(1));
        core.respond_to_confirmation(ConfirmationChoice::Decline);

        let events = poll_until_at_least(&mut core, 2, Duration::from_secs(1));
        match &events[0] {
            CoreEvent::ToolCall {
                result: Err(message),
                ..
            } => assert!(message.contains("declined")),
            other => panic!("expected a declined ToolCall error, got {other:?}"),
        }
        assert!(!root.path().join("new.txt").exists());
    }
}
