//! Terminal rendering ([`ratatui`]) and keyboard input
//! ([`ratatui::crossterm`]).
//!
//! [`App`] holds everything the UI needs — the chat log, input buffer,
//! scroll position — and drives [`crate::core::Core`] only through its
//! public `submit_user_message`/`poll_events` calls, never touching its
//! internals directly. [`draw`] renders one frame from an `&App`;
//! [`App::handle_key`] and [`is_quit_key`] handle input.

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Text};
use ratatui::widgets::{Block, Paragraph};
use unicode_width::UnicodeWidthChar;

use crate::core::{AgentsMdStatus, ConfirmationChoice, Core, CoreEvent, DiffLine};

const SCROLL_STEP: usize = 1;
const PAGE_SCROLL_STEP: usize = 5;

// The input box grows by one row per wrapped line as you type, up to
// this many content rows (plus its 2 border rows) — a fixed cap
// rather than a terminal-relative fraction, so a huge paste can't
// squeeze the chat log down to nothing.
const MAX_INPUT_ROWS: usize = 6;

// Above this many characters, a JSON string value in a tool call's
// arguments is hidden behind a placeholder rather than shown in full —
// see truncate_long_argument_values.
const ARGUMENT_VALUE_TRUNCATION_THRESHOLD: usize = 80;

// A tool call's arguments can carry an entire file (write_file's
// `content`) or a large snippet (edit_file's `old`/`new`) — showing
// those raw would make the chat log line unreadable, the same problem
// format_core_event already avoids for `result` on success. This is
// deliberately tool-name-blind: any JSON string value over the
// threshold is replaced with a placeholder reporting its original
// length, regardless of which tool or field it came from, rather than
// special-casing write_file/edit_file by name.
//
// When nothing needs truncating, the original string is returned
// unchanged, byte-for-byte, rather than being reparsed and
// reserialized — so a short-argument call's formatting never shifts
// just because this function ran.
fn truncate_long_argument_values(arguments: &str) -> String {
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(arguments)
    else {
        // Not a JSON object at all (malformed JSON, or valid JSON that
        // isn't an object) — apply the same short/long rule to the
        // whole raw string as one blob.
        return if arguments.chars().count() > ARGUMENT_VALUE_TRUNCATION_THRESHOLD {
            format!("<{} chars>", arguments.chars().count())
        } else {
            arguments.to_string()
        };
    };

    let mut truncated_any = false;
    let mut new_map = serde_json::Map::with_capacity(map.len());
    for (key, value) in map {
        let value = match &value {
            serde_json::Value::String(s)
                if s.chars().count() > ARGUMENT_VALUE_TRUNCATION_THRESHOLD =>
            {
                truncated_any = true;
                serde_json::Value::String(format!("<{} chars>", s.chars().count()))
            }
            _ => value,
        };
        new_map.insert(key, value);
    }

    if truncated_any {
        serde_json::to_string(&serde_json::Value::Object(new_map))
            .unwrap_or_else(|_| arguments.to_string())
    } else {
        arguments.to_string()
    }
}

