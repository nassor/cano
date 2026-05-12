//! Integration tests for `#[cano::task::stepped]` — both the inherent form (which emits
//! `::cano::` paths) and the trait-impl form. These run outside the `cano` crate so
//! `::cano::` paths resolve correctly and the synthesised `impl Task` companion impl
//! with `::cano::task::stepped::run_stepped` in its body compiles as expected.

use cano::prelude::*;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

// ---------------------------------------------------------------------------
// Shared state enums
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum MyState {
    Work,
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[allow(dead_code)]
enum Key {
    Store,
}

// ---------------------------------------------------------------------------
// 1. Inherent form — inferred `Cursor`, default `config` / `name`
// ---------------------------------------------------------------------------

/// `#[task::stepped(state = S)] impl T { ... }` with NO explicit `type Cursor`.
/// The macro should infer `Cursor = u32` from `cursor: Option<u32>`.
struct InherentInferred {
    total: u32,
}

#[task::stepped(state = MyState)]
impl InherentInferred {
    // config() and name() deliberately omitted — defaults injected by macro.

    async fn step(
        &self,
        _res: &Resources,
        cursor: Option<u32>,
    ) -> Result<StepOutcome<u32, MyState>, CanoError> {
        let n = cursor.unwrap_or(0) + 1;
        if n >= self.total {
            Ok(StepOutcome::Done(TaskResult::Single(MyState::Done)))
        } else {
            Ok(StepOutcome::More(n))
        }
    }
}

#[test]
fn inherent_inferred_has_default_config_with_retries() {
    // Default injected by macro: TaskConfig::default() has exponential backoff
    assert!(
        SteppedTask::<MyState>::config(&InherentInferred { total: 3 })
            .retry_mode
            .max_attempts()
            > 1,
        "default config must have retries"
    );
}

#[test]
fn inherent_inferred_has_default_name() {
    assert!(
        SteppedTask::<MyState>::name(&InherentInferred { total: 1 }).contains("InherentInferred"),
        "default name must contain the type name"
    );
}

#[tokio::test]
async fn inherent_inferred_step_loop_completes() {
    let stepper = InherentInferred { total: 3 };
    let res = Resources::new();
    let result = Task::run(&stepper, &res).await.unwrap();
    assert_eq!(result, TaskResult::Single(MyState::Done));
}

// ---------------------------------------------------------------------------
// 2. Inherent form — explicit `type Cursor`
// ---------------------------------------------------------------------------

/// Same as InherentInferred but with an explicit `type Cursor = u32` in the impl block.
/// The macro should NOT inject a second `type Cursor` injection.
struct InherentExplicitCursor {
    total: u32,
}

#[task::stepped(state = MyState)]
impl InherentExplicitCursor {
    type Cursor = u32;

    async fn step(
        &self,
        _res: &Resources,
        cursor: Option<u32>,
    ) -> Result<StepOutcome<u32, MyState>, CanoError> {
        let n = cursor.unwrap_or(0) + 1;
        if n >= self.total {
            Ok(StepOutcome::Done(TaskResult::Single(MyState::Done)))
        } else {
            Ok(StepOutcome::More(n))
        }
    }
}

#[tokio::test]
async fn inherent_explicit_cursor_compiles_and_runs() {
    let stepper = InherentExplicitCursor { total: 2 };
    let res = Resources::new();
    let result = Task::run(&stepper, &res).await.unwrap();
    assert_eq!(result, TaskResult::Single(MyState::Done));
}

// ---------------------------------------------------------------------------
// 3. Inherent form — with custom key type
// ---------------------------------------------------------------------------

struct InherentWithKey;

#[task::stepped(state = MyState, key = Key)]
impl InherentWithKey {
    async fn step(
        &self,
        _res: &Resources<Key>,
        cursor: Option<u32>,
    ) -> Result<StepOutcome<u32, MyState>, CanoError> {
        let _ = cursor;
        Ok(StepOutcome::Done(TaskResult::Single(MyState::Done)))
    }
}

#[tokio::test]
async fn inherent_with_key_runs() {
    let stepper = InherentWithKey;
    let res: Resources<Key> = Resources::new();
    let result = Task::run(&stepper, &res).await.unwrap();
    assert_eq!(result, TaskResult::Single(MyState::Done));
}

// ---------------------------------------------------------------------------
// 4. Inherent form — config and name overrides
// ---------------------------------------------------------------------------

struct InherentWithOverrides;

#[task::stepped(state = MyState)]
impl InherentWithOverrides {
    fn config(&self) -> TaskConfig {
        TaskConfig::minimal()
    }

    fn name(&self) -> Cow<'static, str> {
        Cow::Borrowed("my-stepped-task")
    }

    async fn step(
        &self,
        _res: &Resources,
        cursor: Option<u32>,
    ) -> Result<StepOutcome<u32, MyState>, CanoError> {
        let _ = cursor;
        Ok(StepOutcome::Done(TaskResult::Single(MyState::Done)))
    }
}

