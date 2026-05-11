//! Integration tests for `#[cano::task::batch]` — both the inherent form (which emits
//! `::cano::` paths) and the trait-impl form. These run outside the `cano` crate so
//! `::cano::` paths resolve correctly and the synthesised `impl Task` companion impl
//! with `::cano::task::batch::run_batch` in its body compiles as expected.

use cano::prelude::*;
use std::borrow::Cow;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

// ---------------------------------------------------------------------------
// Shared state enums
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step {
    Process,
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Key {
    Store,
}

// ---------------------------------------------------------------------------
// 1. Inherent form — inferred `Item` and `ItemOutput`, default `concurrency` / `item_retry`
// ---------------------------------------------------------------------------

/// `#[batch_task(state = S)] impl T { ... }` with NO explicit `type Item` or
/// `type ItemOutput`. The macro should infer both from `process_item`'s parameter
/// and return type respectively.
struct InherentInferred;

#[task::batch(state = Step)]
impl InherentInferred {
    // concurrency() and item_retry() deliberately omitted — defaults injected by macro.

    async fn load(&self, _res: &Resources) -> Result<Vec<u32>, CanoError> {
        Ok(vec![10, 20, 30])
    }

    async fn process_item(&self, item: &u32) -> Result<String, CanoError> {
        Ok(format!("{item}"))
    }

    async fn finish(
        &self,
        _res: &Resources,
        outputs: Vec<Result<String, CanoError>>,
    ) -> Result<TaskResult<Step>, CanoError> {
        let strings: Vec<String> = outputs.into_iter().map(|r| r.unwrap()).collect();
        assert_eq!(strings, vec!["10", "20", "30"]);
        Ok(TaskResult::Single(Step::Done))
    }
}

#[test]
fn inherent_inferred_has_default_concurrency() {
    // Default injected by macro: concurrency() == 1
    assert_eq!(BatchTask::<Step>::concurrency(&InherentInferred), 1);
}

#[test]
fn inherent_inferred_has_default_item_retry_none() {
    // Default injected: RetryMode::None → max_attempts == 1
    assert_eq!(
        BatchTask::<Step>::item_retry(&InherentInferred).max_attempts(),
        1
    );
}

#[test]
fn inherent_inferred_has_default_name() {
    assert!(
        BatchTask::<Step>::name(&InherentInferred).contains("InherentInferred"),
        "default name must contain the type name"
    );
}

#[test]
fn inherent_inferred_companion_task_forwards_config() {
    // Task::config should delegate to BatchTask::config
    let max_attempts = Task::config(&InherentInferred).retry_mode.max_attempts();
    // Default TaskConfig has 4 max_attempts (3 retries + initial)
    assert_eq!(max_attempts, 4);
}

#[test]
fn inherent_inferred_companion_task_forwards_name() {
    assert!(Task::name(&InherentInferred).contains("InherentInferred"));
}

#[tokio::test]
async fn inherent_inferred_task_run_works() {
    let res = Resources::new();
    let result = Task::run(&InherentInferred, &res).await.unwrap();
    assert_eq!(result, TaskResult::Single(Step::Done));
}

#[tokio::test]
async fn inherent_inferred_integrates_with_workflow() {
    let workflow = Workflow::bare()
        .register(Step::Process, InherentInferred)
        .add_exit_state(Step::Done);

    let result = workflow.orchestrate(Step::Process).await.unwrap();
    assert_eq!(result, Step::Done);
}

// ---------------------------------------------------------------------------
// 2. Inherent form — explicit `type Item` and `type ItemOutput` written by user
// ---------------------------------------------------------------------------

/// User writes `type Item` and `type ItemOutput` explicitly inside the inherent impl.
/// The macro must not infer them (they already exist) and must still compile.
struct InherentExplicitTypes;

#[task::batch(state = Step)]
impl InherentExplicitTypes {
    type Item = i64;
    type ItemOutput = i64;

    async fn load(&self, _res: &Resources) -> Result<Vec<i64>, CanoError> {
        Ok(vec![-1, 0, 1])
    }

    async fn process_item(&self, item: &i64) -> Result<i64, CanoError> {
        Ok(item.abs())
    }

    async fn finish(
        &self,
        _res: &Resources,
        outputs: Vec<Result<i64, CanoError>>,
    ) -> Result<TaskResult<Step>, CanoError> {
        let vals: Vec<i64> = outputs.into_iter().map(|r| r.unwrap()).collect();
        assert_eq!(vals, vec![1, 0, 1]);
        Ok(TaskResult::Single(Step::Done))
    }
}

#[tokio::test]
async fn explicit_types_compile_and_run() {
    let res = Resources::new();
    let result = Task::run(&InherentExplicitTypes, &res).await.unwrap();
    assert_eq!(result, TaskResult::Single(Step::Done));
}

// ---------------------------------------------------------------------------
// 3. Inherent form with `key = SomeEnum` — custom `TResourceKey`
// ---------------------------------------------------------------------------

struct InherentWithKey;

#[task::batch(state = Step, key = Key)]
impl InherentWithKey {
    async fn load(&self, res: &Resources<Key>) -> Result<Vec<u32>, CanoError> {
        let store = res.get::<MemoryStore, _>(&Key::Store)?;
        let items: Vec<u32> = store.get("items")?;
        Ok(items)
    }

    async fn process_item(&self, item: &u32) -> Result<u32, CanoError> {
        Ok(item * 2)
    }

    async fn finish(
        &self,
        _res: &Resources<Key>,
        outputs: Vec<Result<u32, CanoError>>,
    ) -> Result<TaskResult<Step>, CanoError> {
        let doubled: Vec<u32> = outputs.into_iter().map(|r| r.unwrap()).collect();
        assert_eq!(doubled, vec![2, 4, 6]);
        Ok(TaskResult::Single(Step::Done))
    }
}

#[tokio::test]
async fn inherent_with_key_uses_custom_resource_key() {
    let store = MemoryStore::new();
    store.put("items", vec![1u32, 2, 3]).unwrap();
    let resources = Resources::<Key>::new().insert(Key::Store, store);

    let result = Task::run(&InherentWithKey, &resources).await.unwrap();
    assert_eq!(result, TaskResult::Single(Step::Done));
}

#[tokio::test]
async fn inherent_with_key_integrates_with_workflow() {
    let store = MemoryStore::new();
    store.put("items", vec![1u32, 2, 3]).unwrap();
    let resources = Resources::<Key>::new().insert(Key::Store, store);

    let workflow = Workflow::new(resources)
        .register(Step::Process, InherentWithKey)
        .add_exit_state(Step::Done);

    let result = workflow.orchestrate(Step::Process).await.unwrap();
    assert_eq!(result, Step::Done);
}

// ---------------------------------------------------------------------------
// 4. Inherent form with overridden `concurrency()` and `item_retry()`
// ---------------------------------------------------------------------------

struct InherentWithOverrides {
    call_count: AtomicU32,
}

#[task::batch(state = Step)]
impl InherentWithOverrides {
    fn concurrency(&self) -> usize {
        4
    }

    fn item_retry(&self) -> RetryMode {
        RetryMode::fixed(2, Duration::from_millis(1))
    }

    async fn load(&self, _res: &Resources) -> Result<Vec<u32>, CanoError> {
        Ok(vec![1, 2, 3, 4])
    }

    async fn process_item(&self, item: &u32) -> Result<u32, CanoError> {
        let n = self.call_count.fetch_add(1, Ordering::Relaxed);
        // Fail on the very first call only; second+ succeed.
        if n == 0 {
            Err(CanoError::task_execution("first call fails"))
        } else {
            Ok(*item)
        }
    }

    async fn finish(
        &self,
        _res: &Resources,
        outputs: Vec<Result<u32, CanoError>>,
    ) -> Result<TaskResult<Step>, CanoError> {
        // All 4 items should succeed after retry
        assert_eq!(outputs.len(), 4);
        assert!(
            outputs.iter().all(|r| r.is_ok()),
            "all items should succeed"
        );
        Ok(TaskResult::Single(Step::Done))
    }
}

#[test]
fn inherent_overrides_concurrency_is_applied() {
    assert_eq!(
        BatchTask::<Step>::concurrency(&InherentWithOverrides {
            call_count: AtomicU32::new(0),
        }),
        4
    );
}

#[test]
fn inherent_overrides_item_retry_is_applied() {
    // fixed(2, ...) → max_attempts == 3 (initial + 2 retries)
    assert_eq!(
        BatchTask::<Step>::item_retry(&InherentWithOverrides {
            call_count: AtomicU32::new(0),
        })
        .max_attempts(),
        3
    );
}

#[tokio::test]
async fn inherent_overrides_retry_recovers_first_item() {
    let task = InherentWithOverrides {
        call_count: AtomicU32::new(0),
    };
    let res = Resources::new();
    let result = Task::run(&task, &res).await.unwrap();
    assert_eq!(result, TaskResult::Single(Step::Done));
    // First item fails once, retries once; items 2-4 succeed first try.
    // So: 2 calls for item 1 (order may vary with concurrency=4) + 1 each for others.
    // Total >= 5 calls.
    assert!(task.call_count.load(Ordering::Relaxed) >= 5);
}

// ---------------------------------------------------------------------------
// 5. Inherent form with overridden `name()` and `config()`
// ---------------------------------------------------------------------------

struct InherentCustomMeta;

#[task::batch(state = Step)]
impl InherentCustomMeta {
    fn config(&self) -> TaskConfig {
        TaskConfig::minimal()
    }

    fn name(&self) -> Cow<'static, str> {
        Cow::Borrowed("my-batch-processor")
    }

    async fn load(&self, _res: &Resources) -> Result<Vec<u32>, CanoError> {
        Ok(vec![1])
    }

    async fn process_item(&self, item: &u32) -> Result<u32, CanoError> {
        Ok(*item)
    }

    async fn finish(
        &self,
        _res: &Resources,
        _outputs: Vec<Result<u32, CanoError>>,
    ) -> Result<TaskResult<Step>, CanoError> {
        Ok(TaskResult::Single(Step::Done))
    }
}

