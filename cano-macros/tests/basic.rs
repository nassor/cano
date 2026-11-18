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

#[task]
trait LifetimeAndGenericOps {
    async fn select_non_empty<'a>(&'a self, primary: &'a str, fallback: Option<&str>) -> &'a str;

    async fn echo_pair(&self, pair: (&str, &[u8])) -> usize;

    async fn map_generic<T: Send + 'static>(&self, value: T) -> T;
}

struct Holder {
    default_label: String,
}

#[task]
impl LifetimeAndGenericOps for Holder {
    async fn select_non_empty<'a>(&'a self, primary: &'a str, fallback: Option<&str>) -> &'a str {
        if !primary.is_empty() {
            primary
        } else if fallback.is_some_and(|f| !f.is_empty()) {
            &self.default_label
        } else {
            ""
        }
    }

    async fn echo_pair(&self, pair: (&str, &[u8])) -> usize {
        pair.0.len() + pair.1.len()
    }

    async fn map_generic<T: Send + 'static>(&self, value: T) -> T {
        value
    }
}

#[tokio::test]
async fn explicit_lifetimes_nested_refs_and_generics_compile_and_run() {
    let h = Holder {
        default_label: "fallback-from-self".to_string(),
    };
    let s = String::from("picked");
    assert_eq!(h.select_non_empty(&s, None).await, "picked");
    let fb = String::from("non-empty");
    assert_eq!(
        h.select_non_empty("", Some(fb.as_str())).await,
        "fallback-from-self"
    );
    assert_eq!(h.echo_pair(("abc", &[1, 2, 3, 4])).await, 7);
    assert_eq!(h.map_generic(123u64).await, 123u64);
}