#[test]
fn inherent_config_override_minimal() {
    let stepper = InherentWithOverrides;
    assert_eq!(
        SteppedTask::<MyState>::config(&stepper)
            .retry_mode
            .max_attempts(),
        1
    );
}

#[test]
fn inherent_name_override() {
    let stepper = InherentWithOverrides;
    assert_eq!(SteppedTask::<MyState>::name(&stepper), "my-stepped-task");
}

#[test]
fn companion_task_forwards_config_and_name() {
    let stepper = InherentWithOverrides;
    assert_eq!(Task::config(&stepper).retry_mode.max_attempts(), 1);
    assert_eq!(Task::name(&stepper), "my-stepped-task");
}

// ---------------------------------------------------------------------------
// 5. Inherent form — struct cursor type with serde
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PageCursor {
    page: u32,
    total: u32,
}

struct PagedProcessor;

#[task::stepped(state = MyState)]
impl PagedProcessor {
    async fn step(
        &self,
        _res: &Resources,
        cursor: Option<PageCursor>,
    ) -> Result<StepOutcome<PageCursor, MyState>, CanoError> {
        let c = cursor.unwrap_or(PageCursor { page: 0, total: 3 });
        if c.page + 1 >= c.total {
            Ok(StepOutcome::Done(TaskResult::Single(MyState::Done)))
        } else {
            Ok(StepOutcome::More(PageCursor {
                page: c.page + 1,
                total: c.total,
            }))
        }
    }
}

#[tokio::test]
async fn struct_cursor_inferred_and_runs() {
    let stepper = PagedProcessor;
    let res = Resources::new();
    let result = Task::run(&stepper, &res).await.unwrap();
    assert_eq!(result, TaskResult::Single(MyState::Done));
}

// ---------------------------------------------------------------------------
// 6. Trait-impl form — explicit SteppedTask<S> for T header
// ---------------------------------------------------------------------------

struct TraitStepper;

#[task::stepped]
impl SteppedTask<MyState> for TraitStepper {
    type Cursor = u32;

    async fn step(
        &self,
        _res: &Resources,
        cursor: Option<u32>,
    ) -> Result<StepOutcome<u32, MyState>, CanoError> {
        let _ = cursor;
        Ok(StepOutcome::Done(TaskResult::Single(MyState::Done)))
    }
}

#[tokio::test]
async fn trait_impl_form_runs() {
    let stepper = TraitStepper;
    let res = Resources::new();
    let result = Task::run(&stepper, &res).await.unwrap();
    assert_eq!(result, TaskResult::Single(MyState::Done));
}

// ---------------------------------------------------------------------------
// 7. Trait-impl form — with config/name overrides
// ---------------------------------------------------------------------------

struct TraitStepperWithOverrides;

#[task::stepped]
impl SteppedTask<MyState> for TraitStepperWithOverrides {
    type Cursor = u32;

    fn config(&self) -> TaskConfig {
        TaskConfig::minimal()
    }

    fn name(&self) -> Cow<'static, str> {
        Cow::Borrowed("trait-stepper-custom")
    }

    async fn step(
        &self,
        _res: &Resources,
        cursor: Option<u32>,
    ) -> Result<StepOutcome<u32, MyState>, CanoError> {
        let _ = cursor;
        Ok(StepOutcome::Done(TaskResult::Single(MyState::Done)))
    }
}

#[test]
fn trait_form_config_override() {
    let stepper = TraitStepperWithOverrides;
    assert_eq!(
        SteppedTask::<MyState>::config(&stepper)
            .retry_mode
            .max_attempts(),
        1
    );
}

#[test]
fn trait_form_name_override() {
    let stepper = TraitStepperWithOverrides;
    assert_eq!(
        SteppedTask::<MyState>::name(&stepper),
        "trait-stepper-custom"
    );
}

// ---------------------------------------------------------------------------
// 8. Dynamic dispatch: Arc<dyn Task<S>>
// ---------------------------------------------------------------------------

#[tokio::test]
async fn stepped_task_as_dyn_task() {
    let stepper: Arc<dyn Task<MyState>> = Arc::new(InherentInferred { total: 1 });
    let res = Resources::new();
    let result = Task::run(stepper.as_ref(), &res).await.unwrap();
    assert_eq!(result, TaskResult::Single(MyState::Done));
}

// ---------------------------------------------------------------------------
// 9. Step count verification via AtomicU32
// ---------------------------------------------------------------------------

struct TrackedStepper {
    calls: Arc<AtomicU32>,
    target: u32,
}