#[test]
fn inherent_custom_name_forwarded_to_batch_task() {
    assert_eq!(
        BatchTask::<Step>::name(&InherentCustomMeta),
        "my-batch-processor"
    );
}

#[test]
fn inherent_custom_name_forwarded_to_task() {
    assert_eq!(Task::name(&InherentCustomMeta), "my-batch-processor");
}

#[test]
fn inherent_custom_config_minimal_forwarded_to_task() {
    // TaskConfig::minimal() → 1 max_attempt (no retries)
    assert_eq!(
        Task::config(&InherentCustomMeta).retry_mode.max_attempts(),
        1
    );
}

// ---------------------------------------------------------------------------
// 6. Trait-impl form: `#[task::batch] impl BatchTask<S> for T { ... }`
// ---------------------------------------------------------------------------

struct TraitBatch;

#[task::batch]
impl BatchTask<Step> for TraitBatch {
    type Item = u32;
    type ItemOutput = u32;

    fn concurrency(&self) -> usize {
        2
    }

    async fn load(&self, _res: &Resources) -> Result<Vec<Self::Item>, CanoError> {
        Ok(vec![100, 200, 300])
    }

    async fn process_item(&self, item: &Self::Item) -> Result<Self::ItemOutput, CanoError> {
        Ok(item + 1)
    }

