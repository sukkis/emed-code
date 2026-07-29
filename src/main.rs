use emed_code::tui::{App, draw, is_quit_key};
use ratatui::crossterm::event::{self, Event};

fn main() -> std::io::Result<()> {
    ratatui::run(|terminal| {
        let mut app = App::new();

        loop {
            terminal.draw(|frame| draw(frame, &app))?;

            if let Event::Key(key) = event::read()? {
                if is_quit_key(&key) {
                    break Ok(());
                }
                app.handle_key(key);
            }
        }
    })
}
