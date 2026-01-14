//! Async helpers for integrating ISO-TP with runtimes (tokio, embassy, ...).

use core::future::Future;
use core::time::Duration;

/// Timeout marker returned by [`AsyncRuntime::timeout`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimedOut;

/// Minimal runtime abstraction: sleeping and applying timeouts to futures.
pub trait AsyncRuntime {
    /// Error type returned by [`AsyncRuntime::timeout`].
    type TimeoutError;

    /// Future returned by [`AsyncRuntime::sleep`].
    type Sleep<'a>: Future<Output = ()> + 'a
    where
        Self: 'a;

    /// Sleep for a duration.
    fn sleep<'a>(&'a self, duration: Duration) -> Self::Sleep<'a>;

    /// Future returned by [`AsyncRuntime::timeout`].
    type Timeout<'a, F>: Future<Output = Result<F::Output, Self::TimeoutError>> + 'a
    where
        Self: 'a,
        F: Future + 'a;

    /// Run `future` but error with [`TimedOut`] if it doesn't complete within `duration`.
    fn timeout<'a, F>(&'a self, duration: Duration, future: F) -> Self::Timeout<'a, F>
    where
        F: Future + 'a;
}