    async fn finish(
        &self,
        _res: &Resources,
        outputs: Vec<Result<Self::ItemOutput, CanoError>>,
    ) -> Result<TaskResult<Step>, CanoError> {
        let vals: Vec<u32> = outputs.into_iter().map(|r| r.unwrap()).collect();
        assert_eq!(vals, vec![101, 201, 301]);
        Ok(TaskResult::Single(Step::Done))
    }
}

#[tokio::test]
async fn trait_form_batch_task_run_works() {
    let res = Resources::new();
    let result = Task::run(&TraitBatch, &res).await.unwrap();
    assert_eq!(result, TaskResult::Single(Step::Done));
}

#[tokio::test]
async fn trait_form_batch_task_as_dyn_task() {
    let task: Arc<dyn Task<Step>> = Arc::new(TraitBatch);
    let res = Resources::new();
    let result = Task::run(task.as_ref(), &res).await.unwrap();
    assert_eq!(result, TaskResult::Single(Step::Done));
}

#[tokio::test]
async fn trait_form_integrates_with_workflow() {
    let workflow = Workflow::bare()
        .register(Step::Process, TraitBatch)
        .add_exit_state(Step::Done);

    let result = workflow.orchestrate(Step::Process).await.unwrap();
    assert_eq!(result, Step::Done);
}

// ---------------------------------------------------------------------------
// 7. Partial-failure tolerance (inherent form): one item Errs, finish decides policy
// ---------------------------------------------------------------------------

struct PartialFailInherent;

#[task::batch(state = Step)]
impl PartialFailInherent {
    async fn load(&self, _res: &Resources) -> Result<Vec<u32>, CanoError> {
        Ok(vec![1, 2, 3])
    }

