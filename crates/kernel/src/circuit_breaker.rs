//! Circuit breaker for external service calls.
//!
//! Prevents cascading failures when an external service (AI provider,
//! SMTP, S3) is down. After N failures within a time window, the circuit
//! opens and subsequent calls fail immediately without attempting the
//! underlying operation.

use std::time::{Duration, Instant};

use parking_lot::RwLock;

/// Circuit breaker configuration.
#[derive(Debug, Clone)]
pub struct BreakerConfig {
    /// Number of failures before opening the circuit.
    pub failure_threshold: u32,
    /// How long the circuit stays open before trying half-open.
    pub recovery_timeout: Duration,
    /// Window for counting recent failures.
    pub failure_window: Duration,
}

impl Default for BreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            recovery_timeout: Duration::from_secs(30),
            failure_window: Duration::from_secs(60),
        }
    }
}

/// Circuit breaker error.
#[derive(Debug)]
pub enum CircuitBreakerError<E> {
    /// The circuit is open — the service is considered unavailable.
    Open,
    /// The underlying operation failed.
    ServiceError(E),
}

impl<E: std::fmt::Display> std::fmt::Display for CircuitBreakerError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Open => write!(f, "circuit breaker is open: service unavailable"),
            Self::ServiceError(e) => write!(f, "{e}"),
        }
    }
}

impl<E: std::fmt::Display + std::fmt::Debug> std::error::Error for CircuitBreakerError<E> {}

/// Circuit breaker state.
#[derive(Debug)]
enum BreakerState {
    /// Circuit is closed (normal operation). Recent failures are tracked.
    Closed { failures: Vec<Instant> },
    /// Circuit is open (service unavailable). Opened at the given time.
    Open { opened_at: Instant },
    /// Circuit is half-open (testing if service recovered).
    HalfOpen,
}

/// Circuit breaker for external service calls.
#[derive(Debug)]
pub struct CircuitBreaker {
    name: String,
    state: RwLock<BreakerState>,
    config: BreakerConfig,
}

impl CircuitBreaker {
    /// Create a new circuit breaker with the given name and config.
    pub fn new(name: impl Into<String>, config: BreakerConfig) -> Self {
        Self {
            name: name.into(),
            state: RwLock::new(BreakerState::Closed {
                failures: Vec::new(),
            }),
            config,
        }
    }

    /// Execute an async operation through the circuit breaker.
    ///
    /// Returns `Err(CircuitBreakerError::Open)` immediately if the circuit
    /// is open. On success, closes the circuit if it was half-open.
    /// On failure, records it and potentially opens the circuit.
    pub async fn call<F, Fut, T, E>(&self, f: F) -> Result<T, CircuitBreakerError<E>>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, E>>,
    {
        // Check if we should allow the call
        if !self.should_allow() {
            tracing::debug!(breaker = %self.name, "circuit breaker is open, rejecting call");
            return Err(CircuitBreakerError::Open);
        }

        match f().await {
            Ok(result) => {
                self.record_success();
                Ok(result)
            }
            Err(e) => {
                self.record_failure();
                Err(CircuitBreakerError::ServiceError(e))
            }
        }
    }

    /// Check if a call should be allowed.
    fn should_allow(&self) -> bool {
        let mut state = self.state.write();
        match &*state {
            BreakerState::Closed { .. } => true,
            BreakerState::Open { opened_at } => {
                if opened_at.elapsed() >= self.config.recovery_timeout {
                    tracing::info!(breaker = %self.name, "circuit breaker transitioning to half-open");
                    *state = BreakerState::HalfOpen;
                    true
                } else {
                    false
                }
            }
            BreakerState::HalfOpen => {
                // Only allow one probe at a time — transition to Open
                // immediately. If the probe succeeds, record_success
                // will close it.
                false
            }
        }
    }

    /// Record a successful call.
    fn record_success(&self) {
        let mut state = self.state.write();
        match &*state {
            BreakerState::HalfOpen => {
                tracing::info!(breaker = %self.name, "circuit breaker closing after successful probe");
                *state = BreakerState::Closed {
                    failures: Vec::new(),
                };
            }
            BreakerState::Closed { .. } => {
                // Reset failures on success
                *state = BreakerState::Closed {
                    failures: Vec::new(),
                };
            }
            BreakerState::Open { .. } => {
                // Shouldn't happen, but handle gracefully
            }
        }
    }

