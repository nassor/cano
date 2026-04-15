//! Basic UI test: a trait with a single `async fn(&self) -> Result<(), ()>`
//! and an impl.  Confirms the fundamental codegen path compiles and runs.

use cano_macros::task;

#[task]
trait Greet {
    /// Required async method — no default body.
    async fn hello(&self) -> Result<String, ()>;
}

struct World;

#[task]
impl Greet for World {
    async fn hello(&self) -> Result<String, ()> {
        Ok("hello".to_string())
    }
}

/// A trait with a reference argument in addition to `&self`.
#[task]
trait Echo {
    async fn echo(&self, msg: &str) -> String;
}

struct Parrot;

#[task]
impl Echo for Parrot {
    async fn echo(&self, msg: &str) -> String {
        msg.to_string()
    }
}

/// Verify that the boxed future is `Send` (required for Tokio multi-thread).
fn assert_send<T: Send>(_: T) {}

#[tokio::test]
async fn test_basic_required_method() {
    let w = World;
    let result = w.hello().await.unwrap();
    assert_eq!(result, "hello");
}

#[tokio::test]
async fn test_basic_required_method_through_dyn() {
    let w: Box<dyn Greet + Send + Sync> = Box::new(World);
    let result = w.hello().await.unwrap();
    assert_eq!(result, "hello");
}

#[tokio::test]
async fn test_echo_with_ref_arg() {
    let p = Parrot;
    assert_eq!(p.echo("cano").await, "cano");
}

#[tokio::test]
async fn test_echo_through_dyn() {
    let p: Box<dyn Echo + Send + Sync> = Box::new(Parrot);
    assert_eq!(p.echo("cano").await, "cano");
}

#[tokio::test]
async fn test_boxed_future_is_send() {
    let w = World;
    // Calling .hello() returns Pin<Box<dyn Future + Send + 'async_trait>>.
    // We can move it across thread boundaries.
    let fut = w.hello();
    assert_send(fut);
}
