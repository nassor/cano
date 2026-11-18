//! UI test: `#[cano_macros::scheduler_store]` rewrites async trait definitions and impls
//! into `Send`, `dyn`-compatible futures.

use cano_macros::scheduler_store;

#[scheduler_store]
trait MiniRegister: Send + Sync + 'static {
    async fn read(&self, id: &str) -> Option<u64>;
    async fn swap(&self, id: &str, expected: Option<u64>, next: u64) -> bool;
}

struct Dummy(u64);

#[scheduler_store]
impl MiniRegister for Dummy {
    async fn read(&self, _id: &str) -> Option<u64> {
        Some(self.0)
    }

    async fn swap(&self, _id: &str, expected: Option<u64>, _next: u64) -> bool {
        expected == Some(self.0)
    }
}

fn assert_send<T: Send>(_: T) {}

#[tokio::test]
async fn scheduler_store_macro_is_dyn_compatible_and_send() {
    let store: std::sync::Arc<dyn MiniRegister> = std::sync::Arc::new(Dummy(7));
    let fut = store.read("flow");
    assert_send(fut);
    assert_eq!(store.read("flow").await, Some(7));
    assert!(store.swap("flow", Some(7), 8).await);
    assert!(!store.swap("flow", Some(6), 8).await);
}
