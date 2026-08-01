// Builds the system prompt sent on every request: a base prompt plus
// optional AGENTS.md content from the user's global config and the
// current project. See docs/system-prompt.md for the reasoning behind
// the base prompt's wording and the combination order.

use std::path::{Path, PathBuf};

// "You have no way to run code, a build, or tests" is only true until
// a run_command tool exists (see docs/harness-roadmap.md) — this line
// needs rewriting the day that lands, not before.
pub(crate) const BASE_SYSTEM_PROMPT: &str = "You are an AI coding assistant operating inside a terminal \
    session, working directly on the user's local project files through the \
    tools available to you.\n\n\
    - This is a single continuous turn: you cannot pause mid-task to ask the \
    user a question and wait for an answer. If something is ambiguous, make \
    the most reasonable assumption, proceed, and state that assumption \
    clearly when you report back.\n\
    - If a tool call fails, finds nothing, or a path doesn't exist, don't \
    conclude the task is impossible from that alone — check what other tools \
    are available to you before giving up or telling the user something \
    can't be done.\n\
    - Only claim something is correct if you've actually checked it with an \
    available tool (for example, re-reading a file after writing it). Don't \
    state something as done or working based on assumption.\n\
    - You have no way to run code, a build, or tests — don't claim to have \
    verified anything that way.";

// Global before project (general before specific, ending closest to
// the actual conversation) and straight concatenation, not an
// override — see docs/system-prompt.md for why no precedence logic is
// needed between the two sources. `base` is a parameter, not read
// directly from BASE_SYSTEM_PROMPT here, so this function's
// combination logic can be tested against a short fixture instead of
// duplicating the real ~200-word prompt — its wording isn't separately
// unit-tested, since it's plain content, not logic; matching
// docs/system-prompt.md is a code-review concern, not a test one.
pub(crate) fn build_system_prompt(
    base: &str,
    global: Option<&str>,
    project: Option<&str>,
) -> String {
    let mut prompt = base.to_string();

    if let Some(global) = global {
        prompt.push_str("\n\n## Your preferences (all projects)\n\n");
        prompt.push_str(global);
    }

    if let Some(project) = project {
        prompt.push_str("\n\n## This project\n\n");
        prompt.push_str(project);
    }

    prompt
}

// Real I/O: reads an AGENTS.md-style file, failing safe to None on any
// error (missing file, permissions, non-UTF8 content) — same treatment
// Settings::load() already gives its own config file, never an error,
// never a panic.
pub(crate) fn read_agents_md(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

// AGENTS.md lives alongside the project's own code, inside the
// sandboxed root — see docs/system-prompt.md for the self-modification
// risk this implies and why it's accepted, and SECURITY.md for the
// current-state note.
pub(crate) fn project_agents_md_path(root: &Path) -> PathBuf {
    root.join("AGENTS.md")
}

// Structurally outside SandboxPath's boundary — same tamper-resistance
// property settings.toml already has. Not unit-tested directly (a real
// dirs::config_dir() call), same treatment Settings::load()'s own path
// resolution already gets.
pub(crate) fn global_agents_md_path() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("emed-code").join("AGENTS.md"))
}

