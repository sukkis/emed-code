// ratatui rendering and crossterm input handling. Talks to `core` only
// through Core::submit_user_message/poll_events.

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::widgets::{Block, Paragraph, Wrap};
use unicode_width::UnicodeWidthChar;

use crate::core::{Core, CoreEvent};

const SCROLL_STEP: usize = 1;
const PAGE_SCROLL_STEP: usize = 5;

fn format_core_event(event: CoreEvent) -> String {
    match event {
        CoreEvent::AssistantChunk(text) => format!("emed-code: {text}"),
        CoreEvent::Error(message) => format!("error: {message}"),
    }
}

// offset is "how many lines scrolled up from the bottom" (0 = latest).
// Clamped to the last line index so it can't scroll past the top —
// including when the log is empty, where that index is 0.
fn scroll_up(offset: usize, log_len: usize, step: usize) -> usize {
    let max = log_len.saturating_sub(1);
    (offset + step).min(max)
}

fn scroll_down(offset: usize, step: usize) -> usize {
    offset.saturating_sub(step)
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

pub fn is_quit_key(key: &KeyEvent) -> bool {
    key.kind == KeyEventKind::Press
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('q'))
}

pub fn draw(frame: &mut Frame, app: &App) {
    let layout = Layout::vertical([Constraint::Min(1), Constraint::Length(3)]);
    let [chat_area, input_area] = frame.area().layout(&layout);

    let chat_block = Block::bordered().title("emed-code");
    let chat_inner = chat_block.inner(chat_area);
    let total_lines = app.log().len();
    let visible_height = chat_inner.height as usize;
    let max_skip = total_lines.saturating_sub(visible_height);
    let skip = max_skip.saturating_sub(app.scroll_offset()) as u16;

    let chat_text = app.log().join("\n");
    let chat = Paragraph::new(chat_text)
        .block(chat_block)
        .wrap(Wrap { trim: false })
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

pub struct App {
    input: InputBox,
    log: Vec<String>,
    core: Core,
    scroll_offset: usize,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        Self {
            input: InputBox::new(),
            log: Vec::new(),
            core: Core::new(),
            scroll_offset: 0,
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }

        match key.code {
            KeyCode::Enter => self.submit(),
            KeyCode::Up => {
                self.scroll_offset = scroll_up(self.scroll_offset, self.log.len(), SCROLL_STEP)
            }
            KeyCode::Down => self.scroll_offset = scroll_down(self.scroll_offset, SCROLL_STEP),
            KeyCode::PageUp => {
                self.scroll_offset = scroll_up(self.scroll_offset, self.log.len(), PAGE_SCROLL_STEP)
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
        if events.is_empty() {
            return;
        }

        for event in events {
            self.log.push(format_core_event(event));
        }
        self.scroll_offset = 0;
    }

    pub fn log(&self) -> &[String] {
        &self.log
    }

    pub fn input_buffer(&self) -> &str {
        self.input.buffer()
    }

    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
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

        let app = App::new();
        terminal.draw(|frame| draw(frame, &app)).unwrap();

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

        terminal.draw(|frame| draw(frame, &app)).unwrap();

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

    // Input area is the bottom 3 rows of a 20x6 backend (Length(3)),
    // bordered on all sides, so its inner content row is (1..19, 4).
    #[test]
    fn cursor_is_at_the_start_of_the_input_box_when_empty() {
        let backend = TestBackend::new(20, 6);
        let mut terminal = Terminal::new(backend).unwrap();
        let app = App::new();

        terminal.draw(|frame| draw(frame, &app)).unwrap();

        terminal.backend_mut().assert_cursor_position((1, 4));
    }

    #[test]
    fn cursor_is_positioned_after_the_typed_text() {
        let backend = TestBackend::new(20, 6);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new();
        app.handle_key(press(KeyCode::Char('h')));
        app.handle_key(press(KeyCode::Char('i')));

        terminal.draw(|frame| draw(frame, &app)).unwrap();

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

    #[test]
    fn scroll_up_increases_offset_by_step() {
        assert_eq!(scroll_up(0, 10, 1), 1);
    }

    #[test]
    fn scroll_up_is_clamped_to_the_last_line_index() {
        assert_eq!(scroll_up(0, 10, 100), 9);
    }

    #[test]
    fn scroll_up_on_an_empty_log_stays_at_zero() {
        assert_eq!(scroll_up(0, 0, 1), 0);
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
    fn up_key_does_nothing_on_an_empty_log() {
        let mut app = App::new();

        app.handle_key(press(KeyCode::Up));

        assert_eq!(app.scroll_offset(), 0);
    }

    #[test]
    fn up_key_scrolls_up_and_down_key_scrolls_back() {
        let mut app = App::new();
        for text in ["a", "b", "c"] {
            for c in text.chars() {
                app.handle_key(press(KeyCode::Char(c)));
            }
            app.handle_key(press(KeyCode::Enter));
        }
        assert_eq!(app.scroll_offset(), 0);

        app.handle_key(press(KeyCode::Up));
        assert_eq!(app.scroll_offset(), 1);

        app.handle_key(press(KeyCode::Down));
        assert_eq!(app.scroll_offset(), 0);
    }

    #[test]
    fn page_up_scrolls_by_more_than_one_line() {
        let mut app = App::new();
        for text in ["a", "b", "c", "d", "e", "f", "g", "h"] {
            for c in text.chars() {
                app.handle_key(press(KeyCode::Char(c)));
            }
            app.handle_key(press(KeyCode::Enter));
        }

        app.handle_key(press(KeyCode::PageUp));

        assert!(app.scroll_offset() > 1);
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
}
