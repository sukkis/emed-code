use clap::Parser;
use emed_code::cli::{Cli, Provider};
use emed_code::core::{
    Core, MistralClient, OllamaClient, credential_log_message, lookup_mistral_api_key,
};
use emed_code::tui::{App, draw, is_quit_key};
use ratatui::crossterm::event::{self, Event};
use std::io;
use std::sync::Arc;
use std::time::Duration;

fn main() -> io::Result<()> {
    let cli = Cli::parse();
    let model = cli.resolved_model();

    let core = match cli.provider {
        Provider::Ollama => Core::with_client(Arc::new(OllamaClient::new(model))),
        Provider::Mistral => {
            let (api_key, source) = lookup_mistral_api_key().ok_or_else(|| {
                io::Error::other(
                    "no Mistral API key found (set MISTRAL_API_KEY, or add \
                     emed-code/mistral/api_key via getfrompass)",
                )
            })?;
            println!("{}", credential_log_message(&source));
            Core::with_client(Arc::new(MistralClient::new(api_key, model)))
        }
    };

    ratatui::run(|terminal| {
        let mut app = App::with_core(core);

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