// The real entry point, called once at Core construction. Not
// unit-tested directly, same treatment Settings::load()'s real I/O
// already gets — read_agents_md, build_system_prompt, and
// combine_with_status are what's independently tested. Reads each file
// exactly once — Core reports this status back via agents_md_status()
// rather than main.rs ever reading either file again itself.
pub(crate) fn load_system_prompt(root: &Path) -> (String, AgentsMdStatus) {
    let global = global_agents_md_path().and_then(|path| read_agents_md(&path));
    let project = read_agents_md(&project_agents_md_path(root));
    combine_with_status(BASE_SYSTEM_PROMPT, global.as_deref(), project.as_deref())
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AgentsMdStatus {
    pub project_found: bool,
    pub global_found: bool,
}

fn combine_with_status(
    base: &str,
    global: Option<&str>,
    project: Option<&str>,
) -> (String, AgentsMdStatus) {
    let status = AgentsMdStatus {
        project_found: project.is_some(),
        global_found: global.is_some(),
    };
    (build_system_prompt(base, global, project), status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    // Hand-rolled instead of a tempfile dev-dependency — same reasoning
    // as sandbox_path.rs's/tools.rs's/core.rs's identical fixture,
    // duplicated rather than shared per this codebase's existing
    // convention.
    struct TempDir(PathBuf);

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    impl TempDir {
        fn new() -> Self {
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let mut path = std::env::temp_dir();
            path.push(format!(
                "emed-code-system-prompt-test-{}-{id}",
                std::process::id()
            ));
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
    fn read_agents_md_returns_none_for_a_missing_file() {
        let dir = TempDir::new();

        assert_eq!(read_agents_md(&dir.path().join("AGENTS.md")), None);
    }

    #[test]
    fn read_agents_md_returns_the_file_contents_when_present() {
        let dir = TempDir::new();
        let path = dir.path().join("AGENTS.md");
        std::fs::write(&path, "run `just test` before committing").unwrap();

        assert_eq!(
            read_agents_md(&path),
            Some("run `just test` before committing".to_string())
        );
    }

    #[test]
    fn project_agents_md_path_is_agents_md_directly_under_root() {
        let dir = TempDir::new();

        assert_eq!(
            project_agents_md_path(dir.path()),
            dir.path().join("AGENTS.md")
        );
    }

    #[test]
    fn build_system_prompt_with_neither_source_returns_just_the_base_prompt() {
        assert_eq!(build_system_prompt("BASE", None, None), "BASE");
    }

    #[test]
    fn build_system_prompt_appends_global_agents_md_under_its_own_heading() {
        assert_eq!(
            build_system_prompt("BASE", Some("prefer tabs"), None),
            "BASE\n\n## Your preferences (all projects)\n\nprefer tabs"
        );
    }

    #[test]
    fn build_system_prompt_appends_project_agents_md_under_its_own_heading() {
        assert_eq!(
            build_system_prompt("BASE", None, Some("run `just test` before committing")),
            "BASE\n\n## This project\n\nrun `just test` before committing"
        );
    }

    #[test]
    fn build_system_prompt_combines_both_sources_global_then_project() {
        assert_eq!(
            build_system_prompt(
                "BASE",
                Some("prefer tabs"),
                Some("run `just test` before committing")
            ),
            "BASE\n\n## Your preferences (all projects)\n\nprefer tabs\n\n\
            ## This project\n\nrun `just test` before committing"
        );
    }

    // combine_with_status's only new job over build_system_prompt is
    // the true/false mapping — whether a source was found flows in as
    // Some/None here exactly the same way read_agents_md's own tests
    // already prove a real file's presence maps to Some/None, so no
    // tempdir is needed to exercise this.
    #[test]
    fn combine_with_status_reports_neither_source_found() {
        let (_, status) = combine_with_status("BASE", None, None);

        assert_eq!(
            status,
            AgentsMdStatus {
                project_found: false,
                global_found: false
            }
        );
    }

    #[test]
    fn combine_with_status_reports_only_global_found() {
        let (_, status) = combine_with_status("BASE", Some("prefer tabs"), None);

        assert_eq!(
            status,
            AgentsMdStatus {
                project_found: false,
                global_found: true
            }
        );
    }

    #[test]
    fn combine_with_status_reports_only_project_found() {
        let (_, status) = combine_with_status("BASE", None, Some("run `just test`"));

        assert_eq!(
            status,
            AgentsMdStatus {
                project_found: true,
                global_found: false
            }
        );
    }

    #[test]
    fn combine_with_status_reports_both_found() {
        let (_, status) = combine_with_status("BASE", Some("prefer tabs"), Some("run `just test`"));

        assert_eq!(
            status,
            AgentsMdStatus {
                project_found: true,
                global_found: true
            }
        );
    }
}
