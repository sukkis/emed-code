use emed_code::tui::draw;

fn main() -> std::io::Result<()> {
    ratatui::run(|terminal| {
        loop {
            terminal.draw(draw)?;
            if ratatui::crossterm::event::read()?.is_key_press() {
                break Ok(());
            }
        }
    })
}
