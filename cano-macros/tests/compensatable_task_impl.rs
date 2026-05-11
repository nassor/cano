//! UI test: `#[cano::saga::compensatable_task(state = S)]` on an inherent `impl T { ... }`
//! block builds the `impl CompensatableTask<S> for T` header — only `type Output`,
//! `run`, and `compensate` need writing (with optional `config` / `name`).

use cano::prelude::*;
use std::borrow::Cow;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step {
    Reserve,
    Boom,
    Done,
}

#[derive(Clone)]
struct Reserve(Arc<AtomicBool>);

#[saga::compensatable_task(state = Step)]
impl Reserve {
    type Output = u32;
    async fn run(&self, _res: &Resources) -> Result<(TaskResult<Step>, u32), CanoError> {
        Ok((TaskResult::Single(Step::Boom), 42))
    }
    async fn compensate(&self, _res: &Resources, output: u32) -> Result<(), CanoError> {
        assert_eq!(output, 42);
        self.0.store(true, Ordering::SeqCst);
        Ok(())
    }
}

/// Exercises `config` / `name` overrides and `Output = ()` in the inherent form.
#[derive(Clone)]
struct ReserveNamed;

#[saga::compensatable_task(state = Step)]
impl ReserveNamed {
    type Output = ();
    fn config(&self) -> TaskConfig {
        TaskConfig::minimal()
    }
    fn name(&self) -> Cow<'static, str> {
        "reserve-named".into()
    }
    async fn run(&self, _res: &Resources) -> Result<(TaskResult<Step>, ()), CanoError> {
        Ok((TaskResult::Single(Step::Done), ()))
    }
    async fn compensate(&self, _res: &Resources, _output: ()) -> Result<(), CanoError> {
        Ok(())
    }
}

#[derive(Clone)]
struct Boom;

#[task(state = Step)]
impl Boom {
    fn config(&self) -> TaskConfig {
        TaskConfig::minimal() // fail-fast so the original error surfaces directly
    }
    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        Err(CanoError::task_execution("boom"))
    }
}

#[tokio::test]
async fn inherent_compensatable_impl_registers_and_compensates() {
    let compensated = Arc::new(AtomicBool::new(false));
    let workflow = Workflow::bare()
        .register_with_compensation(Step::Reserve, Reserve(compensated.clone()))
        .register(Step::Boom, Boom)
        .add_exit_state(Step::Done);

    let err = workflow.orchestrate(Step::Reserve).await.unwrap_err();
    assert_eq!(err.message(), "boom"); // clean rollback -> the original failure is surfaced
    assert!(
        compensated.load(Ordering::SeqCst),
        "Reserve::compensate must have run"
    );

    // ReserveNamed exercises the config/name overrides + `Output = ()` in the inherent
    // form; it runs straight through to the exit state with nothing to compensate.
    let workflow = Workflow::bare()
        .register_with_compensation(Step::Reserve, ReserveNamed)
        .add_exit_state(Step::Done);
    assert_eq!(
        workflow.orchestrate(Step::Reserve).await.unwrap(),
        Step::Done
    );
}
