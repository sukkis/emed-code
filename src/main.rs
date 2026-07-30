use emed_code::tui::{App, draw, is_quit_key};
use ratatui::crossterm::event::{self, Event};
use std::time::Duration;

fn main() -> std::io::Result<()> {
    ratatui::run(|terminal| {
        let mut app = App::new();

        loop {
            if event::poll(Duration::from_millis(100))?
                && let Event::Key(key) = event::read()?
            {
                if is_quit_key(&key) {
                    break Ok(());
                }
                app.handle_key(key);
            }

            app.poll_core_events();
            terminal.draw(|frame| draw(frame, &mut app))?;
        }
    })
}