fn format_core_event(event: CoreEvent) -> String {
    match event {
        CoreEvent::AssistantChunk(text) => format!("emed-code: {text}"),
        // "tool: " is a distinct prefix from both "emed-code: " and
        // "error: " — shows the call (name + arguments) that ran. On
        // success, shows only "ok" — the result content (which could be
        // an entire file's contents) is for the model, not something the
        // chat log echoes back at the user. On failure, the error
        // message itself is short and useful, so it's shown in full.
        // Arguments go through truncate_long_argument_values first, for
        // the same reason — a write_file/edit_file call's arguments can
        // themselves carry huge content.
        CoreEvent::ToolCall {
            name,
            arguments,
            result,
            ..
        } => {
            let arguments = truncate_long_argument_values(&arguments);
            match result {
                Ok(_) => format!("tool: {name}({arguments}) -> ok"),
                Err(error) => format!("tool: {name}({arguments}) -> error: {error}"),
            }
        }
        // Kept only for this match's exhaustiveness — App::apply_core_events
        // intercepts CoreEvent::WriteProposed before it ever reaches
        // format_core_event in the real app, since it needs the real
        // colored diff + numbered menu, not plain text. Unreachable from
        // the live app, but the type is still CoreEvent, so this arm has
        // to exist for the match to compile.
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
// know) — revisit once there's a concrete reason to pad to width.
fn render_diff_lines(diff: &[DiffLine]) -> Vec<Line<'static>> {
    diff.iter()
        .flat_map(|line| match line {
            DiffLine::Added(line_text) => diff_line_with_annotation(
                format!("+{}", line_text.text),
                Some(Style::new().bg(Color::Green)),
                line_text.no_trailing_newline,
            ),
            DiffLine::Removed(line_text) => diff_line_with_annotation(
                format!("-{}", line_text.text),
                Some(Style::new().bg(Color::Red)),
                line_text.no_trailing_newline,
            ),
            DiffLine::Unchanged(line_text) => diff_line_with_annotation(
                format!(" {}", line_text.text),
                None,
                line_text.no_trailing_newline,
            ),
            // Dimmed rather than background-colored like Added/Removed —
            // this isn't real file content, so it doesn't need the
            // background channel content lines reserve for future
            // syntax highlighting.
            DiffLine::Elided(count) => {
                let noun = if *count == 1 { "line" } else { "lines" };
                vec![Line::styled(
                    format!("⋯ {count} unchanged {noun} ⋯"),
                    Style::new().fg(Color::DarkGray),
                )]
            }
        })
        .collect()
}

// Appends git's own "\ No newline at end of file" convention as a
// separate, unstyled line immediately after the affected one — stays
// truthful to what write_file actually puts on disk (a missing final
// newline is a real difference), rather than silently folding it into
// the line's own text or hiding it.
fn diff_line_with_annotation(
    text: String,
    style: Option<Style>,
    no_trailing_newline: bool,
) -> Vec<Line<'static>> {
    let line = match style {
        Some(style) => Line::styled(text, style),
        None => Line::from(text),
    };
    if no_trailing_newline {
        vec![line, Line::from("\\ No newline at end of file")]
    } else {
        vec![line]
    }
}

/// One entry in the chat log, returned by [`App::log`].
// An enum, not a plain String, specifically so a proposed write's diff
// can carry real per-line color through to rendering (Text's
// word-wrapping/plain-string treatment doesn't apply to it the way it
// does to everything else).
#[derive(Debug, Clone, PartialEq)]
pub enum LogEntry {
    Text(String),
    Diff { path: String, diff: Vec<DiffLine> },
}

// Turns one log entry into the wrapped/styled lines draw() renders.
// Text wraps to width same as always; Diff gets a plain header line
// (what's being proposed) followed by render_diff_lines' colored
// output — not wrapped to width yet, same known limitation
// render_diff_lines itself already documents (no edge-to-edge
// background band either, for the same reason).
fn render_log_entry(entry: &LogEntry, width: usize) -> Vec<Line<'static>> {
    match entry {
        LogEntry::Text(text) => wrap_text(text, width).into_iter().map(Line::from).collect(),
        LogEntry::Diff { path, diff } => {
            let mut lines = vec![Line::from(format!("Apply this change to {path}?"))];
            lines.extend(render_diff_lines(diff));
            lines
        }
    }
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

/// Whether this key press should quit the app — `Ctrl-C` or `Ctrl-Q`.
pub fn is_quit_key(key: &KeyEvent) -> bool {
    key.kind == KeyEventKind::Press
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('q'))
}

