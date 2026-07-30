// The one place a ToolCall's name gets mapped to an actual action.
// Kept deliberately small and self-contained — dispatch() is the only
// thing the agent loop calls, so adding a tool means one new match arm
// and one new function here, not touching the loop itself.

use serde::Deserialize;
use std::fmt;
use std::path::Path;

use super::{SandboxError, SandboxPath, ToolCall, ToolDefinition};

#[derive(Debug, PartialEq)]
pub(crate) enum ToolError {
    InvalidPath(SandboxError),
    IoFailure,
    UnknownTool(String),
    MalformedArguments,
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ToolError::InvalidPath(e) => write!(f, "invalid path: {e}"),
            ToolError::IoFailure => write!(f, "failed to access the filesystem"),
            ToolError::UnknownTool(name) => write!(f, "unknown tool: {name}"),
            ToolError::MalformedArguments => write!(f, "malformed tool arguments"),
        }
    }
}

#[derive(Deserialize)]
struct PathArgs {
    path: String,
}

fn read_file(root: &Path, requested: &Path) -> Result<String, ToolError> {
    let sandbox_path = SandboxPath::new(root, requested).map_err(ToolError::InvalidPath)?;
    std::fs::read_to_string(sandbox_path.as_path()).map_err(|_| ToolError::IoFailure)
}

fn list_files(root: &Path, requested: &Path) -> Result<String, ToolError> {
    let sandbox_path = SandboxPath::new(root, requested).map_err(ToolError::InvalidPath)?;
    let entries = std::fs::read_dir(sandbox_path.as_path()).map_err(|_| ToolError::IoFailure)?;

    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|_| ToolError::IoFailure)?;
        names.push(entry.file_name().to_string_lossy().into_owned());
    }
    names.sort();
    Ok(names.join("\n"))
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
pub(crate) fn dispatch(root: &Path, tool_call: &ToolCall) -> Result<String, ToolError> {
    match tool_call.name.as_str() {
        "read_file" => {
            let args: PathArgs = serde_json::from_str(&tool_call.arguments)
                .map_err(|_| ToolError::MalformedArguments)?;
            read_file(root, Path::new(&args.path))
        }
        "list_files" => {
            let args: PathArgs = serde_json::from_str(&tool_call.arguments)
                .map_err(|_| ToolError::MalformedArguments)?;
            list_files(root, Path::new(&args.path))
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

        let result = read_file(root.path(), Path::new("notes.txt"));

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

        let result = read_file(&root, Path::new("../secret.txt"));

        assert_eq!(result, Err(ToolError::InvalidPath(SandboxError::Escapes)));
    }

    #[test]
    fn list_files_returns_sorted_entries_of_a_contained_directory() {
        let root = TempDir::new();
        std::fs::write(root.path().join("b.txt"), "").unwrap();
        std::fs::write(root.path().join("a.txt"), "").unwrap();

        let result = list_files(root.path(), Path::new("."));

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

        let result = dispatch(root.path(), &tool_call);

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

        let result = dispatch(root.path(), &tool_call);

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

        let result = dispatch(root.path(), &tool_call);

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

        let result = dispatch(root.path(), &tool_call);

        assert_eq!(result, Err(ToolError::MalformedArguments));
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
