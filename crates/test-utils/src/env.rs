//! Serialized, self-restoring access to the process environment from tests.
//!
//! # Why this module exists
//!
//! The process environment is one mutable object shared by every thread, and
//! `cargo test` runs the tests of a binary on parallel threads. That makes
//! `std::env::set_var` / `remove_var` in a test two different problems at once:
//!
//! 1. **Undefined behaviour.** Both are `unsafe` in the 2024 edition because
//!    `setenv` is not thread-safe against a concurrent `getenv`, and `getenv` is
//!    called from C code in dependencies we do not control — timezone lookups
//!    and name resolution both do it.
//! 2. **Order-dependent tests.** A test that writes a variable another test
//!    reads produces failures that depend on thread interleaving, and a test
//!    that "cleans up" after its asserts leaks its writes into every test that
//!    runs afterwards the moment one of those asserts fires.
//!
//! # The order of preference
//!
//! **First, do not mutate.** A test only has to steer the environment when the
//! logic under test reads the environment itself. Give that logic its input as
//! a parameter and test the parameterized core: `PluginConfig::from_lookup`,
//! `audit::retention_days_from` and `config::split_search_path_value` all exist
//! for exactly this reason, and their tests touch nothing global. If the code
//! you want to test reads the environment deep inside a call path, splitting a
//! parameterized core out of it is the fix, not reaching for this module.
//!
//! **Second, if a thin env-reading edge really does need covering, use
//! [`EnvGuard`]** — the one mechanism in this workspace for it.
//!
//! # What the guard does and does not guarantee
//!
//! It **does** guarantee that mutation is serialized against every other
//! mutation in the same process (all of them take one lock), and that whatever
//! was written is restored when the guard drops, including while a failing
//! assert unwinds. Those two properties are what make the remaining mutation
//! order-independent.
//!
//! It **does not** make the mutation sound. Readers do not take the lock, and
//! `getenv` inside a dependency's C code cannot be made to. No test-side lock
//! can close that window; only not mutating can. So keep the number of call
//! sites small enough to enumerate, and prefer a parameterized core every time
//! one is available.
//!
//! The lock is reentrant, so calling [`set_env_default`] or [`load_dotenv`] from
//! inside a scope that already holds an [`EnvGuard`] is fine rather than a hang.

use std::ffi::{OsStr, OsString};
use std::sync::LazyLock;

use parking_lot::{ReentrantMutex, ReentrantMutexGuard};

/// The single lock every process-environment mutation in this workspace takes.
///
/// One lock rather than one per variable: `setenv` mutates a single global
/// table, so two writes to unrelated variables still race each other.
///
/// Reentrant rather than a plain [`Mutex`](std::sync::Mutex), for two reasons. A
/// fixture legitimately calls [`set_env_default`] or [`load_dotenv`] from inside
/// a scope that already holds an [`EnvGuard`], and a plain mutex turns that into
/// a self-deadlock — a silent hang, which is a worse failure than the race being
/// fixed. And a reentrant mutex does not poison, so a test that panics while
/// holding it does not turn one failure into a failure in every later test that
/// touches the environment.
///
/// `parking_lot`'s, because `std::sync::ReentrantLock` is still unstable on the
/// pinned toolchain and `parking_lot` is already a workspace dependency, so this
/// adds no third-party surface. A `LazyLock` wrapper rather than a `const`
/// initializer for the same reason: it does not depend on `ReentrantMutex::new`
/// being `const`.
static ENV_LOCK: LazyLock<ReentrantMutex<()>> = LazyLock::new(|| ReentrantMutex::new(()));

/// Take `ENV_LOCK`, blocking until no *other* thread holds it.
fn lock() -> ReentrantMutexGuard<'static, ()> {
    ENV_LOCK.lock()
}

/// An exclusive, self-restoring window in which a test may change environment
/// variables.
///
/// Hold it for the whole mutate-read-assert span, not just across the write:
/// the point is that no other mutation interleaves with the reads being
/// asserted on. Every variable touched through the guard is restored to the
/// value it had — including "was not set" — when the guard drops, so a failing
/// assert cannot leak state into the rest of the binary.
///
/// ```
/// use trovato_test_utils::env::EnvGuard;
///
/// let key = "TROVATO_DOC_EXAMPLE_VAR";
/// {
///     let mut env = EnvGuard::new();
///     env.set(key, "configured");
///     assert_eq!(std::env::var(key).as_deref(), Ok("configured"));
/// }
/// assert!(std::env::var_os(key).is_none());
/// ```
///
/// The guard is `!Send`, so it cannot be held across an `.await` that might
/// resume on another runtime thread — which is deliberate: the lock is blocking,
/// and env-reading edges are synchronous, so read them synchronously.
#[must_use = "the environment is restored when the guard drops, so it has to be bound"]
pub struct EnvGuard {
    /// Held for the guard's whole life; released after `saved` is replayed.
    ///
    /// [`ReentrantMutexGuard`] is `!Send`, which makes `EnvGuard` `!Send` too —
    /// exactly the property that stops one being carried across an `.await` onto
    /// another runtime thread.
    _lock: ReentrantMutexGuard<'static, ()>,
    /// Every key touched, paired with the value it had before the first touch.
    saved: Vec<(OsString, Option<OsString>)>,
}

