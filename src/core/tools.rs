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
    // edit_file's `old` wasn't found anywhere in the file. Like
    // WriteDeclined, not a technical failure — an actionable outcome
    // the model should see and retry from (e.g. its `old` text doesn't
    // match verbatim), not a bug in emed-code itself.
    NoMatch,
    // edit_file's `old` matched more than once with replace_all: false.
    // Carries the match count so the error message can tell the model
    // exactly how ambiguous the match was, letting it choose between
    // narrowing `old` with more context or setting replace_all: true.
    AmbiguousMatch(usize),
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
            ToolError::NoMatch => write!(f, "the given `old` text was not found in the file"),
            ToolError::AmbiguousMatch(count) => write!(
                f,
                "the given `old` text matched {count} times; narrow it with more \
                 surrounding context, or set replace_all to change every occurrence"
            ),
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

// For create_directory: layers is_content_restricted over
// SandboxPath::plan_directory_creation's output, checking every level
// that would actually get created — not just the deepest one. A
// single check against only the final requested path would miss a
// restricted name at an intermediate level (e.g. "something.pem/real"
// — is_content_restricted's .env/.pem/.key checks only look at a
// path's own file name, so only the true leaf of a single checked path
// would ever be caught; checking each planned level closes that gap).
fn plan_and_validate_directory_creation(
    root: &Path,
    requested: &Path,
    file_access_security: FileAccessSecurity,
) -> Result<Vec<SandboxPath>, ToolError> {
    let plan =
        SandboxPath::plan_directory_creation(root, requested).map_err(ToolError::InvalidPath)?;

    if file_access_security == FileAccessSecurity::Strict {
        for level in &plan {
            if is_content_restricted(level.relative_path()) {
                return Err(ToolError::AccessDenied);
            }
        }
    }

    Ok(plan)
}

