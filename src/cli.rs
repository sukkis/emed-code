// Command-line flag parsing. No conversation state (core) or
// rendering/input state (tui) here — just turning argv into a resolved
// provider + model choice for main.rs to act on.

use clap::{Parser, ValueEnum};

#[derive(Debug, Clone, Copy, PartialEq, ValueEnum)]
pub enum Provider {
    Ollama,
    Mistral,
}

#[derive(Debug, Parser)]
pub struct Cli {
    #[arg(long, value_enum, default_value_t = Provider::Ollama)]
    pub provider: Provider,

    #[arg(long)]
    pub model: Option<String>,
}

impl Cli {
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
    fn cli_model_flag_overrides_the_default() {
        let cli = Cli::try_parse_from([
            "emed-code",
            "--provider",
            "mistral",
            "--model",
            "codestral-latest",
        ])
        .unwrap();

        assert_eq!(cli.resolved_model(), "codestral-latest");
    }

    #[test]
    fn cli_rejects_an_unrecognized_provider_value() {
        let result = Cli::try_parse_from(["emed-code", "--provider", "bogus"]);

        assert!(result.is_err());
    }
}
