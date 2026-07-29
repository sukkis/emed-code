// ratatui rendering and crossterm input handling. Talks to `core` only
// through Core::submit_user_message/poll_events.

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::widgets::{Block, Paragraph};

use crate::core::Core;

pub fn is_quit_key(key: &KeyEvent) -> bool {
    key.kind == KeyEventKind::Press
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('q'))
}

pub fn draw(frame: &mut Frame, app: &App) {
    let layout = Layout::vertical([Constraint::Min(1), Constraint::Length(3)]);
    let [chat_area, input_area] = frame.area().layout(&layout);

    let chat_text = app.log().join("\n");
    let chat = Paragraph::new(chat_text).block(Block::bordered().title("emed-code"));
    frame.render_widget(chat, chat_area);

    let input = Paragraph::new(app.input_buffer()).block(Block::bordered().title("input"));
    frame.render_widget(input, input_area);
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
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }

        match key.code {
            KeyCode::Enter => self.submit(),
            _ => self.input.handle_key(key),
        }
    }

    fn submit(&mut self) {
        let text = self.input.take();
        if text.is_empty() {
            return;
        }

        self.core.submit_user_message(text.clone());
        self.log.push(text);
    }

    pub fn log(&self) -> &[String] {
        &self.log
    }

    pub fn input_buffer(&self) -> &str {
        self.input.buffer()
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

        assert_eq!(app.log(), &["hi".to_string()]);
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
}
