//! Integration tests for `#[cano::task::stream]` — the inherent form (which emits
//! `::cano::` paths, so it only resolves *outside* the `cano` crate), the `key =` form,
//! and the explicit trait-impl form. Exercises the companion `impl Task` and the
//! engine-driven `register_stream` path.

use cano::prelude::*;
use futures_util::{Stream, stream};
use std::pin::Pin;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step {
    Consume,
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Key {
    Store,
}

// 1. Inherent form with inferred Item / Output / Cursor and default window / config / name.
struct InherentInferred;

#[task::stream(state = Step)]
impl InherentInferred {
    async fn open(
        &self,
        _res: &Resources,
        _cursor: Option<u64>,
    ) -> Result<Pin<Box<dyn Stream<Item = u32> + Send>>, CanoError> {
        Ok(Box::pin(stream::iter(vec![1u32, 2, 3])) as Pin<Box<dyn Stream<Item = u32> + Send>>)
    }

    async fn process_item(&self, _res: &Resources, item: u32) -> Result<(String, u64), CanoError> {
        Ok((format!("v={item}"), item as u64))
    }

    async fn flush_window(
        &self,
        _res: &Resources,
        outputs: Vec<String>,
    ) -> Result<WindowSignal<Step>, CanoError> {
        // Default window is Count(1): one item per window.
        assert_eq!(outputs.len(), 1);
        Ok(WindowSignal::Continue)
    }

    async fn on_close(
        &self,
        _res: &Resources,
        reason: CloseReason,
    ) -> Result<TaskResult<Step>, CanoError> {
        assert_eq!(reason, CloseReason::Exhausted);
        Ok(TaskResult::Single(Step::Done))
    }
}

#[test]
fn inherent_default_window_is_count_one() {
    assert_eq!(
        StreamTask::<Step>::window(&InherentInferred),
        StreamWindow::Count(1)
    );
}

#[test]
fn inherent_default_error_policy_is_fail_fast() {
    assert_eq!(
        StreamTask::<Step>::on_item_error(&InherentInferred),
        StreamErrorPolicy::FailFast
    );
}

#[test]
fn inherent_default_config_is_minimal() {
    // TaskConfig::minimal() → 1 attempt (no retries).
    assert_eq!(Task::config(&InherentInferred).retry_mode.max_attempts(), 1);
}

#[test]
fn inherent_default_name_contains_type_name() {
    assert!(StreamTask::<Step>::name(&InherentInferred).contains("InherentInferred"));
}

#[tokio::test]
async fn inherent_runs_via_register_stream() {
    let workflow = Workflow::bare()
        .register_stream(Step::Consume, InherentInferred)
        .add_exit_state(Step::Done);
    let result = workflow
        .orchestrate(Step::Consume, CancellationToken::disabled())
        .await
        .unwrap();
    assert_eq!(result, Step::Done);
}

#[tokio::test]
async fn inherent_runs_via_register_in_memory() {
    let res = Resources::new();
    let result = Task::run(&InherentInferred, &res).await.unwrap();
    assert_eq!(result, TaskResult::Single(Step::Done));
}

// 2. Inherent form with `key = Key` and an overridden window.
struct InherentWithKey;

#[task::stream(state = Step, key = Key)]
impl InherentWithKey {
    fn window(&self) -> StreamWindow {
        StreamWindow::Count(2)
    }

    async fn open(
        &self,
        res: &Resources<Key>,
        _cursor: Option<u64>,
    ) -> Result<Pin<Box<dyn Stream<Item = u32> + Send>>, CanoError> {
        // Exercise the enum-keyed resource lookup (constructs `Key::Store`).
        let _ = res.get::<MemoryStore, _>(&Key::Store);
        Ok(Box::pin(stream::iter(vec![10u32, 20, 30, 40]))
            as Pin<Box<dyn Stream<Item = u32> + Send>>)
    }

    async fn process_item(
        &self,
        _res: &Resources<Key>,
        item: u32,
    ) -> Result<(u32, u64), CanoError> {
        Ok((item * 10, item as u64))
    }

    async fn flush_window(
        &self,
        _res: &Resources<Key>,
        outputs: Vec<u32>,
    ) -> Result<WindowSignal<Step>, CanoError> {
        assert_eq!(outputs.len(), 2);
        Ok(WindowSignal::Continue)
    }

    async fn on_close(
        &self,
        _res: &Resources<Key>,
        _reason: CloseReason,
    ) -> Result<TaskResult<Step>, CanoError> {
        Ok(TaskResult::Single(Step::Done))
    }
}

#[tokio::test]
async fn inherent_with_key_runs() {
    let resources = Resources::<Key>::new();
    let result = Task::run(&InherentWithKey, &resources).await.unwrap();
    assert_eq!(result, TaskResult::Single(Step::Done));
}

// 3. Trait-impl form with explicit associated types + an explicit `on_item_error`.
struct TraitStream;

#[task::stream]
impl StreamTask<Step> for TraitStream {
    type Item = u32;
    type Output = u32;
    type Cursor = u64;

    fn on_item_error(&self) -> StreamErrorPolicy {
        StreamErrorPolicy::SkipAndContinue
    }

    async fn open(
        &self,
        _res: &Resources,
        _cursor: Option<Self::Cursor>,
    ) -> Result<Pin<Box<dyn Stream<Item = Self::Item> + Send>>, CanoError> {
        Ok(Box::pin(stream::iter(vec![5u32])) as Pin<Box<dyn Stream<Item = u32> + Send>>)
    }

    async fn process_item(
        &self,
        _res: &Resources,
        item: Self::Item,
    ) -> Result<(Self::Output, Self::Cursor), CanoError> {
        Ok((item + 1, item as u64))
    }

    async fn flush_window(
        &self,
        _res: &Resources,
        outputs: Vec<Self::Output>,
    ) -> Result<WindowSignal<Step>, CanoError> {
        assert_eq!(outputs, vec![6u32]);
        Ok(WindowSignal::Continue)
    }

    async fn on_close(
        &self,
        _res: &Resources,
        _reason: CloseReason,
    ) -> Result<TaskResult<Step>, CanoError> {
        Ok(TaskResult::Single(Step::Done))
    }
}

#[test]
fn trait_form_error_policy_overridden() {
    assert_eq!(
        StreamTask::<Step>::on_item_error(&TraitStream),
        StreamErrorPolicy::SkipAndContinue
    );
}

#[tokio::test]
async fn trait_form_runs_and_is_dyn_task() {
    let task: Arc<dyn Task<Step>> = Arc::new(TraitStream);
    let res = Resources::new();
    let result = Task::run(task.as_ref(), &res).await.unwrap();
    assert_eq!(result, TaskResult::Single(Step::Done));
}
