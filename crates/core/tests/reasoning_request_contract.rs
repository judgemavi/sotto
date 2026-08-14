#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use sotto_core::{
        BoxFuture, BoxStream, CancellationToken, CompletionProvider, CompletionRequest, Delta,
        JsonSchemaConstraint, ProviderError, ReasoningRequest, ReasoningRequestError,
    };

    fn completion() -> CompletionRequest {
        CompletionRequest {
            model: "model".to_owned(),
            system: None,
            messages: Vec::new(),
            max_tokens: None,
            temperature: None,
            stop: Vec::new(),
        }
    }

    #[test]
    fn schema_names_and_nonempty_json_are_validated() -> Result<(), Box<dyn std::error::Error>> {
        let _schema = JsonSchemaConstraint::new("recap-v1", None, r#"{"type":"object"}"#)?;
        assert_eq!(
            JsonSchemaConstraint::new("not valid", None, "{}"),
            Err(ReasoningRequestError::InvalidSchemaName)
        );
        assert_eq!(
            JsonSchemaConstraint::new("recap", None, "   "),
            Err(ReasoningRequestError::EmptySchema),
            "empty schema text must fail before provider dispatch"
        );
        Ok(())
    }

    struct TextOnlyProvider {
        calls: Arc<AtomicUsize>,
    }

    impl CompletionProvider for TextOnlyProvider {
        fn stream(
            &self,
            _req: CompletionRequest,
            _cancellation: CancellationToken,
        ) -> BoxFuture<'_, Result<BoxStream<'static, Result<Delta, ProviderError>>, ProviderError>>
        {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Box::pin(async { Err(ProviderError::Network("test transport reached".to_owned())) })
        }

        fn model_id(&self) -> &str {
            "model"
        }
    }

    #[tokio::test]
    async fn default_provider_rejects_advanced_input_before_transport()
    -> Result<(), Box<dyn std::error::Error>> {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = TextOnlyProvider {
            calls: Arc::clone(&calls),
        };
        let schema = JsonSchemaConstraint::new("recap", None, r#"{"type":"object"}"#)?;
        let result = provider
            .stream_reasoning(
                ReasoningRequest::json_schema(completion(), schema),
                CancellationToken::new(),
            )
            .await;
        assert!(matches!(result, Err(ProviderError::InvalidRequest(_))));
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        Ok(())
    }

    #[tokio::test]
    async fn default_provider_preserves_text_only_dispatch() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = TextOnlyProvider {
            calls: Arc::clone(&calls),
        };
        let result = provider
            .stream_reasoning(
                ReasoningRequest::text(completion()),
                CancellationToken::new(),
            )
            .await;
        assert!(matches!(result, Err(ProviderError::Network(_))));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }
}
