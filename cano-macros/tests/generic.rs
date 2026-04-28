//! Generic test: a trait with generic params and where bounds, mirroring
//! Cano's `Task<TState, TResourceKey>` pattern.

use cano_macros::task;
use std::hash::Hash;

/// A simplified mirror of Cano's `Task` trait.
#[task]
pub trait Task<TState, TResourceKey = String>: Send + Sync
where
    TState: Clone + std::fmt::Debug + Send + Sync + 'static,
    TResourceKey: Hash + Eq + Send + Sync + 'static,
{
    /// Required execution method with a reference arg.
    async fn run(&self, key: &TResourceKey) -> Result<TState, String>;

    /// Default-bodied no-arg method.
    async fn name(&self) -> String {
        std::any::type_name::<Self>().to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step {
    Done,
}

struct MyTask;

#[task]
impl Task<Step, String> for MyTask {
    async fn run(&self, key: &String) -> Result<Step, String> {
        let _ = key;
        Ok(Step::Done)
    }
}

/// An impl that also overrides the default method.
struct NamedTask;

#[task]
impl Task<Step, String> for NamedTask {
    async fn run(&self, _key: &String) -> Result<Step, String> {
        Ok(Step::Done)
    }

    async fn name(&self) -> String {
        "NamedTask".to_string()
    }
}

#[tokio::test]
async fn test_generic_trait_basic() {
    let t = MyTask;
    let key = "test".to_string();
    assert_eq!(t.run(&key).await, Ok(Step::Done));
}

#[tokio::test]
async fn test_generic_trait_default_name() {
    let t = MyTask;
    // The default body returns type_name::<MyTask>().
    let name = t.name().await;
    assert!(name.contains("MyTask"), "got: {name}");
}

#[tokio::test]
async fn test_generic_trait_overridden_name() {
    let t = NamedTask;
    assert_eq!(t.name().await, "NamedTask");
}

#[tokio::test]
async fn test_generic_trait_through_dyn() {
    let tasks: Vec<Box<dyn Task<Step, String> + Send + Sync>> =
        vec![Box::new(MyTask), Box::new(NamedTask)];
    let key = "k".to_string();
    for t in &tasks {
        assert_eq!(t.run(&key).await, Ok(Step::Done));
    }
}
