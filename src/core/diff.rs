/// One line of a diff between a file's old and new contents, shown to
/// the user before a write happens.
// pub, not pub(crate): it appears in CoreEvent::WriteProposed and
// tui::LogEntry::Diff, both public types.
#[derive(Debug, Clone, PartialEq)]
pub enum DiffLine {
    Added(DiffLineText),
    Removed(DiffLineText),
    Unchanged(DiffLineText),
}

/// One line's text, plus whether it's missing a trailing newline in the
/// file it came from — a real difference in what's on disk, not a
/// cosmetic one.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DiffLineText {
    pub text: String,
    pub no_trailing_newline: bool,
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
            let raw = change.value();
            // from_lines splits on the newline but keeps it attached to
            // the line that precedes it — a line missing that
            // terminator is the file's actual last line with no
            // trailing newline, not a coincidence of how lines were
            // split.
            let no_trailing_newline = !raw.ends_with('\n');
            let line_text = DiffLineText {
                text: raw.trim_end_matches(['\n', '\r']).to_string(),
                no_trailing_newline,
            };
            match change.tag() {
                similar::ChangeTag::Delete => DiffLine::Removed(line_text),
                similar::ChangeTag::Insert => DiffLine::Added(line_text),
                similar::ChangeTag::Equal => DiffLine::Unchanged(line_text),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str) -> DiffLineText {
        DiffLineText {
            text: text.to_string(),
            no_trailing_newline: false,
        }
    }

    fn line_missing_newline(text: &str) -> DiffLineText {
        DiffLineText {
            text: text.to_string(),
            no_trailing_newline: true,
        }
    }

    #[test]
    fn generate_diff_marks_identical_content_as_unchanged() {
        let diff = generate_diff("a\nb\n", "a\nb\n");

        assert_eq!(
            diff,
            vec![
                DiffLine::Unchanged(line("a")),
                DiffLine::Unchanged(line("b")),
            ]
        );
    }

    #[test]
    fn generate_diff_marks_every_line_added_for_a_new_file() {
        let diff = generate_diff("", "a\nb\n");

        assert_eq!(
            diff,
            vec![DiffLine::Added(line("a")), DiffLine::Added(line("b"))]
        );
    }

    #[test]
    fn generate_diff_marks_every_line_removed_for_emptied_content() {
        let diff = generate_diff("a\nb\n", "");

        assert_eq!(
            diff,
            vec![DiffLine::Removed(line("a")), DiffLine::Removed(line("b"))]
        );
    }

    #[test]
    fn generate_diff_mixes_unchanged_removed_and_added_lines() {
        let diff = generate_diff("a\nb\nc\n", "a\nx\nc\n");

        assert_eq!(
            diff,
            vec![
                DiffLine::Unchanged(line("a")),
                DiffLine::Removed(line("b")),
                DiffLine::Added(line("x")),
                DiffLine::Unchanged(line("c")),
            ]
        );
    }

    // similar::TextDiff::from_lines compares raw lines including their
    // terminator, so "same" (no newline) and "same\n" count as two
    // different lines even though their visible text is identical.
    // Rather than hiding that difference (which would misrepresent what
    // write_file is actually about to put on disk), each line remembers
    // whether it's missing its trailing newline — tui.rs renders that
    // as an explicit annotation, matching git's own "\ No newline at
    // end of file".
    #[test]
    fn generate_diff_flags_a_line_missing_its_trailing_newline() {
        let diff = generate_diff("same", "same\n");

        assert_eq!(
            diff,
            vec![
                DiffLine::Removed(line_missing_newline("same")),
                DiffLine::Added(line("same")),
            ]
        );
    }

    #[test]
    fn generate_diff_flags_the_other_side_when_a_trailing_newline_is_removed() {
        let diff = generate_diff("same\n", "same");

        assert_eq!(
            diff,
            vec![
                DiffLine::Removed(line("same")),
                DiffLine::Added(line_missing_newline("same")),
            ]
        );
    }
}
