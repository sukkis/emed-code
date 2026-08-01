// Builds the system prompt sent on every request: a base prompt plus
// optional AGENTS.md content from the user's global config and the
// current project. See docs/system-prompt.md for the reasoning behind
// the base prompt's wording and the combination order.

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
