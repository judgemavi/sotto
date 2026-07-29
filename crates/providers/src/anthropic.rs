use secrecy::SecretString;

use crate::{Provider, ProviderKind};

#[must_use]
pub fn configured(model: impl Into<String>, key: SecretString) -> Provider {
    Provider::new(ProviderKind::Anthropic, model, Some(key))
}