/// Renders one frame: the chat log (scrolled to `app`'s current
/// position) and either the input box or, while a write awaits
/// confirmation, a numbered apply/decline menu.
pub fn draw(frame: &mut Frame, app: &mut App) {
    // Border columns take 2 of the terminal's width either way —
    // Layout::vertical only ever splits height, so the input box's
    // usable width (and thus how many rows its current text wraps to)
    // is already known before that split happens below.
    let input_inner_width = frame.area().width.saturating_sub(2) as usize;
    let input_wrapped_lines = wrap_text(app.input_buffer(), input_inner_width);
    let input_content_rows = input_wrapped_lines.len().clamp(1, MAX_INPUT_ROWS) as u16;
    let input_area_height = input_content_rows + 2; // top/bottom border

    let layout = Layout::vertical([Constraint::Min(1), Constraint::Length(input_area_height)]);
    let [chat_area, input_area] = frame.area().layout(&layout);

    let chat_title = format!(
        "emed-code — AI: {} — {}",
        app.provider_label().as_str(),
        agents_md_title_suffix(app.agents_md_status())
    );
    let chat_block = Block::bordered().title(chat_title);
    let chat_inner = chat_block.inner(chat_area);

    let rendered_lines: Vec<Line<'static>> = app
        .log()
        .iter()
        .flat_map(|entry| render_log_entry(entry, chat_inner.width as usize))
        .collect();
    let visible_height = chat_inner.height as usize;
    app.set_max_scroll(rendered_lines.len().saturating_sub(visible_height));
    let skip = chat_scroll_skip(rendered_lines.len(), visible_height, app.scroll_offset());

    let chat = Paragraph::new(Text::from(rendered_lines))
        .block(chat_block)
        .scroll((skip, 0));
    frame.render_widget(chat, chat_area);

    // While a write is pending confirmation, the bottom area becomes a
    // numbered menu instead of the normal typed-input box — chat input
    // is blocked in this state (App::handle_key) anyway, so there's
    // nothing meaningful to type or position a cursor in.
    if let Some(path) = app.pending_confirmation() {
        let menu_block = Block::bordered().title(format!("confirm write to {path}?"));
        let menu = Paragraph::new("1. Yes (recommended)   2. No").block(menu_block);
        frame.render_widget(menu, input_area);
    } else {
        let input_block = Block::bordered().title("input");
        let input_inner = input_block.inner(input_area);
        // Only the most recently wrapped rows are shown once there are
        // more than MAX_INPUT_ROWS of them — the tail is where the
        // cursor is (always at the end of the buffer, still), so
        // that's what needs to stay visible, matching how typing past
        // the edge of a normal terminal input behaves.
        let visible_start = input_wrapped_lines.len().saturating_sub(MAX_INPUT_ROWS);
        let input_text = input_wrapped_lines[visible_start..].join("\n");
        let input = Paragraph::new(input_text).block(input_block);
        frame.render_widget(input, input_area);

        // The cursor is always at the end of the buffer (InputBox is
        // append/backspace-only) — so it's always on the last wrapped
        // row, at that row's own character count, not the whole
        // buffer's. input_content_rows already accounts for the same
        // tail-slicing the rendered text above uses, so "last row" is
        // always the last *visible* row too, even once wrapping
        // exceeds MAX_INPUT_ROWS.
        let last_line_chars = input_wrapped_lines
            .last()
            .map(|line| line.chars().count())
            .unwrap_or(0);
        let cursor_x = input_inner.x + last_line_chars as u16;
        let cursor_y = input_inner.y + input_content_rows.saturating_sub(1);
        frame.set_cursor_position((cursor_x, cursor_y));
    }
}

/// The single-line text buffer backing the chat input box.
#[derive(Debug, Default)]
pub struct InputBox {
    buffer: String,
}

impl InputBox {
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies one key press: typed characters are appended,
    /// `Backspace` removes the last one, everything else is ignored.
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

    /// The buffer's current contents.
    pub fn buffer(&self) -> &str {
        &self.buffer
    }

    // Leaves an empty buffer in place, returning what it held.
    fn take(&mut self) -> String {
        std::mem::take(&mut self.buffer)
    }
}

/// Which provider is active, shown in the chat title for the whole
/// session — a static, startup-time label, not a live switch (provider
/// selection stays a one-time CLI choice).
// Its own enum rather than reusing cli::Provider directly, so tui
// doesn't take on a dependency on the cli module for what is, from
// tui's perspective, just display text.
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

// Kept terse deliberately: this codebase already hit real truncation
// once (see the widened TestBackend width in this function's own
// tests) from adding text to a title bar that's already tight in a
// narrow tmux pane.
fn agents_md_title_suffix(status: AgentsMdStatus) -> &'static str {
    match (status.project_found, status.global_found) {
        (false, false) => "no AGENTS.md",
        (true, false) => "AGENTS.md (project)",
        (false, true) => "AGENTS.md (global)",
        (true, true) => "AGENTS.md (project+global)",
    }
}