impl EnvGuard {
    /// Acquire the environment lock, blocking until no other guard holds it.
    pub fn new() -> Self {
        Self {
            _lock: lock(),
            saved: Vec::new(),
        }
    }

    /// Set `key` to `value` for the life of the guard.
    pub fn set(&mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> &mut Self {
        let key = key.as_ref();
        self.remember(key);
        // SAFETY: `ENV_LOCK` is held for this guard's whole life, so no other
        // mutation in this process interleaves with this write, and the original
        // value is recorded above and replayed on drop. This does NOT stop a
        // concurrent *read* — a `getenv` in a dependency's C code racing this
        // `setenv` is a data race no test-side lock can close. See the module
        // docs: the reason this call site is acceptable is that it is one of a
        // handful, each covering an env-reading edge that has no parameterized
        // form to test instead.
        unsafe { std::env::set_var(key, value) };
        self
    }

    /// Unset `key` for the life of the guard.
    ///
    /// Asserting a default belongs here rather than in a bare test: "the
    /// variable happens to be unset in my shell" is not the same claim as "the
    /// variable is unset", and only the second one holds on a CI runner or a
    /// developer machine that exports it.
    pub fn remove(&mut self, key: impl AsRef<OsStr>) -> &mut Self {
        let key = key.as_ref();
        self.remember(key);
        // SAFETY: as in `set` — serialized by `ENV_LOCK`, restored on drop, and
        // subject to the same irreducible race with concurrent readers.
        unsafe { std::env::remove_var(key) };
        self
    }

    /// Record `key`'s pre-guard value, the first time the guard touches it.
    ///
    /// First value wins, so a key written several times still restores to what
    /// the guard found rather than to an intermediate value.
    fn remember(&mut self, key: &OsStr) {
        if self.saved.iter().any(|(seen, _)| seen == key) {
            return;
        }
        self.saved.push((key.to_os_string(), std::env::var_os(key)));
    }
}

impl Default for EnvGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // Reverse order for symmetry with the writes; the "first value wins"
        // rule in `remember` already makes the result order-independent.
        for (key, value) in self.saved.drain(..).rev() {
            match value {
                // SAFETY: still under `ENV_LOCK` — `_lock` is dropped after
                // this body runs, because fields drop after `Drop::drop`.
                Some(value) => unsafe { std::env::set_var(&key, value) },
                None => unsafe { std::env::remove_var(&key) },
            }
        }
    }
}

