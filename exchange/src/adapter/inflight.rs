//! Share identical pending requests without retaining market data or errors.
use futures::FutureExt;
use futures::future::{BoxFuture, Shared, WeakShared};
use std::{collections::HashMap, future::Future, hash::Hash, sync::Mutex};

const MAX_TRACKED: usize = 64;

pub(super) struct InFlight<K, V> {
    pending: Mutex<HashMap<K, WeakShared<BoxFuture<'static, V>>>>,
}

impl<K, V> Default for InFlight<K, V> {
    fn default() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
        }
    }
}

impl<K: Eq + Hash, V: Clone + Send + Sync + 'static> InFlight<K, V> {
    pub fn get_or_insert<F: Future<Output = V> + Send + 'static>(
        &self,
        key: K,
        create: impl FnOnce() -> F,
    ) -> Shared<BoxFuture<'static, V>> {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        pending.retain(|_, future| future.upgrade().is_some());
        if let Some(future) = pending.get(&key).and_then(WeakShared::upgrade) {
            log::debug!("KLINE SharedPageJoin");
            return future;
        }
        let future = create().boxed().shared();
        // At capacity, run independently. Never evict a live request or keep a
        // strong reference that would outlive all subscribing panels.
        if pending.len() < MAX_TRACKED {
            pending.insert(key, future.downgrade().expect("unpolled shared future"));
        }
        future
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        pin::Pin,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        task::{Context, Poll},
    };

    fn request(
        gate: Arc<AtomicBool>,
        calls: Arc<AtomicUsize>,
    ) -> impl Future<Output = Result<u8, &'static str>> + Send {
        async move {
            calls.fetch_add(1, Ordering::SeqCst);
            futures::future::poll_fn(move |_| {
                if gate.load(Ordering::SeqCst) {
                    Poll::Ready(Ok(7))
                } else {
                    Poll::Pending
                }
            })
            .await
        }
    }

    #[test]
    fn concurrent_waiters_share_one_request_and_survive_one_cancellation() {
        let table = InFlight::default();
        let gate = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(AtomicUsize::new(0));
        let mut first = table.get_or_insert(1, || request(gate.clone(), calls.clone()));
        let mut second = table.get_or_insert(1, || request(gate.clone(), calls.clone()));
        let mut cx = Context::from_waker(futures::task::noop_waker_ref());
        assert!(Pin::new(&mut first).poll(&mut cx).is_pending());
        assert!(Pin::new(&mut second).poll(&mut cx).is_pending());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        drop(first);
        gate.store(true, Ordering::SeqCst);
        assert_eq!(Pin::new(&mut second).poll(&mut cx), Poll::Ready(Ok(7)));
        drop(second);
        let mut next = table.get_or_insert(1, || request(gate.clone(), calls.clone()));
        assert_eq!(Pin::new(&mut next).poll(&mut cx), Poll::Ready(Ok(7)));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn failures_and_cancelled_requests_are_not_cached_and_keys_are_independent() {
        let table = InFlight::<u64, Result<u8, &str>>::default();
        let mut cx = Context::from_waker(futures::task::noop_waker_ref());
        let mut failed = table.get_or_insert(1, || async { Err("offline") });
        assert_eq!(
            Pin::new(&mut failed).poll(&mut cx),
            Poll::Ready(Err("offline"))
        );
        drop(failed);
        let cancelled = table.get_or_insert(1, futures::future::pending);
        let mut other = table.get_or_insert(2, || async { Ok(2) });
        assert_eq!(Pin::new(&mut other).poll(&mut cx), Poll::Ready(Ok(2)));
        drop(cancelled);
        let mut retried = table.get_or_insert(1, || async { Ok(3) });
        assert_eq!(Pin::new(&mut retried).poll(&mut cx), Poll::Ready(Ok(3)));
        assert!(table.pending.lock().unwrap().len() <= MAX_TRACKED);
    }
}
