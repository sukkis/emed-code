use clap::Parser;
use emed_code::cli::{Cli, Provider};
use emed_code::core::{
    AnthropicClient, Core, MistralClient, OllamaClient, Settings, anthropic_credential_log_message,
    credential_log_message, lookup_anthropic_api_key, lookup_mistral_api_key,
};
use emed_code::tui::{App, ProviderLabel, draw, is_quit_key};
use ratatui::crossterm::event::{self, Event};
use std::io;
use std::sync::Arc;
use std::time::Duration;

fn main() -> io::Result<()> {
    let cli = Cli::parse();
    let model = cli.resolved_model();

    let provider_label = match cli.provider {
        Provider::Ollama => ProviderLabel::Ollama,
        Provider::Mistral => ProviderLabel::Mistral,
        Provider::Anthropic => ProviderLabel::Anthropic,
    };

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
        Provider::Anthropic => {
            let (api_key, source) = lookup_anthropic_api_key().ok_or_else(|| {
                io::Error::other(
                    "no Anthropic API key found (set ANTHROPIC_API_KEY, or add \
                     emed-code/anthropic/api_key via getfrompass)",
                )
            })?;
            println!("{}", anthropic_credential_log_message(&source));
            // A second Settings::load() beyond Core::with_client's own
            // internal one — main.rs needs anthropic_thinking before
            // Core::with_client exists to construct the client it's
            // about to receive. Two tiny startup file reads, not a
            // per-request cost; see docs/anthropic-provider.md.
            let thinking = Settings::load().anthropic_thinking;
            Core::with_client(Arc::new(AnthropicClient::new(api_key, model, thinking)))
        }
    };

    let agents_md_status = core.agents_md_status();

    ratatui::run(|terminal| {
        let mut app = App::with_core(core, provider_label, agents_md_status);

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
