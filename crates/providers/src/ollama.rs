use crate::{Provider, ProviderKind};

#[must_use]
pub fn configured(model: impl Into<String>) -> Provider {
    Provider::new(ProviderKind::Ollama, model, None)
}
