//! OpenAI-specific OS credential-store boundary.

use keyring::{Entry, Error as KeyringError};
use secrecy::{ExposeSecret, SecretString};
use sotto_core::ProviderError;

const SERVICE: &str = "dev.sotto.llm";
const ACCOUNT: &str = "openai";

fn entry() -> Result<Entry, ProviderError> {
    Entry::new(SERVICE, ACCOUNT).map_err(|error| ProviderError::CredentialStore(error.to_string()))
}

/// Stores an OpenAI API key in the OS credential store.
pub fn store_api_key(key: &SecretString) -> Result<(), ProviderError> {
    entry()?
        .set_password(key.expose_secret())
        .map_err(|error| ProviderError::CredentialStore(error.to_string()))
}

/// Loads an OpenAI API key without exposing it as an ordinary string.
pub fn load_api_key() -> Result<Option<SecretString>, ProviderError> {
    normalize_load(entry()?.get_password())
}

fn normalize_load(
    result: Result<String, KeyringError>,
) -> Result<Option<SecretString>, ProviderError> {
    match result {
        Ok(key) => Ok(Some(SecretString::from(key))),
        Err(KeyringError::NoEntry) => Ok(None),
        Err(error) => Err(ProviderError::CredentialStore(error.to_string())),
    }
}

/// Deletes the OpenAI API key. A missing entry is already the desired state.
pub fn delete_api_key() -> Result<(), ProviderError> {
    normalize_delete(entry()?.delete_credential())
}

fn normalize_delete(result: Result<(), KeyringError>) -> Result<(), ProviderError> {
    match result {
        Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
        Err(error) => Err(ProviderError::CredentialStore(error.to_string())),
    }
}

/// Reports whether the OpenAI API key exists without returning its contents.
pub fn has_api_key() -> Result<bool, ProviderError> {
    load_api_key().map(|key| key.is_some())
}

#[cfg(test)]
mod tests {
    use keyring::Error as KeyringError;
    use secrecy::SecretString;
    use sotto_core::ProviderError;

    use crate::ProviderKind;

    use super::{ACCOUNT, SERVICE, normalize_delete, normalize_load};

    #[test]
    fn credential_identity_matches_the_existing_openai_keychain_entry() {
        assert_eq!(SERVICE, "dev.sotto.llm");
        assert_eq!(ACCOUNT, "openai");
        assert_eq!(
            ACCOUNT,
            ProviderKind::OpenAi.key_name(),
            "the product seam must retain access to keys stored by the historical adapter"
        );
    }

    #[test]
    fn secret_debug_output_is_redacted() {
        let key = SecretString::from("must-never-appear".to_owned());
        assert!(
            !format!("{key:?}").contains("must-never-appear"),
            "the credential API must retain secrecy's redacted representation"
        );
    }

    #[test]
    fn missing_entry_is_distinct_from_credential_store_failure() {
        assert!(
            normalize_load(Err(KeyringError::NoEntry)).is_ok_and(|value| value.is_none()),
            "a missing key must be an ordinary optional-credential state"
        );
        assert!(
            normalize_delete(Err(KeyringError::NoEntry)).is_ok(),
            "deleting a missing key must be idempotent"
        );
        assert!(
            matches!(
                normalize_load(Err(KeyringError::Invalid(
                    "account".to_owned(),
                    "denied".to_owned(),
                ))),
                Err(ProviderError::CredentialStore(_))
            ),
            "credential-store failures must not masquerade as missing keys"
        );
    }
}
