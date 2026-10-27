//! How much traffic the gateway sends to each provider, and how it retries.
//!
//! Several coding agents working in parallel all reach the same provider
//! through one gateway. Without a cap they burst past the provider's rate limit
//! together, fail together and retry together. A [`TrafficPolicy`] gives each
//! provider a number of slots (requests beyond it wait in line), and retries a
//! failed request on the *same* model with jittered exponential backoff before
//! any configured fallback is considered.
//!
//! Clones share their slots, so one policy handed to every harness gateway in
//! the Agent enforces one limit per provider account, not one per harness.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use rand::Rng;
use relay_core::RelayError;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Retries on the same model after the first attempt fails.
const DEFAULT_RETRIES: u32 = 2;
const DEFAULT_BACKOFF_BASE: Duration = Duration::from_secs(1);
const DEFAULT_BACKOFF_CAP: Duration = Duration::from_secs(8);
/// How long a request waits for a free slot before it is refused. Long enough
/// for a burst of parallel agents to drain, short enough that a stuck provider
/// shows up as an error instead of a hang.
const DEFAULT_QUEUE_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone)]
pub struct TrafficPolicy {
    slots: Arc<HashMap<String, Arc<Semaphore>>>,
    /// Extra attempts on the same model for a retryable failure.
    pub retries: u32,
    pub backoff_base: Duration,
    pub backoff_cap: Duration,
    pub queue_timeout: Duration,
}

impl Default for TrafficPolicy {
    fn default() -> Self {
        Self {
            slots: Arc::new(HashMap::new()),
            retries: DEFAULT_RETRIES,
            backoff_base: DEFAULT_BACKOFF_BASE,
            backoff_cap: DEFAULT_BACKOFF_CAP,
            queue_timeout: DEFAULT_QUEUE_TIMEOUT,
        }
    }
}

impl TrafficPolicy {
    /// Cap in-flight requests per provider id. A limit of 0 means unlimited.
    pub fn with_limits(mut self, limits: impl IntoIterator<Item = (String, usize)>) -> Self {
        self.slots = Arc::new(
            limits
                .into_iter()
                .filter(|(_, limit)| *limit > 0)
                .map(|(provider, limit)| (provider, Arc::new(Semaphore::new(limit))))
                .collect(),
        );
        self
    }

    /// Retry immediately. Tests use this so a retry does not cost seconds.
    pub fn without_delays(mut self) -> Self {
        self.backoff_base = Duration::ZERO;
        self.backoff_cap = Duration::ZERO;
        self
    }

    /// Wait for a slot on `provider`. `None` when the provider is unlimited;
    /// the permit frees the slot when dropped, so callers hold it for as long
    /// as the upstream response (including a whole stream) is in flight.
    pub(crate) async fn acquire(
        &self,
        provider: &str,
    ) -> Result<Option<OwnedSemaphorePermit>, RelayError> {
        let Some(slots) = self.slots.get(provider) else {
            return Ok(None);
        };
        match tokio::time::timeout(self.queue_timeout, slots.clone().acquire_owned()).await {
            Ok(Ok(permit)) => Ok(Some(permit)),
            // The semaphore is never closed; treat it as unlimited if it were.
            Ok(Err(_)) => Ok(None),
            Err(_) => Err(RelayError::ProviderStatus {
                provider: provider.to_string(),
                status: 429,
                message: format!(
                    "every Turnout slot for `{provider}` stayed busy for {}s; raise it with \
                     `turnout providers limit {provider} --max-concurrent <n>`",
                    self.queue_timeout.as_secs()
                ),
            }),
        }
    }

    /// Delay before retry `attempt` (1-based): exponential, capped, with half
    /// of it random. Agents that failed together then retry apart instead of
    /// hitting the provider again in the same instant.
    pub(crate) fn backoff(&self, attempt: u32) -> Duration {
        let exponent = attempt.saturating_sub(1).min(16);
        let ceiling = self
            .backoff_base
            .saturating_mul(1u32 << exponent)
            .min(self.backoff_cap);
        let half = ceiling / 2;
        let jitter_ms = half.as_millis() as u64;
        let jitter = if jitter_ms == 0 {
            Duration::ZERO
        } else {
            Duration::from_millis(rand::thread_rng().gen_range(0..=jitter_ms))
        };
        half + jitter
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_stays_under_the_cap() {
        let policy = TrafficPolicy::default();
        for attempt in 1..=10 {
            let delay = policy.backoff(attempt);
            let ceiling = (DEFAULT_BACKOFF_BASE * 2u32.pow(attempt - 1)).min(DEFAULT_BACKOFF_CAP);
            assert!(delay >= ceiling / 2 && delay <= ceiling, "attempt {attempt}: {delay:?}");
        }
        assert_eq!(policy.clone().without_delays().backoff(3), Duration::ZERO);
    }

    #[tokio::test]
    async fn a_full_provider_makes_the_next_request_wait_and_then_refuses_it() {
        let mut policy = TrafficPolicy::default().with_limits([("evren".to_string(), 1)]);
        policy.queue_timeout = Duration::from_millis(50);
        let held = policy.acquire("evren").await.unwrap();
        assert!(held.is_some());
        // Clones share the slot: a second harness gateway sees it taken.
        let error = policy.clone().acquire("evren").await.unwrap_err();
        assert!(matches!(error, RelayError::ProviderStatus { status: 429, .. }), "{error}");
        drop(held);
        assert!(policy.acquire("evren").await.unwrap().is_some());
        // Providers without a limit never wait.
        assert!(policy.acquire("openai").await.unwrap().is_none());
    }
}
