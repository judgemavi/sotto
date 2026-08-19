//! Product reasoning selection and evaluation through the provider registry.

use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use insight::{ClusterReport, Clusterer, Summarizer, SummaryReport};
use providers::openai::OpenAiProvider;
use providers::{BackendCapability, ReasoningSurface, Registry, ResolvedBackend};
use rag::Store;
use screen::ScreenInspectionSource;
use sotto_core::SessionId;
use sotto_core::{CancellationToken, ProviderError, ReasoningRequest, StopReason, Usage};

/// Selectable v1 reasoning state. Codex is retained only as an explicit unavailable result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackendChoice {
    None,
    OpenAi,
    CodexUnavailable,
}

impl BackendChoice {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "none" => Ok(Self::None),
            "openai" => Ok(Self::OpenAi),
            "codex" => Ok(Self::CodexUnavailable),
            _ => bail!("unknown reasoning backend {value}; expected none, openai, or codex"),
        }
    }
}

/// Builds the same open registry used by product settings and pins Notes for this call.
pub fn resolve_keychain_backend(
    choice: BackendChoice,
    model: Option<&str>,
    surface: ReasoningSurface,
) -> Result<Option<ResolvedBackend>> {
    match choice {
        BackendChoice::None => Ok(None),
        BackendChoice::OpenAi => {
            let model = required_model(model)?;
            let provider = Arc::new(OpenAiProvider::from_keychain(model)?);
            resolve_registered(provider, surface)
        }
        BackendChoice::CodexUnavailable => bail!(
            "Codex reasoning is unavailable: T030 isolation failed; no executable or credential probe was attempted"
        ),
    }
}

/// Registers a concrete OpenAI provider without Keychain access, used by credential-free evals.
pub fn resolve_registered(
    provider: Arc<OpenAiProvider>,
    surface: ReasoningSurface,
) -> Result<Option<ResolvedBackend>> {
    let descriptor = provider.descriptor()?;
    let id = descriptor.id().clone();
    let mut registry = Registry::default();
    registry.register_reasoning(descriptor, provider)?;
    registry.select(surface, Some(&id))?;
    registry.resolve(surface).map_err(Into::into)
}

pub async fn summarize(
    store: &Store,
    session_id: SessionId,
    resolved: &ResolvedBackend,
) -> Result<SummaryReport> {
    summarize_with_inspector(store, session_id, resolved, None).await
}

pub async fn summarize_with_inspector(
    store: &Store,
    session_id: SessionId,
    resolved: &ResolvedBackend,
    inspector: Option<Arc<dyn ScreenInspectionSource>>,
) -> Result<SummaryReport> {
    require_capability(resolved, BackendCapability::JsonObjectOutput, "summary")?;
    let mut summarizer =
        Summarizer::new(store, resolved.provider()).with_reasoning_provider(resolved.provider());
    if let Some(inspector) = inspector {
        summarizer = summarizer.with_screen_inspector(inspector);
    }
    summarizer
        .summarize(session_id)
        .await
        .context("generate persisted-session recap")
}

pub async fn cluster(
    store: &Store,
    session_id: SessionId,
    resolved: &ResolvedBackend,
) -> Result<ClusterReport> {
    cluster_with_inspector(store, session_id, resolved, None).await
}

