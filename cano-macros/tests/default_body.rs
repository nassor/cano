//! Default body test: a trait with a default-bodied `async fn` that calls `.await`,
//! plus an impl that does NOT override it.  This is the exact gap that `trait_variant`
//! 0.1.2 couldn't fill — default async bodies in trait defs.

use cano_macros::task;

#[task]
trait WithDefault {
    /// Default async body — delegates to an inner async call.
    async fn compute(&self) -> u32 {
        self.inner().await * 2
    }

    /// Required helper — impls must provide this.
    async fn inner(&self) -> u32;
}

/// This impl does NOT override `compute` — it inherits the default body.
struct Base;

#[task]
impl WithDefault for Base {
    async fn inner(&self) -> u32 {
        21
    }
}

/// This impl DOES override `compute`.
struct Custom;

#[task]
impl WithDefault for Custom {
    async fn inner(&self) -> u32 {
        0
    }

    async fn compute(&self) -> u32 {
        100
    }
}

#[tokio::test]
async fn test_default_body_used_when_not_overridden() {
    let b = Base;
    // Default body: inner() returns 21, compute() returns 21 * 2 = 42.
    assert_eq!(b.compute().await, 42);
}

#[tokio::test]
async fn test_default_body_overridden() {
    let c = Custom;
    assert_eq!(c.compute().await, 100);
}

#[tokio::test]
async fn test_default_body_through_dyn() {
    let b: Box<dyn WithDefault + Send + Sync> = Box::new(Base);
    assert_eq!(b.compute().await, 42);

    let c: Box<dyn WithDefault + Send + Sync> = Box::new(Custom);
    assert_eq!(c.compute().await, 100);
}