/// All UI state for one session: the chat log, input buffer, scroll
/// position, and the [`crate::core::Core`] it drives. Construct with
/// [`App::new`] (local Ollama) or [`App::with_core`] (any `Core`),
/// feed it key events via [`App::handle_key`], and render it with
/// [`draw`].
pub struct App {
    input: InputBox,
    log: Vec<LogEntry>,
    core: Core,
    provider_label: ProviderLabel,
    agents_md_status: AgentsMdStatus,
    scroll_offset: usize,
    // True ceiling for scroll_offset, as of the last draw call. Stale by
    // at most one frame — see scroll_up's doc comment.
    max_scroll: usize,
    // Some(path) while a write_file confirmation is outstanding; None
    // otherwise. Presence alone (not a separate bool) drives both
    // App::handle_key's input routing and draw's menu-vs-input-box
    // choice, since both need the same "am I waiting" fact and the menu
    // additionally needs the path to render its title.
    pending_confirmation: Option<String>,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    /// An app talking to local Ollama, with the chat title's
    /// `AGENTS.md` status defaulted to "none found" — real callers
    /// should use [`App::with_core`] with the status `Core` actually
    /// computed.
    pub fn new() -> Self {
        Self::with_core(
            Core::new(),
            ProviderLabel::Ollama,
            AgentsMdStatus {
                project_found: false,
                global_found: false,
            },
        )
    }

    /// An app wrapping an already-constructed `Core` — `provider_label`
    /// and `agents_md_status` are passed in explicitly, not derived
    /// from `core`, so they can be set without depending on `Core`'s
    /// own real construction (real file I/O, a real provider choice).
    pub fn with_core(
        core: Core,
        provider_label: ProviderLabel,
        agents_md_status: AgentsMdStatus,
    ) -> Self {
        Self {
            input: InputBox::new(),
            log: Vec::new(),
            core,
            provider_label,
            agents_md_status,
            scroll_offset: 0,
            max_scroll: 0,
            pending_confirmation: None,
        }
    }

