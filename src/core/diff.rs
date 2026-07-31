// Structured, per-line diff data — not a pre-formatted string —
// specifically so the TUI can render Added/Removed with real color
// rather than relying on a text convention like unified diff's +/-
// prefixes. pub, not pub(crate): it appears in CoreEvent::WriteProposed
// and tui::LogEntry::Diff, both public types.
#[derive(Debug, Clone, PartialEq)]
pub enum DiffLine {
    Added(String),
    Removed(String),
    Unchanged(String),
}

// Pure: no I/O. Wraps similar::TextDiff::from_lines, mapping its
// per-line changes into this project's own DiffLine shape. `old`/`new`
// are whole-file contents, not individual lines — from_lines does its
// own line splitting internally.
pub(crate) fn generate_diff(old: &str, new: &str) -> Vec<DiffLine> {
    let text_diff = similar::TextDiff::from_lines(old, new);

    text_diff
        .iter_all_changes()
        .map(|change| {
            // value() includes the line's own trailing newline (from_lines
            // splits on it but keeps it attached) — trimmed here since
            // DiffLine holds one line's text, not the newline that
            // separates it from the next.
            let text = change.value().trim_end_matches(['\n', '\r']).to_string();
            match change.tag() {
                similar::ChangeTag::Delete => DiffLine::Removed(text),
                similar::ChangeTag::Insert => DiffLine::Added(text),
                similar::ChangeTag::Equal => DiffLine::Unchanged(text),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_diff_marks_identical_content_as_unchanged() {
        let diff = generate_diff("a\nb\n", "a\nb\n");

        assert_eq!(
            diff,
            vec![
                DiffLine::Unchanged("a".to_string()),
                DiffLine::Unchanged("b".to_string()),
            ]
        );
    }

    #[test]
    fn generate_diff_marks_every_line_added_for_a_new_file() {
        let diff = generate_diff("", "a\nb\n");

        assert_eq!(
            diff,
            vec![
                DiffLine::Added("a".to_string()),
                DiffLine::Added("b".to_string()),
            ]
        );
    }

    #[test]
    fn generate_diff_marks_every_line_removed_for_emptied_content() {
        let diff = generate_diff("a\nb\n", "");

        assert_eq!(
            diff,
            vec![
                DiffLine::Removed("a".to_string()),
                DiffLine::Removed("b".to_string()),
            ]
        );
    }

    #[test]
    fn generate_diff_mixes_unchanged_removed_and_added_lines() {
        let diff = generate_diff("a\nb\nc\n", "a\nx\nc\n");

        assert_eq!(
            diff,
            vec![
                DiffLine::Unchanged("a".to_string()),
                DiffLine::Removed("b".to_string()),
                DiffLine::Added("x".to_string()),
                DiffLine::Unchanged("c".to_string()),
            ]
        );
    }
}