// Creates each level a validated plan calls for, shallowest first.
// create_dir rather than create_dir_all: the plan already did the
// mkdir-p-equivalent work (and the security-critical validation that
// went with it), so re-deriving "what's missing" via the recursive std
// call here would be redundant and, worse, unvalidated.
fn create_planned_directories(plan: &[SandboxPath]) -> Result<(), ToolError> {
    for level in plan {
        std::fs::create_dir(level.as_path()).map_err(|_| ToolError::IoFailure)?;
    }
    Ok(())
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

fn list_files_recursive(
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

    // relative_path() is empty for the project root itself (querying
    // "."), so this naturally starts every entry unprefixed rather than
    // needing a "." special case.
    let relative_prefix = sandbox_path.relative_path().to_string_lossy().into_owned();

    let mut entries = Vec::new();
    walk_recursive(
        sandbox_path.as_path(),
        &relative_prefix,
        file_access_security,
        &mut entries,
    )?;
    entries.sort();
    Ok(entries.join("\n"))
}

// Distinct from is_content_restricted: this is a usefulness concern
// (don't flood a listing with build artifacts), not a security control,
// so it's never gated by strict/loose — loose mode exists to allow
// reading sensitive files, not to bring back target/node_modules noise.
// Small and deliberately non-exhaustive, same framing as
// is_content_restricted's own list — easy to extend later, not an
// attempt to be complete now.
fn is_noise_directory(name: &str) -> bool {
    matches!(name, "target" | ".git" | "node_modules")
}

// Paths are joined as plain strings, not via PathBuf, so the output is
// always "/"-separated regardless of platform — this string is read by
// an LLM and fed back into read_file/write_file's own path argument,
// not used as a real filesystem path itself.
//
// file_type() (not metadata()) is what makes symlinked directories leaf
// entries for free: it reports a symlink's own type, never following
// it, so is_dir() is simply false for a symlink and it falls straight
// into the leaf branch below with no special-casing needed.
fn walk_recursive(
    absolute_dir: &Path,
    relative_prefix: &str,
    file_access_security: FileAccessSecurity,
    entries: &mut Vec<String>,
) -> Result<(), ToolError> {
    let read_dir = std::fs::read_dir(absolute_dir).map_err(|_| ToolError::IoFailure)?;

    for entry in read_dir {
        let entry = entry.map_err(|_| ToolError::IoFailure)?;
        let file_type = entry.file_type().map_err(|_| ToolError::IoFailure)?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let relative_path = if relative_prefix.is_empty() {
            name.clone()
        } else {
            format!("{relative_prefix}/{name}")
        };

        if file_type.is_dir() {
            entries.push(format!("{relative_path}/"));

            // The name still gets listed above either way — only
            // recursing into it is what these two checks suppress.
            let restricted = file_access_security == FileAccessSecurity::Strict
                && is_content_restricted(Path::new(&relative_path));
            if !restricted && !is_noise_directory(&name) {
                walk_recursive(&entry.path(), &relative_path, file_access_security, entries)?;
            }
        } else {
            entries.push(relative_path);
        }
    }

    Ok(())
}

// Pure string logic, no I/O — edit_file_with_confirmation (Step 4)
// reads old_content from disk and hands it here, same split as
// generate_windowed_diff's own pure-function step. replace_all: false
// requires old to match exactly once, mirroring Claude Code's own Edit
// tool; replace_all: true changes every occurrence and only cares that
// at least one exists.
fn apply_edit(
    old_content: &str,
    old: &str,
    new: &str,
    replace_all: bool,
) -> Result<String, ToolError> {
    let match_count = old_content.matches(old).count();

    if match_count == 0 {
        return Err(ToolError::NoMatch);
    }
    if match_count > 1 && !replace_all {
        return Err(ToolError::AmbiguousMatch(match_count));
    }
    Ok(old_content.replace(old, new))
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

// Shared by write_file_with_confirmation and edit_file_with_confirmation
// below: both end the same way once they've each computed their own
// new_content and diff — send WriteProposed, block for an answer, then
// delegate the actual write back to write_file above (a deliberate,
// cheap redundant re-validation in exchange for one source of truth on
// sandboxing/blocklist logic, rather than duplicating it inline here).
fn propose_and_apply_write(
    root: &Path,
    requested: &Path,
    file_access_security: FileAccessSecurity,
    diff: Vec<DiffLine>,
    new_content: &str,
    tx: &mpsc::Sender<CoreEvent>,
    confirm_rx: &mpsc::Receiver<ConfirmationChoice>,
) -> Result<String, ToolError> {
    // requested was built from the same string CoreEvent::WriteProposed
    // and the "wrote {path}" message need to show — no reason to also
    // thread a separate display-string argument alongside it.
    let display_path = requested.to_string_lossy();

    let _ = tx.send(CoreEvent::WriteProposed {
        path: display_path.to_string(),
        diff,
    });

    // A dropped/errored receive (e.g. the app exiting mid-confirmation)
    // fails safe to Decline — never a silent apply just because no
    // real answer arrived.
    match confirm_rx.recv().unwrap_or(ConfirmationChoice::Decline) {
        ConfirmationChoice::Decline => Err(ToolError::WriteDeclined),
        ConfirmationChoice::Apply => write_file(root, requested, new_content, file_access_security)
            .map(|()| format!("wrote {display_path}")),
    }
}

// The confirmation-gated entry point the agent loop calls for a
// "write_file" tool call — exercised end-to-end by core.rs's
// agent-loop tests via a scripted client. Validates first (so a
// forbidden path never even shows a confirmation prompt), then hands
// off to propose_and_apply_write for the shared propose/confirm/apply
// tail.
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

    propose_and_apply_write(
        root,
        requested,
        file_access_security,
        diff,
        &args.content,
        tx,
        confirm_rx,
    )
}

#[derive(Deserialize)]
struct EditArgs {
    path: String,
    old: String,
    new: String,
    // Absent in a tool call means "no", not malformed input — most
    // edits target exactly one match, so requiring the model to spell
    // this out every time would be pure noise.
    #[serde(default)]
    replace_all: bool,
}

// git diff's own default context radius — familiar to anyone who's read
// a unified diff, no reason to pick a different number.
const EDIT_DIFF_CONTEXT_LINES: usize = 3;

// The confirmation-gated entry point the agent loop calls for an
// "edit_file" tool call, mirroring write_file_with_confirmation's
// shape. Two differences from write_file: SandboxPath::new (not
// new_for_write), since a find/replace target must already exist; and
// apply_edit's own NoMatch/AmbiguousMatch errors are checked before
// ever proposing a diff, same "validate first" principle as the
// sandbox/blocklist check — there's nothing real to confirm until
// apply_edit actually produces a new_content.
pub(crate) fn edit_file_with_confirmation(
    root: &Path,
    file_access_security: FileAccessSecurity,
    tool_call: &ToolCall,
    tx: &mpsc::Sender<CoreEvent>,
    confirm_rx: &mpsc::Receiver<ConfirmationChoice>,
) -> Result<String, ToolError> {
    let args: EditArgs =
        serde_json::from_str(&tool_call.arguments).map_err(|_| ToolError::MalformedArguments)?;
    let requested = Path::new(&args.path);

    let sandbox_path = SandboxPath::new(root, requested).map_err(ToolError::InvalidPath)?;
    if file_access_security == FileAccessSecurity::Strict
        && is_content_restricted(sandbox_path.relative_path())
    {
        return Err(ToolError::AccessDenied);
    }

    let old_content =
        std::fs::read_to_string(sandbox_path.as_path()).map_err(|_| ToolError::IoFailure)?;
    let new_content = apply_edit(&old_content, &args.old, &args.new, args.replace_all)?;
    let diff = super::generate_windowed_diff(&old_content, &new_content, EDIT_DIFF_CONTEXT_LINES);

    propose_and_apply_write(
        root,
        requested,
        file_access_security,
        diff,
        &new_content,
        tx,
        confirm_rx,
    )
}

// The list of tools actually advertised to a provider. Kept next to
// dispatch()'s match arms (not off in core.rs) specifically so the two
// can't drift apart silently — see the names-match test below.
pub(crate) fn tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "read_file".to_string(),
            description: "Read the contents of a file within the project directory. If you \
                aren't sure this exact path exists, confirm it first with \
                list_files_recursive rather than guessing."
                .to_string(),
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
            description: "List files and directories within a directory in the project. Not \
                recursive — a nested directory's contents won't appear. Use \
                list_files_recursive if you need to search deeper than one level. Only call \
                this with a path you've already confirmed exists, from prior tool output or an \
                exact path the user gave you — if a request names a directory without a \
                confirmed full path, call list_files_recursive at the project root (\".\") \
                first instead of guessing this path."
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
        ToolDefinition {
            name: "list_files_recursive".to_string(),
            description: "Recursively list every file and directory nested under a directory \
                in the project, not just its immediate children. Use this whenever you don't \
                know exactly where a file or directory is located, instead of guessing a path \
                or asking the user to clarify. If you're not yet familiar with this \
                repository's layout, or a request references a file/directory without a \
                confirmed path, run this at the project root (\".\") first rather than \
                guessing. Returned paths are always relative to the project root, ready to \
                pass directly to read_file/write_file without modification. Directories end \
                with a trailing \"/\"; build/VCS noise (target, .git, node_modules) is shown \
                by name but not descended into."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the directory, relative to the project root. \
                            Use \".\" for the project root. Treat a directory name mentioned \
                            in a request as a fuzzy description, not a literal path — it could \
                            exist somewhere else in the tree (e.g. \"core\" might really be \
                            \"src/core\"). If you haven't already confirmed the exact path \
                            from prior tool output, use \".\" here instead of guessing the \
                            name directly."
                    }
                },
                "required": ["path"]
            }),
        },
        ToolDefinition {
            name: "write_file".to_string(),
            description: "Create a new file or overwrite an existing one within the project \
                directory. Call this directly to propose the change — the system automatically \
                shows the user a diff and requires their approval before anything is written, \
                so do not ask the user for confirmation yourself first. Do not assume the write \
                has happened until a result confirms it; the user may decline. For a small, \
                targeted change to part of an existing file, prefer edit_file instead — \
                reconstructing and resending the entire file here risks silently losing content \
                you didn't mean to touch."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file, relative to the project root."
                    },
                    "content": {
                        "type": "string",
                        "description": "The full new contents of the file."
                    }
                },
                "required": ["path", "content"]
            }),
        },
        ToolDefinition {
            name: "edit_file".to_string(),
            description: "Replace an exact snippet of text within an existing file, without \
                resending the rest of the file's contents. Call this directly to propose the \
                change — the system automatically shows the user a diff and requires their \
                approval before anything is written, so do not ask the user for confirmation \
                yourself first. `old` must match the file's current content exactly, including \
                whitespace, and must be unique within the file unless replace_all is set — if \
                it isn't found, or matches more than once without replace_all, you'll get an \
                error telling you which; add more surrounding context to `old` to make it \
                unique, or set replace_all to true to change every occurrence. Do not assume \
                the edit has happened until a result confirms it; the user may decline. Prefer \
                this over write_file whenever the change is local to part of the file, not the \
                whole thing."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file, relative to the project root. The \
                            file must already exist."
                    },
                    "old": {
                        "type": "string",
                        "description": "The exact existing text to replace, including \
                            whitespace — must match the file's current content verbatim."
                    },
                    "new": {
                        "type": "string",
                        "description": "The text to replace it with."
                    },
                    "replace_all": {
                        "type": "boolean",
                        "description": "Set to true to replace every occurrence of `old` \
                            instead of requiring exactly one match. Defaults to false."
                    }
                },
                "required": ["path", "old", "new"]
            }),
        },
    ]
}

