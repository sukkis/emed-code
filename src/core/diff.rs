/// One line of a diff between a file's old and new contents, shown to
/// the user before a write happens.
// pub, not pub(crate): it appears in CoreEvent::WriteProposed and
// tui::LogEntry::Diff, both public types.
#[derive(Debug, Clone, PartialEq)]
pub enum DiffLine {
    Added(DiffLineText),
    Removed(DiffLineText),
    Unchanged(DiffLineText),
    /// A run of unchanged lines collapsed out of a windowed diff (see
    /// [`generate_windowed_diff`]), carrying how many lines were elided.
    /// Never produced by [`generate_diff`], which always shows every line.
    Elided(usize),
}

/// One line's text, plus whether it's missing a trailing newline in the
/// file it came from — a real difference in what's on disk, not a
/// cosmetic one.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DiffLineText {
    pub text: String,
    pub no_trailing_newline: bool,
}

// Shared by generate_diff and generate_windowed_diff below, so the two
// only differ in which changes they walk (all of them vs. a
// context-limited window), not in how a single change becomes a
// DiffLine.
fn diff_line_from_change(change: similar::Change<&str>) -> DiffLine {
    let raw = change.value();
    // from_lines splits on the newline but keeps it attached to the
    // line that precedes it — a line missing that terminator is the
    // file's actual last line with no trailing newline, not a
    // coincidence of how lines were split.
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
}

// Pure: no I/O. Wraps similar::TextDiff::from_lines, mapping its
// per-line changes into this project's own DiffLine shape. `old`/`new`
// are whole-file contents, not individual lines — from_lines does its
// own line splitting internally.
pub(crate) fn generate_diff(old: &str, new: &str) -> Vec<DiffLine> {
    let text_diff = similar::TextDiff::from_lines(old, new);
    text_diff
        .iter_all_changes()
        .map(diff_line_from_change)
        .collect()
}