    async fn process_item(&self, item: &u32) -> Result<u32, CanoError> {
        if *item == 2 {
            Err(CanoError::task_execution("item 2 fails"))
        } else {
            Ok(*item * 10)
        }
    }

    async fn finish(
        &self,
        _res: &Resources,
        outputs: Vec<Result<u32, CanoError>>,
    ) -> Result<TaskResult<Step>, CanoError> {
        // Partial success policy: collect Oks, tolerate Errs.
        assert_eq!(outputs.len(), 3);
        assert!(outputs[0].is_ok());
        assert!(outputs[1].is_err(), "item 2 slot must be Err");
        assert!(outputs[2].is_ok());

        let sum: u32 = outputs.into_iter().filter_map(|r| r.ok()).sum();
        assert_eq!(sum, 40); // 10 + 30
        Ok(TaskResult::Single(Step::Done))
    }
}

#[tokio::test]
async fn partial_failure_err_slot_reaches_finish_and_task_succeeds() {
    let res = Resources::new();
    let result = Task::run(&PartialFailInherent, &res).await.unwrap();
    assert_eq!(result, TaskResult::Single(Step::Done));
}

// ---------------------------------------------------------------------------
// 8. load Err propagates; finish never called (inherent form)
// ---------------------------------------------------------------------------

struct LoadErrInherent {
    finish_called: Arc<std::sync::atomic::AtomicBool>,
}

#[task::batch(state = Step)]
impl LoadErrInherent {
    async fn load(&self, _res: &Resources) -> Result<Vec<u32>, CanoError> {
        Err(CanoError::task_execution("load failed"))
    }

    async fn process_item(&self, item: &u32) -> Result<u32, CanoError> {
        Ok(*item)
    }

    async fn finish(
        &self,
        _res: &Resources,
        _outputs: Vec<Result<u32, CanoError>>,
    ) -> Result<TaskResult<Step>, CanoError> {
        self.finish_called
            .store(true, std::sync::atomic::Ordering::Relaxed);
        Ok(TaskResult::Single(Step::Done))
    }
}

#[tokio::test]
async fn load_error_propagates_finish_never_called() {
    let finish_called = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let task = LoadErrInherent {
        finish_called: Arc::clone(&finish_called),
    };
    let res = Resources::new();
    let err = Task::run(&task, &res).await.unwrap_err();
    assert!(
        matches!(err, CanoError::TaskExecution(_)),
        "load Err should propagate as TaskExecution, got: {err:?}"
    );
    assert!(
        !finish_called.load(std::sync::atomic::Ordering::Relaxed),
        "finish must not be called when load fails"
    );
}

// ---------------------------------------------------------------------------
// 9. End-to-end: multi-state workflow (inherent → workflow → exit)
// ---------------------------------------------------------------------------

struct LoadStep;

#[task::batch(state = Step)]
impl LoadStep {
    fn concurrency(&self) -> usize {
        3
    }

    async fn load(&self, res: &Resources) -> Result<Vec<u32>, CanoError> {
        let store = res.get::<MemoryStore, str>("store")?;
        let items: Vec<u32> = store.get("input")?;
        Ok(items)
    }

    async fn process_item(&self, item: &u32) -> Result<u32, CanoError> {
        Ok(item.wrapping_mul(2))
    }

    async fn finish(
        &self,
        res: &Resources,
        outputs: Vec<Result<u32, CanoError>>,
    ) -> Result<TaskResult<Step>, CanoError> {
        let doubled: Vec<u32> = outputs.into_iter().map(|r| r.unwrap()).collect();
        let store = res.get::<MemoryStore, str>("store")?;
        store.put("output", doubled)?;
        Ok(TaskResult::Single(Step::Done))
    }
}

#[tokio::test]
async fn end_to_end_workflow_load_process_finish() {
    let store = MemoryStore::new();
    store.put("input", vec![1u32, 2, 3, 4, 5]).unwrap();

    let resources = Resources::new().insert("store", store.clone());

    let workflow = Workflow::new(resources)
        .register(Step::Process, LoadStep)
        .add_exit_state(Step::Done);

    let result = workflow.orchestrate(Step::Process).await.unwrap();
    assert_eq!(result, Step::Done);

    let output: Vec<u32> = store.get("output").unwrap();
    assert_eq!(output, vec![2, 4, 6, 8, 10]);
}
