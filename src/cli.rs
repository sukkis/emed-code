//! Command-line flag parsing.
//!
//! No conversation state ([`crate::core`]) or rendering/input state
//! ([`crate::tui`]) here — just turning argv into a resolved provider
//! and model choice for `main.rs` to act on.

use clap::{Parser, ValueEnum};

/// Which LLM backend to talk to.
#[derive(Debug, Clone, Copy, PartialEq, ValueEnum)]
pub enum Provider {
    Ollama,
    Mistral,
    Anthropic,
}

/// emed-code — a terminal AI coding assistant.
///
/// Mistral needs an API key: a `getfrompass` entry
/// (`emed-code/mistral/api_key`) or the `MISTRAL_API_KEY` env var.
#[derive(Debug, Parser)]
pub struct Cli {
    /// LLM provider to talk to (defaults to local Ollama)
    #[arg(long, value_enum, default_value_t = Provider::Ollama)]
    pub provider: Provider,

    /// Model name, e.g. "mistral-medium-latest" for Mistral (defaults to
    /// a sensible model per provider)
    #[arg(long)]
    pub model: Option<String>,
}

impl Cli {
    /// The model to actually use: `--model` if given, otherwise a
    /// sensible per-provider default.
    pub fn resolved_model(&self) -> String {
        self.model
            .clone()
            .unwrap_or_else(|| default_model(self.provider).to_string())
    }
}

fn default_model(provider: Provider) -> &'static str {
    match provider {
        Provider::Ollama => crate::core::OLLAMA_MODEL,
        Provider::Mistral => crate::core::MISTRAL_MODEL,
        Provider::Anthropic => crate::core::ANTHROPIC_MODEL,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn cli_defaults_to_ollama_and_its_default_model_when_no_flags_given() {
        let cli = Cli::try_parse_from(["emed-code"]).unwrap();

        assert_eq!(cli.provider, Provider::Ollama);
        assert_eq!(cli.resolved_model(), "mistral-nemo");
    }

    #[test]
    fn cli_defaults_to_mistral_small_latest_when_provider_is_mistral_and_no_model_given() {
        let cli = Cli::try_parse_from(["emed-code", "--provider", "mistral"]).unwrap();

        assert_eq!(cli.provider, Provider::Mistral);
        assert_eq!(cli.resolved_model(), "mistral-small-latest");
    }

    #[test]
    fn cli_defaults_to_claude_sonnet_5_when_provider_is_anthropic_and_no_model_given() {
        let cli = Cli::try_parse_from(["emed-code", "--provider", "anthropic"]).unwrap();

        assert_eq!(cli.provider, Provider::Anthropic);
        assert_eq!(cli.resolved_model(), "claude-sonnet-5");
    }

    #[test]
    fn cli_model_flag_overrides_the_default() {
        let cli = Cli::try_parse_from([
            "emed-code",
            "--provider",
            "mistral",
            "--model",
            "mistral-medium-latest",
        ])
        .unwrap();

        assert_eq!(cli.resolved_model(), "mistral-medium-latest");
    }

    #[test]
    fn cli_rejects_an_unrecognized_provider_value() {
        let result = Cli::try_parse_from(["emed-code", "--provider", "bogus"]);

        assert!(result.is_err());
    }
}