impl TrackedStepper {
    fn new(target: u32) -> (Self, Arc<AtomicU32>) {
        let calls = Arc::new(AtomicU32::new(0));
        (
            Self {
                calls: Arc::clone(&calls),
                target,
            },
            calls,
        )
    }
}

#[task::stepped(state = MyState)]
impl TrackedStepper {
    async fn step(
        &self,
        _res: &Resources,
        cursor: Option<u32>,
    ) -> Result<StepOutcome<u32, MyState>, CanoError> {
        let n = self.calls.fetch_add(1, Ordering::Relaxed);
        let pos = cursor.unwrap_or(0) + 1;
        if n + 1 >= self.target {
            Ok(StepOutcome::Done(TaskResult::Single(MyState::Done)))
        } else {
            Ok(StepOutcome::More(pos))
        }
    }
}

#[tokio::test]
async fn step_count_matches_target() {
    let (stepper, calls) = TrackedStepper::new(5);
    let res = Resources::new();
    let result = Task::run(&stepper, &res).await.unwrap();
    assert_eq!(result, TaskResult::Single(MyState::Done));
    assert_eq!(calls.load(Ordering::Relaxed), 5);
}

// ---------------------------------------------------------------------------
// 10. Workflow integration — register and orchestrate
// ---------------------------------------------------------------------------

#[tokio::test]
async fn stepped_task_in_workflow() {
    let stepper = InherentInferred { total: 4 };

    let workflow = Workflow::bare()
        .register(MyState::Work, stepper)
        .add_exit_state(MyState::Done);

    let result = workflow.orchestrate(MyState::Work).await.unwrap();
    assert_eq!(result, MyState::Done);
}

// ---------------------------------------------------------------------------
// 11. Cursor threading through multiple steps
// ---------------------------------------------------------------------------

struct CursorThreader;

#[task::stepped(state = MyState)]
impl CursorThreader {
    async fn step(
        &self,
        _res: &Resources,
        cursor: Option<Vec<u32>>,
    ) -> Result<StepOutcome<Vec<u32>, MyState>, CanoError> {
        let mut v = cursor.unwrap_or_default();
        v.push(v.len() as u32);
        if v.len() >= 3 {
            Ok(StepOutcome::Done(TaskResult::Single(MyState::Done)))
        } else {
            Ok(StepOutcome::More(v))
        }
    }
}

#[tokio::test]
async fn cursor_accumulates_across_steps() {
    // Manually walk through steps to verify cursor accumulation.
    let stepper = CursorThreader;
    let res = Resources::new();

    let r1 = SteppedTask::step(&stepper, &res, None).await.unwrap();
    let c1 = match r1 {
        StepOutcome::More(c) => c,
        other => panic!("expected More, got {other:?}"),
    };
    assert_eq!(c1, vec![0]);

    let r2 = SteppedTask::step(&stepper, &res, Some(c1)).await.unwrap();
    let c2 = match r2 {
        StepOutcome::More(c) => c,
        other => panic!("expected More, got {other:?}"),
    };
    assert_eq!(c2, vec![0, 1]);

    let r3 = SteppedTask::step(&stepper, &res, Some(c2)).await.unwrap();
    assert_eq!(
        r3,
        StepOutcome::Done(TaskResult::Single(MyState::Done)),
        "third step should be Done"
    );
}

// ---------------------------------------------------------------------------
// 12. Error propagation from step
// ---------------------------------------------------------------------------

struct ErrorStepper;

#[task::stepped(state = MyState)]
impl ErrorStepper {
    async fn step(
        &self,
        _res: &Resources,
        _cursor: Option<u32>,
    ) -> Result<StepOutcome<u32, MyState>, CanoError> {
        Err(CanoError::task_execution("step failed"))
    }
}

#[tokio::test]
async fn error_in_step_propagates() {
    let stepper = ErrorStepper;
    let res = Resources::new();
    let err = Task::run(&stepper, &res).await.unwrap_err();
    assert!(matches!(err, CanoError::TaskExecution(_)));
}

// ---------------------------------------------------------------------------
// 13. StepOutcome::Done with Split result
// ---------------------------------------------------------------------------

struct SplitStepper;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[allow(dead_code)]
enum SplitState {
    Work,
    A,
    B,
    Done,
}

#[task::stepped(state = SplitState)]
impl SplitStepper {
    async fn step(
        &self,
        _res: &Resources,
        _cursor: Option<u32>,
    ) -> Result<StepOutcome<u32, SplitState>, CanoError> {
        Ok(StepOutcome::Done(TaskResult::Split(vec![
            SplitState::A,
            SplitState::B,
        ])))
    }
}

#[tokio::test]
async fn stepped_task_done_with_split() {
    let stepper = SplitStepper;
    let res = Resources::new();
    let result = Task::run(&stepper, &res).await.unwrap();
    assert_eq!(
        result,
        TaskResult::Split(vec![SplitState::A, SplitState::B])
    );
}