// Matches on the tool name first, then parses that specific tool's
// arguments — not the other way around — so an unrecognized name never
// has to care about argument shape at all, and a new tool means one
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
        "list_files_recursive" => {
            let args: PathArgs = serde_json::from_str(&tool_call.arguments)
                .map_err(|_| ToolError::MalformedArguments)?;
            list_files_recursive(root, Path::new(&args.path), file_access_security)
        }
        other => Err(ToolError::UnknownTool(other.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{DiffLine, DiffLineText};
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

    // plan_and_validate_directory_creation: layers is_content_restricted
    // over SandboxPath::plan_directory_creation's output.

    #[test]
    fn plan_and_validate_directory_creation_allows_an_unrestricted_nested_path() {
        let root = TempDir::new();

        let result = plan_and_validate_directory_creation(
            root.path(),
            Path::new("a/b/c"),
            FileAccessSecurity::Strict,
        );

        let relative_paths: Vec<PathBuf> = result
            .unwrap()
            .iter()
            .map(|p| p.relative_path().to_path_buf())
            .collect();
        assert_eq!(
            relative_paths,
            vec![
                PathBuf::from("a"),
                PathBuf::from("a/b"),
                PathBuf::from("a/b/c")
            ]
        );
    }

    #[test]
    fn plan_and_validate_directory_creation_blocks_a_restricted_leaf() {
        let root = TempDir::new();

        let result = plan_and_validate_directory_creation(
            root.path(),
            Path::new("a/.ssh"),
            FileAccessSecurity::Strict,
        );

        assert_eq!(result, Err(ToolError::AccessDenied));
    }

    // The gap a single leaf-only check would miss: .ssh appears in the
    // MIDDLE of the requested path, not at the end.
    #[test]
    fn plan_and_validate_directory_creation_blocks_a_restricted_intermediate_level() {
        let root = TempDir::new();

        let result = plan_and_validate_directory_creation(
            root.path(),
            Path::new(".ssh/b"),
            FileAccessSecurity::Strict,
        );

        assert_eq!(result, Err(ToolError::AccessDenied));
    }

    #[test]
    fn plan_and_validate_directory_creation_skips_the_content_check_in_loose_mode() {
        let root = TempDir::new();

        let result = plan_and_validate_directory_creation(
            root.path(),
            Path::new(".ssh/b"),
            FileAccessSecurity::Loose,
        );

        assert!(result.is_ok());
    }

    #[test]
    fn plan_and_validate_directory_creation_maps_a_sandbox_error() {
        let outer = TempDir::new();
        let root = outer.path().join("project");
        std::fs::create_dir(&root).unwrap();

        let result = plan_and_validate_directory_creation(
            &root,
            Path::new("../outside"),
            FileAccessSecurity::Strict,
        );

        assert_eq!(result, Err(ToolError::InvalidPath(SandboxError::Escapes)));
    }

    // create_planned_directories: the mechanical filesystem step that
    // actually creates each level a validated plan calls for.

    #[test]
    fn create_planned_directories_creates_every_planned_level_in_order() {
        let root = TempDir::new();
        let plan = SandboxPath::plan_directory_creation(root.path(), Path::new("a/b/c")).unwrap();

        let result = create_planned_directories(&plan);

        assert_eq!(result, Ok(()));
        assert!(root.path().join("a").is_dir());
        assert!(root.path().join("a/b").is_dir());
        assert!(root.path().join("a/b/c").is_dir());
    }

    #[test]
    fn create_planned_directories_is_a_noop_for_an_empty_plan() {
        let result = create_planned_directories(&[]);

        assert_eq!(result, Ok(()));
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

    // Directories get their own entry (trailing "/") at every level, not
    // just files — extends list_files's existing one-level behavior
    // (which already shows both kinds) rather than losing information
    // relative to it.
    #[test]
    fn list_files_recursive_returns_sorted_root_relative_paths_for_nested_entries() {
        let root = TempDir::new();
        std::fs::write(root.path().join("notes.txt"), "").unwrap();
        std::fs::create_dir_all(root.path().join("src").join("core")).unwrap();
        std::fs::write(root.path().join("src").join("main.rs"), "").unwrap();
        std::fs::write(root.path().join("src").join("core").join("mod.rs"), "").unwrap();

        let result = list_files_recursive(root.path(), Path::new("."), FileAccessSecurity::Strict);

        assert_eq!(
            result,
            Ok("notes.txt\nsrc/\nsrc/core/\nsrc/core/mod.rs\nsrc/main.rs".to_string())
        );
    }

    // The whole point of this tool: paths are anchored to the project
    // root regardless of what directory was actually queried, so a
    // result can be fed directly into read_file/write_file with no
    // recomposition — querying "src" still returns "src/core/mod.rs",
    // not "core/mod.rs". Note "src" itself isn't in the output: querying
    // a directory lists what's inside it, not the directory itself,
    // matching list_files's own existing behavior.
    #[test]
    fn list_files_recursive_returns_paths_relative_to_the_root_not_the_queried_directory() {
        let root = TempDir::new();
        std::fs::create_dir_all(root.path().join("src").join("core")).unwrap();
        std::fs::write(root.path().join("src").join("main.rs"), "").unwrap();
        std::fs::write(root.path().join("src").join("core").join("mod.rs"), "").unwrap();

        let result =
            list_files_recursive(root.path(), Path::new("src"), FileAccessSecurity::Strict);

        assert_eq!(
            result,
            Ok("src/core/\nsrc/core/mod.rs\nsrc/main.rs".to_string())
        );
    }

    #[test]
    fn list_files_recursive_shows_an_empty_directory_with_a_trailing_slash() {
        let root = TempDir::new();
        std::fs::create_dir(root.path().join("empty_dir")).unwrap();

        let result = list_files_recursive(root.path(), Path::new("."), FileAccessSecurity::Strict);

        assert_eq!(result, Ok("empty_dir/".to_string()));
    }

    // A symlinked directory is listed like any other entry but never
    // descended into — no trailing "/" either, since that would assert
    // something about what it resolves to, which the walk deliberately
    // never checks (DirEntry::file_type() reports a symlink's own type,
    // not its target's, so this falls out of the walk's is_dir() check
    // with no special-casing needed). Avoids both cycle risk (a symlink
    // could point at an ancestor) and symlink-escape risk, for free.
    #[test]
    #[cfg(unix)]
    fn list_files_recursive_lists_a_symlinked_directory_as_a_leaf_without_descending_into_it() {
        let root = TempDir::new();
        std::fs::create_dir(root.path().join("real_target")).unwrap();
        std::fs::write(root.path().join("real_target").join("secret.txt"), "").unwrap();
        std::os::unix::fs::symlink(
            root.path().join("real_target"),
            root.path().join("link_dir"),
        )
        .unwrap();

        let result = list_files_recursive(root.path(), Path::new("."), FileAccessSecurity::Strict);

        assert_eq!(
            result,
            Ok("link_dir\nreal_target/\nreal_target/secret.txt".to_string())
        );
    }

    // Extends list_files's existing per-level rule to every level: a
    // restricted directory's name still shows up (existence isn't
    // hidden), but nothing inside it is ever enumerated, no matter how
    // deep in the tree it's found.
    #[test]
    fn list_files_recursive_skips_enumerating_into_a_restricted_directory_but_still_shows_its_name()
    {
        let root = TempDir::new();
        std::fs::write(root.path().join("notes.txt"), "").unwrap();
        std::fs::create_dir(root.path().join(".ssh")).unwrap();
        std::fs::write(root.path().join(".ssh").join("id_rsa"), "private").unwrap();

        let result = list_files_recursive(root.path(), Path::new("."), FileAccessSecurity::Strict);

        assert_eq!(result, Ok(".ssh/\nnotes.txt".to_string()));
    }

    #[test]
    fn list_files_recursive_rejects_a_directly_restricted_top_level_directory() {
        let root = TempDir::new();
        std::fs::create_dir(root.path().join(".ssh")).unwrap();
        std::fs::write(root.path().join(".ssh").join("id_rsa"), "private").unwrap();

        let result =
            list_files_recursive(root.path(), Path::new(".ssh"), FileAccessSecurity::Strict);

        assert_eq!(result, Err(ToolError::AccessDenied));
    }

    #[test]
    fn list_files_recursive_recurses_into_a_restricted_directory_in_loose_mode() {
        let root = TempDir::new();
        std::fs::create_dir(root.path().join(".ssh")).unwrap();
        std::fs::write(root.path().join(".ssh").join("id_rsa"), "private").unwrap();

        let result = list_files_recursive(root.path(), Path::new("."), FileAccessSecurity::Loose);

        assert_eq!(result, Ok(".ssh/\n.ssh/id_rsa".to_string()));
    }

    // Distinct from restricted-path skipping: a noise directory's
    // contents aren't sensitive, just unhelpful volume (build
    // artifacts), so this is a usefulness concern rather than a
    // security control.
    #[test]
    fn list_files_recursive_skips_a_noise_directory_but_still_shows_its_name() {
        let root = TempDir::new();
        std::fs::write(root.path().join("Cargo.toml"), "").unwrap();
        std::fs::create_dir(root.path().join("target")).unwrap();
        std::fs::write(root.path().join("target").join("binary"), "").unwrap();

        let result = list_files_recursive(root.path(), Path::new("."), FileAccessSecurity::Strict);

        assert_eq!(result, Ok("Cargo.toml\ntarget/".to_string()));
    }

    // Noise-directory skipping isn't gated by strict/loose at all —
    // loose mode exists to allow reading sensitive files, not to bring
    // back build-artifact noise, so target/.git/node_modules stay
    // skipped either way.
    #[test]
    fn list_files_recursive_skips_a_noise_directory_even_in_loose_mode() {
        let root = TempDir::new();
        std::fs::write(root.path().join("Cargo.toml"), "").unwrap();
        std::fs::create_dir(root.path().join("target")).unwrap();
        std::fs::write(root.path().join("target").join("binary"), "").unwrap();

        let result = list_files_recursive(root.path(), Path::new("."), FileAccessSecurity::Loose);

        assert_eq!(result, Ok("Cargo.toml\ntarget/".to_string()));
    }

    // Unlike a restricted path, a noise directory isn't a security
    // boundary — there's no reason to refuse an explicit, deliberate
    // query into it directly. The skip only suppresses incidental
    // recursion into it from a parent listing.
    #[test]
    fn list_files_recursive_lists_a_directly_queried_noise_directory_normally() {
        let root = TempDir::new();
        std::fs::create_dir(root.path().join("target")).unwrap();
        std::fs::write(root.path().join("target").join("binary"), "").unwrap();

        let result =
            list_files_recursive(root.path(), Path::new("target"), FileAccessSecurity::Strict);

        assert_eq!(result, Ok("target/binary".to_string()));
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
    fn dispatch_routes_list_files_recursive_calls() {
        let root = TempDir::new();
        std::fs::create_dir(root.path().join("src")).unwrap();
        std::fs::write(root.path().join("src").join("a.txt"), "").unwrap();
        let tool_call = ToolCall {
            id: "call_1".to_string(),
            name: "list_files_recursive".to_string(),
            arguments: r#"{"path": "."}"#.to_string(),
        };

        let result = dispatch(root.path(), FileAccessSecurity::Strict, &tool_call);

        assert_eq!(result, Ok("src/\nsrc/a.txt".to_string()));
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

    // apply_edit is pure string logic, no I/O — the future
    // edit_file_with_confirmation reads old_content from disk and
    // hands it here, same split as generate_windowed_diff's own
    // pure-function step.
    #[test]
    fn apply_edit_replaces_a_single_exact_match() {
        let result = apply_edit("one\ntwo\nthree\n", "two", "TWO", false);

        assert_eq!(result, Ok("one\nTWO\nthree\n".to_string()));
    }

    #[test]
    fn apply_edit_replaces_every_occurrence_when_replace_all_is_true() {
        let result = apply_edit("a b a c a", "a", "X", true);

        assert_eq!(result, Ok("X b X c X".to_string()));
    }

    #[test]
    fn apply_edit_rejects_old_text_not_found_in_the_file() {
        let result = apply_edit("one\ntwo\nthree\n", "missing", "new", false);

        assert_eq!(result, Err(ToolError::NoMatch));
    }

    // Zero matches is still an error with replace_all: true — there is
    // nothing to replace either way, replace_all only changes what
    // happens when old is found more than once.
    #[test]
    fn apply_edit_rejects_old_text_not_found_even_with_replace_all() {
        let result = apply_edit("one\ntwo\nthree\n", "missing", "new", true);

        assert_eq!(result, Err(ToolError::NoMatch));
    }

    #[test]
    fn apply_edit_rejects_an_ambiguous_match_without_replace_all() {
        let result = apply_edit("a b a c a", "a", "X", false);

        assert_eq!(result, Err(ToolError::AmbiguousMatch(3)));
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
                // "hello" has no trailing newline in the tool call's
                // literal content, so the diff correctly flags it —
                // see diff.rs's DiffLineText.
                assert_eq!(
                    diff,
                    vec![DiffLine::Added(DiffLineText {
                        text: "hello".to_string(),
                        no_trailing_newline: true,
                    })]
                );
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

    // Same confirmation-gated shape as write_file_with_confirmation's
    // own tests above, but the diff is windowed (generate_windowed_diff)
    // rather than full-file — here the whole 3-line file fits within
    // the context window, so no DiffLine::Elided appears; a file large
    // enough to actually elide something isn't this step's concern (see
    // diff.rs's own generate_windowed_diff tests for that).
    #[test]
    fn edit_file_with_confirmation_applies_the_edit_when_confirmed() {
        let root = TempDir::new();
        std::fs::write(root.path().join("notes.txt"), "one\ntwo\nthree\n").unwrap();
        let (tx, rx) = mpsc::channel();
        let (confirm_tx, confirm_rx) = mpsc::channel();
        confirm_tx.send(ConfirmationChoice::Apply).unwrap();
        let tool_call = ToolCall {
            id: "call_1".to_string(),
            name: "edit_file".to_string(),
            arguments: r#"{"path": "notes.txt", "old": "two", "new": "TWO"}"#.to_string(),
        };

        let result = edit_file_with_confirmation(
            root.path(),
            FileAccessSecurity::Strict,
            &tool_call,
            &tx,
            &confirm_rx,
        );

        assert_eq!(result, Ok("wrote notes.txt".to_string()));
        assert_eq!(
            std::fs::read_to_string(root.path().join("notes.txt")).unwrap(),
            "one\nTWO\nthree\n"
        );
        match rx.try_recv().unwrap() {
            CoreEvent::WriteProposed { path, diff } => {
                assert_eq!(path, "notes.txt");
                assert_eq!(
                    diff,
                    vec![
                        DiffLine::Unchanged(DiffLineText {
                            text: "one".to_string(),
                            no_trailing_newline: false,
                        }),
                        DiffLine::Removed(DiffLineText {
                            text: "two".to_string(),
                            no_trailing_newline: false,
                        }),
                        DiffLine::Added(DiffLineText {
                            text: "TWO".to_string(),
                            no_trailing_newline: false,
                        }),
                        DiffLine::Unchanged(DiffLineText {
                            text: "three".to_string(),
                            no_trailing_newline: false,
                        }),
                    ]
                );
            }
            other => panic!("expected WriteProposed, got {other:?}"),
        }
    }

    #[test]
    fn edit_file_with_confirmation_declines_without_writing() {
        let root = TempDir::new();
        std::fs::write(root.path().join("notes.txt"), "one\ntwo\nthree\n").unwrap();
        let (tx, _rx) = mpsc::channel();
        let (confirm_tx, confirm_rx) = mpsc::channel();
        confirm_tx.send(ConfirmationChoice::Decline).unwrap();
        let tool_call = ToolCall {
            id: "call_1".to_string(),
            name: "edit_file".to_string(),
            arguments: r#"{"path": "notes.txt", "old": "two", "new": "TWO"}"#.to_string(),
        };

        let result = edit_file_with_confirmation(
            root.path(),
            FileAccessSecurity::Strict,
            &tool_call,
            &tx,
            &confirm_rx,
        );

        assert_eq!(result, Err(ToolError::WriteDeclined));
        assert_eq!(
            std::fs::read_to_string(root.path().join("notes.txt")).unwrap(),
            "one\ntwo\nthree\n"
        );
    }

    // Same principle as write_file_with_confirmation's own blocked-path
    // test: an invalid path must be rejected before ever asking for
    // confirmation. confirm_rx never receives anything here, so if the
    // implementation asked for confirmation first, this test would hang.
    #[test]
    fn edit_file_with_confirmation_rejects_a_blocked_path_without_asking() {
        let root = TempDir::new();
        std::fs::write(root.path().join(".env"), "SECRET=1").unwrap();
        let (tx, rx) = mpsc::channel();
        let (_confirm_tx, confirm_rx) = mpsc::channel();
        let tool_call = ToolCall {
            id: "call_1".to_string(),
            name: "edit_file".to_string(),
            arguments: r#"{"path": ".env", "old": "SECRET=1", "new": "SECRET=2"}"#.to_string(),
        };

        let result = edit_file_with_confirmation(
            root.path(),
            FileAccessSecurity::Strict,
            &tool_call,
            &tx,
            &confirm_rx,
        );

        assert_eq!(result, Err(ToolError::AccessDenied));
        assert_eq!(
            std::fs::read_to_string(root.path().join(".env")).unwrap(),
            "SECRET=1"
        );
        assert!(rx.try_recv().is_err());
    }

    // An edit that can't even be computed (old matches more than once,
    // replace_all not set) has no real diff to propose — same
    // validate-before-confirming principle as the blocked-path test
    // above, just for apply_edit's own ambiguity check instead of the
    // sandbox/blocklist gate.
    #[test]
    fn edit_file_with_confirmation_rejects_an_ambiguous_match_without_asking() {
        let root = TempDir::new();
        std::fs::write(root.path().join("notes.txt"), "old old old\n").unwrap();
        let (tx, rx) = mpsc::channel();
        let (_confirm_tx, confirm_rx) = mpsc::channel();
        let tool_call = ToolCall {
            id: "call_1".to_string(),
            name: "edit_file".to_string(),
            arguments: r#"{"path": "notes.txt", "old": "old", "new": "new"}"#.to_string(),
        };

        let result = edit_file_with_confirmation(
            root.path(),
            FileAccessSecurity::Strict,
            &tool_call,
            &tx,
            &confirm_rx,
        );

        assert_eq!(result, Err(ToolError::AmbiguousMatch(3)));
        assert_eq!(
            std::fs::read_to_string(root.path().join("notes.txt")).unwrap(),
            "old old old\n"
        );
        assert!(rx.try_recv().is_err());
    }

    // Pins down exactly what's advertised to the model. write_file and
    // edit_file are both included here even though dispatch() itself
    // never routes either — they're handled separately by
    // write_file_with_confirmation/edit_file_with_confirmation (see
    // run_agent_loop) — so this only guards tool_definitions() itself,
    // not a dispatch/definitions correspondence that no longer holds
    // for all five tools.
    #[test]
    fn tool_definitions_advertises_all_five_tools() {
        let definitions = tool_definitions();
        let names: Vec<&str> = definitions
            .iter()
            .map(|definition| definition.name.as_str())
            .collect();

        assert_eq!(
            names,
            vec![
                "read_file",
                "list_files",
                "list_files_recursive",
                "write_file",
                "edit_file"
            ]
        );
    }

    // Guards the specific behavioral addition from docs/tool-descriptions.md
    // Step 1 (a model given an uncertain path should confirm it via
    // list_files_recursive rather than guess) — not a full-text pin of
    // read_file's description, which is a review concern, not a test one.
    #[test]
    fn read_file_description_points_to_list_files_recursive_for_an_uncertain_path() {
        let definitions = tool_definitions();
        let read_file_definition = definitions
            .iter()
            .find(|definition| definition.name == "read_file")
            .expect("read_file should be advertised");

        assert!(
            read_file_definition
                .description
                .contains("list_files_recursive"),
            "expected read_file's description to point to list_files_recursive for an \
             uncertain path, got: {:?}",
            read_file_definition.description
        );
    }

    // Guards docs/tool-descriptions.md Step 2's redirect: list_files
    // itself should warn against guessing an unconfirmed path, not just
    // rely on list_files_recursive's own "use this instead" framing,
    // which a model considering list_files never sees.
    #[test]
    fn list_files_description_warns_against_an_unconfirmed_path() {
        let definitions = tool_definitions();
        let list_files_definition = definitions
            .iter()
            .find(|definition| definition.name == "list_files")
            .expect("list_files should be advertised");

        assert!(
            list_files_definition.description.contains("confirmed"),
            "expected list_files's description to warn against calling it with an \
             unconfirmed path, got: {:?}",
            list_files_definition.description
        );
    }

    // Guards docs/tool-descriptions.md Step 2's scoped addition:
    // list_files_recursive should frame itself as the starting point for
    // not-yet-familiar repo layouts, not only for a specific missing
    // file/directory (the existing "guessing a path" sentence already
    // covers that narrower case).
    #[test]
    fn list_files_recursive_description_covers_unfamiliar_repo_layout() {
        let definitions = tool_definitions();
        let list_files_recursive_definition = definitions
            .iter()
            .find(|definition| definition.name == "list_files_recursive")
            .expect("list_files_recursive should be advertised");

        assert!(
            list_files_recursive_definition
                .description
                .contains("familiar"),
            "expected list_files_recursive's description to cover an unfamiliar repo \
             layout, not just a single missing path, got: {:?}",
            list_files_recursive_definition.description
        );
    }

    // Guards docs/tool-descriptions.md's revised Step 2 scope: manual
    // testing found the guess can survive at the parameter level even
    // once the top-level description steers the model to the right
    // tool — list_files_recursive gets called, but with a guessed path
    // like "core" instead of ".". The tool-level description can't fix
    // an argument choice; only the path parameter's own text can.
    #[test]
    fn list_files_recursive_path_parameter_prefers_root_over_guessing() {
        let definitions = tool_definitions();
        let list_files_recursive_definition = definitions
            .iter()
            .find(|definition| definition.name == "list_files_recursive")
            .expect("list_files_recursive should be advertised");

        let path_description =
            list_files_recursive_definition.parameters["properties"]["path"]["description"]
                .as_str()
                .expect("path parameter should have a string description");

        assert!(
            path_description.contains("guessing"),
            "expected list_files_recursive's path parameter to steer toward the project root \
             over guessing an uncertain nested path, got: {path_description:?}"
        );
    }

    // docs/tool-descriptions.md deliberately deferred this exact change
    // until edit_file existed with a concrete alternative to point at.
    // Same lesson as list_files/list_files_recursive's own cross-pointing
    // (see ARCHITECTURE.md): a model only weighs guidance written on a
    // tool it's already considering, so the warning has to live on
    // write_file's own description, not only on edit_file's.
    #[test]
    fn write_file_description_steers_toward_edit_file_for_a_targeted_change() {
        let definitions = tool_definitions();
        let write_file_definition = definitions
            .iter()
            .find(|definition| definition.name == "write_file")
            .expect("write_file should be advertised");

        assert!(
            write_file_definition.description.contains("edit_file"),
            "expected write_file's description to steer toward edit_file for a targeted \
             change, got: {:?}",
            write_file_definition.description
        );
    }

    // The other direction of the same cross-pointing: edit_file's own
    // description should also name write_file, not rely solely on
    // write_file's side carrying the whole signal.
    #[test]
    fn edit_file_description_steers_over_write_file_for_a_local_change() {
        let definitions = tool_definitions();
        let edit_file_definition = definitions
            .iter()
            .find(|definition| definition.name == "edit_file")
            .expect("edit_file should be advertised");

        assert!(
            edit_file_definition.description.contains("write_file"),
            "expected edit_file's description to steer over write_file for a local change, \
             got: {:?}",
            edit_file_definition.description
        );
    }
}