pub async fn cluster_with_inspector(
    store: &Store,
    session_id: SessionId,
    resolved: &ResolvedBackend,
    inspector: Option<Arc<dyn ScreenInspectionSource>>,
) -> Result<ClusterReport> {
    require_capability(resolved, BackendCapability::JsonObjectOutput, "clustering")?;
    let mut clusterer = Clusterer::new(store, resolved.provider())
        .with_reasoning_provider(resolved.provider())
        .with_backend_fingerprint(resolved.cache_fingerprint().clone());
    if let Some(inspector) = inspector {
        clusterer = clusterer.with_screen_inspector(inspector);
    }
    clusterer
        .cluster(session_id)
        .await
        .context("generate persisted-session topic view")
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct LatencyPercentiles {
    pub samples: usize,
    pub p50_ms: f64,
    pub p95_ms: f64,
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct ReasoningLatency {
    pub first_delta: LatencyPercentiles,
    pub total: LatencyPercentiles,
}

/// One provider-neutral observation used by live, non-CI latency canaries.
#[derive(Clone, Debug)]
pub struct MeasuredCompletion {
    pub text: String,
    pub usage: Usage,
    pub stop_reason: Option<StopReason>,
    pub time_to_first_delta: Option<Duration>,
    pub total: Duration,
}

pub async fn measure_reasoning_call(
    resolved: &ResolvedBackend,
    request: ReasoningRequest,
    cancellation: CancellationToken,
) -> Result<MeasuredCompletion, ProviderError> {
    let started = std::time::Instant::now();
    let mut stream = resolved
        .provider()
        .stream_advanced_reasoning(request, None, cancellation)
        .await?;
    let mut text = String::new();
    let mut usage = Usage::default();
    let mut stop_reason = None;
    let mut time_to_first_delta = None;
    while let Some(delta) = stream.next().await {
        let delta = delta?;
        if time_to_first_delta.is_none() && !delta.text.is_empty() {
            time_to_first_delta = Some(started.elapsed());
        }
        text.push_str(&delta.text);
        if let Some(observed) = delta.usage {
            usage = observed;
        }
        if delta.stop_reason.is_some() {
            stop_reason = delta.stop_reason;
        }
    }
    Ok(MeasuredCompletion {
        text,
        usage,
        stop_reason,
        time_to_first_delta,
        total: started.elapsed(),
    })
}

#[must_use]
pub fn latency_report(first: &mut [Duration], total: &mut [Duration]) -> ReasoningLatency {
    ReasoningLatency {
        first_delta: duration_report(first),
        total: duration_report(total),
    }
}

#[must_use]
pub fn duration_report(values: &mut [Duration]) -> LatencyPercentiles {
    values.sort_unstable();
    LatencyPercentiles {
        samples: values.len(),
        p50_ms: percentile(values, 50),
        p95_ms: percentile(values, 95),
    }
}

fn percentile(values: &[Duration], percent: usize) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let index = (values.len() - 1).saturating_mul(percent).div_ceil(100);
    values[index].as_secs_f64() * 1_000.0
}

fn required_model(model: Option<&str>) -> Result<&str> {
    model
        .filter(|value| !value.trim().is_empty())
        .context("reasoning backend requires --model")
}

fn require_capability(
    resolved: &ResolvedBackend,
    capability: BackendCapability,
    consumer: &str,
) -> Result<()> {
    if resolved.descriptor().capabilities().contains(capability) {
        Ok(())
    } else {
        bail!(
            "reasoning backend {} does not advertise {capability:?} required by {consumer}",
            resolved.descriptor().id()
        )
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{BackendChoice, latency_report, resolve_keychain_backend};
    use providers::ReasoningSurface;

    #[test]
    fn no_reasoning_is_normal_and_codex_is_unavailable() -> Result<(), Box<dyn std::error::Error>> {
        assert!(
            resolve_keychain_backend(BackendChoice::None, None, ReasoningSurface::Notes)?.is_none(),
            "no reasoning must not touch credentials or construct a provider"
        );
        let codex = resolve_keychain_backend(
            BackendChoice::CodexUnavailable,
            None,
            ReasoningSurface::Notes,
        );
        assert!(
            codex
                .as_ref()
                .is_err_and(|error| error.to_string().contains("T030 isolation failed")),
            "T030 FAIL must be authoritative without requiring a model or probing the executable"
        );
        Ok(())
    }

    #[test]
    fn deterministic_latency_percentiles_use_observed_samples() {
        let mut first = [
            Duration::from_millis(10),
            Duration::from_millis(30),
            Duration::from_millis(20),
        ];
        let mut total = [
            Duration::from_millis(40),
            Duration::from_millis(80),
            Duration::from_millis(60),
        ];
        let report = latency_report(&mut first, &mut total);
        assert_eq!(
            report.first_delta.samples, 3,
            "all TTFT observations must be reported"
        );
        assert_eq!(
            report.total.samples, 3,
            "all total observations must be reported"
        );
        assert_eq!(report.first_delta.p50_ms, 20.0, "TTFT p50 must be measured");
        assert_eq!(report.first_delta.p95_ms, 30.0, "TTFT p95 must be measured");
        assert_eq!(report.total.p50_ms, 60.0, "total p50 must be measured");
        assert_eq!(report.total.p95_ms, 80.0, "total p95 must be measured");
    }

    #[test]
    fn latency_populations_are_reported_separately_when_text_is_missing() {
        let mut first = [Duration::from_millis(10)];
        let mut total = [Duration::from_millis(40), Duration::from_millis(80)];
        let report = latency_report(&mut first, &mut total);
        assert_eq!(
            report.first_delta.samples, 1,
            "TTFT cannot include no-text calls"
        );
        assert_eq!(
            report.total.samples, 2,
            "total must include every completed call"
        );
        assert_eq!(
            report.first_delta.p95_ms, 10.0,
            "TTFT uses its own population"
        );
        assert_eq!(
            report.total.p95_ms, 80.0,
            "total uses its independent population"
        );
    }
}