// Like generate_diff, but scoped to `context_lines` of unchanged
// context around each change (git-diff-style hunks) rather than the
// whole file — long unchanged runs between/around hunks collapse into
// a single DiffLine::Elided(count) instead of being listed line by
// line. Built on similar::TextDiff::grouped_ops, which already
// isolates change clusters this same way for its own unified-diff
// output; this just maps its groups into this project's DiffLine shape
// instead of unified-diff text.
//
// An elision marker is only emitted where lines are actually skipped —
// never a spurious Elided(0) at a file boundary a hunk's context
// window already reaches.
pub(crate) fn generate_windowed_diff(old: &str, new: &str, context_lines: usize) -> Vec<DiffLine> {
    let text_diff = similar::TextDiff::from_lines(old, new);
    let old_len = text_diff.old_len();

    let mut result = Vec::new();
    let mut previous_old_end = 0;

    for group in text_diff.grouped_ops(context_lines) {
        let (Some(first), Some(last)) = (group.first(), group.last()) else {
            continue;
        };
        let hunk_old_start = first.old_range().start;
        let hunk_old_end = last.old_range().end;

        let elided_before = hunk_old_start - previous_old_end;
        if elided_before > 0 {
            result.push(DiffLine::Elided(elided_before));
        }

        for op in &group {
            result.extend(text_diff.iter_changes(op).map(diff_line_from_change));
        }

        previous_old_end = hunk_old_end;
    }

    let elided_after = old_len - previous_old_end;
    if elided_after > 0 {
        result.push(DiffLine::Elided(elided_after));
    }

    result
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

    // Ten lines (a..j), one change at line e (index 4). With 3 lines of
    // context on each side, line a (1 line before the window) and lines
    // i/j (2 lines after) fall outside the window and collapse into a
    // single Elided marker each, rather than being listed individually
    // the way generate_diff's full-file diff would.
    #[test]
    fn generate_windowed_diff_shows_limited_context_around_a_single_change() {
        let old = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n";
        let new = "a\nb\nc\nd\nX\nf\ng\nh\ni\nj\n";

        let diff = generate_windowed_diff(old, new, 3);

        assert_eq!(
            diff,
            vec![
                DiffLine::Elided(1),
                DiffLine::Unchanged(line("b")),
                DiffLine::Unchanged(line("c")),
                DiffLine::Unchanged(line("d")),
                DiffLine::Removed(line("e")),
                DiffLine::Added(line("X")),
                DiffLine::Unchanged(line("f")),
                DiffLine::Unchanged(line("g")),
                DiffLine::Unchanged(line("h")),
                DiffLine::Elided(2),
            ]
        );
    }

    // Fourteen lines (a..n), two changes far enough apart (index 2 and
    // index 10) that they form two separate hunks under a 3-line context
    // radius, with exactly one unchanged line (g) between them collapsed
    // into its own Elided marker. Neither hunk touches the file's start
    // or end, so this only exercises the *between-hunks* elision case —
    // see the next test for the file-boundary case.
    #[test]
    fn generate_windowed_diff_elides_an_unchanged_run_between_two_hunks() {
        let old = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\nm\nn\n";
        let new = "a\nb\nX\nd\ne\nf\ng\nh\ni\nj\nY\nl\nm\nn\n";

        let diff = generate_windowed_diff(old, new, 3);

        assert_eq!(
            diff,
            vec![
                DiffLine::Unchanged(line("a")),
                DiffLine::Unchanged(line("b")),
                DiffLine::Removed(line("c")),
                DiffLine::Added(line("X")),
                DiffLine::Unchanged(line("d")),
                DiffLine::Unchanged(line("e")),
                DiffLine::Unchanged(line("f")),
                DiffLine::Elided(1),
                DiffLine::Unchanged(line("h")),
                DiffLine::Unchanged(line("i")),
                DiffLine::Unchanged(line("j")),
                DiffLine::Removed(line("k")),
                DiffLine::Added(line("Y")),
                DiffLine::Unchanged(line("l")),
                DiffLine::Unchanged(line("m")),
                DiffLine::Unchanged(line("n")),
            ]
        );
    }

    // Ten lines (a..j), changes at the very first and very last line.
    // Both hunks' context windows already reach a real file boundary, so
    // there is nothing to elide there — an Elided(0) marker would be
    // meaningless noise, not just a smaller number, so none should
    // appear at either end. The two lines between the hunks (e, f) still
    // collapse normally, same as the between-hunks case above.
    #[test]
    fn generate_windowed_diff_omits_elision_markers_at_the_files_start_and_end() {
        let old = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n";
        let new = "X\nb\nc\nd\ne\nf\ng\nh\ni\nY\n";

        let diff = generate_windowed_diff(old, new, 3);

        assert_eq!(
            diff,
            vec![
                DiffLine::Removed(line("a")),
                DiffLine::Added(line("X")),
                DiffLine::Unchanged(line("b")),
                DiffLine::Unchanged(line("c")),
                DiffLine::Unchanged(line("d")),
                DiffLine::Elided(2),
                DiffLine::Unchanged(line("g")),
                DiffLine::Unchanged(line("h")),
                DiffLine::Unchanged(line("i")),
                DiffLine::Removed(line("j")),
                DiffLine::Added(line("Y")),
            ]
        );
    }

    // Every line differs, so there is no unchanged content anywhere to
    // collapse — the windowed diff should read identically to a plain
    // full-file diff here, with no Elided markers at all.
    #[test]
    fn generate_windowed_diff_shows_no_elision_when_the_whole_file_changed() {
        let old = "a\nb\nc\n";
        let new = "x\ny\nz\n";

        let diff = generate_windowed_diff(old, new, 3);

        assert_eq!(
            diff,
            vec![
                DiffLine::Removed(line("a")),
                DiffLine::Removed(line("b")),
                DiffLine::Removed(line("c")),
                DiffLine::Added(line("x")),
                DiffLine::Added(line("y")),
                DiffLine::Added(line("z")),
            ]
        );
    }
}
