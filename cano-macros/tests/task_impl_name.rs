//! UI test: `#[cano::task(state = S)]` on an inherent `impl T { ... }` block
//! accepts a `name()` override (the friendly task id reported to observers),
//! alongside `run` / `run_bare` / `config`.

use cano::prelude::*;
use std::borrow::Cow;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Step {
    Start,
    Done,
}

#[derive(Clone)]
struct NamedInherentTask;

#[task(state = Step)]
impl NamedInherentTask {
    fn name(&self) -> Cow<'static, str> {
        "named-inherent".into()
    }

    fn config(&self) -> TaskConfig {
        TaskConfig::minimal()
    }

    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        Ok(TaskResult::Single(Step::Done))
    }
}

#[derive(Clone)]
struct DefaultNameTask;

#[task(state = Step)]
impl DefaultNameTask {
    async fn run_bare(&self) -> Result<TaskResult<Step>, CanoError> {
        Ok(TaskResult::Single(Step::Done))
    }
}

#[test]
fn overridden_name_is_used() {
    assert_eq!(NamedInherentTask.name(), "named-inherent");
}

#[test]
fn default_name_is_type_name() {
    // The blanket default returns `std::any::type_name::<Self>()`.
    assert!(DefaultNameTask.name().contains("DefaultNameTask"));
}

#[tokio::test]
async fn task_still_runs_in_a_workflow() {
    let workflow = Workflow::bare()
        .register(Step::Start, NamedInherentTask)
        .add_exit_state(Step::Done);
    assert_eq!(workflow.orchestrate(Step::Start).await.unwrap(), Step::Done);
}
