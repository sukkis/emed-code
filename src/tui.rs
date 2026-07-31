// ratatui rendering and crossterm input handling. Talks to `core` only
// through Core::submit_user_message/poll_events.

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph};
use unicode_width::UnicodeWidthChar;

use crate::core::{Core, CoreEvent, DiffLine};

const SCROLL_STEP: usize = 1;
const PAGE_SCROLL_STEP: usize = 5;

fn format_core_event(event: CoreEvent) -> String {
    match event {
        CoreEvent::AssistantChunk(text) => format!("emed-code: {text}"),
        // "tool: " is a distinct prefix from both "emed-code: " and
        // "error: " — shows the call (name + arguments) that ran. On
        // success, shows only "ok" — the result content (which could be
        // an entire file's contents) is for the model, not something the
        // chat log echoes back at the user. On failure, the error
        // message itself is short and useful, so it's shown in full.
        CoreEvent::ToolCall {
            name,
            arguments,
            result,
            ..
        } => match result {
            Ok(_) => format!("tool: {name}({arguments}) -> ok"),
            Err(error) => format!("tool: {name}({arguments}) -> error: {error}"),
        },
        // Minimal, functional stopgap (Rust's exhaustiveness requires
        // an arm now that CoreEvent::WriteProposed exists) — Step 5b is
        // what renders the real colored diff + numbered confirmation
        // menu, same treatment Phase 3's ToolCall variant got between
        // its own introduction and its dedicated rendering step.
        CoreEvent::WriteProposed { path, diff } => {
            format!("write proposed: {path} ({} lines)", diff.len())
        }
        CoreEvent::Error(message) => format!("error: {message}"),
    }
}

// Turns diff data into styled lines — a red background for removed, a
// green background for added, unstyled for unchanged (matches Claude
// Code's own diff display: a colored line band, not colored text —
// this leaves room for per-line syntax highlighting later without the
// two competing for the same visual channel). Each line keeps a
// "+"/"-"/" " text prefix alongside its color, same convention as a
// unified diff, so a terminal without color support (or a color-blind
// user) still gets a real signal, not just a color-only distinction.
// This is the first per-line-styled content anywhere in this TUI —
// everything else is plain, unstyled text.
//
// The background only covers the line's own text, not the full render
// width (that would need padding to a width this pure function doesn't
// know) — revisit once this is wired into draw's real chat log
// (Step 5), where the actual width is available.
fn render_diff_lines(diff: &[DiffLine]) -> Vec<Line<'static>> {
    diff.iter()
        .map(|line| match line {
            DiffLine::Added(text) => {
                Line::styled(format!("+{text}"), Style::new().bg(Color::Green))
            }
            DiffLine::Removed(text) => {
                Line::styled(format!("-{text}"), Style::new().bg(Color::Red))
            }
            DiffLine::Unchanged(text) => Line::from(format!(" {text}")),
        })
        .collect()
}

// offset is "how many lines scrolled up from the bottom" (0 = latest).
// Clamped to max, the true ceiling as of the last render (total wrapped
// lines minus visible height) — App keeps this up to date via draw, so
// it's always at most one frame stale. Without this clamp, offset could
// overshoot the real top, and scroll_down would have to silently "pay
// off" that overshoot before the view visibly moved again.
fn scroll_up(offset: usize, step: usize, max: usize) -> usize {
    (offset + step).min(max)
}

fn scroll_down(offset: usize, step: usize) -> usize {
    offset.saturating_sub(step)
}

// Converts "how many lines scrolled up from the bottom" into "how many
// lines to skip from the top" for Paragraph::scroll — the render-time
// clamp that keeps scrolling sane regardless of how large offset gets
// (an over-large offset just saturates to 0, "scrolled all the way up").
fn chat_scroll_skip(total_lines: usize, visible_height: usize, scroll_offset: usize) -> u16 {
    let max_skip = total_lines.saturating_sub(visible_height);
    max_skip.saturating_sub(scroll_offset) as u16
}

fn display_width(c: char) -> usize {
    match c {
        '\t' => 4,
        '\n' | '\r' => 0,
        _ => c.width().unwrap_or(0),
    }
}