    /// Applies one key press: scrolling, the confirmation menu
    /// (1/2/Enter) while a write is pending, `Enter` to submit a
    /// message, or plain typing otherwise.
    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }

        match key.code {
            // Scrolling always works, pending confirmation or not — a
            // long diff should be reviewable before deciding.
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
            // While awaiting confirmation, every other key is routed
            // here instead of falling through to normal chat input —
            // 1/2 act immediately (no Enter needed), Enter alone
            // defaults to the recommended choice (1/Apply), anything
            // else is simply ignored rather than reaching InputBox.
            _ if self.pending_confirmation.is_some() => match key.code {
                KeyCode::Char('1') | KeyCode::Enter => {
                    self.resolve_confirmation(ConfirmationChoice::Apply)
                }
                KeyCode::Char('2') => self.resolve_confirmation(ConfirmationChoice::Decline),
                _ => {}
            },
            KeyCode::Enter => self.submit(),
            _ => self.input.handle_key(key),
        }
    }

    fn resolve_confirmation(&mut self, choice: ConfirmationChoice) {
        self.core.respond_to_confirmation(choice);
        self.pending_confirmation = None;
    }

    fn submit(&mut self) {
        let text = self.input.take();
        if text.is_empty() {
            return;
        }

        self.core.submit_user_message(text.clone());
        self.log.push(LogEntry::Text(format!("you: {text}")));
        self.scroll_offset = 0;
    }

    /// Drains `Core`'s pending events and appends them to the chat log
    /// — call this on every UI tick.
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
            match event {
                // Intercepted here rather than going through
                // format_core_event — this needs the real colored diff
                // (as its own LogEntry::Diff) and to flip pending
                // confirmation state, neither of which a pure
                // CoreEvent -> String mapping can do.
                CoreEvent::WriteProposed { path, diff } => {
                    self.pending_confirmation = Some(path.clone());
                    self.log.push(LogEntry::Diff { path, diff });
                }
                other => self.log.push(LogEntry::Text(format_core_event(other))),
            }
        }
    }

    /// The chat log rendered so far, in order.
    pub fn log(&self) -> &[LogEntry] {
        &self.log
    }

    /// The path of a `write_file` call awaiting apply/decline, if any.
    pub fn pending_confirmation(&self) -> Option<&str> {
        self.pending_confirmation.as_deref()
    }

    /// The active provider, for the chat title.
    pub fn provider_label(&self) -> ProviderLabel {
        self.provider_label
    }

    /// Whether an `AGENTS.md` was found, for the chat title.
    pub fn agents_md_status(&self) -> AgentsMdStatus {
        self.agents_md_status
    }

    /// The chat input box's current contents.
    pub fn input_buffer(&self) -> &str {
        self.input.buffer()
    }

    /// How many lines up from the bottom the chat log is scrolled.
    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    /// Sets the scroll ceiling — called by [`draw`] once it knows how
    /// many rendered lines actually exist, since that isn't known
    /// until render time.
    pub fn set_max_scroll(&mut self, max_scroll: usize) {
        self.max_scroll = max_scroll;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::DiffLineText;

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
        // 70, not 40 — the title bar has to fit both the provider label
        // and the AGENTS.md status suffix without truncating; 40 is
        // already too narrow for the provider label by itself.
        let backend = TestBackend::new(70, 6);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = App::with_core(
            Core::new(),
            ProviderLabel::Ollama,
            AgentsMdStatus {
                project_found: true,
                global_found: true,
            },
        );
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
        assert!(
            content.contains("AGENTS.md (project+global)"),
            "expected the AGENTS.md status to render: {content:?}"
        );
    }

    #[test]
    fn draws_mistral_as_the_cloud_provider_in_the_chat_title() {
        let backend = TestBackend::new(70, 6);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = App::with_core(
            Core::new(),
            ProviderLabel::Mistral,
            AgentsMdStatus {
                project_found: false,
                global_found: false,
            },
        );
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
        assert!(
            content.contains("no AGENTS.md"),
            "expected the AGENTS.md status to render: {content:?}"
        );
    }

    #[test]
    fn agents_md_title_suffix_covers_all_four_combinations() {
        assert_eq!(
            agents_md_title_suffix(AgentsMdStatus {
                project_found: false,
                global_found: false
            }),
            "no AGENTS.md"
        );
        assert_eq!(
            agents_md_title_suffix(AgentsMdStatus {
                project_found: true,
                global_found: false
            }),
            "AGENTS.md (project)"
        );
        assert_eq!(
            agents_md_title_suffix(AgentsMdStatus {
                project_found: false,
                global_found: true
            }),
            "AGENTS.md (global)"
        );
        assert_eq!(
            agents_md_title_suffix(AgentsMdStatus {
                project_found: true,
                global_found: true
            }),
            "AGENTS.md (project+global)"
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

    // Input area grows from its fixed 3 rows once the typed text needs
    // more than one wrapped row — proves both the height computation
    // and the Paragraph actually wrapping (rather than clipping) text
    // that used to run off the right edge. No spaces in the typed
    // text, so wrap_line hard-breaks exactly at the inner width (18,
    // for a 20-wide backend minus 2 border columns) rather than
    // backing up to a word boundary — keeps the expected wrap point
    // exact rather than dependent on where a space happens to fall.
    #[test]
    fn input_area_grows_and_wraps_once_text_exceeds_one_row() {
        let backend = TestBackend::new(20, 6);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new();

        let typed = "abcdefghijklmnopqrst"; // 20 chars: wraps to 18 + 2
        for c in typed.chars() {
            app.handle_key(press(KeyCode::Char(c)));
        }

        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let buffer = terminal.backend().buffer();
        let row = |y: u16| -> String {
            (1u16..19)
                .map(|x| buffer.content()[y as usize * 20 + x as usize].symbol())
                .collect()
        };

        assert_eq!(
            row(3).trim_end(),
            "abcdefghijklmnopqr",
            "expected the first wrapped row to fill the input area's inner width"
        );
        assert_eq!(
            row(4).trim_end(),
            "st",
            "expected the text that used to be clipped to now wrap onto a second row"
        );
    }

    // The cursor is still conceptually "at the end of the buffer"
    // (InputBox is append/backspace-only, unchanged) — this only fixes
    // how that position renders once the buffer wraps to more than one
    // row. Same 20-char/18+2-wrap setup as
    // input_area_grows_and_wraps_once_text_exceeds_one_row: the second
    // wrapped row ("st") lands on the input area's second inner row
    // (y=4), so the cursor should land right after it there — not
    // still pinned to the first row, and not walked off using the
    // whole buffer's raw character count (the old formula's bug).
    #[test]
    fn cursor_lands_on_the_last_wrapped_row_once_input_wraps() {
        let backend = TestBackend::new(20, 6);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new();

        let typed = "abcdefghijklmnopqrst"; // wraps to "abcdefghijklmnopqr" + "st"
        for c in typed.chars() {
            app.handle_key(press(KeyCode::Char(c)));
        }

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

        assert_eq!(app.log(), &[LogEntry::Text("you: hi".to_string())]);
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

    // A write_file call's `content` argument can be an entire file —
    // dumping it raw into the chat log makes the line unreadable (this
    // is the actual bug: found manually testing edit_file, triggered by
    // asking a real model to write a long ARCHITECTURE.md). The short
    // `path` value stays exactly as sent; only the long `content` value
    // is replaced, with a placeholder that still reports how long it
    // was — not a per-tool rule, just "any JSON string value over 80
    // chars gets hidden," so this applies identically no matter which
    // tool sent it.
    #[test]
    fn formats_a_tool_call_by_truncating_long_argument_values_only() {
        let long_content = "x".repeat(500);
        let arguments = format!(r#"{{"path":"big.txt","content":"{long_content}"}}"#);

        let formatted = format_core_event(CoreEvent::ToolCall {
            id: "call_1".to_string(),
            name: "write_file".to_string(),
            arguments,
            result: Ok("wrote big.txt".to_string()),
        });

        assert!(
            formatted.contains(r#""path":"big.txt""#),
            "short values should still be shown as-is: {formatted}"
        );
        assert!(
            formatted.contains("<500 chars>"),
            "a long value should be replaced with a length placeholder: {formatted}"
        );
        assert!(
            !formatted.contains(&long_content),
            "the actual long content must never reach the log: {formatted}"
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

    // Tests render_diff_lines directly against constructed DiffLine
    // data, independent of how a real diff reaches the chat log.
    #[test]
    fn render_diff_lines_colors_added_and_removed_line_backgrounds_and_leaves_unchanged_plain() {
        use ratatui::style::Color;

        let diff = vec![
            DiffLine::Unchanged(line("same")),
            DiffLine::Removed(line("old")),
            DiffLine::Added(line("new")),
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
        assert_eq!(buffer[(0, 0)].bg, Color::Reset);
        assert_eq!(buffer[(0, 1)].bg, Color::Red);
        assert_eq!(buffer[(0, 2)].bg, Color::Green);
    }

    // The +/-/space text prefix must survive alongside the color, so a
    // terminal without color support (or a color-blind user) still gets
    // a real signal, not just an invisible-without-color distinction.
    #[test]
    fn render_diff_lines_keeps_a_text_prefix_alongside_color() {
        let diff = vec![
            DiffLine::Unchanged(line("same")),
            DiffLine::Removed(line("old")),
            DiffLine::Added(line("new")),
        ];

        let lines = render_diff_lines(&diff);
        let texts: Vec<String> = lines.iter().map(|line| line.to_string()).collect();

        assert_eq!(texts, vec![" same", "-old", "+new"]);
    }

    // git's own convention for a missing final newline: a separate,
    // unstyled annotation line immediately after the affected one —
    // not folded into the line's own text or colored, since it's a
    // note about the file, not part of its content.
    #[test]
    fn render_diff_lines_annotates_a_line_missing_its_trailing_newline() {
        let diff = vec![DiffLine::Removed(line_missing_newline("old"))];

        let lines = render_diff_lines(&diff);
        let texts: Vec<String> = lines.iter().map(|line| line.to_string()).collect();

        assert_eq!(texts, vec!["-old", "\\ No newline at end of file"]);
    }

    // A run of N context-skipped lines renders as a single marker line,
    // distinct from any real diff content — "line"/"lines" is
    // pluralized on the actual count, not hardcoded, since a windowed
    // diff's smallest possible elision is exactly one line.
    #[test]
    fn render_diff_lines_shows_an_elided_marker_pluralized_by_count() {
        let diff = vec![DiffLine::Elided(1), DiffLine::Elided(3)];

        let lines = render_diff_lines(&diff);
        let texts: Vec<String> = lines.iter().map(|line| line.to_string()).collect();

        assert_eq!(texts, vec!["⋯ 1 unchanged line ⋯", "⋯ 3 unchanged lines ⋯"]);
    }

    // Dimmed, not background-colored like Added/Removed — an elided
    // marker isn't real file content (nothing about it will ever be
    // syntax-highlighted), so it doesn't need the background channel
    // content lines reserve for that; a muted foreground is enough to
    // read as "not a real line."
    #[test]
    fn render_diff_lines_dims_an_elided_marker_instead_of_coloring_its_background() {
        use ratatui::style::Color;

        let diff = vec![DiffLine::Elided(2)];

        let lines = render_diff_lines(&diff);

        let backend = TestBackend::new(30, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(Paragraph::new(lines.clone()), frame.area());
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 0)].bg, Color::Reset);
        assert_eq!(buffer[(0, 0)].fg, Color::DarkGray);
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
    fn write_proposed_event_sets_pending_confirmation() {
        let mut app = App::new();

        app.apply_core_events(vec![CoreEvent::WriteProposed {
            path: "notes.txt".to_string(),
            diff: vec![DiffLine::Added(line("hello"))],
        }]);

        assert_eq!(app.pending_confirmation(), Some("notes.txt"));
    }

    #[test]
    fn pressing_1_while_awaiting_confirmation_resolves_it() {
        let mut app = App::new();
        app.apply_core_events(vec![CoreEvent::WriteProposed {
            path: "notes.txt".to_string(),
            diff: vec![],
        }]);

        app.handle_key(press(KeyCode::Char('1')));

        assert_eq!(app.pending_confirmation(), None);
    }

    #[test]
    fn pressing_enter_while_awaiting_confirmation_defaults_to_apply() {
        let mut app = App::new();
        app.apply_core_events(vec![CoreEvent::WriteProposed {
            path: "notes.txt".to_string(),
            diff: vec![],
        }]);

        app.handle_key(press(KeyCode::Enter));

        assert_eq!(app.pending_confirmation(), None);
    }

    #[test]
    fn pressing_2_while_awaiting_confirmation_resolves_it() {
        let mut app = App::new();
        app.apply_core_events(vec![CoreEvent::WriteProposed {
            path: "notes.txt".to_string(),
            diff: vec![],
        }]);

        app.handle_key(press(KeyCode::Char('2')));

        assert_eq!(app.pending_confirmation(), None);
    }

    #[test]
    fn typing_while_awaiting_confirmation_does_not_reach_the_input_box() {
        let mut app = App::new();
        app.apply_core_events(vec![CoreEvent::WriteProposed {
            path: "notes.txt".to_string(),
            diff: vec![],
        }]);

        app.handle_key(press(KeyCode::Char('h')));
        app.handle_key(press(KeyCode::Char('i')));

        assert_eq!(app.input_buffer(), "");
        assert_eq!(app.pending_confirmation(), Some("notes.txt"));
    }

    #[test]
    fn scrolling_still_works_while_awaiting_confirmation() {
        let mut app = App::new();
        let words: Vec<String> = (1..=40).map(|n| format!("w{n}")).collect();
        type_and_submit(&mut app, &words.join(" "));
        rendered_content(&mut app);
        app.apply_core_events(vec![CoreEvent::WriteProposed {
            path: "notes.txt".to_string(),
            diff: vec![],
        }]);

        app.handle_key(press(KeyCode::Up));

        assert_eq!(app.scroll_offset(), 1);
    }

    // The point of this test: the colored diff and numbered menu
    // actually reach the rendered frame via App::apply_core_events +
    // draw, not just that the underlying pieces (render_diff_lines,
    // pending_confirmation) work in isolation.
    #[test]
    fn draws_a_write_proposal_with_colored_diff_and_confirmation_menu() {
        use ratatui::style::Color;

        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = App::new();
        app.apply_core_events(vec![CoreEvent::WriteProposed {
            path: "notes.txt".to_string(),
            diff: vec![
                DiffLine::Removed(line("old line")),
                DiffLine::Added(line("new line")),
            ],
        }]);
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let buffer = terminal.backend().buffer();
        let content: String = buffer.content().iter().map(|cell| cell.symbol()).collect();

        assert!(
            content.contains("notes.txt"),
            "expected the proposed path to render: {content:?}"
        );
        assert!(
            content.contains("old line") && content.contains("new line"),
            "expected the diff's line text to render: {content:?}"
        );
        assert!(
            content.contains("Yes") && content.contains("No"),
            "expected the numbered confirmation menu to render: {content:?}"
        );
        assert!(
            buffer.content().iter().any(|cell| cell.bg == Color::Red),
            "expected at least one cell with the removed-line background color"
        );
        assert!(
            buffer.content().iter().any(|cell| cell.bg == Color::Green),
            "expected at least one cell with the added-line background color"
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
