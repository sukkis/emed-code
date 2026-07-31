// The one place a ToolCall's name gets mapped to an actual action.
// Kept deliberately small and self-contained — dispatch() is the only
// thing the agent loop calls, so adding a tool means one new match arm
// and one new function here, not touching the loop itself.

use serde::Deserialize;
use std::fmt;
use std::path::Path;
use std::sync::mpsc;

use super::{
    ConfirmationChoice, CoreEvent, DiffLine, FileAccessSecurity, SandboxError, SandboxPath,
    ToolCall, ToolDefinition,
};

#[derive(Debug, PartialEq)]
pub(crate) enum ToolError {
    InvalidPath(SandboxError),
    IoFailure,
    UnknownTool(String),
    MalformedArguments,
    // No need to hide anything in the message the way SandboxError does
    // for a rejected symlink's target — the LLM already knows exactly
    // which path it requested.
    AccessDenied,
    // The user declined a write_file confirmation prompt. Not a
    // technical failure — flows through the same Result<String,
    // ToolError> pipeline as every other outcome so nothing downstream
    // (history, CoreEvent construction) needs a separate code path for
    // it; the TUI's "error: " prefix reads fine for "this didn't
    // happen because you said no."
    WriteDeclined,
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ToolError::InvalidPath(e) => write!(f, "invalid path: {e}"),
            ToolError::IoFailure => write!(f, "failed to access the filesystem"),
            ToolError::UnknownTool(name) => write!(f, "unknown tool: {name}"),
            ToolError::MalformedArguments => write!(f, "malformed tool arguments"),
            ToolError::AccessDenied => {
                write!(f, "access to this path is restricted by security policy")
            }
            ToolError::WriteDeclined => write!(f, "user declined this write"),
        }
    }
}

#[derive(Deserialize)]
struct PathArgs {
    path: String,
}

#[derive(Deserialize)]
struct WriteArgs {
    path: String,
    content: String,
}

// A small, explicitly non-exhaustive starter list of sensitive-by-
// convention paths — easy to extend later, not an attempt to be
// exhaustive now. Matches against the canonicalized, root-relative path
// (SandboxPath::relative_path), not the raw requested string, so a
// symlink with an innocuous name can't bypass this by pointing at a
// blocked file.
fn is_content_restricted(relative_path: &Path) -> bool {
    let components: Vec<&str> = relative_path
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect();

    if components.contains(&".ssh") {
        return true;
    }

    if components.len() >= 2 && components[components.len() - 2] == ".git" {
        return components[components.len() - 1] == "config";
    }

    match relative_path.file_name().and_then(|name| name.to_str()) {
        Some(name) => {
            name == ".env"
                || name.starts_with(".env.")
                || name.ends_with(".pem")
                || name.ends_with(".key")
        }
        None => false,
    }
}

fn read_file(
    root: &Path,
    requested: &Path,
    file_access_security: FileAccessSecurity,
) -> Result<String, ToolError> {
    let sandbox_path = SandboxPath::new(root, requested).map_err(ToolError::InvalidPath)?;
    if file_access_security == FileAccessSecurity::Strict
        && is_content_restricted(sandbox_path.relative_path())
    {
        return Err(ToolError::AccessDenied);
    }
    std::fs::read_to_string(sandbox_path.as_path()).map_err(|_| ToolError::IoFailure)
}

fn list_files(
    root: &Path,
    requested: &Path,
    file_access_security: FileAccessSecurity,
) -> Result<String, ToolError> {
    let sandbox_path = SandboxPath::new(root, requested).map_err(ToolError::InvalidPath)?;
    if file_access_security == FileAccessSecurity::Strict
        && is_content_restricted(sandbox_path.relative_path())
    {
        return Err(ToolError::AccessDenied);
    }
    let entries = std::fs::read_dir(sandbox_path.as_path()).map_err(|_| ToolError::IoFailure)?;

    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|_| ToolError::IoFailure)?;
        names.push(entry.file_name().to_string_lossy().into_owned());
    }
    names.sort();
    Ok(names.join("\n"))
}

