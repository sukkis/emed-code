// ratatui rendering and crossterm input handling. Talks to `core` only
// through Core::submit_user_message/poll_events.

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::widgets::{Block, Paragraph};

pub fn draw(frame: &mut Frame) {
    let layout = Layout::vertical([Constraint::Min(1), Constraint::Length(3)]);
    let [chat_area, input_area] = frame.area().layout(&layout);

    let chat = Paragraph::new("").block(Block::bordered().title("emed-code"));
    frame.render_widget(chat, chat_area);

    let input = Paragraph::new("").block(Block::bordered().title("input"));
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

        terminal.draw(|frame| draw(frame)).unwrap();

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
}
