#![expect(
    clippy::tests_outside_test_module,
    reason = "integration test files compile only under cfg(test)"
)]

use futures_util::StreamExt;
use providers::{Provider, ProviderKind, load_key};
use sotto_core::{
    CancellationToken, CompletionMessage, CompletionProvider, CompletionRequest, MessageRole,
};

async fn run(kind: ProviderKind, model_variable: &str) -> Result<(), Box<dyn std::error::Error>> {
    let model = std::env::var(model_variable)?;
    let key = if kind == ProviderKind::Ollama {
        None
    } else {
        load_key(kind)?
    };
    let provider = Provider::new(kind, &model, key);
    let request = CompletionRequest {
        model,
        system: None,
        messages: vec![CompletionMessage {
            role: MessageRole::User,
            content: "Reply with OK.".to_owned(),
            cache_boundary: false,
        }],
        max_tokens: Some(8),
        temperature: Some(0.0),
        stop: vec![],
    };
    let mut stream = provider.stream(request, CancellationToken::new()).await?;
    let mut received = false;
    while let Some(delta) = stream.next().await {
        received |= !delta?.text.is_empty();
    }
    assert!(
        received,
        "live provider must stream at least one text delta"
    );
    Ok(())
}

macro_rules! live_test {
    ($name:ident, $kind:expr, $model:literal) => {
        #[tokio::test]
        #[ignore = "manual live-provider check; requires a configured key/model"]
        async fn $name() -> Result<(), Box<dyn std::error::Error>> {
            run($kind, $model).await
        }
    };
}

live_test!(
    anthropic_live,
    ProviderKind::Anthropic,
    "SOTTO_ANTHROPIC_MODEL"
);
live_test!(openai_live, ProviderKind::OpenAi, "SOTTO_OPENAI_MODEL");
live_test!(google_live, ProviderKind::Google, "SOTTO_GOOGLE_MODEL");
live_test!(
    openrouter_live,
    ProviderKind::OpenRouter,
    "SOTTO_OPENROUTER_MODEL"
);
live_test!(ollama_live, ProviderKind::Ollama, "SOTTO_OLLAMA_MODEL");