// pub(crate): called both by write_file_with_confirmation below (after
// a user has already approved the write) and, eventually, tests. Same
// sandboxing + blocklist checks as read_file/list_files, plus the
// actual write; returns () rather than echoing content back, since
// there's nothing useful to hand back beyond success/failure itself.
pub(crate) fn write_file(
    root: &Path,
    requested: &Path,
    content: &str,
    file_access_security: FileAccessSecurity,
) -> Result<(), ToolError> {
    let sandbox_path =
        SandboxPath::new_for_write(root, requested).map_err(ToolError::InvalidPath)?;
    if file_access_security == FileAccessSecurity::Strict
        && is_content_restricted(sandbox_path.relative_path())
    {
        return Err(ToolError::AccessDenied);
    }
    std::fs::write(sandbox_path.as_path(), content).map_err(|_| ToolError::IoFailure)
}

// The confirmation-gated entry point the agent loop calls for a
// "write_file" tool call — exercised end-to-end by core.rs's
// agent-loop tests via a scripted client. Validates first (so a
// forbidden path never even shows a confirmation prompt), then
// proposes the change and blocks for an answer, then delegates the
// actual write back to write_file above — a deliberate, cheap
// redundant re-validation in exchange for one
// source of truth on sandboxing/blocklist logic, rather than
// duplicating it inline here.
pub(crate) fn write_file_with_confirmation(
    root: &Path,
    file_access_security: FileAccessSecurity,
    tool_call: &ToolCall,
    tx: &mpsc::Sender<CoreEvent>,
    confirm_rx: &mpsc::Receiver<ConfirmationChoice>,
) -> Result<String, ToolError> {
    let args: WriteArgs =
        serde_json::from_str(&tool_call.arguments).map_err(|_| ToolError::MalformedArguments)?;
    let requested = Path::new(&args.path);

    let sandbox_path =
        SandboxPath::new_for_write(root, requested).map_err(ToolError::InvalidPath)?;
    if file_access_security == FileAccessSecurity::Strict
        && is_content_restricted(sandbox_path.relative_path())
    {
        return Err(ToolError::AccessDenied);
    }

    let old_content = std::fs::read_to_string(sandbox_path.as_path()).unwrap_or_default();
    let diff = super::generate_diff(&old_content, &args.content);

    let _ = tx.send(CoreEvent::WriteProposed {
        path: args.path.clone(),
        diff,
    });

    // A dropped/errored receive (e.g. the app exiting mid-confirmation)
    // fails safe to Decline — never a silent apply just because no
    // real answer arrived.
    match confirm_rx.recv().unwrap_or(ConfirmationChoice::Decline) {
        ConfirmationChoice::Decline => Err(ToolError::WriteDeclined),
        ConfirmationChoice::Apply => {
            write_file(root, requested, &args.content, file_access_security)
                .map(|()| format!("wrote {}", args.path))
        }
    }
}

// The list of tools actually advertised to a provider. Kept next to
// dispatch()'s match arms (not off in core.rs) specifically so the two
// can't drift apart silently — see the names-match test below.
pub(crate) fn tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "read_file".to_string(),
            description: "Read the contents of a file within the project directory.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file, relative to the project root."
                    }
                },
                "required": ["path"]
            }),
        },
        ToolDefinition {
            name: "list_files".to_string(),
            description: "List files and directories within a directory in the project."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the directory, relative to the project root. Use \".\" for the project root."
                    }
                },
                "required": ["path"]
            }),
        },
    ]
}