/// Install a process-wide default for `key`, if and only if it is unset.
/// Returns whether the write happened.
///
/// This is for integration-test fixtures that have to steer logic which reads
/// the environment lazily at request time, where there is no `Config` field to
/// override and therefore no parameterized core to test against. It is
/// deliberately **not** an [`EnvGuard`]: the value is a default for the whole
/// test binary, not an override for one test's span, so there is nothing to
/// restore it to.
///
/// The check and the write happen together under `ENV_LOCK`, which is what
/// makes it safe to call from several fixtures in one binary: whichever runs
/// first wins, no reader ever observes the value change, and a variable already
/// present in the environment is always left alone.
///
/// It does not make the write sound against a concurrent reader — nothing on
/// the test side can, see the module docs. Reach for a `Config` field override
/// whenever the value has one.
pub fn set_env_default(key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> bool {
    let _lock = lock();
    let key = key.as_ref();
    if std::env::var_os(key).is_some() {
        return false;
    }
    // SAFETY: serialized by `ENV_LOCK`, and write-once — the branch above means
    // a value that any thread could already have read is never overwritten.
    // The residual race with a concurrent reader is documented above.
    unsafe { std::env::set_var(key, value) };
    true
}

/// Load `.env` under `ENV_LOCK`.
///
/// `dotenvy::dotenv` calls `set_var` for every line it applies, which makes it
/// an environment mutation like any other and means it has to be serialized
/// against the rest of them. Test fixtures call it per test rather than once per
/// process, so this is the busiest mutation site in the suite.
///
/// A missing or unreadable `.env` is not an error: CI supplies the same
/// variables through the job environment, and `dotenvy` never overwrites a
/// variable that is already set.
pub fn load_dotenv() {
    let _lock = lock();
    let _ = dotenvy::dotenv();
}

#[cfg(test)]
mod tests {
    use super::{EnvGuard, set_env_default};

    /// The guard restores "was not set", which is the case a hand-rolled
    /// `remove_var` cleanup gets wrong.
    #[test]
    fn restores_an_absent_variable() {
        let key = "TROVATO_ENV_GUARD_ABSENT";
        {
            let mut env = EnvGuard::new();
            env.set(key, "written");
            assert_eq!(std::env::var(key).as_deref(), Ok("written"));
        }
        assert!(std::env::var_os(key).is_none());
    }

    /// A variable the guard found set goes back to its original value, not to
    /// unset — the difference between restoring and clearing, and the bug in
    /// every `remove_var`-as-cleanup test.
    #[test]
    fn restores_a_pre_existing_value() {
        let key = "TROVATO_ENV_GUARD_PRESENT";
        // `set_env_default` is deliberately permanent, so it establishes a value
        // that exists *before* the guard does.
        assert!(set_env_default(key, "original"));
        {
            let mut env = EnvGuard::new();
            env.set(key, "overwritten");
            assert_eq!(std::env::var(key).as_deref(), Ok("overwritten"));
            env.remove(key);
            assert!(std::env::var_os(key).is_none());
        }
        assert_eq!(std::env::var(key).as_deref(), Ok("original"));
    }

    /// Repeated writes to one key restore to what the guard first saw, not to
    /// the last intermediate value.
    #[test]
    fn first_value_seen_is_the_one_restored() {
        let key = "TROVATO_ENV_GUARD_REPEATED";
        {
            let mut env = EnvGuard::new();
            env.set(key, "one");
            env.set(key, "two");
            env.remove(key);
            env.set(key, "three");
        }
        assert!(std::env::var_os(key).is_none());
    }

    /// The whole point: a failing assert must not leak the write. Without the
    /// guard, a cleanup that runs after the asserts never runs at all here.
    #[test]
    fn restores_while_a_failing_assert_unwinds() {
        let key = "TROVATO_ENV_GUARD_PANIC";
        let outcome = std::panic::catch_unwind(|| {
            let mut env = EnvGuard::new();
            env.set(key, "leaked");
            assert_eq!(std::env::var(key).as_deref(), Ok("something else"));
        });
        assert!(outcome.is_err(), "the inner assert is meant to fail");
        assert!(
            std::env::var_os(key).is_none(),
            "a panicking test must not leave its write behind"
        );
    }

    /// A panic while the lock is held leaves it usable, so one failing test does
    /// not cascade into every later test that touches the environment.
    #[test]
    fn a_panicking_guard_does_not_block_later_guards() {
        let key = "TROVATO_ENV_GUARD_PANIC_RELOCK";
        let _ = std::panic::catch_unwind(|| {
            let mut env = EnvGuard::new();
            env.set(key, "written");
            panic!("unwind while holding the lock");
        });
        let mut env = EnvGuard::new();
        env.set(key, "after");
        assert_eq!(std::env::var(key).as_deref(), Ok("after"));
    }

    /// `set_env_default` writes once and then leaves the value alone, which is
    /// what lets several fixtures in one binary call it without ordering.
    #[test]
    fn set_env_default_never_overwrites() {
        let key = "TROVATO_ENV_DEFAULT";
        assert!(
            set_env_default(key, "first"),
            "an unset variable is written"
        );
        assert!(
            !set_env_default(key, "second"),
            "a set variable is left alone"
        );
        assert_eq!(std::env::var(key).as_deref(), Ok("first"));
    }

    /// The lock is reentrant, so a fixture may call [`set_env_default`] or
    /// `load_dotenv` from inside a scope that already holds a guard. With a
    /// plain mutex this test hangs rather than fails, which is why the lock is
    /// not one.
    #[test]
    fn taking_the_lock_again_on_one_thread_does_not_hang() {
        let key = "TROVATO_ENV_DEFAULT_NESTED";
        let mut env = EnvGuard::new();
        env.remove(key);
        assert!(set_env_default(key, "default"));
        assert_eq!(std::env::var(key).as_deref(), Ok("default"));
        // The guard still wins on drop: it restores what it found, which was
        // "unset". A permanent default and a scoped override are different
        // claims, and the scoped one is the narrower.
        drop(env);
        assert!(std::env::var_os(key).is_none());
    }
}