// Ported from emed's src/wrap.rs (wrapped_lines), adapted to a plain
// &str instead of a rope-buffer line. Breaks at the nearest space at or
// before the width limit, keeping the space attached to the end of the
// earlier chunk; a word with no space that's itself longer than width
// is hard-broken, since there's no space to back up to. Concatenating
// all returned chunks reproduces the original line exactly. An empty
// line still returns one (empty) chunk — unlike emed's version, which
// leaves that to a separate caller we don't have here.
fn wrap_line(line: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }

    let chars: Vec<char> = line.chars().filter(|&c| c != '\n').collect();

    if chars.is_empty() {
        return vec![String::new()];
    }

    let mut chunks = Vec::new();
    let mut chunk_start = 0;
    let mut cols_used = 0;
    let mut last_space_index: Option<usize> = None;
    let mut i = 0;

    while i < chars.len() {
        let char_width = display_width(chars[i]);

        if cols_used + char_width > width {
            let break_at = last_space_index.map_or(i, |space| space + 1);
            chunks.push(chars[chunk_start..break_at].iter().collect());

            chunk_start = break_at;
            i = chunk_start;
            cols_used = 0;
            last_space_index = None;
            continue;
        }

        if chars[i] == ' ' {
            last_space_index = Some(i);
        }
        cols_used += char_width;
        i += 1;
    }

    if chunk_start < chars.len() {
        chunks.push(chars[chunk_start..].iter().collect());
    }

    chunks
}

// Splits on real line breaks first (preserving blank lines and each
// line's own leading indentation), then word-wraps each individual
// line only if it's actually too wide. Calling wrap_line directly on a
// whole multi-paragraph block would strip every embedded newline (it
// only expects a single already-split line), collapsing paragraph
// structure entirely.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return wrap_line("", width);
    }

    text.lines()
        .flat_map(|line| wrap_line(line, width))
        .collect()
}

pub fn is_quit_key(key: &KeyEvent) -> bool {
    key.kind == KeyEventKind::Press
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('q'))
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    let layout = Layout::vertical([Constraint::Min(1), Constraint::Length(3)]);
    let [chat_area, input_area] = frame.area().layout(&layout);

    let chat_title = format!("emed-code — AI: {}", app.provider_label().as_str());
    let chat_block = Block::bordered().title(chat_title);
    let chat_inner = chat_block.inner(chat_area);

    let wrapped_lines: Vec<String> = app
        .log()
        .iter()
        .flat_map(|entry| wrap_text(entry, chat_inner.width as usize))
        .collect();
    let visible_height = chat_inner.height as usize;
    app.set_max_scroll(wrapped_lines.len().saturating_sub(visible_height));
    let skip = chat_scroll_skip(wrapped_lines.len(), visible_height, app.scroll_offset());

    let chat_text = wrapped_lines.join("\n");
    let chat = Paragraph::new(chat_text)
        .block(chat_block)
        .scroll((skip, 0));
    frame.render_widget(chat, chat_area);

    let input_block = Block::bordered().title("input");
    let input_inner = input_block.inner(input_area);
    let input = Paragraph::new(app.input_buffer()).block(input_block);
    frame.render_widget(input, input_area);

    let cursor_x = input_inner.x + app.input_buffer().chars().count() as u16;
    frame.set_cursor_position((cursor_x, input_inner.y));
}

#[derive(Debug, Default)]
pub struct InputBox {
    buffer: String,
}

impl InputBox {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }

        match key.code {
            KeyCode::Char(c) => self.buffer.push(c),
            KeyCode::Backspace => {
                self.buffer.pop();
            }
            _ => {}
        }
    }

    pub fn buffer(&self) -> &str {
        &self.buffer
    }

    // Leaves an empty buffer in place, returning what it held.
    fn take(&mut self) -> String {
        std::mem::take(&mut self.buffer)
    }
}

// Static, startup-time label for which provider is active — not a live
// switch (provider selection stays a one-time CLI choice). Its own enum
// rather than reusing cli::Provider directly, so tui doesn't take on a
// dependency on the cli module for what is, from tui's perspective,
// just display text.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ProviderLabel {
    Ollama,
    Mistral,
}

impl ProviderLabel {
    fn as_str(self) -> &'static str {
        match self {
            ProviderLabel::Ollama => "local (ollama)",
            ProviderLabel::Mistral => "cloud (mistral)",
        }
    }
}