// Matches on the tool name first, then parses that specific tool's
// arguments — not the other way around — so an unrecognized name never
// has to care about argument shape at all, and a third tool means one
// new match arm plus one new function, nothing else.
pub(crate) fn dispatch(
    root: &Path,
    file_access_security: FileAccessSecurity,
    tool_call: &ToolCall,
) -> Result<String, ToolError> {
    match tool_call.name.as_str() {
        "read_file" => {
            let args: PathArgs = serde_json::from_str(&tool_call.arguments)
                .map_err(|_| ToolError::MalformedArguments)?;
            read_file(root, Path::new(&args.path), file_access_security)
        }
        "list_files" => {
            let args: PathArgs = serde_json::from_str(&tool_call.arguments)
                .map_err(|_| ToolError::MalformedArguments)?;
            list_files(root, Path::new(&args.path), file_access_security)
        }
        other => Err(ToolError::UnknownTool(other.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    // Same hand-rolled fixture as sandbox_path.rs's tests — duplicated
    // rather than shared, matching this codebase's existing convention
    // (e.g. poll_until_nonempty is duplicated across test modules too).
    struct TempDir(PathBuf);

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    impl TempDir {
        fn new() -> Self {
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let mut path = std::env::temp_dir();
            path.push(format!("emed-code-tools-test-{}-{id}", std::process::id()));
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

    #[test]
    fn read_file_returns_contents_of_a_contained_file() {
        let root = TempDir::new();
        std::fs::write(root.path().join("notes.txt"), "hello there").unwrap();

        let result = read_file(
            root.path(),
            Path::new("notes.txt"),
            FileAccessSecurity::Strict,
        );

        assert_eq!(result, Ok("hello there".to_string()));
    }

    // The point of this test: a path escaping the sandbox must come back
    // as ToolError::InvalidPath (wrapping SandboxError), never a raw
    // std::io::Error from an unvalidated filesystem call that could
    // embed the real outside-sandbox path in its message.
    #[test]
    fn read_file_rejects_a_path_escaping_the_sandbox() {
        let outer = TempDir::new();
        let root = outer.path().join("project");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(outer.path().join("secret.txt"), "secret").unwrap();

        let result = read_file(
            &root,
            Path::new("../secret.txt"),
            FileAccessSecurity::Strict,
        );

        assert_eq!(result, Err(ToolError::InvalidPath(SandboxError::Escapes)));
    }

    #[test]
    fn list_files_returns_sorted_entries_of_a_contained_directory() {
        let root = TempDir::new();
        std::fs::write(root.path().join("b.txt"), "").unwrap();
        std::fs::write(root.path().join("a.txt"), "").unwrap();

        let result = list_files(root.path(), Path::new("."), FileAccessSecurity::Strict);

        assert_eq!(result, Ok("a.txt\nb.txt".to_string()));
    }

    #[test]
    fn dispatch_routes_read_file_calls() {
        let root = TempDir::new();
        std::fs::write(root.path().join("notes.txt"), "hi").unwrap();
        let tool_call = ToolCall {
            id: "call_1".to_string(),
            name: "read_file".to_string(),
            arguments: r#"{"path": "notes.txt"}"#.to_string(),
        };

        let result = dispatch(root.path(), FileAccessSecurity::Strict, &tool_call);

        assert_eq!(result, Ok("hi".to_string()));
    }

    #[test]
    fn dispatch_routes_list_files_calls() {
        let root = TempDir::new();
        std::fs::write(root.path().join("a.txt"), "").unwrap();
        let tool_call = ToolCall {
            id: "call_1".to_string(),
            name: "list_files".to_string(),
            arguments: r#"{"path": "."}"#.to_string(),
        };

        let result = dispatch(root.path(), FileAccessSecurity::Strict, &tool_call);

        assert_eq!(result, Ok("a.txt".to_string()));
    }

    #[test]
    fn dispatch_returns_an_error_for_an_unrecognized_tool_name() {
        let root = TempDir::new();
        let tool_call = ToolCall {
            id: "call_1".to_string(),
            name: "delete_everything".to_string(),
            arguments: "{}".to_string(),
        };

        let result = dispatch(root.path(), FileAccessSecurity::Strict, &tool_call);

        assert_eq!(
            result,
            Err(ToolError::UnknownTool("delete_everything".to_string()))
        );
    }

    #[test]
    fn dispatch_returns_an_error_for_malformed_arguments() {
        let root = TempDir::new();
        let tool_call = ToolCall {
            id: "call_1".to_string(),
            name: "read_file".to_string(),
            arguments: "not valid json".to_string(),
        };

        let result = dispatch(root.path(), FileAccessSecurity::Strict, &tool_call);

        assert_eq!(result, Err(ToolError::MalformedArguments));
    }

    #[test]
    fn read_file_blocks_a_dot_env_file_in_strict_mode() {
        let root = TempDir::new();
        std::fs::write(root.path().join(".env"), "SECRET=1").unwrap();

        let result = read_file(root.path(), Path::new(".env"), FileAccessSecurity::Strict);

        assert_eq!(result, Err(ToolError::AccessDenied));
    }

    #[test]
    fn read_file_allows_a_dot_env_file_in_loose_mode() {
        let root = TempDir::new();
        std::fs::write(root.path().join(".env"), "SECRET=1").unwrap();

        let result = read_file(root.path(), Path::new(".env"), FileAccessSecurity::Loose);

        assert_eq!(result, Ok("SECRET=1".to_string()));
    }

    #[test]
    fn read_file_blocks_a_file_inside_a_dot_ssh_directory() {
        let root = TempDir::new();
        std::fs::create_dir(root.path().join(".ssh")).unwrap();
        std::fs::write(root.path().join(".ssh").join("id_rsa"), "private").unwrap();

        let result = read_file(
            root.path(),
            Path::new(".ssh/id_rsa"),
            FileAccessSecurity::Strict,
        );

        assert_eq!(result, Err(ToolError::AccessDenied));
    }

    #[test]
    fn read_file_blocks_git_config() {
        let root = TempDir::new();
        std::fs::create_dir(root.path().join(".git")).unwrap();
        std::fs::write(root.path().join(".git").join("config"), "[core]").unwrap();

        let result = read_file(
            root.path(),
            Path::new(".git/config"),
            FileAccessSecurity::Strict,
        );

        assert_eq!(result, Err(ToolError::AccessDenied));
    }

    #[test]
    fn read_file_blocks_a_pem_file() {
        let root = TempDir::new();
        std::fs::write(root.path().join("key.pem"), "-----BEGIN").unwrap();

        let result = read_file(
            root.path(),
            Path::new("key.pem"),
            FileAccessSecurity::Strict,
        );

        assert_eq!(result, Err(ToolError::AccessDenied));
    }

    // The concrete case that justified checking the canonicalized path
    // rather than the raw requested string: an innocuously-named
    // symlink pointing at a blocked file must still be blocked.
    #[test]
    #[cfg(unix)]
    fn read_file_blocks_a_symlink_that_resolves_to_a_blocked_file() {
        let root = TempDir::new();
        std::fs::write(root.path().join(".env"), "SECRET=1").unwrap();
        std::os::unix::fs::symlink(root.path().join(".env"), root.path().join("innocuous.txt"))
            .unwrap();

        let result = read_file(
            root.path(),
            Path::new("innocuous.txt"),
            FileAccessSecurity::Strict,
        );

        assert_eq!(result, Err(ToolError::AccessDenied));
    }

    #[test]
    fn list_files_blocks_enumerating_into_a_dot_ssh_directory() {
        let root = TempDir::new();
        std::fs::create_dir(root.path().join(".ssh")).unwrap();
        std::fs::write(root.path().join(".ssh").join("id_rsa"), "private").unwrap();

        let result = list_files(root.path(), Path::new(".ssh"), FileAccessSecurity::Strict);

        assert_eq!(result, Err(ToolError::AccessDenied));
    }

    // A blocked entry's name still shows up in its *parent* listing —
    // existence isn't hidden, only enumerating into it / reading it is
    // refused.
    #[test]
    fn list_files_still_shows_a_blocked_entrys_name_in_its_parent_listing() {
        let root = TempDir::new();
        std::fs::write(root.path().join(".env"), "SECRET=1").unwrap();
        std::fs::write(root.path().join("notes.txt"), "hi").unwrap();

        let result = list_files(root.path(), Path::new("."), FileAccessSecurity::Strict);

        assert_eq!(result, Ok(".env\nnotes.txt".to_string()));
    }

    #[test]
    fn dispatch_returns_access_denied_for_a_blocked_tool_call() {
        let root = TempDir::new();
        std::fs::write(root.path().join(".env"), "SECRET=1").unwrap();
        let tool_call = ToolCall {
            id: "call_1".to_string(),
            name: "read_file".to_string(),
            arguments: r#"{"path": ".env"}"#.to_string(),
        };

        let result = dispatch(root.path(), FileAccessSecurity::Strict, &tool_call);

        assert_eq!(result, Err(ToolError::AccessDenied));
    }

    // write_file isn't added to tool_definitions()/dispatch — it's
    // routed around dispatch by name (see write_file_with_confirmation
    // below), so it's tested directly here instead.
    #[test]
    fn write_file_creates_a_new_file_with_the_given_content() {
        let root = TempDir::new();

        let result = write_file(
            root.path(),
            Path::new("new.txt"),
            "hello there",
            FileAccessSecurity::Strict,
        );

        assert_eq!(result, Ok(()));
        assert_eq!(
            std::fs::read_to_string(root.path().join("new.txt")).unwrap(),
            "hello there"
        );
    }

    #[test]
    fn write_file_overwrites_an_existing_file() {
        let root = TempDir::new();
        std::fs::write(root.path().join("existing.txt"), "old content").unwrap();

        let result = write_file(
            root.path(),
            Path::new("existing.txt"),
            "new content",
            FileAccessSecurity::Strict,
        );

        assert_eq!(result, Ok(()));
        assert_eq!(
            std::fs::read_to_string(root.path().join("existing.txt")).unwrap(),
            "new content"
        );
    }

    #[test]
    fn write_file_rejects_a_path_escaping_the_sandbox() {
        let outer = TempDir::new();
        let root = outer.path().join("project");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(outer.path().join("secret.txt"), "secret").unwrap();

        let result = write_file(
            &root,
            Path::new("../secret.txt"),
            "pwned",
            FileAccessSecurity::Strict,
        );

        assert_eq!(result, Err(ToolError::InvalidPath(SandboxError::Escapes)));
        assert_eq!(
            std::fs::read_to_string(outer.path().join("secret.txt")).unwrap(),
            "secret"
        );
    }

    #[test]
    fn write_file_rejects_a_missing_parent_directory() {
        let root = TempDir::new();

        let result = write_file(
            root.path(),
            Path::new("no_such_dir/new.txt"),
            "content",
            FileAccessSecurity::Strict,
        );

        assert_eq!(result, Err(ToolError::InvalidPath(SandboxError::NotFound)));
    }

    #[test]
    fn write_file_blocks_a_dot_env_file_in_strict_mode() {
        let root = TempDir::new();

        let result = write_file(
            root.path(),
            Path::new(".env"),
            "SECRET=evil",
            FileAccessSecurity::Strict,
        );

        assert_eq!(result, Err(ToolError::AccessDenied));
        assert!(!root.path().join(".env").exists());
    }

    #[test]
    fn write_file_allows_a_dot_env_file_in_loose_mode() {
        let root = TempDir::new();

        let result = write_file(
            root.path(),
            Path::new(".env"),
            "SECRET=1",
            FileAccessSecurity::Loose,
        );

        assert_eq!(result, Ok(()));
        assert_eq!(
            std::fs::read_to_string(root.path().join(".env")).unwrap(),
            "SECRET=1"
        );
    }

    // Tests the confirmation-gated write path. confirm_tx is
    // pre-loaded with a choice before calling, since mpsc::channel is
    // unbounded — the function's own confirm_rx.recv() then picks it up
    // immediately rather than actually blocking, keeping these tests
    // synchronous (the real cross-thread blocking is exercised by
    // core.rs's agent-loop-level test instead).
    #[test]
    fn write_file_with_confirmation_applies_the_write_when_confirmed() {
        let root = TempDir::new();
        let (tx, rx) = mpsc::channel();
        let (confirm_tx, confirm_rx) = mpsc::channel();
        confirm_tx.send(ConfirmationChoice::Apply).unwrap();
        let tool_call = ToolCall {
            id: "call_1".to_string(),
            name: "write_file".to_string(),
            arguments: r#"{"path": "new.txt", "content": "hello"}"#.to_string(),
        };

        let result = write_file_with_confirmation(
            root.path(),
            FileAccessSecurity::Strict,
            &tool_call,
            &tx,
            &confirm_rx,
        );

        assert_eq!(result, Ok("wrote new.txt".to_string()));
        assert_eq!(
            std::fs::read_to_string(root.path().join("new.txt")).unwrap(),
            "hello"
        );
        match rx.try_recv().unwrap() {
            CoreEvent::WriteProposed { path, diff } => {
                assert_eq!(path, "new.txt");
                assert_eq!(diff, vec![DiffLine::Added("hello".to_string())]);
            }
            other => panic!("expected WriteProposed, got {other:?}"),
        }
    }

    #[test]
    fn write_file_with_confirmation_declines_without_writing() {
        let root = TempDir::new();
        let (tx, _rx) = mpsc::channel();
        let (confirm_tx, confirm_rx) = mpsc::channel();
        confirm_tx.send(ConfirmationChoice::Decline).unwrap();
        let tool_call = ToolCall {
            id: "call_1".to_string(),
            name: "write_file".to_string(),
            arguments: r#"{"path": "new.txt", "content": "hello"}"#.to_string(),
        };

        let result = write_file_with_confirmation(
            root.path(),
            FileAccessSecurity::Strict,
            &tool_call,
            &tx,
            &confirm_rx,
        );

        assert_eq!(result, Err(ToolError::WriteDeclined));
        assert!(!root.path().join("new.txt").exists());
    }

    // The point of this test: a forbidden path must be rejected before
    // ever asking for confirmation — confirm_rx never receives anything
    // here, so if the implementation asked for confirmation first, this
    // test would hang (recv() blocks forever with no timeout) rather
    // than return the wrong answer.
    #[test]
    fn write_file_with_confirmation_rejects_a_blocked_path_without_asking() {
        let root = TempDir::new();
        let (tx, rx) = mpsc::channel();
        let (_confirm_tx, confirm_rx) = mpsc::channel();
        let tool_call = ToolCall {
            id: "call_1".to_string(),
            name: "write_file".to_string(),
            arguments: r#"{"path": ".env", "content": "SECRET=evil"}"#.to_string(),
        };

        let result = write_file_with_confirmation(
            root.path(),
            FileAccessSecurity::Strict,
            &tool_call,
            &tx,
            &confirm_rx,
        );

        assert_eq!(result, Err(ToolError::AccessDenied));
        assert!(!root.path().join(".env").exists());
        assert!(rx.try_recv().is_err());
    }

    // Guards against drift between what's advertised to the model and
    // what dispatch() actually recognizes — a typo in either place
    // would otherwise only surface as a confusing runtime UnknownTool
    // error against a real provider.
    #[test]
    fn tool_definitions_names_match_dispatchs_known_tool_names() {
        let definitions = tool_definitions();
        let names: Vec<&str> = definitions
            .iter()
            .map(|definition| definition.name.as_str())
            .collect();

        assert_eq!(names, vec!["read_file", "list_files"]);
    }
}