    /// Record a failed call.
    fn record_failure(&self) {
        let mut state = self.state.write();
        match &mut *state {
            BreakerState::HalfOpen => {
                tracing::warn!(breaker = %self.name, "circuit breaker re-opening after failed probe");
                *state = BreakerState::Open {
                    opened_at: Instant::now(),
                };
            }
            BreakerState::Closed { failures } => {
                let now = Instant::now();
                // Remove failures outside the window
                failures.retain(|t| now.duration_since(*t) < self.config.failure_window);
                failures.push(now);

                if failures.len() >= self.config.failure_threshold as usize {
                    tracing::warn!(
                        breaker = %self.name,
                        failures = failures.len(),
                        threshold = self.config.failure_threshold,
                        "circuit breaker opening after reaching failure threshold"
                    );
                    *state = BreakerState::Open { opened_at: now };
                }
            }
            BreakerState::Open { .. } => {
                // Already open
            }
        }
    }

    /// Get the current state name for monitoring.
    pub fn state_name(&self) -> &'static str {
        match &*self.state.read() {
            BreakerState::Closed { .. } => "closed",
            BreakerState::Open { .. } => "open",
            BreakerState::HalfOpen => "half_open",
        }
    }

    /// Get the breaker name.
    pub fn name(&self) -> &str {
        &self.name
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn closed_allows_calls() {
        let cb = CircuitBreaker::new("test", BreakerConfig::default());
        let result: Result<i32, CircuitBreakerError<String>> = cb.call(|| async { Ok(42) }).await;
        assert_eq!(result.unwrap(), 42);
    }

    #[tokio::test]
    async fn opens_after_threshold_failures() {
        let cb = CircuitBreaker::new(
            "test",
            BreakerConfig {
                failure_threshold: 3,
                recovery_timeout: Duration::from_secs(60),
                failure_window: Duration::from_secs(60),
            },
        );

        for _ in 0..3 {
            let _: Result<i32, _> = cb
                .call(|| async { Err::<i32, String>("fail".into()) })
                .await;
        }

        assert_eq!(cb.state_name(), "open");

        // Next call should be rejected immediately
        let result: Result<i32, CircuitBreakerError<String>> = cb.call(|| async { Ok(42) }).await;
        assert!(matches!(result, Err(CircuitBreakerError::Open)));
    }

    #[tokio::test]
    async fn recovers_after_timeout() {
        let cb = CircuitBreaker::new(
            "test",
            BreakerConfig {
                failure_threshold: 1,
                recovery_timeout: Duration::from_millis(10),
                failure_window: Duration::from_secs(60),
            },
        );

        let _: Result<i32, _> = cb
            .call(|| async { Err::<i32, String>("fail".into()) })
            .await;
        assert_eq!(cb.state_name(), "open");

        tokio::time::sleep(Duration::from_millis(20)).await;

        // Should transition to half-open and allow the call
        let result: Result<i32, CircuitBreakerError<String>> = cb.call(|| async { Ok(42) }).await;
        assert_eq!(result.unwrap(), 42);
        assert_eq!(cb.state_name(), "closed");
    }

    #[tokio::test]
    async fn half_open_reopens_on_failure() {
        let cb = CircuitBreaker::new(
            "test",
            BreakerConfig {
                failure_threshold: 1,
                recovery_timeout: Duration::from_millis(10),
                failure_window: Duration::from_secs(60),
            },
        );

        let _: Result<i32, _> = cb
            .call(|| async { Err::<i32, String>("fail".into()) })
            .await;
        tokio::time::sleep(Duration::from_millis(20)).await;

        // Half-open probe fails
        let _: Result<i32, _> = cb
            .call(|| async { Err::<i32, String>("still failing".into()) })
            .await;
        assert_eq!(cb.state_name(), "open");
    }

    #[test]
    fn default_config() {
        let config = BreakerConfig::default();
        assert_eq!(config.failure_threshold, 5);
        assert_eq!(config.recovery_timeout, Duration::from_secs(30));
        assert_eq!(config.failure_window, Duration::from_secs(60));
    }

    #[test]
    fn error_display() {
        let open: CircuitBreakerError<String> = CircuitBreakerError::Open;
        assert!(open.to_string().contains("circuit breaker is open"));

        let svc: CircuitBreakerError<String> = CircuitBreakerError::ServiceError("timeout".into());
        assert_eq!(svc.to_string(), "timeout");
    }
}
