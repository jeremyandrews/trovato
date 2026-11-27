//! The one place the kernel opens an async Redis connection (B247).
//!
//! `redis::Client::get_multiplexed_async_connection()` has no timeout of any
//! kind: not on the connect, and not on a command once connected. When the peer
//! stops answering without closing the connection (a black-holed route, a SYN
//! flood, a network partition) the await simply never returns. That is how one
//! cron run sat in its queue pop for ten hours in production while its lock was
//! renewed and every later cron tick was skipped behind it.
//!
//! Every async connection the kernel opens goes through [`BoundedRedis`]
//! instead, so every caller gets a connection timeout and a per-command
//! response timeout, and a Redis that goes silent turns into an ordinary error
//! the caller already handles. The test at the bottom of this file fails the
//! build if any other kernel source opens a connection directly.
//!
//! Sessions do not use this client: they go through the `fred` pool in
//! [`crate::session`], which is given the same two values.

use std::future::Future;
use std::time::Duration;

use redis::aio::MultiplexedConnection;
use redis::{AsyncConnectionConfig, Client, RedisResult};

/// How long opening a connection may take, setup commands included.
///
/// A healthy Redis on the same network accepts and answers setup in
/// milliseconds; five seconds is room for a slow DNS answer or a busy server,
/// and short enough that a request waiting on it is not left hanging. Matches
/// the session pool's command timeout.
pub const REDIS_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// How long one command may wait for its reply.
///
/// Every command the kernel sends is a single-key operation, a short Lua
/// script, or a bounded scan, which Redis answers in well under a millisecond.
/// Five seconds is generous for any of them under load and is the same bound
/// the session pool already uses (`default_command_timeout`), so a Redis that
/// has stopped answering fails every caller in the same way at the same time.
pub const REDIS_RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);

/// Added to a blocking command's own block time to get its response timeout.
///
/// A `BLPOP` for N seconds legitimately waits N seconds for its reply, so it
/// cannot share [`REDIS_RESPONSE_TIMEOUT`]; it gets N plus this.
pub const REDIS_BLOCKING_MARGIN: Duration = REDIS_RESPONSE_TIMEOUT;

/// Opens bounded async connections. Implemented for [`redis::Client`].
pub trait BoundedRedis {
    /// A multiplexed connection with [`REDIS_CONNECT_TIMEOUT`] on the connect
    /// and [`REDIS_RESPONSE_TIMEOUT`] on every command sent over it.
    fn bounded_connection(&self)
    -> impl Future<Output = RedisResult<MultiplexedConnection>> + Send;

    /// A connection for a command that blocks server-side for up to `block`
    /// (`BLPOP` and friends): its response timeout is `block` plus
    /// [`REDIS_BLOCKING_MARGIN`].
    fn bounded_blocking_connection(
        &self,
        block: Duration,
    ) -> impl Future<Output = RedisResult<MultiplexedConnection>> + Send;
}

impl BoundedRedis for Client {
    fn bounded_connection(
        &self,
    ) -> impl Future<Output = RedisResult<MultiplexedConnection>> + Send {
        connect(self, REDIS_RESPONSE_TIMEOUT)
    }

    fn bounded_blocking_connection(
        &self,
        block: Duration,
    ) -> impl Future<Output = RedisResult<MultiplexedConnection>> + Send {
        connect(self, block.saturating_add(REDIS_BLOCKING_MARGIN))
    }
}

/// The connection config every kernel connection is built with.
pub fn connection_config(response_timeout: Duration) -> AsyncConnectionConfig {
    AsyncConnectionConfig::new()
        .set_connection_timeout(REDIS_CONNECT_TIMEOUT)
        .set_response_timeout(response_timeout)
}

async fn connect(
    client: &Client,
    response_timeout: Duration,
) -> RedisResult<MultiplexedConnection> {
    client
        .get_multiplexed_async_connection_with_config(&connection_config(response_timeout))
        .await
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::path::Path;

    /// What a call that opens a Redis connection without our timeouts looks
    /// like. Prefixes and fragments rather than whole names, so a variant
    /// (`_with_timeouts`, `_async_std_`, a `_manager_with_backoff`) is caught
    /// without being listed. This file is the one place allowed to use them.
    const UNBOUNDED_CALLS: &[&str] = &[
        "get_multiplexed_",
        "create_multiplexed_",
        "get_async_connection",
        "get_async_std_connection",
        "get_tokio_connection",
        "connection_manager",
        "ConnectionManager",
        "get_async_pubsub",
        "get_async_monitor",
        "get_connection(",
        "get_connection_with_timeout",
    ];

    /// Every `redis::Client` method that opens a connection, at the locked
    /// version (0.27.6, `src/client.rs`). Kept by hand: when the crate is
    /// upgraded, compare this with the new `Client` and add what is new.
    const CLIENT_CONNECTION_METHODS: &[&str] = &[
        "get_connection",
        "get_connection_with_timeout",
        "get_async_connection",
        "get_tokio_connection",
        "get_async_std_connection",
        "get_multiplexed_async_connection",
        "get_multiplexed_async_connection_with_timeouts",
        "get_multiplexed_async_connection_with_config",
        "get_multiplexed_tokio_connection_with_response_timeouts",
        "get_multiplexed_tokio_connection",
        "get_multiplexed_async_std_connection_with_timeouts",
        "get_multiplexed_async_std_connection",
        "create_multiplexed_tokio_connection_with_response_timeout",
        "create_multiplexed_tokio_connection",
        "create_multiplexed_async_std_connection_with_response_timeout",
        "create_multiplexed_async_std_connection",
        "get_tokio_connection_manager",
        "get_connection_manager",
        "get_tokio_connection_manager_with_backoff",
        "get_tokio_connection_manager_with_backoff_and_timeouts",
        "get_connection_manager_with_backoff_and_timeouts",
        "get_connection_manager_with_config",
        "get_connection_manager_with_backoff",
        "get_async_pubsub",
        "get_async_monitor",
    ];

    /// The guard below is only as good as its patterns: each way the locked
    /// crate offers to open a connection, written as a call, must match one.
    #[test]
    fn the_guard_catches_every_way_the_redis_client_opens_a_connection() {
        let missed: Vec<&str> = CLIENT_CONNECTION_METHODS
            .iter()
            .copied()
            .filter(|method| {
                let call = format!("client.{method}().await");
                !UNBOUNDED_CALLS.iter().any(|pattern| call.contains(pattern))
            })
            .collect();
        assert!(
            missed.is_empty(),
            "the guard would let these through unbounded: {missed:?}"
        );
    }

    fn rust_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                rust_files(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    /// Every async Redis connection the kernel builds goes through
    /// [`super::BoundedRedis`]. A grep over the kernel's own sources, tests included:
    /// a bare call anywhere is one more await that can hang for ever.
    #[test]
    fn every_kernel_redis_connection_goes_through_the_bounded_constructor() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let this = src.join("redis_conn.rs");
        let mut files = Vec::new();
        rust_files(&src, &mut files);
        assert!(
            files.len() > 50,
            "the walk found only {} files",
            files.len()
        );

        let mut offenders = Vec::new();
        for file in files.iter().filter(|f| **f != this) {
            let text = std::fs::read_to_string(file).unwrap();
            for (n, line) in text.lines().enumerate() {
                for call in UNBOUNDED_CALLS {
                    if line.contains(call) {
                        offenders.push(format!("{}:{}: {}", file.display(), n + 1, line.trim()));
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "these open a Redis connection with no timeout; use \
             `redis_conn::BoundedRedis` instead:\n{}",
            offenders.join("\n")
        );
    }
}