pub struct App {
    input: InputBox,
    log: Vec<String>,
    core: Core,
    provider_label: ProviderLabel,
    scroll_offset: usize,
    // True ceiling for scroll_offset, as of the last draw call. Stale by
    // at most one frame — see scroll_up's doc comment.
    max_scroll: usize,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        Self::with_core(Core::new(), ProviderLabel::Ollama)
    }

    pub fn with_core(core: Core, provider_label: ProviderLabel) -> Self {
        Self {
            input: InputBox::new(),
            log: Vec::new(),
            core,
            provider_label,
            scroll_offset: 0,
            max_scroll: 0,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }

        match key.code {
            KeyCode::Enter => self.submit(),
            KeyCode::Up => {
                self.scroll_offset = scroll_up(self.scroll_offset, SCROLL_STEP, self.max_scroll)
            }
            KeyCode::Down => self.scroll_offset = scroll_down(self.scroll_offset, SCROLL_STEP),
            KeyCode::PageUp => {
                self.scroll_offset =
                    scroll_up(self.scroll_offset, PAGE_SCROLL_STEP, self.max_scroll)
            }
            KeyCode::PageDown => {
                self.scroll_offset = scroll_down(self.scroll_offset, PAGE_SCROLL_STEP)
            }
            _ => self.input.handle_key(key),
        }
    }

    fn submit(&mut self) {
        let text = self.input.take();
        if text.is_empty() {
            return;
        }

        self.core.submit_user_message(text.clone());
        self.log.push(format!("you: {text}"));
        self.scroll_offset = 0;
    }

    pub fn poll_core_events(&mut self) {
        let events = self.core.poll_events();
        self.apply_core_events(events);
    }

    // Deliberately does not touch scroll_offset: if the user has
    // scrolled up to read something, new content arriving (e.g. more of
    // a streamed reply) shouldn't yank them back to the bottom. Staying
    // at 0 (the default) already means "following" — chat_scroll_skip
    // shows the latest content automatically as total_lines grows, with
    // no reset needed.
    fn apply_core_events(&mut self, events: Vec<CoreEvent>) {
        for event in events {
            self.log.push(format_core_event(event));
        }
    }

    pub fn log(&self) -> &[String] {
        &self.log
    }

    pub fn provider_label(&self) -> ProviderLabel {
        self.provider_label
    }

    pub fn input_buffer(&self) -> &str {
        self.input.buffer()
    }

    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    pub fn set_max_scroll(&mut self, max_scroll: usize) {
        self.max_scroll = max_scroll;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    // Renders against an in-memory TestBackend rather than a real
    // terminal, then checks the two titles actually made it into the
    // buffer. Checking substring containment (not exact line content)
    // so this doesn't depend on exact border/title spacing details.
    #[test]
    fn draws_titled_log_and_input_areas() {
        let backend = TestBackend::new(20, 6);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = App::new();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();

        assert!(
            content.contains("emed-code"),
            "expected the log area's title to render: {content:?}"
        );
        assert!(
            content.contains("input"),
            "expected the input area's title to render: {content:?}"
        );
    }

    // Same TestBackend approach as draws_titled_log_and_input_areas
    // above — checks the label text renders, not exact spacing. Two
    // providers, two tests, so the label is proven enum-driven rather
    // than one hardcoded string that happens to say "ollama".
    #[test]
    fn draws_ollama_as_the_local_provider_in_the_chat_title() {
        let backend = TestBackend::new(40, 6);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = App::with_core(Core::new(), ProviderLabel::Ollama);
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();

        assert!(
            content.contains("local (ollama)"),
            "expected the active provider label to render: {content:?}"
        );
    }

    #[test]
    fn draws_mistral_as_the_cloud_provider_in_the_chat_title() {
        let backend = TestBackend::new(40, 6);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = App::with_core(Core::new(), ProviderLabel::Mistral);
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();

        assert!(
            content.contains("cloud (mistral)"),
            "expected the active provider label to render: {content:?}"
        );
    }

    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    // Narrow width so this line would need to wrap; tall height so the
    // wrapped continuation lines are still all visible in one draw.
    // Without wrapping, Paragraph truncates the line at the box width,
    // so "message" would never be written to any cell at all.
    #[test]
    fn long_chat_lines_wrap_instead_of_being_cut_off() {
        let backend = TestBackend::new(10, 15);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new();
        for c in "this is a long message".chars() {
            app.handle_key(press(KeyCode::Char(c)));
        }
        app.handle_key(press(KeyCode::Enter));

        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();

        assert!(
            content.contains("message"),
            "expected the long line to wrap rather than being cut off: {content:?}"
        );
    }

    // Regression coverage for the bug this step fixes: with the old
    // entry-count-based scroll math, a single long entry wrapping into
    // many lines left the view stuck unable to show or scroll to new
    // content. 40 short words at width 8 wrap into far more lines than
    // the ~10 visible rows (a generous margin — this isn't testing an
    // exact boundary, just that scrolling meaningfully happens at all).
    fn type_and_submit(app: &mut App, text: &str) {
        for c in text.chars() {
            app.handle_key(press(KeyCode::Char(c)));
        }
        app.handle_key(press(KeyCode::Enter));
    }

    fn rendered_content(app: &mut App) -> String {
        let backend = TestBackend::new(10, 15);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn default_scroll_shows_the_end_of_a_long_wrapped_message() {
        let mut app = App::new();
        let words: Vec<String> = (1..=40).map(|n| format!("w{n}")).collect();
        type_and_submit(&mut app, &words.join(" "));

        let content = rendered_content(&mut app);

        assert!(
            content.contains("w40"),
            "expected the most recent part of a long message to be visible by default: {content:?}"
        );
        assert!(
            !content.contains("w1 "),
            "expected the earliest part of a long message to have scrolled out of view by default: {content:?}"
        );
    }

    #[test]
    fn scrolling_up_reveals_the_start_of_a_long_wrapped_message() {
        let mut app = App::new();
        let words: Vec<String> = (1..=40).map(|n| format!("w{n}")).collect();
        type_and_submit(&mut app, &words.join(" "));
        // scroll_up now clamps to App's last-known ceiling, which is only
        // set by draw — so a render has to happen at least once before
        // scrolling for the ceiling to be anything other than 0.
        rendered_content(&mut app);

        for _ in 0..30 {
            app.handle_key(press(KeyCode::Up));
        }

        let content = rendered_content(&mut app);

        assert!(
            content.contains("w1 "),
            "expected scrolling up enough to reveal the start of a long message: {content:?}"
        );
    }

    // Input area is the bottom 3 rows of a 20x6 backend (Length(3)),
    // bordered on all sides, so its inner content row is (1..19, 4).
    #[test]
    fn cursor_is_at_the_start_of_the_input_box_when_empty() {
        let backend = TestBackend::new(20, 6);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new();

        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        terminal.backend_mut().assert_cursor_position((1, 4));
    }

    #[test]
    fn cursor_is_positioned_after_the_typed_text() {
        let backend = TestBackend::new(20, 6);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new();
        app.handle_key(press(KeyCode::Char('h')));
        app.handle_key(press(KeyCode::Char('i')));

        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        terminal.backend_mut().assert_cursor_position((3, 4));
    }

    #[test]
    fn typing_appends_characters() {
        let mut input = InputBox::new();

        input.handle_key(press(KeyCode::Char('h')));
        input.handle_key(press(KeyCode::Char('i')));

        assert_eq!(input.buffer(), "hi");
    }

    #[test]
    fn backspace_removes_the_last_character() {
        let mut input = InputBox::new();
        input.handle_key(press(KeyCode::Char('h')));
        input.handle_key(press(KeyCode::Char('i')));

        input.handle_key(press(KeyCode::Backspace));

        assert_eq!(input.buffer(), "h");
    }

    #[test]
    fn backspace_on_empty_input_does_nothing() {
        let mut input = InputBox::new();

        input.handle_key(press(KeyCode::Backspace));

        assert_eq!(input.buffer(), "");
    }

    // Windows always reports a Release event alongside Press; Unix only
    // does if a terminal opts into it. Either way, typing shouldn't be
    // double-counted, so Release must be a no-op.
    #[test]
    fn key_release_events_are_ignored() {
        let mut input = InputBox::new();

        input.handle_key(KeyEvent::new_with_kind(
            KeyCode::Char('x'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        ));

        assert_eq!(input.buffer(), "");
    }

    // These only check App's own observable state (log content, input
    // cleared) — not whether Core "really" got called. Core's own
    // correctness is already covered by its own tests; asserting on it
    // here too would mean adding a mock/seam purely to make that
    // assertion possible, which isn't earning its keep yet.
    #[test]
    fn enter_with_non_empty_input_appends_to_log_and_clears_input() {
        let mut app = App::new();
        app.handle_key(press(KeyCode::Char('h')));
        app.handle_key(press(KeyCode::Char('i')));

        app.handle_key(press(KeyCode::Enter));

        assert_eq!(app.log(), &["you: hi".to_string()]);
        assert_eq!(app.input_buffer(), "");
    }

    #[test]
    fn enter_with_empty_input_does_nothing() {
        let mut app = App::new();

        app.handle_key(press(KeyCode::Enter));

        assert!(app.log().is_empty());
    }

    #[test]
    fn non_enter_keys_are_forwarded_to_the_input_box() {
        let mut app = App::new();

        app.handle_key(press(KeyCode::Char('a')));

        assert_eq!(app.input_buffer(), "a");
    }

    #[test]
    fn ctrl_c_is_a_quit_key() {
        assert!(is_quit_key(&KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        )));
    }

    #[test]
    fn ctrl_q_is_a_quit_key() {
        assert!(is_quit_key(&KeyEvent::new(
            KeyCode::Char('q'),
            KeyModifiers::CONTROL
        )));
    }

    #[test]
    fn plain_c_without_control_is_not_a_quit_key() {
        assert!(!is_quit_key(&KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::NONE
        )));
    }

    #[test]
    fn ctrl_c_release_is_not_a_quit_key() {
        assert!(!is_quit_key(&KeyEvent::new_with_kind(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
            KeyEventKind::Release
        )));
    }

    use crate::core::CoreEvent;

    #[test]
    fn formats_an_assistant_chunk_with_a_prefix() {
        assert_eq!(
            format_core_event(CoreEvent::AssistantChunk("hi there".to_string())),
            "emed-code: hi there"
        );
    }

    #[test]
    fn formats_an_error_with_a_prefix() {
        assert_eq!(
            format_core_event(CoreEvent::Error("connection refused".to_string())),
            "error: connection refused"
        );
    }

    // "tool: " is a distinct prefix from both "emed-code: " (assistant
    // replies) and "error: " — the point of this step, so a user can
    // tell tool activity apart from either at a glance. Success shows
    // only "ok", not the full result content — a read_file result could
    // be an entire file's contents, which the model needs but the
    // chat log doesn't need to echo back at the user.
    #[test]
    fn formats_a_successful_tool_call_without_echoing_the_full_result() {
        assert_eq!(
            format_core_event(CoreEvent::ToolCall {
                id: "call_1".to_string(),
                name: "read_file".to_string(),
                arguments: r#"{"path": "notes.txt"}"#.to_string(),
                result: Ok("the entire contents of a very long file".to_string()),
            }),
            r#"tool: read_file({"path": "notes.txt"}) -> ok"#
        );
    }

    // Unlike a success, the error message itself is short and useful —
    // shown in full, not hidden behind a generic "failed".
    #[test]
    fn formats_a_failed_tool_call_with_its_error_message() {
        assert_eq!(
            format_core_event(CoreEvent::ToolCall {
                id: "call_1".to_string(),
                name: "read_file".to_string(),
                arguments: r#"{"path": "../secret.txt"}"#.to_string(),
                result: Err("invalid path: path escapes the sandboxed directory".to_string()),
            }),
            r#"tool: read_file({"path": "../secret.txt"}) -> error: invalid path: path escapes the sandboxed directory"#
        );
    }

    // TestBackend-level check, same style as the provider-label tests —
    // proves a ToolCall event actually reaches the rendered chat log via
    // App::apply_core_events, not just that format_core_event itself
    // produces the right string in isolation.
    #[test]
    fn draws_a_tool_call_in_the_chat_log() {
        let backend = TestBackend::new(60, 8);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = App::new();
        app.apply_core_events(vec![CoreEvent::ToolCall {
            id: "call_1".to_string(),
            name: "read_file".to_string(),
            arguments: r#"{"path": "notes.txt"}"#.to_string(),
            result: Ok("hello".to_string()),
        }]);
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();

        assert!(
            content.contains("tool: read_file"),
            "expected the tool-call prefix to render: {content:?}"
        );
    }

    // Phase 4 Step 3: colored diff rendering. Not wired into App's log/
    // draw pipeline yet — tested directly against constructed DiffLine
    // data (Step 5 is what makes a real CoreEvent produce diffs to
    // show).
    #[test]
    fn render_diff_lines_colors_added_and_removed_line_backgrounds_and_leaves_unchanged_plain() {
        use ratatui::style::Color;

        let diff = vec![
            DiffLine::Unchanged("same".to_string()),
            DiffLine::Removed("old".to_string()),
            DiffLine::Added("new".to_string()),
        ];

        let lines = render_diff_lines(&diff);

        let backend = TestBackend::new(20, 3);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(Paragraph::new(lines.clone()), frame.area());
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer.get(0, 0).bg, Color::Reset);
        assert_eq!(buffer.get(0, 1).bg, Color::Red);
        assert_eq!(buffer.get(0, 2).bg, Color::Green);
    }

    // The +/-/space text prefix must survive alongside the color, so a
    // terminal without color support (or a color-blind user) still gets
    // a real signal, not just an invisible-without-color distinction.
    #[test]
    fn render_diff_lines_keeps_a_text_prefix_alongside_color() {
        let diff = vec![
            DiffLine::Unchanged("same".to_string()),
            DiffLine::Removed("old".to_string()),
            DiffLine::Added("new".to_string()),
        ];

        let lines = render_diff_lines(&diff);
        let texts: Vec<String> = lines.iter().map(|line| line.to_string()).collect();

        assert_eq!(texts, vec![" same", "-old", "+new"]);
    }

    #[test]
    fn scroll_up_increases_offset_by_step() {
        assert_eq!(scroll_up(0, 1, 10), 1);
    }

    #[test]
    fn scroll_up_does_not_exceed_the_given_ceiling() {
        assert_eq!(scroll_up(4, 1, 5), 5);
        assert_eq!(scroll_up(5, 1, 5), 5);
    }

    #[test]
    fn scroll_down_decreases_offset_by_step() {
        assert_eq!(scroll_down(5, 1), 4);
    }

    #[test]
    fn scroll_down_does_not_go_below_zero() {
        assert_eq!(scroll_down(0, 1), 0);
    }

    #[test]
    fn chat_scroll_skip_shows_the_bottom_by_default() {
        assert_eq!(chat_scroll_skip(10, 4, 0), 6);
    }

    #[test]
    fn chat_scroll_skip_moves_up_as_offset_increases() {
        assert_eq!(chat_scroll_skip(10, 4, 2), 4);
    }

    #[test]
    fn chat_scroll_skip_is_clamped_to_the_top() {
        assert_eq!(chat_scroll_skip(10, 4, 100), 0);
    }

    #[test]
    fn chat_scroll_skip_is_zero_when_all_content_already_fits() {
        assert_eq!(chat_scroll_skip(3, 10, 0), 0);
    }

    #[test]
    fn chat_scroll_skip_on_an_empty_log() {
        assert_eq!(chat_scroll_skip(0, 4, 0), 0);
    }

    #[test]
    fn up_key_scrolls_up_and_down_key_scrolls_back() {
        let mut app = App::new();
        let words: Vec<String> = (1..=40).map(|n| format!("w{n}")).collect();
        type_and_submit(&mut app, &words.join(" "));
        // scroll_up clamps to App's last-known ceiling, set only by
        // draw — content long enough to actually need scrolling, and a
        // render establishing that ceiling, are both required here.
        rendered_content(&mut app);
        assert_eq!(app.scroll_offset(), 0);

        app.handle_key(press(KeyCode::Up));
        assert_eq!(app.scroll_offset(), 1);

        app.handle_key(press(KeyCode::Down));
        assert_eq!(app.scroll_offset(), 0);
    }

    #[test]
    fn page_up_scrolls_by_more_than_one_line() {
        let mut app = App::new();
        let words: Vec<String> = (1..=40).map(|n| format!("w{n}")).collect();
        type_and_submit(&mut app, &words.join(" "));
        rendered_content(&mut app);

        app.handle_key(press(KeyCode::PageUp));

        assert!(app.scroll_offset() > 1);
    }

    // Regression test for the bug where new content arriving (e.g. a
    // streamed reply still coming in) forcibly reset scroll_offset to 0,
    // yanking the view back to the bottom even if the user had
    // deliberately scrolled up to read something. apply_core_events is
    // called directly (bypassing Core/the network entirely) since
    // that's the only way to feed it fake events without a live Ollama
    // server.
    #[test]
    fn new_core_events_do_not_reset_a_manual_scroll_position() {
        let mut app = App::new();
        let words: Vec<String> = (1..=40).map(|n| format!("w{n}")).collect();
        type_and_submit(&mut app, &words.join(" "));
        rendered_content(&mut app);
        app.handle_key(press(KeyCode::Up));
        assert_eq!(app.scroll_offset(), 1);

        app.apply_core_events(vec![CoreEvent::AssistantChunk("reply".to_string())]);

        assert_eq!(
            app.scroll_offset(),
            1,
            "expected a manually-scrolled position to survive new content arriving"
        );
    }

    // Regression test for the overshoot bug documented in
    // ARCHITECTURE.md: without a ceiling, scrolling well past the true
    // top would "bank" overshoot that Down then has to silently pay off
    // before the view visibly moves. 500 Up presses is far more than
    // this content has wrapped lines for, so it would overshoot badly
    // if scroll_up were still unbounded.
    #[test]
    fn scrolling_past_the_top_does_not_leave_scrolling_down_unresponsive() {
        let mut app = App::new();
        let words: Vec<String> = (1..=40).map(|n| format!("w{n}")).collect();
        type_and_submit(&mut app, &words.join(" "));
        rendered_content(&mut app);

        for _ in 0..500 {
            app.handle_key(press(KeyCode::Up));
        }
        let content_at_top = rendered_content(&mut app);

        app.handle_key(press(KeyCode::Down));
        let content_after_one_down = rendered_content(&mut app);

        assert_ne!(
            content_at_top, content_after_one_down,
            "expected a single Down press to move the view immediately, even after scrolling far past the top"
        );
    }

    #[test]
    fn display_width_of_a_plain_ascii_char_is_one() {
        assert_eq!(display_width('a'), 1);
    }

    #[test]
    fn display_width_of_a_wide_char_is_two() {
        assert_eq!(display_width('中'), 2);
    }

    #[test]
    fn display_width_of_tab_is_four() {
        assert_eq!(display_width('\t'), 4);
    }

    #[test]
    fn wrap_line_returns_the_line_unchanged_when_it_fits() {
        assert_eq!(wrap_line("hello", 20), vec!["hello".to_string()]);
    }

    #[test]
    fn wrap_line_wraps_on_a_space_keeping_it_with_the_earlier_chunk() {
        assert_eq!(
            wrap_line("hello world", 8),
            vec!["hello ".to_string(), "world".to_string()]
        );
    }

    #[test]
    fn wrap_line_hard_breaks_a_word_longer_than_the_width() {
        assert_eq!(
            wrap_line("abcdefgh", 3),
            vec!["abc".to_string(), "def".to_string(), "gh".to_string()]
        );
    }

    #[test]
    fn wrap_line_of_an_empty_string_returns_one_empty_chunk() {
        assert_eq!(wrap_line("", 10), vec![String::new()]);
    }

    #[test]
    fn wrap_line_with_zero_width_returns_no_chunks() {
        assert_eq!(wrap_line("hello", 0), Vec::<String>::new());
    }

    #[test]
    fn wrap_text_preserves_blank_lines_between_paragraphs() {
        assert_eq!(
            wrap_text("para one\n\npara two", 20),
            vec![
                "para one".to_string(),
                String::new(),
                "para two".to_string()
            ]
        );
    }

    #[test]
    fn wrap_text_preserves_indentation_of_each_line() {
        assert_eq!(
            wrap_text("  indented line\nnormal line", 30),
            vec!["  indented line".to_string(), "normal line".to_string()]
        );
    }

    #[test]
    fn wrap_text_still_word_wraps_a_line_that_is_too_wide() {
        assert_eq!(
            wrap_text("hello world", 8),
            vec!["hello ".to_string(), "world".to_string()]
        );
    }

    #[test]
    fn wrap_text_of_an_empty_string_returns_one_empty_chunk() {
        assert_eq!(wrap_text("", 10), vec![String::new()]);
    }
}
