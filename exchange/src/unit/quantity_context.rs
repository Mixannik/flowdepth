//! Generation fence for quantities while adapters still use a process preference.
use super::qty::SizeUnit;
use std::sync::{
    RwLock,
    atomic::{AtomicU64, Ordering},
};

static EPOCH: QuantityEpoch = QuantityEpoch::new();

struct QuantityEpoch {
    // Low bit is the preference, remaining bits are the restart generation.
    value: AtomicU64,
    transition: RwLock<()>,
}

impl QuantityEpoch {
    const fn new() -> Self {
        Self {
            value: AtomicU64::new(1),
            transition: RwLock::new(()),
        }
    }

    fn capture(&self) -> QuantityContext {
        QuantityContext {
            value: self.value.load(Ordering::Acquire),
        }
    }

    fn set(&self, preference: SizeUnit) {
        let _guard = self
            .transition
            .write()
            .unwrap_or_else(|error| error.into_inner());
        let generation = (self.value.load(Ordering::Relaxed) & !1)
            .checked_add(2)
            .expect("quantity generation exhausted");
        self.value
            .store(generation | preference as u64, Ordering::Release);
    }

    fn ensure(&self, context: QuantityContext) -> Result<(), StaleQuantityContext> {
        if self.capture() == context {
            Ok(())
        } else {
            Err(StaleQuantityContext)
        }
    }

    fn with_current<T>(
        &self,
        context: QuantityContext,
        action: impl FnOnce() -> T,
    ) -> Result<T, StaleQuantityContext> {
        let _guard = self
            .transition
            .read()
            .unwrap_or_else(|error| error.into_inner());
        self.ensure(context)?;
        Ok(action())
    }

    async fn check_future<T>(
        &self,
        context: QuantityContext,
        request: impl std::future::Future<Output = T>,
    ) -> Result<T, StaleQuantityContext> {
        self.ensure(context)?;
        let result = request.await;
        self.ensure(context)?;
        Ok(result)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct QuantityContext {
    value: u64,
}

#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("Quantity context changed; discard this request and retry in the current session")]
pub struct StaleQuantityContext;

impl QuantityContext {
    pub fn capture() -> Self {
        EPOCH.capture()
    }
    pub fn preference(self) -> SizeUnit {
        if self.value & 1 == 0 {
            SizeUnit::Base
        } else {
            SizeUnit::Quote
        }
    }
    pub fn is_current(self) -> bool {
        EPOCH.ensure(self).is_ok()
    }
    pub fn ensure_current(self) -> Result<(), StaleQuantityContext> {
        EPOCH.ensure(self)
    }

    /// Synchronous boundary only: no network waits while holding this fence.
    /// Makes validation and cache/derived calculation atomic with a unit switch.
    pub fn with_current<T>(self, action: impl FnOnce() -> T) -> Result<T, StaleQuantityContext> {
        EPOCH.with_current(self, action)
    }

    /// Validate both before dispatch and after receipt, including ABA switches.
    pub async fn check_future<T>(
        self,
        request: impl std::future::Future<Output = T>,
    ) -> Result<T, StaleQuantityContext> {
        EPOCH.check_future(self, request).await
    }
}

pub(super) fn set_preference(preference: SizeUnit) {
    EPOCH.set(preference);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn round_trip_and_same_unit_restart_invalidate_old_contexts() {
        let epoch = QuantityEpoch::new();
        let before = epoch.capture();
        epoch.set(SizeUnit::Base);
        epoch.set(SizeUnit::Quote);
        assert_eq!(before.preference(), epoch.capture().preference());
        assert!(epoch.ensure(before).is_err());
        let before = epoch.capture();
        epoch.set(SizeUnit::Quote);
        assert!(epoch.ensure(before).is_err());
    }

    #[test]
    fn stale_context_cannot_execute_cache_or_delivery_action() {
        let epoch = QuantityEpoch::new();
        let context = epoch.capture();
        epoch.set(SizeUnit::Base);
        let mut records = vec![];
        assert!(epoch.with_current(context, || records.push(100)).is_err());
        assert!(records.is_empty());
        epoch
            .with_current(epoch.capture(), || records.push(1))
            .unwrap();
        assert_eq!(records, vec![1]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn delayed_response_is_rejected_after_unit_change() {
        let epoch = QuantityEpoch::new();
        let context = epoch.capture();
        let response = epoch
            .check_future(context, async {
                tokio::task::yield_now().await;
                epoch.set(SizeUnit::Base);
                "old quote data"
            })
            .await;
        assert!(response.is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn obsolete_queued_request_is_not_polled() {
        let epoch = QuantityEpoch::new();
        let context = epoch.capture();
        epoch.set(SizeUnit::Base);
        let mut called = false;
        assert!(
            epoch
                .check_future(context, async {
                    called = true;
                })
                .await
                .is_err()
        );
        assert!(!called);
    }
}
