// User-adjustable settings, read from ~/.config/emed-code/settings.toml
// (platform-equivalent path via dirs::config_dir()) — deliberately
// outside the sandboxed project root, so a future write_file tool can
// never reach and self-modify it. A missing or malformed file fails
// safe to every field's default (Strict here), never panics and never
// fails open.
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum FileAccessSecurity {
    #[default]
    Strict,
    Loose,
}

// Mirrors FileAccessSecurity's shape exactly. Anthropic-specific today
// (Claude Sonnet 5 runs adaptive thinking by default unless told
// otherwise — see docs/anthropic-provider.md design question 9), but
// named for the concept, not the provider, the same way ChatError's
// ResponseTruncated/Refused variants are.
#[derive(Debug, Clone, Copy, PartialEq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum AnthropicThinking {
    #[default]
    Disabled,
    Adaptive,
}

#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub(crate) struct Settings {
    #[serde(default)]
    pub(crate) file_access_security: FileAccessSecurity,
    #[serde(default)]
    pub(crate) anthropic_thinking: AnthropicThinking,
}

impl Settings {
    // Pure: no filesystem access, so this is what the tests below cover
    // directly. Any parse failure (malformed TOML, an unrecognized
    // file_access_security value) is treated the same as a missing
    // file — default() — rather than surfaced as an error anywhere.
    pub(crate) fn parse(toml_str: &str) -> Settings {
        basic_toml::from_str(toml_str).unwrap_or_default()
    }

    // The real entry point: resolves the real config path and reads it.
    // Not unit-tested directly, same as Core::with_client's real
    // std::env::current_dir() call — only the pure logic around it
    // (parse, above) is.
    pub(crate) fn load() -> Settings {
        let contents = dirs::config_dir()
            .map(|dir| dir.join("emed-code").join("settings.toml"))
            .and_then(|path| std::fs::read_to_string(path).ok());

        match contents {
            Some(toml_str) => Settings::parse(&toml_str),
            None => Settings::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_defaults_to_strict_for_an_empty_file() {
        let settings = Settings::parse("");

        assert_eq!(settings.file_access_security, FileAccessSecurity::Strict);
    }

    #[test]
    fn parse_reads_an_explicit_strict_value() {
        let settings = Settings::parse(r#"file_access_security = "strict""#);

        assert_eq!(settings.file_access_security, FileAccessSecurity::Strict);
    }

    #[test]
    fn parse_reads_an_explicit_loose_value() {
        let settings = Settings::parse(r#"file_access_security = "loose""#);

        assert_eq!(settings.file_access_security, FileAccessSecurity::Loose);
    }

    #[test]
    fn parse_defaults_to_strict_for_malformed_toml() {
        let settings = Settings::parse("this is not valid toml {{{");

        assert_eq!(settings.file_access_security, FileAccessSecurity::Strict);
    }

    #[test]
    fn parse_defaults_to_strict_for_an_unrecognized_value() {
        let settings = Settings::parse(r#"file_access_security = "yolo""#);

        assert_eq!(settings.file_access_security, FileAccessSecurity::Strict);
    }

    // anthropic_thinking mirrors file_access_security's own tests above —
    // same fail-safe-to-default shape. The malformed-TOML fallback case
    // isn't re-tested here: Settings::default() covers both fields via
    // the same code path already proven by
    // parse_defaults_to_strict_for_malformed_toml above.

    #[test]
    fn parse_defaults_to_disabled_for_an_empty_file() {
        let settings = Settings::parse("");

        assert_eq!(settings.anthropic_thinking, AnthropicThinking::Disabled);
    }

    #[test]
    fn parse_reads_an_explicit_disabled_value() {
        let settings = Settings::parse(r#"anthropic_thinking = "disabled""#);

        assert_eq!(settings.anthropic_thinking, AnthropicThinking::Disabled);
    }

    #[test]
    fn parse_reads_an_explicit_adaptive_value() {
        let settings = Settings::parse(r#"anthropic_thinking = "adaptive""#);

        assert_eq!(settings.anthropic_thinking, AnthropicThinking::Adaptive);
    }

    #[test]
    fn parse_defaults_to_disabled_for_an_unrecognized_value() {
        let settings = Settings::parse(r#"anthropic_thinking = "yolo""#);

        assert_eq!(settings.anthropic_thinking, AnthropicThinking::Disabled);
    }
}
