//! Provider wrapper that limits only active model episodes.
//!
//! A claimed run may spend substantial time reconstructing session memory,
//! invoking MCP capabilities, or committing a checkpoint. None of those
//! phases should consume a provider in-flight slot for another chat room.

use std::fmt;
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use krw_agent_execution_contracts::{
    DeliveryCertainty, DependencyFailure, Provider, RuntimeStageTimings,
};
use krw_agent_provider_wire::{
    EpisodeContext, MessagesRequest, PreparedMessagesRequest, ProviderEpisodeV1,
};
use tokio::sync::Semaphore;

#[derive(Clone)]
pub struct PermitBoundProvider<P> {
    inner: Arc<P>,
    permits: Arc<Semaphore>,
    timings: Arc<RuntimeStageTimings>,
}

impl<P> PermitBoundProvider<P> {
    pub fn new(inner: Arc<P>, permits: Arc<Semaphore>, timings: Arc<RuntimeStageTimings>) -> Self {
        Self {
            inner,
            permits,
            timings,
        }
    }
}

impl<P: fmt::Debug> fmt::Debug for PermitBoundProvider<P> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PermitBoundProvider")
            .field("inner", &self.inner)
            .field("permits_available", &self.permits.available_permits())
            .finish()
    }
}

#[async_trait]
impl<P> Provider for PermitBoundProvider<P>
where
    P: Provider + 'static,
{
    async fn complete(
        &self,
        request: &MessagesRequest,
        context: &EpisodeContext,
    ) -> Result<ProviderEpisodeV1, DependencyFailure> {
        let started = Instant::now();
        let permit = self.permits.clone().acquire_owned().await.map_err(|_| {
            DependencyFailure::redacted(
                "provider_permit_closed",
                "provider episode permit semaphore was closed",
                true,
                DeliveryCertainty::NotDispatched,
            )
        })?;
        self.timings.add_provider_queue_wait(started.elapsed());
        let result = self.inner.complete(request, context).await;
        drop(permit);
        result
    }

    async fn complete_prepared(
        &self,
        prepared: &PreparedMessagesRequest<'_>,
        context: &EpisodeContext,
    ) -> Result<ProviderEpisodeV1, DependencyFailure> {
        let started = Instant::now();
        let permit = self.permits.clone().acquire_owned().await.map_err(|_| {
            DependencyFailure::redacted(
                "provider_permit_closed",
                "provider episode permit semaphore was closed",
                true,
                DeliveryCertainty::NotDispatched,
            )
        })?;
        self.timings.add_provider_queue_wait(started.elapsed());
        let result = self.inner.complete_prepared(prepared, context).await;
        drop(permit);
        result
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use krw_agent_protocol::ThinkingMode;
    use tokio::sync::Notify;

    #[derive(Debug)]
    struct BlockingProvider {
        started: Arc<Notify>,
        release: Arc<Notify>,
        active: Arc<AtomicUsize>,
        peak: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Provider for BlockingProvider {
        async fn complete(
            &self,
            _request: &MessagesRequest,
            _context: &EpisodeContext,
        ) -> Result<ProviderEpisodeV1, DependencyFailure> {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(active, Ordering::SeqCst);
            self.started.notify_one();
            self.release.notified().await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            Err(DependencyFailure::redacted(
                "test_provider",
                "test provider completed",
                false,
                DeliveryCertainty::NotDispatched,
            ))
        }
    }

    fn request() -> MessagesRequest {
        MessagesRequest {
            model: "glm-5.3".into(),
            messages: Vec::new(),
            system: "system".into(),
            max_tokens: 1024,
            tools: Vec::new(),
            tool_choice: None,
            output_config: None,
            response_format: None,
            thinking: krw_agent_provider_wire::ThinkingConfig {
                kind: ThinkingMode::Disabled,
                budget_tokens: None,
            },
            stream: false,
            metadata: None,
        }
    }

    fn context() -> EpisodeContext {
        EpisodeContext {
            tool_schema_hash: krw_agent_protocol::ContentHash::sha256("tools"),
            agent_image_hash: krw_agent_protocol::ContentHash::sha256("image"),
            api_version: "anthropic-messages-v1".into(),
        }
    }

    #[tokio::test]
    async fn permit_is_held_only_during_complete() {
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let inner = Arc::new(BlockingProvider {
            started: started.clone(),
            release: release.clone(),
            active: active.clone(),
            peak: peak.clone(),
        });
        let provider = Arc::new(PermitBoundProvider::new(
            inner,
            Arc::new(Semaphore::new(1)),
            Arc::new(RuntimeStageTimings::default()),
        ));
        let first = tokio::spawn({
            let provider = provider.clone();
            async move { provider.complete(&request(), &context()).await }
        });
        started.notified().await;
        assert_eq!(provider.permits.available_permits(), 0);
        let second = tokio::spawn({
            let provider = provider.clone();
            async move { provider.complete(&request(), &context()).await }
        });
        tokio::task::yield_now().await;
        assert_eq!(peak.load(Ordering::SeqCst), 1);
        release.notify_one();
        // The first call releases its permit and the second call can now enter.
        started.notified().await;
        release.notify_one();
        let _ = first.await;
        let _ = second.await;
        assert_eq!(peak.load(Ordering::SeqCst), 1);
        assert_eq!(provider.permits.available_permits(), 1);
    }
}
