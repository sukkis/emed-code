// ratatui rendering and crossterm input handling. Talks to `core` only
// through Core::submit_user_message/poll_events.

use ratatui::Frame;
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
}
