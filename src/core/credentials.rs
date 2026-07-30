use zeroize::Zeroizing;

const MISTRAL_PASS_KEY: &str = "emed-code/mistral/api_key";
const MISTRAL_API_KEY_ENV_VAR: &str = "MISTRAL_API_KEY";

#[derive(Debug, PartialEq)]
pub enum CredentialSource {
    Pass,
    EnvVar,
}

// pass wins when both are present — the more trustworthy source. None
// covers "pass had no value for any reason" (try_get_from_pass can't
// distinguish "no entry" from other failures), not specifically "no
// entry".
fn resolve_mistral_api_key(
    pass_value: Option<Zeroizing<String>>,
    env_var_value: Option<String>,
) -> Option<(Zeroizing<String>, CredentialSource)> {
    if let Some(key) = pass_value {
        return Some((key, CredentialSource::Pass));
    }
    env_var_value.map(|key| (Zeroizing::new(key), CredentialSource::EnvVar))
}

pub fn credential_log_message(source: &CredentialSource) -> &'static str {
    match source {
        CredentialSource::Pass => "Mistral key: from getfrompass",
        CredentialSource::EnvVar => {
            "Mistral key: from env var (no value emed-code/mistral/api_key from getfrompass)"
        }
    }
}

// The real getfrompass/env var lookup. Not unit tested directly — same
// as fetch_ollama_reply/fetch_mistral_reply — see resolve_mistral_api_key
// for the tested decision logic this just feeds real values into.
pub fn lookup_mistral_api_key() -> Option<(Zeroizing<String>, CredentialSource)> {
    let pass_value = getfrompass::try_get_from_pass(MISTRAL_PASS_KEY);
    let env_var_value = std::env::var(MISTRAL_API_KEY_ENV_VAR).ok();
    resolve_mistral_api_key(pass_value, env_var_value)
}

#[cfg(test)]
mod tests {
    use super::*;

    // getfrompass's try_get_from_pass returns Option<Zeroizing<String>>,
    // not Result — None covers "no entry" and every other failure mode
    // (pass not installed, gpg-agent locked, etc.) indistinguishably.
    // resolve_mistral_api_key takes that Option as a parameter rather
    // than calling getfrompass directly, so the "pass succeeded/failed"
    // and "env var present/absent" cases can be tested without a real
    // pass store.
    #[test]
    fn resolve_mistral_api_key_prefers_pass_when_both_are_available() {
        let pass_value = Some(Zeroizing::new("from-pass".to_string()));
        let env_var_value = Some("from-env".to_string());

        let (key, source) = resolve_mistral_api_key(pass_value, env_var_value).unwrap();

        assert_eq!(*key, "from-pass");
        assert_eq!(source, CredentialSource::Pass);
    }

    #[test]
    fn resolve_mistral_api_key_falls_back_to_env_var_when_pass_has_no_value() {
        let pass_value = None;
        let env_var_value = Some("from-env".to_string());

        let (key, source) = resolve_mistral_api_key(pass_value, env_var_value).unwrap();

        assert_eq!(*key, "from-env");
        assert_eq!(source, CredentialSource::EnvVar);
    }

    #[test]
    fn resolve_mistral_api_key_returns_none_when_neither_is_available() {
        let result = resolve_mistral_api_key(None, None);

        assert!(result.is_none());
    }

    #[test]
    fn credential_log_message_for_pass_names_getfrompass_not_pass() {
        assert_eq!(
            credential_log_message(&CredentialSource::Pass),
            "Mistral key: from getfrompass"
        );
    }

    #[test]
    fn credential_log_message_for_env_var_names_getfrompass_not_pass() {
        assert_eq!(
            credential_log_message(&CredentialSource::EnvVar),
            "Mistral key: from env var (no value emed-code/mistral/api_key from getfrompass)"
        );
    }
}
